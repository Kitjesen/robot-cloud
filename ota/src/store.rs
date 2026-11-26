use crate::{
    archive, download,
    health::{ComponentHttpHealthCheck, HealthCheck, HealthReport, SafetyPreCheck},
    manifest::{
        ArtifactType, Manifest, ManifestEnvelope, compare_versions, normalize_version,
        validate_component_name, validate_requires, validate_unit_name,
    },
    policy::PolicyAdminClient,
};
use anyhow::{Context, Result, ensure};
use ed25519_dalek::{VerifyingKey, pkcs8::DecodePublicKey};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub const DEFAULT_KEEP_HISTORY: usize = 2;
pub const DEFAULT_MIN_VBUS_V: f32 = 44.0;
pub const DEFAULT_OTA_UNIT: &str = "doso-ota.service";

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
    WaitingSafeWindow,
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
            Self::WaitingSafeWindow => "waiting_safe_window",
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafetyDecision {
    Safe,
    Unsafe(String),
}

#[derive(Debug, Clone)]
pub struct ComponentLayout {
    pub component: String,
    pub releases_dir: PathBuf,
    pub current_link: PathBuf,
    pub last_good_link: PathBuf,
    pub state_dir: PathBuf,
    pub systemd_unit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingSelfUpgrade {
    pub job_id: String,
    pub release_id: String,
    pub version: String,
    pub component: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_release_path: Option<String>,
    pub installed_release_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<String>,
    pub created_at_ms: i64,
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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub component_versions: BTreeMap<String, String>,
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
        ensure!(status.success(), "service_restart_failed: {unit}");
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
    pub components_root: Option<PathBuf>,
    pub keep_history: usize,
    pub require_can_read_only: bool,
    pub min_vbus_v: f32,
    pub ota_unit: String,
    pub policy_admin_url: Option<String>,
    pub policy_admin_token: Option<String>,
    pub allow_insecure_http_loopback: bool,
}

pub struct Updater {
    config: UpdaterConfig,
    profile: Mutex<DeviceProfile>,
    verify_key: VerifyingKey,
    restarter: Arc<dyn ServiceRestarter>,
    safety_check: Option<Arc<dyn SafetyPreCheck>>,
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
        validate_unit_name(&config.ota_unit)?;
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
        if let Some(components_root) = &config.components_root {
            fs::create_dir_all(components_root)?;
        }
        let persisted_version = read_trimmed(&config.root.join("current-version"))
            .unwrap_or_else(|| config.current_version.clone());
        let mut component_versions = load_component_versions(&config.root);
        component_versions
            .entry("brainstem".into())
            .or_insert_with(|| persisted_version.clone());
        let profile = DeviceProfile {
            device_id: config.device_id.clone(),
            model: config.model.clone(),
            hardware_revision: config.hardware_revision.clone(),
            bootloader_version: config.bootloader_version.clone(),
            brainstem_version: component_versions
                .get("brainstem")
                .cloned()
                .unwrap_or_else(|| config.brainstem_version.clone()),
            current_version: persisted_version,
            active_slot: read_slot(&config.root.join("current-slot")).unwrap_or(Slot::A),
            battery_percent: config.battery_percent,
            online: config.online,
            task_running: config.task_running,
            component_versions,
        };
        Ok(Self {
            config,
            profile: Mutex::new(profile),
            verify_key,
            restarter,
            safety_check: None,
        })
    }

    pub fn with_safety_check(mut self, safety: Arc<dyn SafetyPreCheck>) -> Self {
        self.safety_check = Some(safety);
        self
    }

    pub fn set_safety_check(&mut self, safety: Option<Arc<dyn SafetyPreCheck>>) {
        self.safety_check = safety;
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
        self.config.releases_dir.is_some() || self.config.components_root.is_some()
    }

