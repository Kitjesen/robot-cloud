use crate::{
    archive, download,
    health::{HealthCheck, HealthReport},
    manifest::{ArtifactType, Manifest, ManifestEnvelope, normalize_version},
    policy::PolicyAdminClient,
};
use anyhow::{Context, Result, ensure};
use ed25519_dalek::{VerifyingKey, pkcs8::DecodePublicKey};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Slot {
    A,
    B,
}

impl Slot {
    pub fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobPhase {
    Scheduled,
    Downloading,
    Verifying,
    Installing,
    Rebooting,
    HealthChecking,
    Succeeded,
    RollbackPending,
    RollingBack,
    RolledBack,
    RecoveryRequired,
    Failed,
    Cancelled,
}

impl JobPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Downloading => "downloading",
            Self::Verifying => "verifying",
            Self::Installing => "installing",
            Self::Rebooting => "rebooting",
            Self::HealthChecking => "health_checking",
            Self::Succeeded => "succeeded",
            Self::RollbackPending => "rollback_pending",
            Self::RollingBack => "rolling_back",
            Self::RolledBack => "rolled_back",
            Self::RecoveryRequired => "recovery_required",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::RolledBack
                | Self::Failed
                | Self::RecoveryRequired
                | Self::Cancelled
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceProfile {
    pub device_id: String,
    pub model: String,
    pub hardware_revision: String,
    pub bootloader_version: String,
    pub brainstem_version: String,
    pub current_version: String,
    pub active_slot: Slot,
    pub battery_percent: u8,
    pub online: bool,
    pub task_running: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub job_id: String,
    pub idempotency_key: String,
    pub device_id: String,
    pub release_id: String,
    pub version: String,
    pub manifest: Manifest,
    pub phase: JobPhase,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub active_slot: Slot,
    pub last_known_good_slot: Slot,
    pub last_known_good_version: String,
    pub progress_sequence: u64,
    pub boot_id_before: Option<String>,
    pub boot_id_after: Option<String>,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_release_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_release_path: Option<String>,
}

pub trait ServiceRestarter: Send + Sync {
    fn restart(&self, unit: &str) -> Result<()>;
}

#[derive(Debug, Default)]
pub struct SystemdRestarter;

impl ServiceRestarter for SystemdRestarter {
    fn restart(&self, unit: &str) -> Result<()> {
        if std::env::var("DOSO_OTA_SKIP_RESTART").as_deref() == Ok("1") {
            return Ok(());
        }
        // Clear any prior failed/start-limit state (e.g., after a broken release crash loop)
        // so rollback unconditionally revives the service.
        let _ = Command::new("systemctl")
            .args(["reset-failed", "--", unit])
            .status();
        let status = Command::new("systemctl")
            .args(["restart", "--no-block", "--", unit])
            .status()
            .context("systemctl_restart")?;
        ensure!(status.success(), "brainstem_restart_failed");
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct UpdaterConfig {
    pub root: PathBuf,
    pub device_id: String,
    pub model: String,
    pub hardware_revision: String,
    pub bootloader_version: String,
    pub brainstem_version: String,
    pub current_version: String,
    pub battery_percent: u8,
    pub online: bool,
    pub task_running: bool,
    pub boot_id_path: PathBuf,
    pub verify_key_path: PathBuf,
    pub brainstem_unit: String,
    pub staging_dir: Option<PathBuf>,
    pub releases_dir: Option<PathBuf>,
    pub current_link: Option<PathBuf>,
    pub last_good_link: Option<PathBuf>,
    pub policy_admin_url: Option<String>,
    pub policy_admin_token: Option<String>,
    pub allow_insecure_http_loopback: bool,
}

pub struct Updater {
    config: UpdaterConfig,
    profile: Mutex<DeviceProfile>,
    verify_key: VerifyingKey,
    restarter: Arc<dyn ServiceRestarter>,
}

impl Updater {
    pub fn new(config: UpdaterConfig) -> Result<Self> {
        Self::with_restarter(config, Arc::new(SystemdRestarter))
    }

    pub fn with_restarter(
        config: UpdaterConfig,
        restarter: Arc<dyn ServiceRestarter>,
    ) -> Result<Self> {
        validate_unit_name(&config.brainstem_unit)?;
        let verify_key = load_public_key(&config.verify_key_path)?;
        let staging = config
            .staging_dir
            .clone()
            .unwrap_or_else(|| config.root.join("staging"));
        fs::create_dir_all(&staging)?;
        fs::create_dir_all(config.root.join("slots/a"))?;
        fs::create_dir_all(config.root.join("slots/b"))?;
        if let Some(releases_dir) = &config.releases_dir {
            fs::create_dir_all(releases_dir)?;
        }
        let persisted_version = read_trimmed(&config.root.join("current-version"))
            .unwrap_or_else(|| config.current_version.clone());
        let profile = DeviceProfile {
            device_id: config.device_id.clone(),
            model: config.model.clone(),
            hardware_revision: config.hardware_revision.clone(),
            bootloader_version: config.bootloader_version.clone(),
            brainstem_version: config.brainstem_version.clone(),
            current_version: persisted_version,
            active_slot: read_slot(&config.root.join("current-slot")).unwrap_or(Slot::A),
            battery_percent: config.battery_percent,
            online: config.online,
            task_running: config.task_running,
        };
        Ok(Self {
            config,
            profile: Mutex::new(profile),
            verify_key,
            restarter,
        })
    }

    pub fn profile(&self) -> DeviceProfile {
        self.profile
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn config(&self) -> &UpdaterConfig {
        &self.config
    }

    pub fn staging_dir(&self) -> PathBuf {
        self.config
            .staging_dir
            .clone()
            .unwrap_or_else(|| self.config.root.join("staging"))
    }

    pub fn is_release_link_mode(&self) -> bool {
        self.config.releases_dir.is_some()
    }

    fn current_link_path(&self) -> PathBuf {
        self.config
            .current_link
            .clone()
            .unwrap_or_else(|| PathBuf::from("/opt/doso/current"))
    }

    fn last_good_link_path(&self) -> PathBuf {
        self.config
            .last_good_link
            .clone()
            .unwrap_or_else(|| PathBuf::from("/opt/doso/last-known-good"))
    }

    pub fn policy_client(&self) -> PolicyAdminClient {
        let url = self
            .config
            .policy_admin_url
            .clone()
            .unwrap_or_else(|| "http://127.0.0.1:5054".into());
        PolicyAdminClient::new(url, self.config.policy_admin_token.clone())
    }

    pub fn job(&self) -> Result<Option<Job>> {
        read_json(&self.config.root.join("job.json"))
    }

    pub fn cancel(&self) -> Result<Job> {
        let mut job = self.required_job()?;
        ensure!(
            matches!(job.phase, JobPhase::Scheduled | JobPhase::Downloading),
            "ota_cancel_phase_invalid"
        );
        job.phase = JobPhase::Cancelled;
        job.progress_sequence += 1;
        write_json(&self.config.root.join("job.json"), &job)?;
        Ok(job)
    }

    pub fn schedule(&self, envelope: &ManifestEnvelope, idempotency_key: &str) -> Result<Job> {
        self.schedule_with_id(envelope, idempotency_key, None)
    }

    pub fn schedule_with_id(
        &self,
        envelope: &ManifestEnvelope,
        idempotency_key: &str,
        job_id_override: Option<String>,
    ) -> Result<Job> {
        ensure!(
            !idempotency_key.trim().is_empty(),
            "ota_idempotency_key_required"
        );
        if let Some(job) = self.job().context("read existing ota job")? {
            if job.idempotency_key == idempotency_key {
                return Ok(job);
            }
            ensure!(job.phase.is_terminal(), "ota_job_already_active");
        }
        let manifest = envelope
            .decode(&self.verify_key)
            .context("decode ota manifest")?;
        let last_sequence = self
            .job()?
            .filter(|job| matches!(job.phase, JobPhase::Succeeded))
            .map(|job| job.manifest.sequence)
            .unwrap_or(0);
        let allow_http = self.config.allow_insecure_http_loopback
            || std::env::var("DOSO_OTA_ALLOW_HTTP").as_deref() == Ok("1");
        let profile = self.profile();
        manifest
            .validate_with_http_policy(now_ms(), &profile, last_sequence, allow_http)
            .context("validate ota manifest")?;
        let active = profile.active_slot;
        let previous_release_path = if self.is_release_link_mode() {
            read_link_target(&self.current_link_path()).map(|p| p.to_string_lossy().into_owned())
        } else {
            None
        };
        let job = Job {
            job_id: job_id_override.unwrap_or_else(|| Uuid::new_v4().to_string()),
            idempotency_key: idempotency_key.to_owned(),
            device_id: profile.device_id.clone(),
            release_id: manifest.release_id.clone(),
            version: manifest.version.clone(),
            manifest: manifest.clone(),
            phase: JobPhase::Scheduled,
            downloaded_bytes: 0,
            total_bytes: manifest.artifact_size_bytes,
            active_slot: active,
            last_known_good_slot: active,
            last_known_good_version: profile.current_version.clone(),
            progress_sequence: 1,
            boot_id_before: read_boot_id(&self.config.boot_id_path),
            boot_id_after: None,
            error: None,
            previous_release_path,
            installed_release_path: None,
        };
        write_json(&self.config.root.join("job.json"), &job).context("persist ota job")?;
        Ok(job)
    }

    pub async fn download(&self) -> Result<Job> {
        let mut job = self.required_job()?;
        ensure!(
            matches!(job.phase, JobPhase::Scheduled | JobPhase::Downloading),
            "ota_download_phase_invalid"
        );
        let manifest = job.manifest.clone();
        job.phase = JobPhase::Downloading;
        write_json(&self.config.root.join("job.json"), &job)?;
        let destination = self.staging_artifact_path(&job.job_id);
        download::download_resumable(
            &manifest.artifact_url,
            &destination,
            manifest.artifact_size_bytes,
        )
        .await?;
        download::verify_hash(
            &destination,
            manifest.artifact_size_bytes,
            Some(&manifest.artifact_sha256),
        )?;
        job.downloaded_bytes = manifest.artifact_size_bytes;
        job.phase = JobPhase::Verifying;
        job.progress_sequence += 1;
        write_json(&self.config.root.join("job.json"), &job)?;
        Ok(job)
    }

    /// Installs a verified Channel A `brainstem_release` artifact (either canonical `/opt/doso`
    /// tarball + atomic symlink swap when `releases_dir` is set, or legacy `slots/{a,b}` copy).
    pub fn install(&self) -> Result<Job> {
        let mut job = self.required_job()?;
        ensure!(
            matches!(job.phase, JobPhase::Verifying),
            "ota_install_phase_invalid"
        );
        ensure!(
            job.manifest.artifact_type == ArtifactType::BrainstemRelease,
            "ota_policy_requires_async_install"
        );
        let manifest = job.manifest.clone();
        let artifact = self.staging_artifact_path(&job.job_id);
        download::verify_hash(
            &artifact,
            manifest.artifact_size_bytes,
            Some(&manifest.artifact_sha256),
        )?;

        if let Some(releases_dir) = &self.config.releases_dir {
            validate_release_dir_segment(&manifest.version)?;
            let target_release = releases_dir.join(&manifest.version);
            job.phase = JobPhase::Installing;
            job.progress_sequence += 1;
            write_json(&self.config.root.join("job.json"), &job)?;

            archive::extract_release_tar_gz(&artifact, &target_release)?;

            let current_link = self.current_link_path();
            let last_good_link = self.last_good_link_path();
            let prev_target = read_link_target(&current_link);
            if let Some(prev) = &prev_target
                && prev != &target_release
            {
                atomic_swap_link(&last_good_link, prev)?;
                job.previous_release_path = Some(prev.to_string_lossy().into_owned());
            }
            atomic_swap_link(&current_link, &target_release)?;
            job.installed_release_path = Some(target_release.to_string_lossy().into_owned());
            job.phase = JobPhase::Rebooting;
            job.progress_sequence += 1;
            write_json(&self.config.root.join("job.json"), &job)?;

            if let Err(error) = self.restarter.restart(&self.config.brainstem_unit) {
                if let Some(prev) = &job.previous_release_path {
                    let _ = atomic_swap_link(&current_link, Path::new(prev));
                }
                job.phase = JobPhase::Failed;
                job.error = Some(error.to_string());
                job.progress_sequence += 1;
                write_json(&self.config.root.join("job.json"), &job)?;
                return Err(error);
            }
            return Ok(job);
        }

        // Legacy A/B slot bundle installation path.
        let target_slot = job.active_slot.other();
        let target = self
            .config
            .root
            .join(format!("slots/{}/brainstem.bundle", slot_name(target_slot)));
        download::copy_verified(
            &artifact,
            &target,
            manifest.artifact_size_bytes,
            &manifest.artifact_sha256,
        )?;
        write_json(&target.with_extension("json"), &manifest)?;
        write_slot(&self.config.root.join("current-slot"), target_slot)?;
        job.active_slot = target_slot;
        job.phase = JobPhase::Rebooting;
        job.progress_sequence += 1;
        write_json(&self.config.root.join("job.json"), &job)?;
        if let Err(error) = self.restarter.restart(&self.config.brainstem_unit) {
            let _ = write_slot(
                &self.config.root.join("current-slot"),
                job.last_known_good_slot,
            );
            job.phase = JobPhase::Failed;
            job.error = Some(error.to_string());
            job.progress_sequence += 1;
            write_json(&self.config.root.join("job.json"), &job)?;
            return Err(error);
        }
        Ok(job)
    }

    /// Installs a verified Channel B `policy_package` via zero-restart `:5054/v1/policies/push`.
    /// Never restarts `doso-brainstem.service`; automatically rolls back slot on post-mutation failure.
    pub async fn install_policy(&self) -> Result<Job> {
        let mut job = self.required_job()?;
        ensure!(
            matches!(job.phase, JobPhase::Verifying | JobPhase::Installing),
            "ota_policy_install_phase_invalid"
        );
        ensure!(
            job.manifest.artifact_type == ArtifactType::PolicyPackage,
            "ota_not_policy_package"
        );
        let artifact = self.staging_artifact_path(&job.job_id);
        download::verify_hash(
            &artifact,
            job.manifest.artifact_size_bytes,
            Some(&job.manifest.artifact_sha256),
        )?;
        let extract_dir = self.staging_dir().join(format!("{}.policy", job.job_id));
        archive::extract_policy_tar_gz(&artifact, &extract_dir)?;

        job.phase = JobPhase::Installing;
        job.progress_sequence += 1;
        write_json(&self.config.root.join("job.json"), &job)?;

        let client = self.policy_client();
        let push_result = client.push_policy_dir(&extract_dir, &job.manifest).await;
        let _ = fs::remove_dir_all(&extract_dir);

        match push_result {
            Ok(_) => {
                job.phase = JobPhase::Succeeded;
                job.progress_sequence += 1;
                let _ = fs::remove_file(&artifact);
                write_json(&self.config.root.join("job.json"), &job)?;
                Ok(job)
            }
            Err(push_err) => {
                let err_msg = push_err.to_string();
                if !push_err.is_pre_mutation_gate()
                    && let Some(slot) = job.manifest.policy_slot.clone()
                {
                    job.phase = JobPhase::RollbackPending;
                    job.error = Some(err_msg.clone());
                    job.progress_sequence += 1;
                    write_json(&self.config.root.join("job.json"), &job)?;

                    job.phase = JobPhase::RollingBack;
                    job.progress_sequence += 1;
                    write_json(&self.config.root.join("job.json"), &job)?;

                    match client.rollback_slot(&slot).await {
                        Ok(_) => {
                            job.phase = JobPhase::RolledBack;
                        }
                        Err(rb_err) => {
                            job.phase = JobPhase::RecoveryRequired;
                            job.error =
                                Some(format!("{err_msg}; policy_rollback_failed: {rb_err}"));
                        }
                    }
                    let _ = fs::remove_file(&artifact);
                    job.progress_sequence += 1;
                    write_json(&self.config.root.join("job.json"), &job)?;
                    return Ok(job);
                }
                job.phase = JobPhase::Failed;
                job.error = Some(err_msg);
                job.progress_sequence += 1;
                let _ = fs::remove_file(&artifact);
                write_json(&self.config.root.join("job.json"), &job)?;
                Ok(job)
            }
        }
    }

    pub fn check_health<C: HealthCheck + ?Sized>(&self, checker: &C) -> Result<Job> {
        let mut job = self.required_job()?;
        ensure!(
            matches!(job.phase, JobPhase::Rebooting | JobPhase::HealthChecking),
            "ota_health_phase_invalid"
        );
        job.phase = JobPhase::HealthChecking;
        let report = checker.check();
        match report {
            Ok(health) => match self.finish_health(job.clone(), health, true) {
                Ok(succeeded) => Ok(succeeded),
                Err(error) => {
                    job.phase = JobPhase::RollbackPending;
                    job.error = Some(error.to_string());
                    job.progress_sequence += 1;
                    write_json(&self.config.root.join("job.json"), &job)?;
                    Ok(job)
                }
            },
            Err(error) => {
                job.phase = JobPhase::RollbackPending;
                job.error = Some(error.to_string());
                job.progress_sequence += 1;
                write_json(&self.config.root.join("job.json"), &job)?;
                Ok(job)
            }
        }
    }

    /// Polls `checker` until `rollback_policy.health_timeout_ms` expires, then either commits
    /// `Succeeded` or automatically performs rollback to `last-known-good`.
    pub async fn wait_and_verify_health<C: HealthCheck + ?Sized + 'static>(
        &self,
        checker: Arc<C>,
        poll_interval: Duration,
    ) -> Result<Job> {
        let mut job = self.required_job()?;
        ensure!(
            matches!(job.phase, JobPhase::Rebooting | JobPhase::HealthChecking),
            "ota_health_phase_invalid"
        );
        job.phase = JobPhase::HealthChecking;
        job.progress_sequence += 1;
        write_json(&self.config.root.join("job.json"), &job)?;

        let timeout = Duration::from_millis(job.manifest.rollback_policy.health_timeout_ms);
        let deadline = Instant::now() + timeout;

        let last_error = loop {
            let checker_clone = checker.clone();
            let check_res = tokio::task::spawn_blocking(move || checker_clone.check())
                .await
                .map_err(|e| anyhow::anyhow!("health_check_join: {e}"))
                .and_then(|r| r);
            let err = match check_res {
                Ok(report) => match self.finish_health(job.clone(), report, true) {
                    Ok(succeeded) => return Ok(succeeded),
                    Err(error) => error.to_string(),
                },
                Err(error) => error.to_string(),
            };
            if Instant::now() >= deadline {
                break err;
            }
            tokio::time::sleep(poll_interval).await;
        };

        job.phase = JobPhase::RollbackPending;
        job.error = Some(last_error);
        job.progress_sequence += 1;
        write_json(&self.config.root.join("job.json"), &job)?;
        self.rollback_with_poll(checker, poll_interval).await
    }

    pub fn rollback<C: HealthCheck + ?Sized>(&self, checker: &C) -> Result<Job> {
        let mut job = self.required_job()?;
        ensure!(
            matches!(job.phase, JobPhase::RollbackPending | JobPhase::RollingBack),
            "ota_rollback_phase_invalid"
        );
        job.phase = JobPhase::RollingBack;
        write_json(&self.config.root.join("job.json"), &job)?;
        if let Err(error) = self.restore_previous_target(&job) {
            job.phase = JobPhase::RecoveryRequired;
            job.error = Some(error.to_string());
            job.progress_sequence += 1;
            write_json(&self.config.root.join("job.json"), &job)?;
            return Ok(job);
        }
        if let Err(error) = self.restarter.restart(&self.config.brainstem_unit) {
            job.phase = JobPhase::RecoveryRequired;
            job.error = Some(error.to_string());
            job.progress_sequence += 1;
            write_json(&self.config.root.join("job.json"), &job)?;
            return Ok(job);
        }
        self.evaluate_rollback_health(job, checker.check())
    }

    pub async fn rollback_with_poll<C: HealthCheck + ?Sized + 'static>(
        &self,
        checker: Arc<C>,
        poll_interval: Duration,
    ) -> Result<Job> {
        let mut job = self.required_job()?;
        ensure!(
            matches!(job.phase, JobPhase::RollbackPending | JobPhase::RollingBack),
            "ota_rollback_phase_invalid"
        );
        job.phase = JobPhase::RollingBack;
        job.progress_sequence += 1;
        write_json(&self.config.root.join("job.json"), &job)?;
        if let Err(error) = self.restore_previous_target(&job) {
            job.phase = JobPhase::RecoveryRequired;
            job.error = Some(error.to_string());
            job.progress_sequence += 1;
            write_json(&self.config.root.join("job.json"), &job)?;
            return Ok(job);
        }
        if let Err(error) = self.restarter.restart(&self.config.brainstem_unit) {
            job.phase = JobPhase::RecoveryRequired;
            job.error = Some(error.to_string());
            job.progress_sequence += 1;
            write_json(&self.config.root.join("job.json"), &job)?;
            return Ok(job);
        }
        let timeout = Duration::from_millis(job.manifest.rollback_policy.health_timeout_ms)
            .max(Duration::from_millis(1000));
        let deadline = Instant::now() + timeout;
        let last_check = loop {
            let checker_clone = checker.clone();
            let check_res = tokio::task::spawn_blocking(move || checker_clone.check())
                .await
                .map_err(|e| anyhow::anyhow!("health_check_join: {e}"))
                .and_then(|r| r);
            if let Ok(ref health) = check_res
                && self.rollback_health_ok(&job, health)
            {
                break check_res;
            }
            if Instant::now() >= deadline {
                break check_res;
            }
            tokio::time::sleep(poll_interval).await;
        };
        self.evaluate_rollback_health(job, last_check)
    }

    /// Runs an active or interrupted persisted job all the way to a terminal phase
    /// (supports both Channel A release upgrades and Channel B policy hot-OTA, as well
    /// as post-reboot/crash boot recovery).
    pub async fn execute_active_job<C, F>(
        &self,
        checker: Option<Arc<C>>,
        health_poll_interval: Duration,
        mut on_progress: F,
    ) -> Result<Option<Job>>
    where
        C: HealthCheck + ?Sized + 'static,
        F: FnMut(&Job),
    {
        let Some(mut job) = self.job()? else {
            return Ok(None);
        };
        if job.phase.is_terminal() {
            return Ok(Some(job));
        }

        if matches!(job.phase, JobPhase::Scheduled | JobPhase::Downloading) {
            job.phase = JobPhase::Downloading;
            on_progress(&job);
            match self.download().await {
                Ok(next) => {
                    job = next;
                    on_progress(&job);
                }
                Err(error) => {
                    job.phase = JobPhase::Failed;
                    job.error = Some(error.to_string());
                    job.progress_sequence += 1;
                    write_json(&self.config.root.join("job.json"), &job)?;
                    on_progress(&job);
                    return Ok(Some(job));
                }
            }
        }

        if matches!(job.phase, JobPhase::Verifying) {
            match job.manifest.artifact_type {
                ArtifactType::PolicyPackage => {
                    job.phase = JobPhase::Installing;
                    on_progress(&job);
                    job = self.install_policy().await?;
                    on_progress(&job);
                    return Ok(Some(job));
                }
                ArtifactType::BrainstemRelease => {
                    job.phase = JobPhase::Installing;
                    on_progress(&job);
                    match self.install() {
                        Ok(next) => {
                            job = next;
                            on_progress(&job);
                        }
                        Err(error) => {
                            // `install()` already reverted link/slot and persisted Failed.
                            let latest = self.job()?.unwrap_or_else(|| {
                                job.phase = JobPhase::Failed;
                                job.error = Some(error.to_string());
                                job
                            });
                            on_progress(&latest);
                            return Ok(Some(latest));
                        }
                    }
                }
            }
        }

        if matches!(job.phase, JobPhase::Installing) {
            match job.manifest.artifact_type {
                ArtifactType::PolicyPackage => {
                    let artifact = self.staging_artifact_path(&job.job_id);
                    if artifact.is_file() {
                        job = self.install_policy().await?;
                    } else {
                        job.phase = JobPhase::Failed;
                        job.error = Some("interrupted_policy_install_missing_artifact".into());
                        job.progress_sequence += 1;
                        write_json(&self.config.root.join("job.json"), &job)?;
                    }
                    on_progress(&job);
                    return Ok(Some(job));
                }
                ArtifactType::BrainstemRelease => {
                    // Crashed during unpack/symlink swap: check if current_link already points to target.
                    let already_switched = self.is_release_link_mode()
                        && read_link_target(&self.current_link_path())
                            .map(|p| p.to_string_lossy().into_owned())
                            == job.installed_release_path
                        && job.installed_release_path.is_some();
                    if already_switched {
                        job.phase = JobPhase::HealthChecking;
                        job.progress_sequence += 1;
                        write_json(&self.config.root.join("job.json"), &job)?;
                    } else {
                        job.phase = JobPhase::RollbackPending;
                        job.error = Some("interrupted_during_install".into());
                        job.progress_sequence += 1;
                        write_json(&self.config.root.join("job.json"), &job)?;
                    }
                }
            }
        }

        if matches!(job.phase, JobPhase::Rebooting | JobPhase::HealthChecking) {
            let Some(checker) = checker else {
                anyhow::bail!("health_check_not_configured");
            };
            job.phase = JobPhase::HealthChecking;
            on_progress(&job);
            job = self
                .wait_and_verify_health(checker, health_poll_interval)
                .await?;
            on_progress(&job);
            return Ok(Some(job));
        }

        if matches!(job.phase, JobPhase::RollbackPending | JobPhase::RollingBack) {
            let Some(checker) = checker else {
                anyhow::bail!("health_check_not_configured");
            };
            on_progress(&job);
            job = self
                .rollback_with_poll(checker, health_poll_interval)
                .await?;
            on_progress(&job);
            return Ok(Some(job));
        }

        Ok(Some(job))
    }

    fn staging_artifact_path(&self, job_id: &str) -> PathBuf {
        self.staging_dir().join(format!("{job_id}.artifact"))
    }

    fn restore_previous_target(&self, job: &Job) -> Result<()> {
        if self.is_release_link_mode() {
            let previous = job
                .previous_release_path
                .as_deref()
                .map(PathBuf::from)
                .or_else(|| read_link_target(&self.last_good_link_path()))
                .context("last_known_good_release_missing")?;
            atomic_swap_link(&self.current_link_path(), &previous)?;
            Ok(())
        } else {
            write_slot(
                &self.config.root.join("current-slot"),
                job.last_known_good_slot,
            )
        }
    }

    fn rollback_health_ok(&self, job: &Job, health: &HealthReport) -> bool {
        if !(health.ok && health.brainstem_ready && health.strategy_ready && health.can_read_only) {
            return false;
        }
        if self.is_release_link_mode() {
            if !health.version.is_empty() {
                normalize_version(&health.version)
                    == normalize_version(&job.last_known_good_version)
            } else {
                true
            }
        } else {
            normalize_version(&health.version) == normalize_version(&job.last_known_good_version)
                && !health.release_id.is_empty()
        }
    }

    fn evaluate_rollback_health(&self, mut job: Job, check: Result<HealthReport>) -> Result<Job> {
        match check {
            Ok(health) if self.rollback_health_ok(&job, &health) => {
                job.phase = JobPhase::RolledBack;
                job.active_slot = job.last_known_good_slot;
                if !health.boot_id.is_empty() {
                    job.boot_id_after = Some(health.boot_id);
                }
            }
            Ok(_) => {
                job.phase = JobPhase::RecoveryRequired;
                job.error = Some("rollback_health_mismatch".into());
            }
            Err(error) => {
                job.phase = JobPhase::RecoveryRequired;
                job.error = Some(format!("rollback_health_failed: {error}"));
            }
        }
        let _ = fs::remove_file(self.staging_artifact_path(&job.job_id));
        job.progress_sequence += 1;
        write_json(&self.config.root.join("job.json"), &job)?;
        Ok(job)
    }

    fn finish_health(&self, mut job: Job, report: HealthReport, healthy: bool) -> Result<Job> {
        ensure!(
            healthy
                && report.ok
                && report.brainstem_ready
                && report.strategy_ready
                && report.can_read_only,
            "health_not_ready"
        );
        if let Some(before) = job.boot_id_before.as_deref()
            && !report.boot_id.is_empty()
        {
            ensure!(before != report.boot_id.as_str(), "boot_id_unchanged");
        }
        if !self.is_release_link_mode() || !report.version.is_empty() {
            ensure!(
                normalize_version(&report.version) == normalize_version(&job.manifest.version),
                "health_version_mismatch"
            );
        }
        if !self.is_release_link_mode() || !report.release_id.is_empty() {
            ensure!(
                report.release_id == job.manifest.release_id,
                "health_release_mismatch"
            );
        }
        job.phase = JobPhase::Succeeded;
        if !report.boot_id.is_empty() {
            job.boot_id_after = Some(report.boot_id);
        }
        job.progress_sequence += 1;
        if self.is_release_link_mode() {
            if let Some(installed) = &job.installed_release_path {
                atomic_swap_link(&self.last_good_link_path(), Path::new(installed))?;
            }
        } else {
            write_slot(
                &self.config.root.join("last-known-good-slot"),
                job.active_slot,
            )?;
        }
        atomic_write(
            &self.config.root.join("current-version"),
            format!("{}\n", job.version).as_bytes(),
        )?;
        {
            let mut profile = self.profile.lock().unwrap_or_else(|p| p.into_inner());
            profile.current_version = job.version.clone();
            profile.active_slot = job.active_slot;
        }
        let _ = fs::remove_file(self.staging_artifact_path(&job.job_id));
        write_json(&self.config.root.join("job.json"), &job)?;
        Ok(job)
    }

    fn required_job(&self) -> Result<Job> {
        self.job()?.context("ota_job_missing")
    }
}

/// Atomically swaps `link` to point to `target`.
/// Uses real POSIX atomic symlink rename on Unix; falls back to an atomic pointer file on
/// unprivileged Windows hosts where symlink creation raises ERROR_PRIVILEGE_NOT_HELD.
pub fn atomic_swap_link(link: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = link.parent() {
        fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    {
        let parent = link.parent().unwrap_or_else(|| Path::new("."));
        let name = link
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("current");
        let temp = parent.join(format!(".{name}.{}.tmp", Uuid::new_v4()));
        std::os::unix::fs::symlink(target, &temp).with_context(|| {
            format!("create symlink {} -> {}", temp.display(), target.display())
        })?;
        fs::rename(&temp, link)
            .with_context(|| format!("swap symlink {} -> {}", link.display(), target.display()))?;
        Ok(())
    }
    #[cfg(windows)]
    {
        let parent = link.parent().unwrap_or_else(|| Path::new("."));
        let name = link
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("current");
        let temp = parent.join(format!(".{name}.{}.tmp", Uuid::new_v4()));
        match std::os::windows::fs::symlink_dir(target, &temp) {
            Ok(()) => {
                if link.exists() || fs::symlink_metadata(link).is_ok() {
                    let meta = fs::symlink_metadata(link)?;
                    if meta.file_type().is_symlink() || meta.is_dir() {
                        let _ = fs::remove_dir(link);
                    } else {
                        let _ = fs::remove_file(link);
                    }
                }
                fs::rename(&temp, link)?;
                Ok(())
            }
            Err(_) => {
                let _ = fs::remove_dir(&temp);
                if let Ok(meta) = fs::symlink_metadata(link)
                    && meta.file_type().is_symlink()
                {
                    let _ = fs::remove_dir(link);
                }
                atomic_write(link, format!("{}\n", target.display()).as_bytes())
            }
        }
    }
}

/// Reads the target path of a release link (either an OS symlink or Windows pointer file).
pub fn read_link_target(link: &Path) -> Option<PathBuf> {
    let meta = fs::symlink_metadata(link).ok()?;
    if meta.file_type().is_symlink() {
        return fs::read_link(link).ok();
    }
    if meta.is_file() {
        let raw = fs::read_to_string(link).ok()?;
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    None
}

fn validate_release_dir_segment(version: &str) -> Result<()> {
    ensure!(
        !version.is_empty()
            && !version.contains('/')
            && !version.contains('\\')
            && !version.contains("..")
            && version
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ".-_+".contains(c)),
        "invalid_release_version_dir"
    );
    Ok(())
}

fn load_public_key(path: &Path) -> Result<VerifyingKey> {
    Ok(VerifyingKey::from_public_key_pem(&fs::read_to_string(
        path,
    )?)?)
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

fn read_slot(path: &Path) -> Option<Slot> {
    fs::read_to_string(path).ok().and_then(|v| match v.trim() {
        "a" => Some(Slot::A),
        "b" => Some(Slot::B),
        _ => None,
    })
}

fn write_slot(path: &Path, slot: Slot) -> Result<()> {
    atomic_write(
        path,
        match slot {
            Slot::A => b"a\n",
            Slot::B => b"b\n",
        },
    )
}

fn slot_name(slot: Slot) -> &'static str {
    match slot {
        Slot::A => "a",
        Slot::B => "b",
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&fs::read(path)?)?))
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    atomic_write(path, &serde_json::to_vec_pretty(value)?)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create ota state directory {}", parent.display()))?;
    }
    let temp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    fs::write(&temp, bytes).with_context(|| format!("write temporary {}", temp.display()))?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&temp)
        .with_context(|| format!("open temporary {}", temp.display()))?;
    file.sync_all()
        .with_context(|| format!("sync temporary {}", temp.display()))?;
    drop(file);
    if let Err(error) = fs::rename(&temp, path) {
        if error.kind() != std::io::ErrorKind::PermissionDenied || !path.exists() {
            return Err(error)
                .with_context(|| format!("replace {} with {}", path.display(), temp.display()));
        }
        // Windows std::fs::rename cannot replace an existing file.
        fs::remove_file(path)
            .with_context(|| format!("remove old ota state {}", path.display()))?;
        fs::rename(&temp, path)
            .with_context(|| format!("replace {} with {}", path.display(), temp.display()))?;
    }
    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn read_boot_id(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|v| v.trim().to_owned())
}

