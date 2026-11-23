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
        let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(15));
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

/// Runs startup recovery for any persisted incomplete job, reporting the outcome if a cloud
/// client is configured.
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
        let has_incomplete = guard.job()?.is_some_and(|j| !j.phase.is_terminal());
        if !has_incomplete {
            return Ok(None);
        }
        guard
            .execute_active_job(checker, health_poll_interval, |job| {
                let ver = guard.profile().current_version;
                progress_log.push((job.job_id.clone(), job_to_report(job, &ver)));
            })
            .await?
    };
    if let Some(cloud) = cloud {
        for (job_id, report) in progress_log {
            let _ = cloud.report_job(&device_id, &job_id, &report).await;
        }
    }
    Ok(result)
}

/// Performs one cloud poll cycle: checks `next-job`, schedules & executes the job end-to-end,
/// and reports every phase transition back to `cloud/release`.
pub async fn poll_and_execute_once<C: HealthCheck + ?Sized + 'static>(
    updater: &Arc<Mutex<Updater>>,
    checker: Option<Arc<C>>,
    cloud: &CloudClient,
    health_poll_interval: Duration,
) -> Result<Option<Job>> {
    let device_id = { updater.lock().await.profile().device_id };
    let Some(next) = cloud.fetch_next_job(&device_id).await? else {
        return Ok(None);
    };
    let mut progress_log: Vec<CloudJobReport> = Vec::new();
    let executed = {
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
                    message: Some(error.to_string()),
                };
                drop(guard);
                let _ = cloud.report_job(&device_id, &next.job_id, &report).await;
                return Err(error);
            }
        }
        guard
            .execute_active_job(checker, health_poll_interval, |job| {
                let ver = guard.profile().current_version;
                progress_log.push(job_to_report(job, &ver));
            })
            .await?
    };
    for report in &progress_log {
        let _ = cloud.report_job(&device_id, &next.job_id, report).await;
    }
    Ok(executed)
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
    use axum::{Json, Router, routing::get};

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
}
