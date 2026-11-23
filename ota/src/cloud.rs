use crate::{HealthCheck, Job, ManifestEnvelope, Updater};
use anyhow::{Context, Result, bail};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
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
