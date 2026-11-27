use crate::DeviceRecord;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path as FsPath, PathBuf},
};

fn default_ring() -> String {
    "prod".into()
}

fn default_stage_percent() -> u8 {
    100
}

fn default_max_failure_count() -> usize {
    1
}

fn default_max_failure_rate() -> f64 {
    0.05
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RolloutStatus {
    Active,
    PausedManual,
    PausedCircuitOpen,
    Completed,
    Aborted,
}

impl RolloutStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Aborted)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RolloutStage {
    pub name: String,
    #[serde(default = "default_ring")]
    pub ring: String,
    #[serde(default = "default_stage_percent")]
    pub percent: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_devices: Option<usize>,
    #[serde(default)]
    pub bake_seconds: u64,
    #[serde(default)]
    pub auto_promote: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CircuitBreakerConfig {
    #[serde(default = "default_max_failure_count")]
    pub max_failure_count: usize,
    #[serde(default = "default_max_failure_rate")]
    pub max_failure_rate: f64,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            max_failure_count: default_max_failure_count(),
            max_failure_rate: default_max_failure_rate(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RolloutDeviceState {
    pub device_id: String,
    pub stage_index: usize,
    pub job_id: String,
    pub phase: String,
    pub assigned_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RolloutCampaign {
    pub rollout_id: String,
    pub release_id: String,
    pub target_model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_hardware_revision: Option<String>,
    #[serde(default)]
    pub target_tags: Vec<String>,
    pub stages: Vec<RolloutStage>,
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerConfig,
    pub status: RolloutStatus,
    pub current_stage_index: usize,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub stage_started_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub circuit_open_reason: Option<String>,
    #[serde(default)]
    pub device_states: BTreeMap<String, RolloutDeviceState>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateRolloutRequest {
    #[serde(default)]
    pub rollout_id: Option<String>,
    pub release_id: String,
    #[serde(default)]
    pub target_model: Option<String>,
    #[serde(default)]
    pub target_hardware_revision: Option<String>,
    #[serde(default)]
    pub target_tags: Vec<String>,
    pub stages: Vec<RolloutStage>,
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RolloutStageCounts {
    pub stage_index: usize,
    pub name: String,
    pub ring: String,
    pub percent: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_devices: Option<usize>,
    pub total: usize,
    pub in_progress: usize,
    pub waiting_safe_window: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub rolled_back: usize,
    pub recovery_required: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CircuitBreakerState {
    pub open: bool,
    pub failure_count: usize,
    pub total_evaluated: usize,
    pub failure_rate: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RolloutView {
    #[serde(flatten)]
    pub campaign: RolloutCampaign,
    pub stage_counts: Vec<RolloutStageCounts>,
    pub circuit_breaker_state: CircuitBreakerState,
}

/// Stable deterministic bucket in `1..=100` from `sha256(rollout_id + ":" + device_id)`.
pub fn device_bucket(rollout_id: &str, device_id: &str) -> u8 {
    let mut hasher = Sha256::new();
    hasher.update(rollout_id.as_bytes());
    hasher.update(b":");
    hasher.update(device_id.as_bytes());
    let digest = hasher.finalize();
    let prefix = u64::from_be_bytes([
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
    ]);
    ((prefix % 100) as u8) + 1
}

pub fn is_failure_phase(phase: &str) -> bool {
    matches!(phase, "rolled_back" | "failed" | "recovery_required")
}

pub fn validate_rollout_id(rollout_id: &str) -> Result<&str> {
    let id = rollout_id.trim();
    ensure!(
        !id.is_empty()
            && id.len() <= 128
            && !id.starts_with('.')
            && !id.contains("..")
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
        "rollout_id_invalid"
    );
    Ok(id)
}

pub fn validate_rollout_request(req: &CreateRolloutRequest) -> Result<()> {
    ensure!(!req.release_id.trim().is_empty(), "release_id_required");
    if let Some(ref rid) = req.rollout_id {
        validate_rollout_id(rid)?;
    }
    ensure!(!req.stages.is_empty(), "rollout_stages_empty");
    for stage in &req.stages {
        ensure!(!stage.name.trim().is_empty(), "rollout_stage_name_empty");
        ensure!(
            matches!(stage.ring.as_str(), "dev" | "canary" | "prod"),
            "rollout_stage_ring_invalid"
        );
        ensure!(
            (1..=100).contains(&stage.percent),
            "rollout_stage_percent_invalid"
        );
        if let Some(max) = stage.max_devices {
            ensure!(max > 0, "rollout_stage_max_devices_invalid");
        }
    }
    ensure!(
        (0.0..=1.0).contains(&req.circuit_breaker.max_failure_rate)
            && !req.circuit_breaker.max_failure_rate.is_nan(),
        "circuit_breaker_rate_invalid"
    );
    Ok(())
}

pub fn device_matches_rollout_target(rollout: &RolloutCampaign, device: &DeviceRecord) -> bool {
    if device.model != rollout.target_model {
        return false;
    }
    if let Some(ref hw) = rollout.target_hardware_revision
        && !doso_ota::hardware_revision_matches(hw, &device.hardware_revision)
    {
        return false;
    }
    rollout
        .target_tags
        .iter()
        .all(|tag| device.tags.iter().any(|t| t == tag))
}

/// Returns `Some(stage_index)` if `device_id` is eligible to be assigned a job right now
/// under `rollout` (must be `Active`, not yet assigned, matching model/hw/tags/ring, bucket,
/// and stage `max_devices`).
pub fn eligible_stage_for_device(
    rollout: &RolloutCampaign,
    device_id: &str,
    device: &DeviceRecord,
) -> Option<usize> {
    if rollout.status != RolloutStatus::Active {
        return None;
    }
    if rollout.device_states.contains_key(device_id) {
        return None;
    }
    if !device_matches_rollout_target(rollout, device) {
        return None;
    }
    let max_idx = rollout
        .current_stage_index
        .min(rollout.stages.len().saturating_sub(1));
    // Pick the highest active/reached stage index matching the device's ring so progressive
    // prod percentage expansions (10% -> 50% -> 100%) use the latest percentage cap.
    let (stage_idx, stage) = (0..=max_idx)
        .rev()
        .filter_map(|idx| rollout.stages.get(idx).map(|s| (idx, s)))
        .find(|(_, s)| s.ring == device.ring)?;

    let bucket = device_bucket(&rollout.rollout_id, device_id);
    if bucket > stage.percent {
        return None;
    }
    if let Some(max_devices) = stage.max_devices {
        let assigned_in_stage = rollout
            .device_states
            .values()
            .filter(|st| st.stage_index == stage_idx)
            .count();
        if assigned_in_stage >= max_devices {
            return None;
        }
    }
    Some(stage_idx)
}

/// Evaluates circuit breaker thresholds after a failure report and trips the rollout to
/// `RolloutStatus::PausedCircuitOpen` if exceeded. Returns `true` if tripped.
pub fn evaluate_circuit_breaker(
    rollout: &mut RolloutCampaign,
    trigger_device: &str,
    trigger_phase: &str,
    now: i64,
) -> bool {
    if rollout.status != RolloutStatus::Active || !is_failure_phase(trigger_phase) {
        return false;
    }
    let failures = rollout
        .device_states
        .values()
        .filter(|st| is_failure_phase(&st.phase))
        .count();
    let total = rollout.device_states.len().max(1);
    let failure_rate = failures as f64 / total as f64;
    let count_tripped = rollout.circuit_breaker.max_failure_count > 0
        && failures >= rollout.circuit_breaker.max_failure_count;
    let rate_tripped = failure_rate > rollout.circuit_breaker.max_failure_rate;
    if count_tripped || rate_tripped {
        rollout.status = RolloutStatus::PausedCircuitOpen;
        rollout.circuit_open_reason = Some(format!(
            "circuit_open: failures={failures}/{total} ({:.1}%) triggered by {trigger_device} ({trigger_phase})",
            failure_rate * 100.0
        ));
        rollout.updated_at_ms = now;
        return true;
    }
    false
}

/// Auto-promotes an `Active` rollout when the current stage has `auto_promote == true`,
/// `bake_seconds` have elapsed, zero circuit failures exist, and all targeted devices for
/// the current stage have reported `succeeded`. Returns `true` if rollout state changed.
pub fn maybe_auto_promote(
    rollout: &mut RolloutCampaign,
    registry: &HashMap<String, DeviceRecord>,
    now: i64,
) -> bool {
    if rollout.status != RolloutStatus::Active {
        return false;
    }
    let Some(stage) = rollout.stages.get(rollout.current_stage_index) else {
        return false;
    };
    if !stage.auto_promote {
        return false;
    }
    if rollout
        .device_states
        .values()
        .any(|st| is_failure_phase(&st.phase))
    {
        return false;
    }

    let eligible_ids: Vec<&String> = registry
        .iter()
        .filter(|(dev_id, rec)| {
            rec.enabled
                && device_matches_rollout_target(rollout, rec)
                && rec.ring == stage.ring
                && device_bucket(&rollout.rollout_id, dev_id) <= stage.percent
        })
        .map(|(dev_id, _)| dev_id)
        .collect();

    let already_earlier = eligible_ids
        .iter()
        .filter(|id| {
            rollout
                .device_states
                .get(id.as_str())
                .is_some_and(|st| st.stage_index < rollout.current_stage_index)
        })
        .count();
    let remaining_eligible = eligible_ids.len().saturating_sub(already_earlier);
    let expected_stage_count = match stage.max_devices {
        Some(max) => remaining_eligible.min(max),
        None => remaining_eligible,
    };
    if expected_stage_count == 0 {
        return false;
    }

    let stage_devices: Vec<&RolloutDeviceState> = rollout
        .device_states
        .values()
        .filter(|st| st.stage_index == rollout.current_stage_index)
        .collect();

    if stage_devices.is_empty() || stage_devices.len() < expected_stage_count {
        return false;
    }
    if !stage_devices.iter().all(|st| st.phase == "succeeded") {
        return false;
    }
    let bake_ms = i64::try_from(stage.bake_seconds)
        .unwrap_or(i64::MAX)
        .saturating_mul(1000);
    let bake_anchor_ms = stage_devices
        .iter()
        .map(|st| st.updated_at_ms)
        .max()
        .unwrap_or(rollout.stage_started_at_ms)
        .max(rollout.stage_started_at_ms);
    if now.saturating_sub(bake_anchor_ms) < bake_ms {
        return false;
    }
    if rollout.current_stage_index + 1 < rollout.stages.len() {
        rollout.current_stage_index += 1;
        rollout.stage_started_at_ms = now;
        rollout.updated_at_ms = now;
    } else {
        rollout.status = RolloutStatus::Completed;
        rollout.updated_at_ms = now;
    }
    true
}

pub fn build_rollout_view(campaign: &RolloutCampaign) -> RolloutView {
    let stage_counts = campaign
        .stages
        .iter()
        .enumerate()
        .map(|(idx, stage)| {
            let mut counts = RolloutStageCounts {
                stage_index: idx,
                name: stage.name.clone(),
                ring: stage.ring.clone(),
                percent: stage.percent,
                max_devices: stage.max_devices,
                total: 0,
                in_progress: 0,
                waiting_safe_window: 0,
                succeeded: 0,
                failed: 0,
                rolled_back: 0,
                recovery_required: 0,
            };
            for st in campaign
                .device_states
                .values()
                .filter(|s| s.stage_index == idx)
            {
                counts.total += 1;
                match st.phase.as_str() {
                    "succeeded" => counts.succeeded += 1,
                    "failed" => counts.failed += 1,
                    "rolled_back" => counts.rolled_back += 1,
                    "recovery_required" => counts.recovery_required += 1,
                    "waiting_safe_window" => counts.waiting_safe_window += 1,
                    "cancelled" => {}
                    _ => counts.in_progress += 1,
                }
            }
            counts
        })
        .collect();

    let failure_count = campaign
        .device_states
        .values()
        .filter(|st| is_failure_phase(&st.phase))
        .count();
    let total_evaluated = campaign.device_states.len();
    let failure_rate = if total_evaluated == 0 {
        0.0
    } else {
        failure_count as f64 / total_evaluated as f64
    };
    RolloutView {
        campaign: campaign.clone(),
        stage_counts,
        circuit_breaker_state: CircuitBreakerState {
            open: campaign.status == RolloutStatus::PausedCircuitOpen,
            failure_count,
            total_evaluated,
            failure_rate,
            reason: campaign.circuit_open_reason.clone(),
        },
    }
}

pub fn load_rollouts(root: &FsPath) -> Result<HashMap<String, RolloutCampaign>> {
    let mut out = HashMap::new();
    let dir = root.join("rollouts");
    if !dir.exists() {
        return Ok(out);
    }
    for entry in fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.extension().and_then(|v| v.to_str()) != Some("json") {
            continue;
        }
        let campaign: RolloutCampaign = serde_json::from_slice(&fs::read(&path)?)
            .with_context(|| format!("parse rollout {}", path.display()))?;
        out.insert(campaign.rollout_id.clone(), campaign);
    }
    Ok(out)
}

fn rollout_path(root: &FsPath, rollout_id: &str) -> PathBuf {
    root.join("rollouts").join(format!("{rollout_id}.json"))
}

pub fn persist_rollout(root: &FsPath, rollout: &RolloutCampaign) -> Result<()> {
    crate::atomic_write_json(&rollout_path(root, &rollout.rollout_id), rollout)
}