    fn effective_components_root(&self) -> PathBuf {
        self.config
            .components_root
            .clone()
            .or_else(|| {
                self.config
                    .releases_dir
                    .as_ref()
                    .and_then(|r| r.parent().map(|p| p.join("components")))
            })
            .unwrap_or_else(|| self.config.root.join("components"))
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

    pub fn component_layout(
        &self,
        component: &str,
        unit_override: Option<&str>,
    ) -> Result<ComponentLayout> {
        validate_component_name(component)?;
        let systemd_unit = match unit_override {
            Some(unit) => {
                validate_unit_name(unit)?;
                unit.to_owned()
            }
            None => match component {
                "brainstem" => self.config.brainstem_unit.clone(),
                "ota" => self.config.ota_unit.clone(),
                other => format!("doso-{other}.service"),
            },
        };
        let state_dir = self.config.root.join("components").join(component);
        if component == "brainstem"
            && self.config.components_root.is_none()
            && let Some(releases_dir) = &self.config.releases_dir
        {
            return Ok(ComponentLayout {
                component: component.to_owned(),
                releases_dir: releases_dir.clone(),
                current_link: self.current_link_path(),
                last_good_link: self.last_good_link_path(),
                state_dir,
                systemd_unit,
            });
        }
        let base = self.effective_components_root().join(component);
        Ok(ComponentLayout {
            component: component.to_owned(),
            releases_dir: base.join("releases"),
            current_link: base.join("current"),
            last_good_link: base.join("last-known-good"),
            state_dir,
            systemd_unit,
        })
    }

    pub fn component_version(&self, component: &str) -> Option<String> {
        read_trimmed(
            &self
                .config
                .root
                .join("components")
                .join(component)
                .join("current-version"),
        )
        .or_else(|| self.profile().component_versions.get(component).cloned())
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

    pub fn pending_self_upgrade(&self) -> Result<Option<PendingSelfUpgrade>> {
        read_json(&self.pending_self_upgrade_path())
    }

    fn pending_self_upgrade_path(&self) -> PathBuf {
        self.config.root.join("pending-self-upgrade.json")
    }

    /// Evaluates the live real-robot safety window gate before mutating or restarting
    /// `brainstem` or activating a new RL policy.
    pub fn evaluate_safety_pre_check(&self, manifest: &Manifest) -> SafetyDecision {
        if !requires_safety_gate(manifest) {
            return SafetyDecision::Safe;
        }
        let profile = self.profile();
        if profile.task_running {
            return SafetyDecision::Unsafe("task_running".into());
        }
        if let Some(checker) = &self.safety_check {
            evaluate_safety_result(
                checker.query_safety(),
                &profile,
                manifest,
                self.config.min_vbus_v,
            )
        } else if profile.battery_percent < manifest.minimum_battery_percent {
            SafetyDecision::Unsafe(format!(
                "battery_too_low: {}% < {}%",
                profile.battery_percent, manifest.minimum_battery_percent
            ))
        } else {
            SafetyDecision::Safe
        }
    }

    /// Async-safe wrapper around `evaluate_safety_pre_check` that offloads blocking HTTP
    /// queries to `tokio::task::spawn_blocking` so single-threaded runtimes never stall.
    pub async fn evaluate_safety_pre_check_async(&self, manifest: &Manifest) -> SafetyDecision {
        if !requires_safety_gate(manifest) {
            return SafetyDecision::Safe;
        }
        let profile = self.profile();
        if profile.task_running {
            return SafetyDecision::Unsafe("task_running".into());
        }
        if let Some(checker) = self.safety_check.clone() {
            let query_res = tokio::task::spawn_blocking(move || checker.query_safety())
                .await
                .map_err(|e| anyhow::anyhow!("safety_check_join: {e}"))
                .and_then(|r| r);
            evaluate_safety_result(query_res, &profile, manifest, self.config.min_vbus_v)
        } else if profile.battery_percent < manifest.minimum_battery_percent {
            SafetyDecision::Unsafe(format!(
                "battery_too_low: {}% < {}%",
                profile.battery_percent, manifest.minimum_battery_percent
            ))
        } else {
            SafetyDecision::Safe
        }
    }

    pub fn cancel(&self) -> Result<Job> {
        let mut job = self.required_job()?;
        ensure!(
            matches!(
                job.phase,
                JobPhase::Scheduled | JobPhase::Downloading | JobPhase::WaitingSafeWindow
            ),
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
        let last_sequence = self.last_sequence_for(&manifest)?;
        let allow_http = self.config.allow_insecure_http_loopback
            || std::env::var("DOSO_OTA_ALLOW_HTTP").as_deref() == Ok("1");
        let profile = self.profile();
        manifest
            .validate_for_schedule(now_ms(), &profile, last_sequence, allow_http)
            .context("validate ota manifest")?;
        let active = profile.active_slot;
        let (previous_release_path, last_known_good_version) = match manifest.artifact_type {
            ArtifactType::ComponentRelease => {
                let comp = manifest.component.as_deref().unwrap_or("brainstem");
                let layout = self.component_layout(comp, manifest.systemd_unit.as_deref())?;
                let prev = read_link_target(&layout.current_link)
                    .map(|p| p.to_string_lossy().into_owned());
                let prev_ver = self
                    .component_version(comp)
                    .unwrap_or_else(|| "0.0.0".into());
                (prev, prev_ver)
            }
            ArtifactType::BrainstemRelease => {
                let prev = if self.is_release_link_mode() {
                    let layout =
                        self.component_layout("brainstem", manifest.systemd_unit.as_deref())?;
                    read_link_target(&layout.current_link).map(|p| p.to_string_lossy().into_owned())
                } else {
                    None
                };
                (prev, profile.current_version.clone())
            }
            ArtifactType::PolicyPackage | ArtifactType::ReleaseBundle => {
                (None, profile.current_version.clone())
            }
        };
        let total_bytes = if manifest.artifact_type == ArtifactType::ReleaseBundle
            && manifest.artifact_size_bytes == 0
        {
            manifest
                .bundle_items
                .iter()
                .map(|i| i.artifact_size_bytes)
                .sum()
        } else {
            manifest.artifact_size_bytes
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
            total_bytes,
            active_slot: active,
            last_known_good_slot: active,
            last_known_good_version,
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

        if manifest.artifact_type == ArtifactType::ReleaseBundle {
            let bundle_dir = self.staging_job_dir(&job.job_id);
            fs::create_dir_all(&bundle_dir)?;
            let mut total_downloaded = 0u64;
            for item in &manifest.bundle_items {
                let dest = bundle_dir.join(format!("{}.artifact", item.component));
                download::download_resumable(&item.artifact_url, &dest, item.artifact_size_bytes)
                    .await?;
                download::verify_hash(
                    &dest,
                    item.artifact_size_bytes,
                    Some(&item.artifact_sha256),
                )?;
                total_downloaded += item.artifact_size_bytes;
            }
            job.downloaded_bytes = total_downloaded;
            job.total_bytes = total_downloaded;
            job.phase = JobPhase::Verifying;
            job.progress_sequence += 1;
            write_json(&self.config.root.join("job.json"), &job)?;
            return Ok(job);
        }

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

    /// Installs a verified Channel A `brainstem_release` or `component_release` artifact.
    /// Enforces real-robot `SafetyPreCheck` before mutating or restarting brainstem; if unsafe,
    /// transitions to `JobPhase::WaitingSafeWindow` without touching slots or killing motors.
    pub fn install(&self) -> Result<Job> {
        self.install_inner(false)
    }

    pub async fn install_async(&self) -> Result<Job> {
        let mut job = self.required_job()?;
        ensure!(
            matches!(job.phase, JobPhase::Verifying | JobPhase::WaitingSafeWindow),
            "ota_install_phase_invalid"
        );
        if let SafetyDecision::Unsafe(reason) =
            self.evaluate_safety_pre_check_async(&job.manifest).await
        {
            job.phase = JobPhase::WaitingSafeWindow;
            job.error = Some(reason);
            job.progress_sequence += 1;
            write_json(&self.config.root.join("job.json"), &job)?;
            return Ok(job);
        }
        self.install_inner(true)
    }

    fn install_inner(&self, safety_already_verified: bool) -> Result<Job> {
        let mut job = self.required_job()?;
        ensure!(
            matches!(job.phase, JobPhase::Verifying | JobPhase::WaitingSafeWindow),
            "ota_install_phase_invalid"
        );
        ensure!(
            matches!(
                job.manifest.artifact_type,
                ArtifactType::BrainstemRelease | ArtifactType::ComponentRelease
            ),
            "ota_install_requires_async_path"
        );
        let manifest = job.manifest.clone();

        if !safety_already_verified
            && let SafetyDecision::Unsafe(reason) = self.evaluate_safety_pre_check(&manifest)
        {
            job.phase = JobPhase::WaitingSafeWindow;
            job.error = Some(reason);
            job.progress_sequence += 1;
            write_json(&self.config.root.join("job.json"), &job)?;
            return Ok(job);
        }
        job.error = None;

        let artifact = self.staging_artifact_path(&job.job_id);
        if let Err(err) = download::verify_hash(
            &artifact,
            manifest.artifact_size_bytes,
            Some(&manifest.artifact_sha256),
        ) {
            return Err(self.fail_job_pre_switch(job, err));
        }

        if manifest.artifact_type == ArtifactType::ComponentRelease {
            let component = match manifest.component.as_deref().context("component_required") {
                Ok(c) => c,
                Err(err) => return Err(self.fail_job_pre_switch(job, err)),
            };
            if let Err(err) = validate_release_dir_segment(&manifest.version) {
                return Err(self.fail_job_pre_switch(job, err));
            }
            let layout = match self.component_layout(component, manifest.systemd_unit.as_deref()) {
                Ok(l) => l,
                Err(err) => return Err(self.fail_job_pre_switch(job, err)),
            };
            if let Err(err) = fs::create_dir_all(&layout.releases_dir) {
                return Err(self.fail_job_pre_switch(job, err.into()));
            }
            let target_release = layout.releases_dir.join(&manifest.version);
            job.phase = JobPhase::Installing;
            job.progress_sequence += 1;
            write_json(&self.config.root.join("job.json"), &job)?;

            if let Err(err) =
                archive::extract_component_tar_gz(&artifact, &target_release, component)
            {
                return Err(self.fail_job_pre_switch(job, err));
            }
            if component == "ota"
                && let Err(err) = prepare_ota_release_dir(&target_release)
            {
                let _ = fs::remove_dir_all(&target_release);
                return Err(self.fail_job_pre_switch(job, err));
            }
            if component == "brainstem"
                && let Err(err) = ensure_brainstem_release_manifest(
                    &target_release,
                    &manifest.version,
                    &manifest.release_id,
                )
            {
                return Err(self.fail_job_pre_switch(job, err));
            }

            let prev_target = read_link_target(&layout.current_link);
            if let Some(prev) = &prev_target
                && prev != &target_release
            {
                atomic_swap_link(&layout.last_good_link, prev)?;
                job.previous_release_path = Some(prev.to_string_lossy().into_owned());
            }
            atomic_swap_link(&layout.current_link, &target_release)?;
            job.installed_release_path = Some(target_release.to_string_lossy().into_owned());
            job.phase = JobPhase::Rebooting;
            job.progress_sequence += 1;
            write_json(&self.config.root.join("job.json"), &job)?;

            if component == "ota" {
                let pending = PendingSelfUpgrade {
                    job_id: job.job_id.clone(),
                    release_id: job.release_id.clone(),
                    version: job.version.clone(),
                    component: "ota".into(),
                    previous_release_path: job.previous_release_path.clone(),
                    installed_release_path: target_release.to_string_lossy().into_owned(),
                    previous_version: self.component_version("ota"),
                    created_at_ms: now_ms(),
                };
                write_json(&self.pending_self_upgrade_path(), &pending)?;
            }

            if let Err(error) = self.restarter.restart(&layout.systemd_unit) {
                if let Some(prev) = &job.previous_release_path {
                    let _ = atomic_swap_link(&layout.current_link, Path::new(prev));
                }
                if component == "ota" {
                    let _ = fs::remove_file(self.pending_self_upgrade_path());
                }
                job.phase = JobPhase::Failed;
                job.error = Some(error.to_string());
                job.progress_sequence += 1;
                write_json(&self.config.root.join("job.json"), &job)?;
                return Err(error);
            }
            return Ok(job);
        }

        // BrainstemRelease: release-link mode (legacy `/opt/doso` or `components/brainstem`).
        if self.is_release_link_mode() {
            if let Err(err) = validate_release_dir_segment(&manifest.version) {
                return Err(self.fail_job_pre_switch(job, err));
            }
            let layout = match self.component_layout("brainstem", manifest.systemd_unit.as_deref())
            {
                Ok(l) => l,
                Err(err) => return Err(self.fail_job_pre_switch(job, err)),
            };
            if let Err(err) = fs::create_dir_all(&layout.releases_dir) {
                return Err(self.fail_job_pre_switch(job, err.into()));
            }
            let target_release = layout.releases_dir.join(&manifest.version);
            job.phase = JobPhase::Installing;
            job.progress_sequence += 1;
            write_json(&self.config.root.join("job.json"), &job)?;

            if let Err(err) = archive::extract_release_tar_gz(&artifact, &target_release) {
                return Err(self.fail_job_pre_switch(job, err));
            }
            if let Err(err) = ensure_brainstem_release_manifest(
                &target_release,
                &manifest.version,
                &manifest.release_id,
            ) {
                return Err(self.fail_job_pre_switch(job, err));
            }

            let prev_target = read_link_target(&layout.current_link);
            if let Some(prev) = &prev_target
                && prev != &target_release
            {
                atomic_swap_link(&layout.last_good_link, prev)?;
                job.previous_release_path = Some(prev.to_string_lossy().into_owned());
            }
            atomic_swap_link(&layout.current_link, &target_release)?;
            job.installed_release_path = Some(target_release.to_string_lossy().into_owned());
            job.phase = JobPhase::Rebooting;
            job.progress_sequence += 1;
            write_json(&self.config.root.join("job.json"), &job)?;

            if let Err(error) = self.restarter.restart(&layout.systemd_unit) {
                if let Some(prev) = &job.previous_release_path {
                    let _ = atomic_swap_link(&layout.current_link, Path::new(prev));
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

    /// Reconciles an in-flight `doso-ota` self-upgrade on process startup.
    /// Commits `last-known-good` + GC if `self_check()` succeeds, or atomically rolls back
    /// `components/ota/current` to `last-known-good` and restarts `doso-ota` if it fails.
    pub fn reconcile_self_upgrade_with_check<F>(&self, self_check: F) -> Result<Option<Job>>
    where
        F: FnOnce() -> Result<()>,
    {
        let Some(pending) = self.pending_self_upgrade()? else {
            return Ok(None);
        };
        let layout = self.component_layout("ota", None)?;
        let mut job = self.job()?.filter(|j| j.job_id == pending.job_id);
        match self_check() {
            Ok(()) => {
                atomic_swap_link(
                    &layout.last_good_link,
                    Path::new(&pending.installed_release_path),
                )?;
                self.record_component_commit(
                    "ota",
                    &pending.version,
                    job.as_ref().map(|j| j.manifest.sequence),
                )?;
                let _ = gc_old_releases(
                    &layout.releases_dir,
                    &layout.current_link,
                    &layout.last_good_link,
                    self.config.keep_history,
                );
                let _ = fs::remove_file(self.pending_self_upgrade_path());
                self.cleanup_staging(&pending.job_id);
                if let Some(ref mut j) = job {
                    j.phase = JobPhase::Succeeded;
                    j.error = None;
                    j.progress_sequence += 1;
                    write_json(&self.config.root.join("job.json"), j)?;
                }
                Ok(job)
            }
            Err(check_err) => {
                let rollback_target = pending
                    .previous_release_path
                    .as_deref()
                    .map(PathBuf::from)
                    .or_else(|| read_link_target(&layout.last_good_link));
                let mut rolled_back_ok = false;
                let mut err_msg = format!("self_upgrade_health_failed: {check_err}");
                if let Some(prev) = rollback_target {
                    match atomic_swap_link(&layout.current_link, &prev) {
                        Ok(()) => match self.restarter.restart(&layout.systemd_unit) {
                            Ok(()) => {
                                rolled_back_ok = true;
                            }
                            Err(restart_err) => {
                                err_msg = format!(
                                    "{err_msg}; self_upgrade_rollback_restart_failed: {restart_err}"
                                );
                            }
                        },
                        Err(link_err) => {
                            err_msg =
                                format!("{err_msg}; self_upgrade_rollback_link_failed: {link_err}");
                        }
                    }
                } else {
                    err_msg = format!("{err_msg}; last_known_good_release_missing");
                }
                let _ = fs::remove_file(self.pending_self_upgrade_path());
                self.cleanup_staging(&pending.job_id);
                if let Some(ref mut j) = job {
                    j.phase = if rolled_back_ok {
                        JobPhase::RolledBack
                    } else {
                        JobPhase::RecoveryRequired
                    };
                    j.error = Some(err_msg);
                    j.progress_sequence += 1;
                    write_json(&self.config.root.join("job.json"), j)?;
                }
                Ok(job)
            }
        }
    }

    pub fn reconcile_self_upgrade(&self, self_healthy: bool) -> Result<Option<Job>> {
        self.reconcile_self_upgrade_with_check(|| {
            ensure!(self_healthy, "self_upgrade_unhealthy");
            Ok(())
        })
    }

    /// Installs a verified Channel B `policy_package` via zero-restart `:5054/v1/policies/push`.
    /// Never restarts `doso-brainstem.service`; enforces safety pre-check when `activate == true`,
    /// and automatically rolls back slot on post-mutation failure.
    pub async fn install_policy(&self) -> Result<Job> {
        let mut job = self.required_job()?;
        ensure!(
            matches!(
                job.phase,
                JobPhase::Verifying | JobPhase::WaitingSafeWindow | JobPhase::Installing
            ),
            "ota_policy_install_phase_invalid"
        );
        ensure!(
            job.manifest.artifact_type == ArtifactType::PolicyPackage,
            "ota_not_policy_package"
        );

        if let SafetyDecision::Unsafe(reason) =
            self.evaluate_safety_pre_check_async(&job.manifest).await
        {
            job.phase = JobPhase::WaitingSafeWindow;
            job.error = Some(reason);
            job.progress_sequence += 1;
            write_json(&self.config.root.join("job.json"), &job)?;
            return Ok(job);
        }
        job.error = None;

        let artifact = self.staging_artifact_path(&job.job_id);
        if let Err(err) = download::verify_hash(
            &artifact,
            job.manifest.artifact_size_bytes,
            Some(&job.manifest.artifact_sha256),
        ) {
            return Ok(self.mark_job_failed(job, &err.to_string()));
        }
        let extract_dir = self.staging_dir().join(format!("{}.policy", job.job_id));
        if let Err(err) = archive::extract_policy_tar_gz(&artifact, &extract_dir) {
            let _ = fs::remove_dir_all(&extract_dir);
            return Ok(self.mark_job_failed(job, &err.to_string()));
        }

        job.phase = JobPhase::Installing;
        job.progress_sequence += 1;
        write_json(&self.config.root.join("job.json"), &job)?;

        let client = self.policy_client();
        let push_result = client.push_policy_dir(&extract_dir, &job.manifest).await;
        let _ = fs::remove_dir_all(&extract_dir);

        match push_result {
            Ok(_) => {
                self.record_policy_sequence(
                    job.manifest.policy_slot.as_deref(),
                    job.manifest.sequence,
                )?;
                job.phase = JobPhase::Succeeded;
                job.progress_sequence += 1;
                self.cleanup_staging(&job.job_id);
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
                    self.cleanup_staging(&job.job_id);
                    job.progress_sequence += 1;
                    write_json(&self.config.root.join("job.json"), &job)?;
                    return Ok(job);
                }
                job.phase = JobPhase::Failed;
                job.error = Some(err_msg);
                job.progress_sequence += 1;
                self.cleanup_staging(&job.job_id);
                write_json(&self.config.root.join("job.json"), &job)?;
                Ok(job)
            }
        }
    }

    /// Installs a `ReleaseBundle` sequentially in manifest order with per-item dependency checks,
    /// health verification, GC, and reverse-order rollback on failure.
    pub async fn install_bundle<C: HealthCheck + ?Sized + 'static>(
        &self,
        brainstem_checker: Option<Arc<C>>,
        poll_interval: Duration,
    ) -> Result<Job> {
        let mut job = self.required_job()?;
        ensure!(
            matches!(
                job.phase,
                JobPhase::Verifying | JobPhase::WaitingSafeWindow | JobPhase::Installing
            ),
            "ota_bundle_install_phase_invalid"
        );
        ensure!(
            job.manifest.artifact_type == ArtifactType::ReleaseBundle,
            "ota_not_release_bundle"
        );

        if let SafetyDecision::Unsafe(reason) =
            self.evaluate_safety_pre_check_async(&job.manifest).await
        {
            job.phase = JobPhase::WaitingSafeWindow;
            job.error = Some(reason);
            job.progress_sequence += 1;
            write_json(&self.config.root.join("job.json"), &job)?;
            return Ok(job);
        }
        job.error = None;
        job.phase = JobPhase::Installing;
        job.progress_sequence += 1;
        write_json(&self.config.root.join("job.json"), &job)?;

        let bundle_dir = self.staging_job_dir(&job.job_id);
        let timeout = Duration::from_millis(job.manifest.rollback_policy.health_timeout_ms);
        struct AppliedBundleItem {
            layout: ComponentLayout,
            target_release: PathBuf,
            version: String,
            prev_target: Option<PathBuf>,
            prev_last_good: Option<PathBuf>,
        }
        let mut applied: Vec<AppliedBundleItem> = Vec::new();
        let mut effective_versions = self.profile().component_versions;
        let mut failure_reason: Option<String> = None;

        for item in &job.manifest.bundle_items {
            if item.component == "ota" {
                failure_reason = Some("bundle_ota_self_upgrade_unsupported".into());
                break;
            }
            if let Err(err) = validate_requires(&item.requires, &effective_versions) {
                failure_reason = Some(err.to_string());
                break;
            }
            if let Err(err) = validate_release_dir_segment(&item.version) {
                failure_reason = Some(err.to_string());
                break;
            }
            let layout = match self.component_layout(&item.component, item.systemd_unit.as_deref())
            {
                Ok(l) => l,
                Err(err) => {
                    failure_reason = Some(err.to_string());
                    break;
                }
            };
            let artifact = bundle_dir.join(format!("{}.artifact", item.component));
            if let Err(err) = download::verify_hash(
                &artifact,
                item.artifact_size_bytes,
                Some(&item.artifact_sha256),
            ) {
                failure_reason = Some(err.to_string());
                break;
            }
            let target_release = layout.releases_dir.join(&item.version);
            if let Err(err) = fs::create_dir_all(&layout.releases_dir).and_then(|_| {
                archive::extract_component_tar_gz(&artifact, &target_release, &item.component)
                    .map_err(std::io::Error::other)
            }) {
                failure_reason = Some(err.to_string());
                break;
            }
            if item.component == "brainstem"
                && let Err(err) = ensure_brainstem_release_manifest(
                    &target_release,
                    &item.version,
                    &job.release_id,
                )
            {
                failure_reason = Some(err.to_string());
                break;
            }

            let prev_target = read_link_target(&layout.current_link);
            let prev_last_good = read_link_target(&layout.last_good_link);
            let item_boot_id_before = (item.component == "brainstem")
                .then(|| read_boot_id(&self.config.boot_id_path))
                .flatten();
            if let Err(err) = atomic_swap_link(&layout.current_link, &target_release) {
                failure_reason = Some(err.to_string());
                break;
            }
            applied.push(AppliedBundleItem {
                layout: layout.clone(),
                target_release: target_release.clone(),
                version: item.version.clone(),
                prev_target,
                prev_last_good,
            });

            if let Err(err) = self.restarter.restart(&layout.systemd_unit) {
                failure_reason = Some(err.to_string());
                break;
            }

            let health_ok = if item.component == "brainstem" {
                if let Some(ref checker) = brainstem_checker {
                    let boot_before = item_boot_id_before.clone();
                    poll_health_until(checker.clone(), timeout, poll_interval, |rep| {
                        let boot_ok = boot_before
                            .as_deref()
                            .is_none_or(|b| rep.boot_id.is_empty() || rep.boot_id != b);
                        boot_ok
                            && self.report_basic_ok(rep)
                            && (rep.version.is_empty()
                                || normalize_version(&rep.version)
                                    == normalize_version(&item.version))
                    })
                    .await
                } else {
                    Ok(())
                }
            } else if let Some(ref url) = item.health_url {
                let comp_checker = Arc::new(ComponentHttpHealthCheck::new(url.clone()));
                poll_health_until(comp_checker, timeout, poll_interval, |rep| {
                    rep.ok
                        && (rep.version.is_empty()
                            || normalize_version(&rep.version) == normalize_version(&item.version))
                })
                .await
            } else {
                Ok(())
            };

            if let Err(err) = health_ok {
                failure_reason = Some(format!(
                    "bundle_item_health_failed({}): {err}",
                    item.component
                ));
                break;
            }

            effective_versions.insert(item.component.clone(), item.version.clone());
        }

        if let Some(err) = failure_reason {
            job.phase = JobPhase::RollingBack;
            job.error = Some(err);
            job.progress_sequence += 1;
            write_json(&self.config.root.join("job.json"), &job)?;

            let mut rollback_failed = false;
            for item in applied.into_iter().rev() {
                if let Some(prev) = item.prev_target {
                    if atomic_swap_link(&item.layout.current_link, &prev).is_err() {
                        rollback_failed = true;
                    }
                    if self.restarter.restart(&item.layout.systemd_unit).is_err() {
                        rollback_failed = true;
                    }
                } else if remove_link_if_exists(&item.layout.current_link).is_err() {
                    rollback_failed = true;
                }
                if let Some(prev_lg) = item.prev_last_good
                    && atomic_swap_link(&item.layout.last_good_link, &prev_lg).is_err()
                {
                    rollback_failed = true;
                }
            }
            self.cleanup_staging(&job.job_id);
            job.phase = if rollback_failed {
                JobPhase::RecoveryRequired
            } else {
                JobPhase::RolledBack
            };
            job.progress_sequence += 1;
            write_json(&self.config.root.join("job.json"), &job)?;
            return Ok(job);
        }

        // All bundle items passed health verification — commit last-known-good, versions, and GC.
        for item in &applied {
            atomic_swap_link(&item.layout.last_good_link, &item.target_release)?;
            self.record_component_commit(&item.layout.component, &item.version, None)?;
            let _ = gc_old_releases(
                &item.layout.releases_dir,
                &item.layout.current_link,
                &item.layout.last_good_link,
                self.config.keep_history,
            );
        }

        self.record_bundle_sequence(job.manifest.sequence)?;
        atomic_write(
            &self.config.root.join("bundle-version"),
            format!("{}\n", job.version).as_bytes(),
        )?;
        self.cleanup_staging(&job.job_id);
        job.phase = JobPhase::Succeeded;
        job.progress_sequence += 1;
        write_json(&self.config.root.join("job.json"), &job)?;
        Ok(job)
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
        let unit = self.target_unit_for_job(&job)?;
        if let Err(error) = self.restarter.restart(&unit) {
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
        let unit = self.target_unit_for_job(&job)?;
        if let Err(error) = self.restarter.restart(&unit) {
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
    /// (or pauses cleanly in `WaitingSafeWindow` when the robot is not safe to mutate/restart).
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

        if matches!(job.phase, JobPhase::Verifying | JobPhase::WaitingSafeWindow) {
            match job.manifest.artifact_type {
                ArtifactType::PolicyPackage => {
                    job = self.install_policy().await?;
                    on_progress(&job);
                    return Ok(Some(job));
                }
                ArtifactType::ReleaseBundle => {
                    job = self.install_bundle(checker, health_poll_interval).await?;
                    on_progress(&job);
                    return Ok(Some(job));
                }
                ArtifactType::BrainstemRelease | ArtifactType::ComponentRelease => {
                    match self.install_async().await {
                        Ok(next) => {
                            job = next;
                            on_progress(&job);
                            if job.phase == JobPhase::WaitingSafeWindow {
                                return Ok(Some(job));
                            }
                            // Self-upgrade hands off commit/rollback to the restarted `doso-ota` process.
                            if job.manifest.artifact_type == ArtifactType::ComponentRelease
                                && job.manifest.component.as_deref() == Some("ota")
                            {
                                return Ok(Some(job));
                            }
                        }
                        Err(error) => {
                            let mut latest = self.job()?.unwrap_or_else(|| {
                                job.phase = JobPhase::Failed;
                                job.error = Some(error.to_string());
                                job
                            });
                            if !latest.phase.is_terminal()
                                && !matches!(
                                    latest.phase,
                                    JobPhase::Rebooting | JobPhase::WaitingSafeWindow
                                )
                            {
                                latest = self.mark_job_failed(latest, &error.to_string());
                            }
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
                ArtifactType::ReleaseBundle => {
                    job.phase = JobPhase::Failed;
                    job.error = Some("interrupted_during_bundle_install".into());
                    job.progress_sequence += 1;
                    write_json(&self.config.root.join("job.json"), &job)?;
                    on_progress(&job);
                    return Ok(Some(job));
                }
                ArtifactType::BrainstemRelease | ArtifactType::ComponentRelease => {
                    let comp = job.manifest.effective_component().unwrap_or("brainstem");
                    let layout = self
                        .component_layout(comp, job.manifest.systemd_unit.as_deref())
                        .ok();
                    let already_switched = layout
                        .as_ref()
                        .and_then(|l| read_link_target(&l.current_link))
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
            if job.manifest.artifact_type == ArtifactType::ComponentRelease {
                let comp = job.manifest.component.as_deref().unwrap_or("");
                if comp != "brainstem" {
                    if let Some(ref url) = job.manifest.health_url {
                        let comp_checker = Arc::new(ComponentHttpHealthCheck::new(url.clone()));
                        job.phase = JobPhase::HealthChecking;
                        on_progress(&job);
                        job = self
                            .wait_and_verify_health(comp_checker, health_poll_interval)
                            .await?;
                        on_progress(&job);
                        return Ok(Some(job));
                    } else {
                        // Non-brainstem component with no HTTP health URL: commit after systemd restart.
                        let synthetic = HealthReport {
                            boot_id: String::new(),
                            version: job.version.clone(),
                            release_id: job.release_id.clone(),
                            brainstem_ready: true,
                            strategy_ready: true,
                            can_read_only: true,
                            ok: true,
                            motor_output_enabled: Some(false),
                            cms_state: None,
                            online_mask: None,
                            imu_fresh: None,
                            vbus_min_v: None,
                            battery_percent: None,
                        };
                        job = self.finish_health(job, synthetic, true)?;
                        on_progress(&job);
                        return Ok(Some(job));
                    }
                }
            }
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
            if job.manifest.artifact_type == ArtifactType::ComponentRelease
                && job.manifest.component.as_deref() != Some("brainstem")
            {
                if let Some(ref url) = job.manifest.health_url {
                    let comp_checker = Arc::new(ComponentHttpHealthCheck::new(url.clone()));
                    on_progress(&job);
                    job = self
                        .rollback_with_poll(comp_checker, health_poll_interval)
                        .await?;
                    on_progress(&job);
                    return Ok(Some(job));
                }
                // Non-brainstem component with no HTTP health URL: restore link & restart without
                // invoking brainstem's health checker.
                on_progress(&job);
                let synthetic_ok = SyntheticRollbackHealth {
                    version: job.last_known_good_version.clone(),
                };
                job = self.rollback(&synthetic_ok)?;
                on_progress(&job);
                return Ok(Some(job));
            }
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

    fn staging_job_dir(&self, job_id: &str) -> PathBuf {
        self.staging_dir().join(job_id)
    }

    fn cleanup_staging(&self, job_id: &str) {
        let artifact = self.staging_artifact_path(job_id);
        let _ = fs::remove_file(&artifact);
        let _ = fs::remove_file(artifact.with_extension("part"));
        let _ = fs::remove_file(artifact.with_extension("journal"));
        let _ = fs::remove_dir_all(self.staging_dir().join(format!("{job_id}.policy")));
        let _ = fs::remove_dir_all(self.staging_job_dir(job_id));
    }

    fn mark_job_failed(&self, mut job: Job, reason: &str) -> Job {
        self.cleanup_staging(&job.job_id);
        job.phase = JobPhase::Failed;
        job.error = Some(reason.to_owned());
        job.progress_sequence += 1;
        let _ = write_json(&self.config.root.join("job.json"), &job);
        job
    }

    fn fail_job_pre_switch(&self, job: Job, error: anyhow::Error) -> anyhow::Error {
        let _ = self.mark_job_failed(job, &error.to_string());
        error
    }

    fn policy_sequence_path(&self, slot: Option<&str>) -> Option<PathBuf> {
        let raw = slot.unwrap_or("default").trim();
        let valid = !raw.is_empty()
            && !raw.contains("..")
            && raw
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
        valid.then(|| {
            self.config
                .root
                .join("policies")
                .join(raw)
                .join("last-sequence")
        })
    }

    fn record_policy_sequence(&self, slot: Option<&str>, sequence: u64) -> Result<()> {
        if let Some(path) = self.policy_sequence_path(slot) {
            atomic_write(&path, format!("{sequence}\n").as_bytes())?;
        }
        Ok(())
    }

    fn record_bundle_sequence(&self, sequence: u64) -> Result<()> {
        atomic_write(
            &self.config.root.join("bundle-last-sequence"),
            format!("{sequence}\n").as_bytes(),
        )
    }

    fn last_sequence_for(&self, manifest: &Manifest) -> Result<u64> {
        match manifest.artifact_type {
            ArtifactType::BrainstemRelease => {
                let path = self
                    .config
                    .root
                    .join("components")
                    .join("brainstem")
                    .join("last-sequence");
                if let Some(seq) = read_trimmed(&path).and_then(|v| v.parse::<u64>().ok()) {
                    return Ok(seq);
                }
                Ok(self
                    .job()?
                    .filter(|job| {
                        matches!(job.phase, JobPhase::Succeeded)
                            && job.manifest.effective_component() == Some("brainstem")
                    })
                    .map(|job| job.manifest.sequence)
                    .unwrap_or(0))
            }
            ArtifactType::ComponentRelease => {
                let Some(comp) = manifest
                    .component
                    .as_deref()
                    .filter(|c| validate_component_name(c).is_ok())
                else {
                    return Ok(0);
                };
                let path = self
                    .config
                    .root
                    .join("components")
                    .join(comp)
                    .join("last-sequence");
                if let Some(seq) = read_trimmed(&path).and_then(|v| v.parse::<u64>().ok()) {
                    return Ok(seq);
                }
                Ok(self
                    .job()?
                    .filter(|job| {
                        matches!(job.phase, JobPhase::Succeeded)
                            && job.manifest.effective_component() == Some(comp)
                    })
                    .map(|job| job.manifest.sequence)
                    .unwrap_or(0))
            }
            ArtifactType::PolicyPackage => {
                if let Some(path) = self.policy_sequence_path(manifest.policy_slot.as_deref())
                    && let Some(seq) = read_trimmed(&path).and_then(|v| v.parse::<u64>().ok())
                {
                    return Ok(seq);
                }
                Ok(self
                    .job()?
                    .filter(|job| {
                        matches!(job.phase, JobPhase::Succeeded)
                            && job.manifest.artifact_type == ArtifactType::PolicyPackage
                            && job.manifest.policy_slot == manifest.policy_slot
                    })
                    .map(|job| job.manifest.sequence)
                    .unwrap_or(0))
            }
            ArtifactType::ReleaseBundle => {
                let path = self.config.root.join("bundle-last-sequence");
                if let Some(seq) = read_trimmed(&path).and_then(|v| v.parse::<u64>().ok()) {
                    return Ok(seq);
                }
                Ok(self
                    .job()?
                    .filter(|job| {
                        matches!(job.phase, JobPhase::Succeeded)
                            && job.manifest.artifact_type == ArtifactType::ReleaseBundle
                    })
                    .map(|job| job.manifest.sequence)
                    .unwrap_or(0))
            }
        }
    }

    fn target_unit_for_job(&self, job: &Job) -> Result<String> {
        match job.manifest.artifact_type {
            ArtifactType::ComponentRelease => {
                let comp = job.manifest.component.as_deref().unwrap_or("brainstem");
                Ok(self
                    .component_layout(comp, job.manifest.systemd_unit.as_deref())?
                    .systemd_unit)
            }
            _ => Ok(job
                .manifest
                .systemd_unit
                .clone()
                .unwrap_or_else(|| self.config.brainstem_unit.clone())),
        }
    }

    fn restore_previous_target(&self, job: &Job) -> Result<()> {
        if job.manifest.artifact_type == ArtifactType::ComponentRelease {
            let comp = job.manifest.component.as_deref().unwrap_or("brainstem");
            let layout = self.component_layout(comp, job.manifest.systemd_unit.as_deref())?;
            let previous = job
                .previous_release_path
                .as_deref()
                .map(PathBuf::from)
                .or_else(|| read_link_target(&layout.last_good_link))
                .context("last_known_good_release_missing")?;
            atomic_swap_link(&layout.current_link, &previous)?;
            return Ok(());
        }
        if self.is_release_link_mode() {
            let layout =
                self.component_layout("brainstem", job.manifest.systemd_unit.as_deref())?;
            let previous = job
                .previous_release_path
                .as_deref()
                .map(PathBuf::from)
                .or_else(|| read_link_target(&layout.last_good_link))
                .context("last_known_good_release_missing")?;
            atomic_swap_link(&layout.current_link, &previous)?;
            Ok(())
        } else {
            write_slot(
                &self.config.root.join("current-slot"),
                job.last_known_good_slot,
            )
        }
    }

    fn report_basic_ok(&self, health: &HealthReport) -> bool {
        let can_ok = !self.config.require_can_read_only || health.can_read_only;
        health.ok
            && health.brainstem_ready
            && health.strategy_ready
            && can_ok
            && !health.motor_output_enabled.unwrap_or(false)
    }

    fn rollback_health_ok(&self, job: &Job, health: &HealthReport) -> bool {
        if !self.report_basic_ok(health) {
            return false;
        }
        let is_link_backed = job.manifest.artifact_type == ArtifactType::ComponentRelease
            || self.is_release_link_mode();
        if is_link_backed {
            if !health.version.is_empty() && job.last_known_good_version != "0.0.0" {
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
        self.cleanup_staging(&job.job_id);
        job.progress_sequence += 1;
        write_json(&self.config.root.join("job.json"), &job)?;
        Ok(job)
    }

    fn record_component_commit(
        &self,
        component: &str,
        version: &str,
        sequence: Option<u64>,
    ) -> Result<()> {
        let comp_state_dir = self.config.root.join("components").join(component);
        atomic_write(
            &comp_state_dir.join("current-version"),
            format!("{version}\n").as_bytes(),
        )?;
        if let Some(seq) = sequence {
            atomic_write(
                &comp_state_dir.join("last-sequence"),
                format!("{seq}\n").as_bytes(),
            )?;
        }
        if component == "brainstem" {
            atomic_write(
                &self.config.root.join("current-version"),
                format!("{version}\n").as_bytes(),
            )?;
        }
        let mut profile = self.profile.lock().unwrap_or_else(|p| p.into_inner());
        profile
            .component_versions
            .insert(component.to_owned(), version.to_owned());
        if component == "brainstem" {
            profile.current_version = version.to_owned();
            profile.brainstem_version = version.to_owned();
        }
        Ok(())
    }

    fn finish_health(&self, mut job: Job, report: HealthReport, healthy: bool) -> Result<Job> {
        ensure!(healthy && self.report_basic_ok(&report), "health_not_ready");
        let is_brainstem = job.manifest.effective_component() == Some("brainstem");
        if is_brainstem
            && let Some(before) = job.boot_id_before.as_deref()
            && !report.boot_id.is_empty()
        {
            ensure!(before != report.boot_id.as_str(), "boot_id_unchanged");
        }
        let is_link_backed = job.manifest.artifact_type == ArtifactType::ComponentRelease
            || self.is_release_link_mode();
        if !is_link_backed || !report.version.is_empty() {
            ensure!(
                normalize_version(&report.version) == normalize_version(&job.manifest.version),
                "health_version_mismatch"
            );
        }
        if !is_link_backed || !report.release_id.is_empty() {
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

        if job.manifest.artifact_type == ArtifactType::ComponentRelease {
            let comp = job.manifest.component.as_deref().unwrap_or("brainstem");
            let layout = self.component_layout(comp, job.manifest.systemd_unit.as_deref())?;
            if let Some(installed) = &job.installed_release_path {
                atomic_swap_link(&layout.last_good_link, Path::new(installed))?;
            }
            self.record_component_commit(comp, &job.version, Some(job.manifest.sequence))?;
            let _ = gc_old_releases(
                &layout.releases_dir,
                &layout.current_link,
                &layout.last_good_link,
                self.config.keep_history,
            );
        } else if self.is_release_link_mode() {
            let layout =
                self.component_layout("brainstem", job.manifest.systemd_unit.as_deref())?;
            if let Some(installed) = &job.installed_release_path {
                atomic_swap_link(&layout.last_good_link, Path::new(installed))?;
            }
            self.record_component_commit("brainstem", &job.version, Some(job.manifest.sequence))?;
            let _ = gc_old_releases(
                &layout.releases_dir,
                &layout.current_link,
                &layout.last_good_link,
                self.config.keep_history,
            );
        } else {
            write_slot(
                &self.config.root.join("last-known-good-slot"),
                job.active_slot,
            )?;
            self.record_component_commit("brainstem", &job.version, Some(job.manifest.sequence))?;
            let mut profile = self.profile.lock().unwrap_or_else(|p| p.into_inner());
            profile.active_slot = job.active_slot;
        }

        self.cleanup_staging(&job.job_id);
        write_json(&self.config.root.join("job.json"), &job)?;
        Ok(job)
    }

    fn required_job(&self) -> Result<Job> {
        self.job()?.context("ota_job_missing")
    }
}

struct SyntheticRollbackHealth {
    version: String,
}

impl HealthCheck for SyntheticRollbackHealth {
    fn check(&self) -> Result<HealthReport> {
        Ok(HealthReport {
            boot_id: String::new(),
            version: self.version.clone(),
            release_id: String::new(),
            brainstem_ready: true,
            strategy_ready: true,
            can_read_only: true,
            ok: true,
            motor_output_enabled: Some(false),
            cms_state: None,
            online_mask: None,
            imu_fresh: None,
            vbus_min_v: None,
            battery_percent: None,
        })
    }
}

fn evaluate_safety_result(
    query_res: Result<crate::health::SafetyStatus>,
    profile: &DeviceProfile,
    manifest: &Manifest,
    min_vbus_v: f32,
) -> SafetyDecision {
    match query_res {
        Ok(status) => {
            if status.motor_output_enabled {
                return SafetyDecision::Unsafe("motor_output_enabled".into());
            }
            if let Some(state) = status.cms_state.as_deref() {
                let trimmed = state.trim();
                if !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("grounded") {
                    return SafetyDecision::Unsafe(format!("cms_state_not_grounded: {trimmed}"));
                }
            }
            if let Some(vbus) = status.vbus_min_v
                && vbus > 0.0
                && vbus < min_vbus_v
            {
                return SafetyDecision::Unsafe(format!(
                    "vbus_too_low: {vbus:.2}V < {min_vbus_v:.2}V"
                ));
            }
            let battery = status.battery_percent.unwrap_or(profile.battery_percent);
            if battery < manifest.minimum_battery_percent {
                return SafetyDecision::Unsafe(format!(
                    "battery_too_low: {battery}% < {}%",
                    manifest.minimum_battery_percent
                ));
            }
            SafetyDecision::Safe
        }
        Err(error) => SafetyDecision::Unsafe(format!("safety_check_unreachable: {error}")),
    }
}

fn requires_safety_gate(manifest: &Manifest) -> bool {
    match manifest.artifact_type {
        ArtifactType::BrainstemRelease => true,
        ArtifactType::ComponentRelease => manifest.component.as_deref() == Some("brainstem"),
        ArtifactType::PolicyPackage => manifest.effective_activate(),
        ArtifactType::ReleaseBundle => manifest
            .bundle_items
            .iter()
            .any(|item| item.component == "brainstem"),
    }
}

async fn poll_health_until<C, P>(
    checker: Arc<C>,
    timeout: Duration,
    poll_interval: Duration,
    predicate: P,
) -> Result<()>
where
    C: HealthCheck + ?Sized + 'static,
    P: Fn(&HealthReport) -> bool,
{
    let deadline = Instant::now() + timeout;
    loop {
        let c = checker.clone();
        let res = tokio::task::spawn_blocking(move || c.check())
            .await
            .map_err(|e| anyhow::anyhow!("health_check_join: {e}"))
            .and_then(|r| r);
        match res {
            Ok(report) if predicate(&report) => return Ok(()),
            Ok(_) => {
                if Instant::now() >= deadline {
                    anyhow::bail!("health_predicate_failed");
                }
            }
            Err(err) => {
                if Instant::now() >= deadline {
                    return Err(err);
                }
            }
        }
        tokio::time::sleep(poll_interval).await;
    }
}

/// Garbage-collects old release directories in `releases_dir`, always preserving the targets of
/// `current_link` and `last_good_link` plus at most `keep_history` newest remaining directories.
/// Returns the list of removed directory paths.
pub fn gc_old_releases(
    releases_dir: &Path,
    current_link: &Path,
    last_good_link: &Path,
    keep_history: usize,
) -> Result<Vec<PathBuf>> {
    if !releases_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut protected = HashSet::new();
    for link in [current_link, last_good_link] {
        if let Ok(canon) = fs::canonicalize(link) {
            protected.insert(canon);
        }
        if let Some(target) = read_link_target(link) {
            protected.insert(target.clone());
            let resolved = if target.is_relative()
                && let Some(parent) = link.parent()
            {
                parent.join(&target)
            } else {
                target
            };
            protected.insert(resolved.clone());
            if let Ok(canon) = fs::canonicalize(&resolved) {
                protected.insert(canon);
            }
        }
    }

    let mut candidates: Vec<(PathBuf, u128, String)> = Vec::new();
    for entry in fs::read_dir(releases_dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_dir() || meta.file_type().is_symlink() {
            continue;
        }
        let canon = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if protected.contains(&path) || protected.contains(&canon) {
            continue;
        }
        let mtime_nanos = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_owned();
        candidates.push((path, mtime_nanos, name));
    }

    // Sort newest first: modification time descending, breaking ties by SemVer descending then name.
    candidates.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| compare_versions(&b.2, &a.2).unwrap_or(Ordering::Equal))
            .then_with(|| b.2.cmp(&a.2))
    });

    let mut removed = Vec::new();
    for (path, _, _) in candidates.into_iter().skip(keep_history) {
        if fs::remove_dir_all(&path).is_ok() {
            removed.push(path);
        }
    }
    Ok(removed)
}

fn load_component_versions(root: &Path) -> BTreeMap<String, String> {
    let mut versions = BTreeMap::new();
    let dir = root.join("components");
    let Ok(entries) = fs::read_dir(&dir) else {
        return versions;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if validate_component_name(name).is_err() {
            continue;
        }
        if let Some(ver) = read_trimmed(&path.join("current-version")) {
            versions.insert(name.to_owned(), ver);
        }
    }
    versions
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

fn is_nonempty_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

fn prepare_ota_release_dir(target_release: &Path) -> Result<()> {
    let root_bin = target_release.join("doso-ota");
    let sub_bin = target_release.join("bin/doso-ota");
    let has_root = is_nonempty_file(&root_bin);
    let has_sub = is_nonempty_file(&sub_bin);
    ensure!(has_root || has_sub, "ota_release_missing_binary");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for bin in [&root_bin, &sub_bin] {
            if is_nonempty_file(bin) {
                let mut perms = fs::metadata(bin)?.permissions();
                perms.set_mode(0o755);
                fs::set_permissions(bin, perms)?;
            }
        }
        if has_sub && fs::symlink_metadata(&root_bin).is_err() {
            std::os::unix::fs::symlink("bin/doso-ota", &root_bin)?;
        } else if has_root && fs::symlink_metadata(&sub_bin).is_err() {
            if let Some(parent) = sub_bin.parent() {
                fs::create_dir_all(parent)?;
            }
            std::os::unix::fs::symlink("../doso-ota", &sub_bin)?;
        }
    }
    #[cfg(not(unix))]
    {
        if has_sub && !root_bin.exists() {
            let _ = fs::copy(&sub_bin, &root_bin);
        } else if has_root && !sub_bin.exists() {
            if let Some(parent) = sub_bin.parent() {
                fs::create_dir_all(parent)?;
            }
            let _ = fs::copy(&root_bin, &sub_bin);
        }
    }
    Ok(())
}

fn ensure_brainstem_release_manifest(
    target_release: &Path,
    version: &str,
    release_id: &str,
) -> Result<()> {
    let manifest_path = target_release.join("manifest.json");
    if !manifest_path.exists() {
        let doc = serde_json::json!({
            "version": version,
            "release_id": release_id,
        });
        return write_json(&manifest_path, &doc);
    }
    if let Ok(bytes) = fs::read(&manifest_path)
        && let Ok(serde_json::Value::Object(mut obj)) = serde_json::from_slice(&bytes)
    {
        let existing_ver = obj
            .get("version")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let version_matches = existing_ver
            .map(|v| normalize_version(v) == normalize_version(version))
            .unwrap_or(true);
        if version_matches {
            let mut changed = false;
            if existing_ver.is_none() {
                obj.insert("version".into(), serde_json::json!(version));
                changed = true;
            }
            let existing_rel = obj
                .get("release_id")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            if existing_rel.is_none() {
                obj.insert("release_id".into(), serde_json::json!(release_id));
                changed = true;
            }
            if changed {
                return write_json(&manifest_path, &serde_json::Value::Object(obj));
            }
        }
    }
    Ok(())
}

fn remove_link_if_exists(link: &Path) -> Result<()> {
    let meta = match fs::symlink_metadata(link) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if meta.is_dir() && !meta.file_type().is_symlink() {
        fs::remove_dir_all(link)?;
    } else if fs::remove_file(link).is_err() {
        fs::remove_dir(link)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RollbackPolicy;
    use crate::health::{HealthReport, SafetyStatus};
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
                motor_output_enabled: Some(false),
                cms_state: Some("grounded".into()),
                online_mask: None,
                imu_fresh: None,
                vbus_min_v: Some(48.0),
                battery_percent: Some(85),
            })
        }
    }

    struct RecordingRestarter {
        units: Mutex<Vec<String>>,
    }

    impl RecordingRestarter {
        fn new() -> Self {
            Self {
                units: Mutex::new(Vec::new()),
            }
        }

        fn restarted_units(&self) -> Vec<String> {
            self.units.lock().unwrap().clone()
        }
    }

    impl ServiceRestarter for RecordingRestarter {
        fn restart(&self, unit: &str) -> Result<()> {
            self.units.lock().unwrap().push(unit.to_owned());
            Ok(())
        }
    }

    struct MockSafety {
        status: Mutex<SafetyStatus>,
    }

    impl MockSafety {
        fn new(status: SafetyStatus) -> Self {
            Self {
                status: Mutex::new(status),
            }
        }

        fn set(&self, status: SafetyStatus) {
            *self.status.lock().unwrap() = status;
        }
    }

    impl SafetyPreCheck for MockSafety {
        fn query_safety(&self) -> Result<SafetyStatus> {
            Ok(self.status.lock().unwrap().clone())
        }
    }

    fn base_manifest() -> Manifest {
        Manifest {
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
            component: None,
            systemd_unit: None,
            health_url: None,
            requires: BTreeMap::new(),
            bundle_items: Vec::new(),
            delta: None,
        }
    }

    fn sign(manifest: &Manifest, key: &SigningKey) -> ManifestEnvelope {
        let payload = serde_json::to_vec(manifest).unwrap();
        ManifestEnvelope {
            payload: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&payload),
            signature: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(key.sign(&payload).to_bytes()),
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
            components_root: None,
            keep_history: DEFAULT_KEEP_HISTORY,
            require_can_read_only: false,
            min_vbus_v: DEFAULT_MIN_VBUS_V,
            ota_unit: DEFAULT_OTA_UNIT.into(),
            policy_admin_url: None,
            policy_admin_token: None,
            allow_insecure_http_loopback: true,
        };
        let updater = Updater::with_restarter(config, Arc::new(RecordingRestarter::new())).unwrap();
        let envelope = sign(&base_manifest(), &key);
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

    #[test]
    fn safety_pre_check_blocks_while_walking_and_unblocks_when_grounded() {
        let (dir, mut updater, envelope) = setup();
        let mock_safety = Arc::new(MockSafety::new(SafetyStatus {
            motor_output_enabled: true,
            cms_state: Some("walking".into()),
            vbus_min_v: Some(48.0),
            battery_percent: Some(80),
        }));
        updater.set_safety_check(Some(mock_safety.clone()));

        updater.schedule(&envelope, "key-safe").unwrap();
        let job_id = updater.job().unwrap().unwrap().job_id;
        let artifact = dir.path().join(format!("state/staging/{job_id}.artifact"));
        fs::create_dir_all(artifact.parent().unwrap()).unwrap();
        fs::write(&artifact, b"abc").unwrap();
        let mut job = updater.job().unwrap().unwrap();
        job.phase = JobPhase::Verifying;
        write_json(&updater.config.root.join("job.json"), &job).unwrap();

        // While walking with motors enabled, install() pauses cleanly in WaitingSafeWindow!
        let paused = updater.install().unwrap();
        assert_eq!(paused.phase, JobPhase::WaitingSafeWindow);
        assert!(
            paused
                .error
                .as_deref()
                .unwrap_or("")
                .contains("motor_output_enabled")
        );

        // Even with motors disabled, low vbus (< 44.0V) still waits.
        mock_safety.set(SafetyStatus {
            motor_output_enabled: false,
            cms_state: Some("grounded".into()),
            vbus_min_v: Some(41.5),
            battery_percent: Some(80),
        });
        let paused_vbus = updater.install().unwrap();
        assert_eq!(paused_vbus.phase, JobPhase::WaitingSafeWindow);
        assert!(
            paused_vbus
                .error
                .as_deref()
                .unwrap_or("")
                .contains("vbus_too_low")
        );

        // Once robot is grounded, motors disabled, and voltage nominal -> proceeds to Rebooting!
        mock_safety.set(SafetyStatus {
            motor_output_enabled: false,
            cms_state: Some("grounded".into()),
            vbus_min_v: Some(48.5),
            battery_percent: Some(80),
        });
        let resumed = updater.install().unwrap();
        assert_eq!(resumed.phase, JobPhase::Rebooting);
        assert!(resumed.error.is_none());
    }

    #[test]
    fn gc_keeps_current_last_good_and_keep_history_newest() {
        let dir = tempfile::tempdir().unwrap();
        let releases = dir.path().join("releases");
        let current = dir.path().join("current");
        let last_good = dir.path().join("last-known-good");
        for ver in ["1.0.0", "1.0.1", "1.0.2", "1.0.3", "1.0.4", "1.0.5"] {
            let p = releases.join(ver);
            fs::create_dir_all(&p).unwrap();
            fs::write(p.join("marker"), ver).unwrap();
        }
        atomic_swap_link(&current, &releases.join("1.0.5")).unwrap();
        atomic_swap_link(&last_good, &releases.join("1.0.0")).unwrap();

        let removed = gc_old_releases(&releases, &current, &last_good, 2).unwrap();
        assert_eq!(removed.len(), 2);
        // Protected targets always preserved:
        assert!(releases.join("1.0.5").is_dir());
        assert!(releases.join("1.0.0").is_dir());
        // 2 newest remaining directories preserved:
        assert!(releases.join("1.0.4").is_dir());
        assert!(releases.join("1.0.3").is_dir());
        // Older directories deleted:
        assert!(!releases.join("1.0.1").exists());
        assert!(!releases.join("1.0.2").exists());
    }

    #[test]
    fn ota_self_upgrade_commits_when_healthy_and_rolls_back_when_unhealthy() {
        let dir = tempfile::tempdir().unwrap();
        let key = SigningKey::from_bytes(&[17; 32]);
        let key_path = dir.path().join("key.pem");
        fs::write(
            &key_path,
            key.verifying_key()
                .to_public_key_pem(Default::default())
                .unwrap(),
        )
        .unwrap();

        let components_root = dir.path().join("opt/doso/components");
        let ota_v1 = components_root.join("ota/releases/1.0.0");
        fs::create_dir_all(ota_v1.join("bin")).unwrap();
        fs::write(ota_v1.join("bin/doso-ota"), b"ota-v1").unwrap();
        atomic_swap_link(&components_root.join("ota/current"), &ota_v1).unwrap();
        atomic_swap_link(&components_root.join("ota/last-known-good"), &ota_v1).unwrap();

        // Build tarball for ota 1.1.0
        let src = dir.path().join("ota_src");
        fs::create_dir_all(src.join("bin")).unwrap();
        fs::write(src.join("bin/doso-ota"), b"ota-v2").unwrap();
        let archive_path = dir.path().join("ota-1.1.0.tar.gz");
        let st = Command::new("tar")
            .args(["-czf"])
            .arg(&archive_path)
            .args(["-C"])
            .arg(&src)
            .arg(".")
            .status()
            .unwrap();
        assert!(st.success());
        let bytes = fs::read(&archive_path).unwrap();

        let restarter = Arc::new(RecordingRestarter::new());
        let make_cfg = || UpdaterConfig {
            root: dir.path().join("state"),
            device_id: "d1".into(),
            model: "m1".into(),
            hardware_revision: "h1".into(),
            bootloader_version: "1.0.0".into(),
            brainstem_version: "1.0.0".into(),
            current_version: "1.0.0".into(),
            battery_percent: 90,
            online: true,
            task_running: false,
            boot_id_path: dir.path().join("boot_id"),
            verify_key_path: key_path.clone(),
            brainstem_unit: "doso-brainstem.service".into(),
            staging_dir: None,
            releases_dir: None,
            current_link: None,
            last_good_link: None,
            components_root: Some(components_root.clone()),
            keep_history: 2,
            require_can_read_only: false,
            min_vbus_v: 44.0,
            ota_unit: "doso-ota.service".into(),
            policy_admin_url: None,
            policy_admin_token: None,
            allow_insecure_http_loopback: true,
        };

        let updater = Updater::with_restarter(make_cfg(), restarter.clone()).unwrap();
        let mut m = base_manifest();
        m.artifact_type = ArtifactType::ComponentRelease;
        m.component = Some("ota".into());
        m.version = "1.1.0".into();
        m.artifact_size_bytes = bytes.len() as u64;
        m.artifact_sha256 = Manifest::artifact_hash(&bytes);
        let env = sign(&m, &key);
        let job = updater.schedule(&env, "self-upg-1").unwrap();
        let staged = updater.staging_artifact_path(&job.job_id);
        fs::write(&staged, &bytes).unwrap();
        let mut job = updater.job().unwrap().unwrap();
        job.phase = JobPhase::Verifying;
        write_json(&updater.config.root.join("job.json"), &job).unwrap();

        let installed = updater.install().unwrap();
        assert_eq!(installed.phase, JobPhase::Rebooting);
        assert_eq!(restarter.restarted_units(), vec!["doso-ota.service"]);
        assert!(updater.pending_self_upgrade().unwrap().is_some());
        assert_eq!(
            read_link_target(&components_root.join("ota/current")).unwrap(),
            components_root.join("ota/releases/1.1.0")
        );

        // Simulate new process startup failing self-check -> rolls back to ota v1.0.0!
        let restarted_bad = Updater::with_restarter(make_cfg(), restarter.clone()).unwrap();
        let rolled = restarted_bad
            .reconcile_self_upgrade(false)
            .unwrap()
            .unwrap();
        assert_eq!(rolled.phase, JobPhase::RolledBack);
        assert!(restarted_bad.pending_self_upgrade().unwrap().is_none());
        assert_eq!(
            read_link_target(&components_root.join("ota/current")).unwrap(),
            ota_v1
        );
        assert_eq!(
            restarter.restarted_units(),
            vec!["doso-ota.service", "doso-ota.service"]
        );

        // Now try again with sequence 2 and healthy reconcile -> commits v1.1.0!
        m.sequence = 2;
        let env2 = sign(&m, &key);
        let job2 = restarted_bad.schedule(&env2, "self-upg-2").unwrap();
        fs::write(restarted_bad.staging_artifact_path(&job2.job_id), &bytes).unwrap();
        let mut job2 = restarted_bad.job().unwrap().unwrap();
        job2.phase = JobPhase::Verifying;
        write_json(&restarted_bad.config.root.join("job.json"), &job2).unwrap();
        restarted_bad.install().unwrap();

        let restarted_good = Updater::with_restarter(make_cfg(), restarter.clone()).unwrap();
        let committed = restarted_good
            .reconcile_self_upgrade(true)
            .unwrap()
            .unwrap();
        assert_eq!(committed.phase, JobPhase::Succeeded);
        assert!(restarted_good.pending_self_upgrade().unwrap().is_none());
        assert_eq!(
            restarted_good.component_version("ota").as_deref(),
            Some("1.1.0")
        );
        assert_eq!(
            read_link_target(&components_root.join("ota/last-known-good")).unwrap(),
            components_root.join("ota/releases/1.1.0")
        );
    }

    #[test]
    fn schedule_defers_task_running_and_low_battery_to_waiting_safe_window() {
        let dir = tempfile::tempdir().unwrap();
        let key = SigningKey::from_bytes(&[18; 32]);
        let key_path = dir.path().join("key.pem");
        fs::write(
            &key_path,
            key.verifying_key()
                .to_public_key_pem(Default::default())
                .unwrap(),
        )
        .unwrap();

        let make_cfg = |task_running: bool, battery_percent: u8| UpdaterConfig {
            root: dir.path().join("state"),
            device_id: "d-safe".into(),
            model: "m1".into(),
            hardware_revision: "h1".into(),
            bootloader_version: "1.0.0".into(),
            brainstem_version: "1.0.0".into(),
            current_version: "1.0.0".into(),
            battery_percent,
            online: true,
            task_running,
            boot_id_path: dir.path().join("boot_id"),
            verify_key_path: key_path.clone(),
            brainstem_unit: "doso-brainstem.service".into(),
            staging_dir: None,
            releases_dir: None,
            current_link: None,
            last_good_link: None,
            components_root: None,
            keep_history: DEFAULT_KEEP_HISTORY,
            require_can_read_only: false,
            min_vbus_v: DEFAULT_MIN_VBUS_V,
            ota_unit: DEFAULT_OTA_UNIT.into(),
            policy_admin_url: None,
            policy_admin_token: None,
            allow_insecure_http_loopback: true,
        };

        // Even when task_running=true and battery=15% (< min 40%), schedule() succeeds instead of
        // hard-failing (which would falsely trip B3 rollout circuit breaker), and install() parks
        // the job cleanly in WaitingSafeWindow!
        let updater =
            Updater::with_restarter(make_cfg(true, 15), Arc::new(RecordingRestarter::new()))
                .unwrap();
        let envelope = sign(&base_manifest(), &key);
        let job = updater.schedule(&envelope, "defer-key-1").unwrap();
        assert_eq!(job.phase, JobPhase::Scheduled);

        // Stage artifact and advance to Verifying, then attempt install -> WaitingSafeWindow.
        let staged = updater.staging_artifact_path(&job.job_id);
        fs::write(&staged, b"abc").unwrap();
        let mut job = updater.job().unwrap().unwrap();
        job.phase = JobPhase::Verifying;
        write_json(&updater.config.root.join("job.json"), &job).unwrap();

        let waiting = updater.install().unwrap();
        assert_eq!(waiting.phase, JobPhase::WaitingSafeWindow);
        assert_eq!(waiting.error.as_deref(), Some("task_running"));

        // Direct Manifest::validate remains backward-compatible and still enforces live checks.
        assert!(
            base_manifest()
                .validate(100, &updater.profile(), 0)
                .is_err()
        );

        // Clear task_running but keep battery low -> still WaitingSafeWindow (battery_too_low).
        let updater_low_batt =
            Updater::with_restarter(make_cfg(false, 15), Arc::new(RecordingRestarter::new()))
                .unwrap();
        let waiting_batt = updater_low_batt.install().unwrap();
        assert_eq!(waiting_batt.phase, JobPhase::WaitingSafeWindow);
        assert!(
            waiting_batt
                .error
                .as_deref()
                .unwrap_or("")
                .contains("battery_too_low")
        );

        // Once task clears and battery charges to 80% -> install proceeds!
        let updater_ready =
            Updater::with_restarter(make_cfg(false, 80), Arc::new(RecordingRestarter::new()))
                .unwrap();
        let installed = updater_ready.install().unwrap();
        assert_eq!(installed.phase, JobPhase::Rebooting);
    }

    #[tokio::test]
    async fn non_brainstem_rollback_without_health_url_skips_brainstem_checker() {
        let dir = tempfile::tempdir().unwrap();
        let key = SigningKey::from_bytes(&[19; 32]);
        let key_path = dir.path().join("key.pem");
        fs::write(
            &key_path,
            key.verifying_key()
                .to_public_key_pem(Default::default())
                .unwrap(),
        )
        .unwrap();
        let components_root = dir.path().join("opt/doso/components");
        let v1 = components_root.join("voice/releases/0.1.0");
        let v2 = components_root.join("voice/releases/0.2.0");
        fs::create_dir_all(&v1).unwrap();
        fs::create_dir_all(&v2).unwrap();
        atomic_swap_link(&components_root.join("voice/current"), &v2).unwrap();
        atomic_swap_link(&components_root.join("voice/last-known-good"), &v1).unwrap();

        let restarter = Arc::new(RecordingRestarter::new());
        let cfg = UpdaterConfig {
            root: dir.path().join("state"),
            device_id: "d-rb".into(),
            model: "m1".into(),
            hardware_revision: "h1".into(),
            bootloader_version: "1.0.0".into(),
            brainstem_version: "1.0.0".into(),
            current_version: "1.0.0".into(),
            battery_percent: 90,
            online: true,
            task_running: false,
            boot_id_path: dir.path().join("boot_id"),
            verify_key_path: key_path,
            brainstem_unit: "doso-brainstem.service".into(),
            staging_dir: None,
            releases_dir: None,
            current_link: None,
            last_good_link: None,
            components_root: Some(components_root.clone()),
            keep_history: 2,
            require_can_read_only: false,
            min_vbus_v: 44.0,
            ota_unit: DEFAULT_OTA_UNIT.into(),
            policy_admin_url: None,
            policy_admin_token: None,
            allow_insecure_http_loopback: true,
        };
        let updater = Updater::with_restarter(cfg, restarter.clone()).unwrap();
        let mut m = base_manifest();
        m.artifact_type = ArtifactType::ComponentRelease;
        m.component = Some("voice".into());
        m.version = "0.2.0".into();
        m.health_url = None;
        let persisted = Job {
            job_id: "job-voice-rb".into(),
            idempotency_key: "idem-voice-rb".into(),
            device_id: "d-rb".into(),
            release_id: "r1".into(),
            version: "0.2.0".into(),
            manifest: m,
            phase: JobPhase::RollbackPending,
            downloaded_bytes: 3,
            total_bytes: 3,
            active_slot: Slot::A,
            last_known_good_slot: Slot::A,
            last_known_good_version: "0.1.0".into(),
            progress_sequence: 2,
            boot_id_before: None,
            boot_id_after: None,
            error: Some("interrupted_during_install".into()),
            previous_release_path: Some(v1.to_string_lossy().into_owned()),
            installed_release_path: Some(v2.to_string_lossy().into_owned()),
        };
        write_json(&updater.config.root.join("job.json"), &persisted).unwrap();

        // Execute recovery WITHOUT any brainstem health checker configured -> rolls back voice cleanly!
        let rolled = updater
            .execute_active_job::<crate::SystemHealthCheck, _>(
                None,
                Duration::from_millis(10),
                |_| {},
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rolled.phase, JobPhase::RolledBack);
        assert_eq!(
            read_link_target(&components_root.join("voice/current")).unwrap(),
            v1
        );
        assert_eq!(restarter.restarted_units(), vec!["doso-voice.service"]);
    }

    #[test]
    fn sequence_tracking_is_scoped_per_channel_and_survives_failed_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let key = SigningKey::from_bytes(&[20; 32]);
        let key_path = dir.path().join("key.pem");
        fs::write(
            &key_path,
            key.verifying_key()
                .to_public_key_pem(Default::default())
                .unwrap(),
        )
        .unwrap();

        let cfg = UpdaterConfig {
            root: dir.path().join("state"),
            device_id: "d-seq".into(),
            model: "m1".into(),
            hardware_revision: "h1".into(),
            bootloader_version: "1.0.0".into(),
            brainstem_version: "0.0.3".into(),
            current_version: "0.0.3".into(),
            battery_percent: 90,
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
            ota_unit: DEFAULT_OTA_UNIT.into(),
            policy_admin_url: None,
            policy_admin_token: None,
            allow_insecure_http_loopback: true,
        };
        let updater = Updater::with_restarter(cfg, Arc::new(RecordingRestarter::new())).unwrap();

        // Simulate Dog3's exact state: job.json has succeeded PolicyPackage(flat) sequence=201.
        let mut pol_flat = base_manifest();
        pol_flat.minimum_brainstem_version = "0.0.1".into();
        pol_flat.artifact_type = ArtifactType::PolicyPackage;
        pol_flat.policy_slot = Some("flat".into());
        pol_flat.sequence = 201;
        let done_pol = Job {
            job_id: "job-pol-201".into(),
            idempotency_key: "idem-pol-201".into(),
            device_id: "d-seq".into(),
            release_id: "rel-pol-201".into(),
            version: "1.0.3".into(),
            manifest: pol_flat.clone(),
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
        write_json(&updater.config.root.join("job.json"), &done_pol).unwrap();
        updater.record_policy_sequence(Some("flat"), 201).unwrap();

        // 1) BrainstemRelease with sequence=1 MUST NOT be blocked by PolicyPackage sequence=201!
        let mut bs = base_manifest();
        bs.minimum_brainstem_version = "0.0.1".into();
        bs.artifact_type = ArtifactType::BrainstemRelease;
        bs.version = "0.0.4".into();
        bs.sequence = 1;
        updater.schedule(&sign(&bs, &key), "idem-bs-1").unwrap();

        // Overwrite job.json with a Failed flat policy job -> anti-replay MUST still reject seq <= 201 on flat!
        let mut failed_pol = done_pol;
        failed_pol.job_id = "job-pol-202".into();
        failed_pol.idempotency_key = "idem-pol-202".into();
        failed_pol.manifest.sequence = 202;
        failed_pol.phase = JobPhase::Failed;
        write_json(&updater.config.root.join("job.json"), &failed_pol).unwrap();

        let mut replay_flat = pol_flat;
        replay_flat.sequence = 150;
        assert!(
            updater
                .schedule(&sign(&replay_flat, &key), "idem-flat-replay")
                .is_err()
        );

        // PolicyPackage on a different slot ("stairs") with sequence=1 IS allowed!
        let mut pol_stairs = replay_flat;
        pol_stairs.policy_slot = Some("stairs".into());
        pol_stairs.sequence = 1;
        updater
            .schedule(&sign(&pol_stairs, &key), "idem-stairs-1")
            .unwrap();
    }

    #[tokio::test]
    async fn pre_switch_hash_failure_marks_job_failed_instead_of_wedging_verifying() {
        let dir = tempfile::tempdir().unwrap();
        let key = SigningKey::from_bytes(&[21; 32]);
        let key_path = dir.path().join("key.pem");
        fs::write(
            &key_path,
            key.verifying_key()
                .to_public_key_pem(Default::default())
                .unwrap(),
        )
        .unwrap();

        let cfg = UpdaterConfig {
            root: dir.path().join("state"),
            device_id: "d-hash".into(),
            model: "m1".into(),
            hardware_revision: "h1".into(),
            bootloader_version: "1.0.0".into(),
            brainstem_version: "1.0.0".into(),
            current_version: "1.0.0".into(),
            battery_percent: 90,
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
            ota_unit: DEFAULT_OTA_UNIT.into(),
            policy_admin_url: None,
            policy_admin_token: None,
            allow_insecure_http_loopback: true,
        };
        let updater = Updater::with_restarter(cfg, Arc::new(RecordingRestarter::new())).unwrap();

        let job = updater
            .schedule(&sign(&base_manifest(), &key), "idem-corrupt")
            .unwrap();
        let staged = updater.staging_artifact_path(&job.job_id);
        fs::write(&staged, b"corrupted").unwrap();
        let mut verifying = updater.job().unwrap().unwrap();
        verifying.phase = JobPhase::Verifying;
        write_json(&updater.config.root.join("job.json"), &verifying).unwrap();

        let mut phases = Vec::new();
        let outcome = updater
            .execute_active_job(Some(Arc::new(Good)), Duration::from_millis(10), |j| {
                phases.push(j.phase);
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(outcome.phase, JobPhase::Failed);
        assert!(
            outcome
                .error
                .as_deref()
                .unwrap_or("")
                .contains("artifact_size_mismatch")
        );
        assert!(!staged.exists());
        assert_eq!(phases.last().copied(), Some(JobPhase::Failed));

        // Subsequent valid job can be scheduled cleanly because job.json is terminal!
        let mut m2 = base_manifest();
        m2.sequence = 2;
        assert!(updater.schedule(&sign(&m2, &key), "idem-next").is_ok());
    }

    #[test]
    fn prepare_ota_release_dir_and_brainstem_manifest_work_for_both_layouts() {
        let dir = tempfile::tempdir().unwrap();
        let empty_ota = dir.path().join("ota-empty");
        fs::create_dir_all(&empty_ota).unwrap();
        assert!(prepare_ota_release_dir(&empty_ota).is_err());

        let flat_ota = dir.path().join("ota-flat");
        fs::create_dir_all(&flat_ota).unwrap();
        fs::write(flat_ota.join("doso-ota"), b"elf-bytes").unwrap();
        prepare_ota_release_dir(&flat_ota).unwrap();
        assert!(flat_ota.join("doso-ota").exists());
        assert!(flat_ota.join("bin/doso-ota").exists());

        let bs_dir = dir.path().join("bs-0.0.4");
        fs::create_dir_all(bs_dir.join("bin")).unwrap();
        ensure_brainstem_release_manifest(&bs_dir, "v0.0.4", "rel-0.0.4").unwrap();
        let written: serde_json::Value =
            serde_json::from_slice(&fs::read(bs_dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(written["version"], "v0.0.4");
        assert_eq!(written["release_id"], "rel-0.0.4");

        // Existing manifest with matching version and missing release_id gets release_id filled.
        let bs_dir2 = dir.path().join("bs-0.0.5");
        fs::create_dir_all(&bs_dir2).unwrap();
        fs::write(
            bs_dir2.join("manifest.json"),
            r#"{"version":"v0.0.5","extra":"keep"}"#,
        )
        .unwrap();
        ensure_brainstem_release_manifest(&bs_dir2, "0.0.5", "rel-0.0.5").unwrap();
        let patched: serde_json::Value =
            serde_json::from_slice(&fs::read(bs_dir2.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(patched["release_id"], "rel-0.0.5");
        assert_eq!(patched["extra"], "keep");
    }

    #[test]
    fn gc_protects_relative_symlink_targets() {
        let dir = tempfile::tempdir().unwrap();
        let comp = dir.path().join("ota");
        let releases = comp.join("releases");
        for ver in ["0.0.1", "0.0.2", "0.0.3", "0.0.4"] {
            let p = releases.join(ver);
            fs::create_dir_all(&p).unwrap();
            fs::write(p.join("doso-ota"), ver).unwrap();
        }
        let current = comp.join("current");
        let last_good = comp.join("last-known-good");
        // Use relative targets like `releases/0.0.1` to verify parent-relative canonicalization!
        atomic_swap_link(&current, Path::new("releases/0.0.4")).unwrap();
        atomic_swap_link(&last_good, Path::new("releases/0.0.1")).unwrap();

        let removed = gc_old_releases(&releases, &current, &last_good, 1).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(releases.join("0.0.4").is_dir());
        assert!(releases.join("0.0.1").is_dir());
        assert!(releases.join("0.0.3").is_dir());
        assert!(!releases.join("0.0.2").exists());
    }
}
