use crate::{HealthCheck, Job, ManifestEnvelope, Updater};
use anyhow::{Context, Result, bail};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::sync::{Mutex, watch};

#[derive(Debug, Clone)]
pub struct CloudPollConfig {
    pub cloud_url: String,
    pub token: Option<String>,
    pub poll_interval: Duration,
    pub health_poll_interval: Duration,
}

impl CloudPollConfig {
    pub fn from_env() -> Option<Self> {
        let cloud_url = std::env::var("DOSO_OTA_CLOUD_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())?;
        let token = std::env::var("DOSO_OTA_CLOUD_TOKEN")
            .ok()
            .filter(|v| !v.trim().is_empty());
        let poll_secs: u64 = std::env::var("DOSO_OTA_POLL_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|v| *v > 0)
            .unwrap_or(15);
        Some(Self {
            cloud_url: cloud_url.trim_end_matches('/').to_owned(),
            token,
            poll_interval: Duration::from_secs(poll_secs),
            health_poll_interval: Duration::from_millis(500),
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudNextJob {
    pub job_id: String,
    pub idempotency_key: String,
    pub manifest: ManifestEnvelope,
}

#[derive(Debug, Clone, Deserialize)]
struct WrappedNextJob {
    job: Option<CloudNextJob>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudJobReport {
    pub phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_downloaded: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalPolicyEntry {
    pub name: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default, alias = "warmup_us")]
    pub warmup_us: u64,
    #[serde(default, alias = "golden_steps")]
    pub golden_steps: usize,
    #[serde(default, alias = "golden_max_err")]
    pub golden_max_err: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalRtLoopMetrics {
    #[serde(default, alias = "max_tick_us")]
    pub max_tick_us: u64,
    #[serde(default, alias = "max_jitter_us")]
    pub max_jitter_us: u64,
    #[serde(default, alias = "max_mutex_wait_us")]
    pub max_mutex_wait_us: u64,
    #[serde(default)]
    pub ticks: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStatusReport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_id: Option<String>,
    pub brainstem_ready: bool,
    pub strategy_ready: bool,
    pub can_read_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_profile: Option<String>,
    #[serde(default)]
    pub slots: BTreeMap<String, String>,
    #[serde(default)]
    pub policies: Vec<LocalPolicyEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rt_loop: Option<LocalRtLoopMetrics>,
}

#[derive(Debug, Deserialize)]
struct LocalHealthJson {
    #[serde(default)]
    boot_id: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    release_id: Option<String>,
    #[serde(default)]
    brainstem_ready: bool,
    #[serde(default)]
    strategy_ready: bool,
    #[serde(default)]
    can_read_only: bool,
    #[serde(default)]
    active_profile: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LocalPoliciesJson {
    #[serde(default)]
    active_profile: Option<String>,
    #[serde(default)]
    slots: BTreeMap<String, String>,
    #[serde(default)]
    policies: Vec<LocalPolicyEntry>,
    #[serde(default)]
    rt_loop: Option<LocalRtLoopMetrics>,
}

/// Collects live firmware version, active profile, loaded policy slots, and RT loop metrics from
/// local `doso-brainstem` (`:5054/health` + `:5054/v1/policies`) for cloud inventory reporting.
pub async fn collect_local_brainstem_status(
    admin_base_url: &str,
    admin_token: Option<&str>,
) -> Result<DeviceStatusReport> {
    let base = admin_base_url.trim_end_matches('/');
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()?;
    let health_resp = client
        .get(format!("{base}/health"))
        .send()
        .await
        .context("local_health_send")?;
    let health: LocalHealthJson =
        serde_json::from_slice(&health_resp.bytes().await?).context("local_health_json")?;

    let mut pol_req = client.get(format!("{base}/v1/policies"));
    if let Some(tok) = admin_token.filter(|t| !t.trim().is_empty()) {
        pol_req = pol_req.bearer_auth(tok);
    }
    let pol_opt: Option<LocalPoliciesJson> = match pol_req.send().await {
        Ok(resp) if resp.status().is_success() => resp
            .bytes()
            .await
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok()),
        _ => None,
    };

    let (active_profile, slots, policies, rt_loop) = match pol_opt {
        Some(p) => (
            p.active_profile.or(health.active_profile),
            p.slots,
            p.policies,
            p.rt_loop,
        ),
        None => (health.active_profile, BTreeMap::new(), Vec::new(), None),
    };

    Ok(DeviceStatusReport {
        boot_id: health.boot_id,
        firmware_version: health.version,
        release_id: health.release_id,
        brainstem_ready: health.brainstem_ready,
        strategy_ready: health.strategy_ready,
        can_read_only: health.can_read_only,
        active_profile,
        slots,
        policies,
        rt_loop,
    })
}

#[derive(Clone)]
pub struct CloudClient {
    base_url: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl CloudClient {
    pub fn new(base_url: String, token: Option<String>) -> Result<Self> {
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(15));
        if url::Url::parse(&base_url)
            .ok()
            .and_then(|u| {
                u.host_str()
                    .map(|h| matches!(h, "127.0.0.1" | "localhost" | "::1"))
            })
            .unwrap_or(false)
        {
            builder = builder.no_proxy();
        }
        let http = builder.build()?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            token: token.filter(|t| !t.trim().is_empty()),
            http,
        })
    }

    pub async fn fetch_next_job(&self, device_id: &str) -> Result<Option<CloudNextJob>> {
        let url = format!("{}/v1/agent/devices/{device_id}/next-job", self.base_url);
        let mut req = self.http.get(&url);
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await.context("cloud_next_job_send")?;
        if resp.status() == StatusCode::NO_CONTENT {
            return Ok(None);
        }
        if !resp.status().is_success() {
            bail!("cloud_next_job_http_{}", resp.status().as_u16());
        }
        let body = resp.bytes().await?;
        if body.is_empty() {
            return Ok(None);
        }
        if let Ok(job) = serde_json::from_slice::<CloudNextJob>(&body) {
            return Ok(Some(job));
        }
        let wrapped: WrappedNextJob =
            serde_json::from_slice(&body).context("cloud_next_job_json")?;
        Ok(wrapped.job)
    }

    pub async fn report_job(
        &self,
        device_id: &str,
        job_id: &str,
        report: &CloudJobReport,
    ) -> Result<()> {
        let url = format!(
            "{}/v1/agent/devices/{device_id}/jobs/{job_id}/report",
            self.base_url
        );
        let mut req = self.http.post(&url).json(report);
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await.context("cloud_report_send")?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            bail!("cloud_report_http_{status}: {text}");
        }
        Ok(())
    }

    pub async fn report_device_status(
        &self,
        device_id: &str,
        status: &DeviceStatusReport,
    ) -> Result<()> {
        let url = format!("{}/v1/agent/devices/{device_id}/status", self.base_url);
        let mut req = self.http.post(&url).json(status);
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await.context("cloud_device_status_send")?;
        if !resp.status().is_success() {
            let code = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            bail!("cloud_device_status_http_{code}: {text}");
        }
        Ok(())
    }
}

fn job_to_report(job: &Job, current_version: &str) -> CloudJobReport {
    CloudJobReport {
        phase: job.phase.as_str().to_owned(),
        bytes_downloaded: Some(job.downloaded_bytes),
        current_version: Some(current_version.to_owned()),
        message: job.error.clone(),
    }
}

fn report_version_for_job(updater: &Updater, job: &Job) -> String {
    if job.manifest.artifact_type == crate::ArtifactType::ComponentRelease
        && let Some(comp) = job.manifest.component.as_deref()
        && let Some(ver) = updater.component_version(comp)
    {
        return ver;
    }
    updater.profile().current_version
}

/// Runs startup recovery for any persisted incomplete job (including reconciling any pending
/// `doso-ota` self-upgrade), reporting the outcome if a cloud client is configured.
pub async fn recover_on_startup<C: HealthCheck + ?Sized + 'static>(
    updater: &Arc<Mutex<Updater>>,
    checker: Option<Arc<C>>,
    cloud: Option<&CloudClient>,
    health_poll_interval: Duration,
) -> Result<Option<Job>> {
    let device_id;
    let mut progress_log: Vec<(String, CloudJobReport)> = Vec::new();
    let result = {
        let guard = updater.lock().await;
        device_id = guard.profile().device_id;
        if let Some(reconciled) = guard.reconcile_self_upgrade(true)? {
            let ver = report_version_for_job(&guard, &reconciled);
            progress_log.push((reconciled.job_id.clone(), job_to_report(&reconciled, &ver)));
            Some(reconciled)
        } else {
            let has_incomplete = guard.job()?.is_some_and(|j| !j.phase.is_terminal());
            if !has_incomplete {
                return Ok(None);
            }
            guard
                .execute_active_job(checker, health_poll_interval, |job| {
                    let ver = report_version_for_job(&guard, job);
                    progress_log.push((job.job_id.clone(), job_to_report(job, &ver)));
                })
                .await?
        }
    };
    if let Some(cloud) = cloud {
        for (job_id, report) in progress_log {
            if let Err(error) = cloud.report_job(&device_id, &job_id, &report).await {
                eprintln!("ota_cloud_report_error: {error}");
            }
        }
    }
    Ok(result)
}

/// Performs one cloud poll cycle: retries any local job parked in `WaitingSafeWindow`, or checks
/// `next-job`, schedules & executes the job end-to-end, and reports every phase transition back
/// to `cloud/release`.
pub async fn poll_and_execute_once<C: HealthCheck + ?Sized + 'static>(
    updater: &Arc<Mutex<Updater>>,
    checker: Option<Arc<C>>,
    cloud: &CloudClient,
    health_poll_interval: Duration,
) -> Result<Option<Job>> {
    // If a local job is waiting for a safe robot window, retry it before fetching new work.
    {
        let mut waiting_reports: Vec<(String, CloudJobReport)> = Vec::new();
        let (device_id, exec_res) = {
            let guard = updater.lock().await;
            let device_id = guard.profile().device_id;
            let is_waiting = guard
                .job()?
                .is_some_and(|j| j.phase == crate::JobPhase::WaitingSafeWindow);
            if is_waiting {
                let res = guard
                    .execute_active_job(checker.clone(), health_poll_interval, |job| {
                        let ver = report_version_for_job(&guard, job);
                        waiting_reports.push((job.job_id.clone(), job_to_report(job, &ver)));
                    })
                    .await;
                (device_id, Some(res))
            } else {
                (device_id, None)
            }
        };
        for (job_id, report) in &waiting_reports {
            if let Err(error) = cloud.report_job(&device_id, job_id, report).await {
                eprintln!("ota_cloud_report_error: {error}");
            }
        }
        if let Some(res) = exec_res
            && let Some(job) = res?
        {
            return Ok(Some(job));
        }
    }

    let device_id = { updater.lock().await.profile().device_id };
    let Some(next) = cloud.fetch_next_job(&device_id).await? else {
        return Ok(None);
    };
    let mut progress_log: Vec<CloudJobReport> = Vec::new();
    let exec_res = {
        let guard = updater.lock().await;
        match guard.schedule_with_id(
            &next.manifest,
            &next.idempotency_key,
            Some(next.job_id.clone()),
        ) {
            Ok(_) => {}
            Err(error) => {
                let ver = guard.profile().current_version;
                let report = CloudJobReport {
                    phase: "failed".into(),
                    bytes_downloaded: Some(0),
                    current_version: Some(ver),
                    message: Some(format!("{error:#}")),
                };
                drop(guard);
                if let Err(rep_err) = cloud.report_job(&device_id, &next.job_id, &report).await {
                    eprintln!("ota_cloud_report_error: {rep_err}");
                }
                return Err(error);
            }
        }
        let res = guard
            .execute_active_job(checker, health_poll_interval, |job| {
                let ver = report_version_for_job(&guard, job);
                progress_log.push(job_to_report(job, &ver));
            })
            .await;
        // If a terminal job already existed locally (previous report_job lost to network drop),
        // re-report its terminal phase so cloud/release stops returning it on next-job.
        if let Ok(Some(ref job)) = res
            && progress_log.is_empty()
            && job.phase.is_terminal()
        {
            let ver = report_version_for_job(&guard, job);
            progress_log.push(job_to_report(job, &ver));
        }
        res
    };
    for report in &progress_log {
        if let Err(error) = cloud.report_job(&device_id, &next.job_id, report).await {
            eprintln!("ota_cloud_report_error: {error}");
        }
    }
    exec_res
}

/// Background autonomous cloud polling loop (interruptible by `shutdown_rx`).
pub async fn run_cloud_loop<C: HealthCheck + 'static>(
    updater: Arc<Mutex<Updater>>,
    checker: Option<Arc<C>>,
    config: CloudPollConfig,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let Ok(cloud) = CloudClient::new(config.cloud_url.clone(), config.token.clone()) else {
        eprintln!("ota_cloud_client_init_failed");
        return;
    };
    if let Err(error) = recover_on_startup(
        &updater,
        checker.clone(),
        Some(&cloud),
        config.health_poll_interval,
    )
    .await
    {
        eprintln!("ota_startup_recovery_error: {error}");
    }
    let admin_url = std::env::var("DOSO_POLICY_ADMIN_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:5054".into());
    let admin_token = std::env::var("DOSO_POLICY_ADMIN_TOKEN")
        .ok()
        .filter(|v| !v.trim().is_empty());

    loop {
        if *shutdown_rx.borrow() {
            break;
        }
        if let Err(error) = poll_and_execute_once(
            &updater,
            checker.clone(),
            &cloud,
            config.health_poll_interval,
        )
        .await
        {
            eprintln!("ota_cloud_poll_error: {error}");
        }
        let device_id = { updater.lock().await.profile().device_id };
        if let Ok(status) = collect_local_brainstem_status(&admin_url, admin_token.as_deref()).await
            && let Err(error) = cloud.report_device_status(&device_id, &status).await
        {
            eprintln!("ota_device_status_report_error: {error}");
        }
        tokio::select! {
            _ = tokio::time::sleep(config.poll_interval) => {}
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ArtifactType, JobPhase, Manifest, ManifestEnvelope, RollbackPolicy, ServiceRestarter, Slot,
        SystemHealthCheck, UpdaterConfig,
    };
    use axum::{
        Json, Router,
        extract::State,
        routing::{get, post},
    };
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::{Signer, SigningKey, pkcs8::EncodePublicKey};

    struct NoopRestarter;
    impl ServiceRestarter for NoopRestarter {
        fn restart(&self, _unit: &str) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn collects_local_brainstem_health_and_policies_json() {
        let app = Router::new()
            .route(
                "/health",
                get(|| async {
                    Json(serde_json::json!({
                        "ok": true,
                        "boot_id": "boot-xyz",
                        "version": "v0.0.1",
                        "release_id": "v0.0.1",
                        "brainstem_ready": true,
                        "strategy_ready": true,
                        "can_read_only": true,
                        "active_profile": "flat_w5"
                    }))
                }),
            )
            .route(
                "/v1/policies",
                get(|| async {
                    Json(serde_json::json!({
                        "ok": true,
                        "active_profile": "flat_w5",
                        "slots": { "flat": "flat_w5", "stairs": "stairs_cts_v39" },
                        "policies": [
                            { "name": "flat_w5", "sha256": "deadbeef", "warmup_us": 42, "golden_steps": 1, "golden_max_err": 0.0 }
                        ],
                        "rt_loop": { "max_tick_us": 50, "max_jitter_us": 75, "max_mutex_wait_us": 0, "ticks": 999 }
                    }))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let report = collect_local_brainstem_status(&format!("http://{addr}"), None)
            .await
            .unwrap();
        assert_eq!(report.boot_id.as_deref(), Some("boot-xyz"));
        assert_eq!(report.firmware_version.as_deref(), Some("v0.0.1"));
        assert_eq!(report.active_profile.as_deref(), Some("flat_w5"));
        assert_eq!(
            report.slots.get("stairs").map(String::as_str),
            Some("stairs_cts_v39")
        );
        assert_eq!(report.policies.len(), 1);
        assert_eq!(
            report.rt_loop.as_ref().map(|r| r.max_mutex_wait_us),
            Some(0)
        );
        server.abort();
    }

    #[tokio::test]
    async fn poll_re_reports_already_terminal_job_if_cloud_report_was_previously_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let key = SigningKey::from_bytes(&[41; 32]);
        let key_path = dir.path().join("pub.pem");
        std::fs::write(
            &key_path,
            key.verifying_key()
                .to_public_key_pem(Default::default())
                .unwrap(),
        )
        .unwrap();

        let cfg = UpdaterConfig {
            root: dir.path().join("state"),
            device_id: "dev-rereport".into(),
            model: "THUNDER 01".into(),
            hardware_revision: "rdk-s100p".into(),
            bootloader_version: "1.0.0".into(),
            brainstem_version: "0.0.3".into(),
            current_version: "0.0.3".into(),
            battery_percent: 95,
            online: true,
            task_running: false,
            boot_id_path: dir.path().join("boot_id"),
            verify_key_path: key_path,
            brainstem_unit: "doso-brainstem.service".into(),
            staging_dir: None,
            releases_dir: None,
            current_link: None,
            last_good_link: None,
            components_root: Some(dir.path().join("components")),
            keep_history: 2,
            require_can_read_only: false,
            min_vbus_v: 44.0,
            ota_unit: "doso-ota.service".into(),
            policy_admin_url: None,
            policy_admin_token: None,
            allow_insecure_http_loopback: true,
        };
        let updater = Updater::with_restarter(cfg, Arc::new(NoopRestarter)).unwrap();
        let manifest = Manifest {
            manifest_version: 1,
            release_id: "rel-rereport".into(),
            channel: "qa".into(),
            sequence: 5,
            version: "0.0.4".into(),
            issued_at_ms: 1,
            expires_at_ms: i64::MAX,
            model: "THUNDER 01".into(),
            hardware_revision: "rdk-s100p".into(),
            minimum_bootloader_version: "1.0.0".into(),
            minimum_brainstem_version: "0.0.1".into(),
            minimum_battery_percent: 20,
            artifact_url: "https://example.test/bs.tar.gz".into(),
            artifact_size_bytes: 3,
            artifact_sha256: Manifest::artifact_hash(b"abc"),
            key_id: "k1".into(),
            rollback_policy: RollbackPolicy {
                mode: "automatic".into(),
                health_timeout_ms: 1000,
            },
            artifact_type: ArtifactType::BrainstemRelease,
            policy_slot: None,
            shadow_frames: None,
            activate: None,
            component: None,
            systemd_unit: None,
            health_url: None,
            requires: BTreeMap::new(),
            bundle_items: Vec::new(),
            delta: None,
        };
        let payload = serde_json::to_vec(&manifest).unwrap();
        let env = ManifestEnvelope {
            payload: URL_SAFE_NO_PAD.encode(&payload),
            signature: URL_SAFE_NO_PAD.encode(key.sign(&payload).to_bytes()),
        };

        // Persist a locally completed terminal job for idem-dup (simulating prior missed cloud report).
        let done_job = Job {
            job_id: "job-dup-1".into(),
            idempotency_key: "idem-dup".into(),
            device_id: "dev-rereport".into(),
            release_id: "rel-rereport".into(),
            version: "0.0.4".into(),
            manifest: manifest.clone(),
            phase: JobPhase::Succeeded,
            downloaded_bytes: 3,
            total_bytes: 3,
            active_slot: Slot::A,
            last_known_good_slot: Slot::A,
            last_known_good_version: "0.0.3".into(),
            progress_sequence: 4,
            boot_id_before: None,
            boot_id_after: None,
            error: None,
            previous_release_path: None,
            installed_release_path: None,
        };
        std::fs::write(
            dir.path().join("state/job.json"),
            serde_json::to_vec_pretty(&done_job).unwrap(),
        )
        .unwrap();

        let reports: Arc<std::sync::Mutex<Vec<serde_json::Value>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let reports_srv = reports.clone();
        let env_srv = env.clone();
        let app = Router::new()
            .route(
                "/v1/agent/devices/{id}/next-job",
                get(move || {
                    let e = env_srv.clone();
                    async move {
                        Json(serde_json::json!({
                            "jobId": "job-dup-1",
                            "idempotencyKey": "idem-dup",
                            "manifest": e
                        }))
                    }
                }),
            )
            .route(
                "/v1/agent/devices/{id}/jobs/{job_id}/report",
                post(
                    move |State(rep): State<Arc<std::sync::Mutex<Vec<serde_json::Value>>>>,
                          Json(body): Json<serde_json::Value>| async move {
                        rep.lock().unwrap().push(body);
                        Json(serde_json::json!({"ok": true}))
                    },
                ),
            )
            .with_state(reports_srv);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let client = CloudClient::new(format!("http://{addr}"), None).unwrap();
        let updater = Arc::new(Mutex::new(updater));
        let res = poll_and_execute_once::<SystemHealthCheck>(
            &updater,
            None,
            &client,
            Duration::from_millis(10),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(res.phase, JobPhase::Succeeded);
        let posted = reports.lock().unwrap();
        assert_eq!(posted.len(), 1);
        assert_eq!(posted[0]["phase"], "succeeded");
        server.abort();
    }
}