fn validate_unit_name(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ".@_:-".contains(c)),
        "invalid_brainstem_unit"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RollbackPolicy;
    use crate::health::HealthReport;
    use base64::Engine;
    use ed25519_dalek::{Signer, SigningKey, pkcs8::EncodePublicKey};

    struct Good;
    impl HealthCheck for Good {
        fn check(&self) -> Result<HealthReport> {
            Ok(HealthReport {
                boot_id: "new".into(),
                version: "1.1.0".into(),
                release_id: "r1".into(),
                brainstem_ready: true,
                strategy_ready: true,
                can_read_only: true,
                ok: true,
            })
        }
    }

    struct NoopRestarter;
    impl ServiceRestarter for NoopRestarter {
        fn restart(&self, _unit: &str) -> Result<()> {
            Ok(())
        }
    }

    fn setup() -> (tempfile::TempDir, Updater, ManifestEnvelope) {
        let dir = tempfile::tempdir().unwrap();
        let key = SigningKey::from_bytes(&[9; 32]);
        let key_path = dir.path().join("key.pem");
        std::fs::write(
            &key_path,
            key.verifying_key()
                .to_public_key_pem(Default::default())
                .unwrap(),
        )
        .unwrap();
        let config = UpdaterConfig {
            root: dir.path().join("state"),
            device_id: "d1".into(),
            model: "m1".into(),
            hardware_revision: "h1".into(),
            bootloader_version: "1.0.0".into(),
            brainstem_version: "1.0.0".into(),
            current_version: "1.0.0".into(),
            battery_percent: 80,
            online: true,
            task_running: false,
            boot_id_path: dir.path().join("brainstem.boot_id"),
            verify_key_path: key_path,
            brainstem_unit: "doso-brainstem.service".into(),
            staging_dir: None,
            releases_dir: None,
            current_link: None,
            last_good_link: None,
            policy_admin_url: None,
            policy_admin_token: None,
            allow_insecure_http_loopback: true,
        };
        let updater = Updater::with_restarter(config, Arc::new(NoopRestarter)).unwrap();
        let manifest = Manifest {
            manifest_version: 1,
            release_id: "r1".into(),
            channel: "qa".into(),
            sequence: 1,
            version: "1.1.0".into(),
            issued_at_ms: 1,
            expires_at_ms: i64::MAX,
            model: "m1".into(),
            hardware_revision: "h1".into(),
            minimum_bootloader_version: "1.0.0".into(),
            minimum_brainstem_version: "1.0.0".into(),
            minimum_battery_percent: 40,
            artifact_url: "https://example.test/a".into(),
            artifact_size_bytes: 3,
            artifact_sha256: Manifest::artifact_hash(b"abc"),
            key_id: "k".into(),
            rollback_policy: RollbackPolicy {
                mode: "automatic".into(),
                health_timeout_ms: 1000,
            },
            artifact_type: ArtifactType::BrainstemRelease,
            policy_slot: None,
            shadow_frames: None,
            activate: None,
        };
        let payload = serde_json::to_vec(&manifest).unwrap();
        let envelope = ManifestEnvelope {
            payload: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&payload),
            signature: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(key.sign(&payload).to_bytes()),
        };
        (dir, updater, envelope)
    }

    #[test]
    fn schedules_only_one_active_job_and_persists_slots() {
        let (_dir, updater, envelope) = setup();
        let job = updater.schedule(&envelope, "key-1").unwrap();
        assert!(matches!(job.phase, JobPhase::Scheduled));
        assert_eq!(job.active_slot, Slot::A);
        assert_eq!(
            updater.schedule(&envelope, "key-1").unwrap().job_id,
            job.job_id
        );
        assert!(updater.schedule(&envelope, "key-2").is_err());
    }

    #[test]
    fn healthy_install_commits_last_known_good() {
        let (_dir, updater, envelope) = setup();
        updater.schedule(&envelope, "key-1").unwrap();
        let artifact = updater.config.root.join(format!(
            "staging/{}.artifact",
            updater.job().unwrap().unwrap().job_id
        ));
        std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
        std::fs::write(&artifact, b"abc").unwrap();
        let mut job = updater.job().unwrap().unwrap();
        job.phase = JobPhase::Verifying;
        job.downloaded_bytes = 3;
        std::fs::write(
            updater.config.root.join("job.json"),
            serde_json::to_vec(&job).unwrap(),
        )
        .unwrap();
        let job = updater.install().unwrap();
        assert!(matches!(job.phase, JobPhase::Rebooting));
        let job = updater.check_health(&Good).unwrap();
        assert!(matches!(job.phase, JobPhase::Succeeded));
        assert_eq!(updater.profile().current_version, "1.1.0");
        assert_eq!(
            std::fs::read_to_string(updater.config.root.join("last-known-good-slot"))
                .unwrap()
                .trim(),
            "b"
        );
    }
}
