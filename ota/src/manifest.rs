use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};

/// Returns true when a manifest or rollout `hardwareRevision` targets any compatible ARM64 board
/// (e.g. RDK S100P, Jetson Xavier NX) rather than one specific board revision.
pub fn is_universal_hardware_revision(hw: &str) -> bool {
    matches!(
        hw.trim().to_ascii_lowercase().as_str(),
        "*" | "any" | "arm64-universal"
    )
}

/// Returns true when a release/rollout `manifest_hw` is compatible with a device's `device_hw`.
pub fn hardware_revision_matches(manifest_hw: &str, device_hw: &str) -> bool {
    is_universal_hardware_revision(manifest_hw) || manifest_hw == device_hw
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactType {
    #[default]
    BrainstemRelease,
    PolicyPackage,
    ComponentRelease,
    ReleaseBundle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RollbackPolicy {
    pub mode: String,
    pub health_timeout_ms: u64,
}

fn default_delta_codec() -> String {
    "zstd_patch".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeltaSpec {
    #[serde(alias = "base_version")]
    pub base_version: String,
    #[serde(alias = "base_sha256")]
    pub base_sha256: String,
    #[serde(alias = "patch_url")]
    pub patch_url: String,
    #[serde(alias = "patch_size_bytes")]
    pub patch_size_bytes: u64,
    #[serde(alias = "patch_sha256")]
    pub patch_sha256: String,
    #[serde(default = "default_delta_codec")]
    pub codec: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleItem {
    pub component: String,
    pub version: String,
    #[serde(alias = "artifact_url")]
    pub artifact_url: String,
    #[serde(alias = "artifact_size_bytes")]
    pub artifact_size_bytes: u64,
    #[serde(alias = "artifact_sha256")]
    pub artifact_sha256: String,
    #[serde(
        default,
        alias = "systemd_unit",
        skip_serializing_if = "Option::is_none"
    )]
    pub systemd_unit: Option<String>,
    #[serde(
        default,
        alias = "health_url",
        alias = "health_check_url",
        skip_serializing_if = "Option::is_none"
    )]
    pub health_url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub requires: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<DeltaSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub manifest_version: u8,
    pub release_id: String,
    pub channel: String,
    pub sequence: u64,
    pub version: String,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub model: String,
    pub hardware_revision: String,
    pub minimum_bootloader_version: String,
    pub minimum_brainstem_version: String,
    pub minimum_battery_percent: u8,
    pub artifact_url: String,
    pub artifact_size_bytes: u64,
    pub artifact_sha256: String,
    pub key_id: String,
    pub rollback_policy: RollbackPolicy,
    #[serde(default, alias = "artifact_type")]
    pub artifact_type: ArtifactType,
    #[serde(
        default,
        alias = "policy_slot",
        skip_serializing_if = "Option::is_none"
    )]
    pub policy_slot: Option<String>,
    #[serde(
        default,
        alias = "shadow_frames",
        skip_serializing_if = "Option::is_none"
    )]
    pub shadow_frames: Option<u32>,
    #[serde(default, alias = "activate", skip_serializing_if = "Option::is_none")]
    pub activate: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    #[serde(
        default,
        alias = "systemd_unit",
        skip_serializing_if = "Option::is_none"
    )]
    pub systemd_unit: Option<String>,
    #[serde(
        default,
        alias = "health_url",
        alias = "health_check_url",
        skip_serializing_if = "Option::is_none"
    )]
    pub health_url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub requires: BTreeMap<String, String>,
    #[serde(default, alias = "bundle_items", skip_serializing_if = "Vec::is_empty")]
    pub bundle_items: Vec<BundleItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<DeltaSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestEnvelope {
    pub payload: String,
    pub signature: String,
}

impl ManifestEnvelope {
    pub fn decode(&self, key: &VerifyingKey) -> Result<Manifest> {
        let payload = URL_SAFE_NO_PAD
            .decode(&self.payload)
            .context("manifest_payload")?;
        ensure!(payload.len() <= 64 * 1024, "manifest_too_large");
        let signature = Signature::from_slice(
            &URL_SAFE_NO_PAD
                .decode(&self.signature)
                .context("manifest_signature")?,
        )?;
        key.verify_strict(&payload, &signature)
            .context("manifest_signature_invalid")?;
        serde_json::from_slice(&payload).context("manifest_json")
    }
}

impl Manifest {
    pub fn effective_shadow_frames(&self) -> u32 {
        self.shadow_frames.unwrap_or(50)
    }

    pub fn effective_activate(&self) -> bool {
        self.activate.unwrap_or(true)
    }

    pub fn effective_component(&self) -> Option<&str> {
        match self.artifact_type {
            ArtifactType::BrainstemRelease => Some("brainstem"),
            ArtifactType::ComponentRelease => self.component.as_deref(),
            ArtifactType::PolicyPackage | ArtifactType::ReleaseBundle => None,
        }
    }

    pub fn validate(
        &self,
        now_ms: i64,
        profile: &crate::DeviceProfile,
        last_sequence: u64,
    ) -> Result<()> {
        let allow_http = std::env::var("DOSO_OTA_ALLOW_HTTP").as_deref() == Ok("1");
        self.validate_with_http_policy(now_ms, profile, last_sequence, allow_http)
    }

    pub fn validate_with_http_policy(
        &self,
        now_ms: i64,
        profile: &crate::DeviceProfile,
        last_sequence: u64,
        allow_http_loopback: bool,
    ) -> Result<()> {
        self.validate_inner(now_ms, profile, last_sequence, allow_http_loopback, true)
    }

    /// Validates static manifest integrity, compatibility, and sequence checks at job schedule
    /// time while deferring live-mutable safety checks (`task_running`, `battery_percent`) to
    /// `Updater::evaluate_safety_pre_check` so jobs park in `WaitingSafeWindow` instead of
    /// hard-failing and tripping rollout circuit breakers.
    pub fn validate_for_schedule(
        &self,
        now_ms: i64,
        profile: &crate::DeviceProfile,
        last_sequence: u64,
        allow_http_loopback: bool,
    ) -> Result<()> {
        self.validate_inner(now_ms, profile, last_sequence, allow_http_loopback, false)
    }

    fn validate_inner(
        &self,
        now_ms: i64,
        profile: &crate::DeviceProfile,
        last_sequence: u64,
        allow_http_loopback: bool,
        enforce_live_safety: bool,
    ) -> Result<()> {
        ensure!(self.manifest_version == 1, "manifest_version_unsupported");
        ensure!(
            !self.release_id.is_empty() && !self.key_id.is_empty(),
            "manifest_identity_invalid"
        );
        ensure!(
            matches!(self.channel.as_str(), "dev" | "qa" | "stable"),
            "release_channel_invalid"
        );
        const ISSUED_AT_SKEW_LEEWAY_MS: i64 = 60_000;
        ensure!(
            self.issued_at_ms <= now_ms.saturating_add(ISSUED_AT_SKEW_LEEWAY_MS)
                && self.expires_at_ms > now_ms,
            "manifest_expired"
        );
        ensure!(
            self.rollback_policy.mode == "automatic",
            "rollback_policy_invalid"
        );
        ensure!(
            self.rollback_policy.health_timeout_ms > 0,
            "rollback_timeout_invalid"
        );
        ensure!(self.model == profile.model, "target_model_mismatch");
        ensure!(
            hardware_revision_matches(&self.hardware_revision, &profile.hardware_revision),
            "target_hardware_mismatch"
        );
        ensure!(
            matches!(
                compare_core_versions(
                    &self.minimum_bootloader_version,
                    &profile.bootloader_version
                )?,
                Ordering::Less | Ordering::Equal
            ),
            "bootloader_too_old"
        );
        ensure!(
            matches!(
                compare_core_versions(&self.minimum_brainstem_version, &profile.brainstem_version)?,
                Ordering::Less | Ordering::Equal
            ),
            "brainstem_too_old"
        );
        ensure!(parse_version(&self.version).is_ok(), "version_invalid");

        if let Some(unit) = &self.systemd_unit {
            validate_unit_name(unit)?;
        }
        if let Some(health_url) = &self.health_url {
            validate_health_url(health_url)?;
        }
        if let Some(delta) = &self.delta {
            validate_delta_spec(delta, allow_http_loopback)?;
        }

        let installed = effective_installed_components(profile);
        validate_requires(&self.requires, &installed)?;

        match self.artifact_type {
            ArtifactType::BrainstemRelease => {
                if let Some(comp) = &self.component {
                    validate_component_name(comp)?;
                }
                ensure!(
                    matches!(
                        compare_versions(&self.version, &profile.current_version)?,
                        Ordering::Greater
                    ),
                    "version_not_newer"
                );
            }
            ArtifactType::PolicyPackage => {
                if let Some(slot) = &self.policy_slot {
                    ensure!(
                        !slot.trim().is_empty()
                            && !slot.contains("..")
                            && slot
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)),
                        "policy_slot_invalid"
                    );
                }
            }
            ArtifactType::ComponentRelease => {
                let comp = self.component.as_deref().context("component_required")?;
                validate_component_name(comp)?;
                if let Some(existing_ver) = installed.get(comp) {
                    ensure!(
                        matches!(
                            compare_versions(&self.version, existing_ver)?,
                            Ordering::Greater
                        ),
                        "version_not_newer"
                    );
                }
            }
            ArtifactType::ReleaseBundle => {
                ensure!(!self.bundle_items.is_empty(), "bundle_items_empty");
                let mut seen = BTreeSet::new();
                let mut simulated_installed = installed;
                for item in &self.bundle_items {
                    validate_component_name(&item.component)?;
                    ensure!(
                        item.component != "ota",
                        "bundle_ota_self_upgrade_unsupported"
                    );
                    ensure!(
                        seen.insert(item.component.clone()),
                        "bundle_duplicate_component: {}",
                        item.component
                    );
                    ensure!(parse_version(&item.version).is_ok(), "version_invalid");
                    if let Some(unit) = &item.systemd_unit {
                        validate_unit_name(unit)?;
                    }
                    if let Some(health_url) = &item.health_url {
                        validate_health_url(health_url)?;
                    }
                    if let Some(delta) = &item.delta {
                        validate_delta_spec(delta, allow_http_loopback)?;
                    }
                    ensure!(item.artifact_size_bytes > 0, "artifact_size_invalid");
                    validate_sha256_hex(&item.artifact_sha256, "artifact_hash_invalid")?;
                    validate_artifact_url(&item.artifact_url, allow_http_loopback)?;
                    validate_requires(&item.requires, &simulated_installed)?;
                    simulated_installed.insert(item.component.clone(), item.version.clone());
                }
            }
        }
        ensure!(profile.online, "device_offline");
        if enforce_live_safety {
            ensure!(!profile.task_running, "task_in_progress");
            ensure!(
                profile.battery_percent >= self.minimum_battery_percent,
                "battery_too_low"
            );
        }
        ensure!(self.sequence > last_sequence, "release_sequence_not_new");
        if self.artifact_type != ArtifactType::ReleaseBundle
            || self.artifact_size_bytes > 0
            || !self.artifact_url.is_empty()
        {
            ensure!(self.artifact_size_bytes > 0, "artifact_size_invalid");
            validate_sha256_hex(&self.artifact_sha256, "artifact_hash_invalid")?;
            validate_artifact_url(&self.artifact_url, allow_http_loopback)?;
        }
        Ok(())
    }

    pub fn artifact_hash(bytes: &[u8]) -> String {
        let mut hash = Sha256::new();
        hash.update(bytes);
        format!("{:x}", hash.finalize())
    }
}

fn effective_installed_components(profile: &crate::DeviceProfile) -> BTreeMap<String, String> {
    let mut map = profile.component_versions.clone();
    map.entry("brainstem".into())
        .or_insert_with(|| profile.current_version.clone());
    map
}

pub fn validate_component_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 64
            && !name.starts_with('-')
            && !name.ends_with('-')
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "component_name_invalid"
    );
    Ok(())
}

pub fn validate_unit_name(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 128
            && !value.contains("..")
            && !value.contains('/')
            && !value.contains('\\')
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ".@_:-".contains(c)),
        "invalid_systemd_unit"
    );
    Ok(())
}

pub fn validate_requires(
    requires: &BTreeMap<String, String>,
    installed: &BTreeMap<String, String>,
) -> Result<()> {
    for (dep, min_ver) in requires {
        validate_component_name(dep)?;
        ensure!(parse_version(min_ver).is_ok(), "requires_version_invalid");
        let installed_ver = installed
            .get(dep)
            .with_context(|| format!("component_dependency_missing: {dep}"))?;
        ensure!(
            matches!(
                compare_core_versions(min_ver, installed_ver)?,
                Ordering::Less | Ordering::Equal
            ),
            "component_dependency_too_old: {dep} requires >= {min_ver}, installed {installed_ver}"
        );
    }
    Ok(())
}

fn validate_sha256_hex(hash: &str, code: &str) -> Result<()> {
    ensure!(
        hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
        "{code}"
    );
    Ok(())
}

fn validate_artifact_url(raw: &str, allow_http_loopback: bool) -> Result<()> {
    let url = url::Url::parse(raw).context("artifact_url_invalid")?;
    ensure!(
        url.scheme() == "https"
            || (matches!(url.host_str(), Some("127.0.0.1" | "localhost")) && allow_http_loopback),
        "artifact_url_must_be_https"
    );
    Ok(())
}

fn validate_health_url(raw: &str) -> Result<()> {
    let url = url::Url::parse(raw).context("health_url_invalid")?;
    ensure!(
        matches!(url.scheme(), "http" | "https"),
        "health_url_scheme_invalid"
    );
    Ok(())
}

fn validate_delta_spec(delta: &DeltaSpec, allow_http_loopback: bool) -> Result<()> {
    ensure!(
        parse_version(&delta.base_version).is_ok(),
        "delta_base_version_invalid"
    );
    validate_sha256_hex(&delta.base_sha256, "delta_base_hash_invalid")?;
    ensure!(delta.patch_size_bytes > 0, "delta_patch_size_invalid");
    validate_sha256_hex(&delta.patch_sha256, "delta_patch_hash_invalid")?;
    validate_artifact_url(&delta.patch_url, allow_http_loopback)?;
    ensure!(
        !delta.codec.trim().is_empty()
            && delta
                .codec
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')),
        "delta_codec_invalid"
    );
    Ok(())
}

pub fn normalize_version(value: &str) -> &str {
    value.trim().strip_prefix('v').unwrap_or(value.trim())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SemVer {
    major: u64,
    minor: u64,
    patch: u64,
    prerelease: Option<String>,
}

impl Ord for SemVer {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (&self.prerelease, &other.prerelease) {
                (None, None) => Ordering::Equal,
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (Some(a), Some(b)) => a.cmp(b),
            })
    }
}

impl PartialOrd for SemVer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn parse_version(value: &str) -> Result<SemVer> {
    let normalized = normalize_version(value);
    let (core, prerelease) = match normalized.split_once('-') {
        Some((c, pre)) => {
            ensure!(
                !pre.is_empty()
                    && pre
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-'),
                "version_invalid"
            );
            (c, Some(pre.to_owned()))
        }
        None => (normalized, None),
    };
    let parts: Vec<_> = core.split('.').collect();
    ensure!(parts.len() == 3, "version_invalid");
    Ok(SemVer {
        major: parts[0].parse()?,
        minor: parts[1].parse()?,
        patch: parts[2].parse()?,
        prerelease,
    })
}

pub fn compare_versions(left: &str, right: &str) -> Result<std::cmp::Ordering> {
    Ok(parse_version(left)?.cmp(&parse_version(right)?))
}

/// Compares major.minor.patch only so build-flavor suffixes like `1.0.0-rust` satisfy
/// minimum compatibility gates like `minimum_brainstem_version: "1.0.0"`.
pub fn compare_core_versions(left: &str, right: &str) -> Result<std::cmp::Ordering> {
    let l = parse_version(left)?;
    let r = parse_version(right)?;
    Ok((l.major, l.minor, l.patch).cmp(&(r.major, r.minor, r.patch)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Slot;
    use ed25519_dalek::{Signer, SigningKey};

    fn fixture() -> (Manifest, SigningKey) {
        (
            Manifest {
                manifest_version: 1,
                release_id: "r1".into(),
                channel: "qa".into(),
                sequence: 2,
                version: "1.2.0".into(),
                issued_at_ms: 1,
                expires_at_ms: 10_000,
                model: "THUNDER 01".into(),
                hardware_revision: "rev-a".into(),
                minimum_bootloader_version: "1.0.0".into(),
                minimum_brainstem_version: "1.0.0".into(),
                minimum_battery_percent: 40,
                artifact_url: "https://example.test/release".into(),
                artifact_size_bytes: 3,
                artifact_sha256: Manifest::artifact_hash(b"abc"),
                key_id: "prod-1".into(),
                rollback_policy: RollbackPolicy {
                    mode: "automatic".into(),
                    health_timeout_ms: 30_000,
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
            },
            SigningKey::from_bytes(&[7; 32]),
        )
    }

    fn base_profile() -> crate::DeviceProfile {
        crate::DeviceProfile {
            device_id: "d".into(),
            model: "THUNDER 01".into(),
            hardware_revision: "rev-a".into(),
            bootloader_version: "1.1.0".into(),
            brainstem_version: "1.1.0".into(),
            current_version: "1.1.0".into(),
            active_slot: Slot::A,
            battery_percent: 80,
            online: true,
            task_running: false,
            component_versions: BTreeMap::new(),
        }
    }

    #[test]
    fn signed_manifest_is_verified_before_decode() {
        let (manifest, key) = fixture();
        let payload = serde_json::to_vec(&manifest).unwrap();
        let envelope = ManifestEnvelope {
            payload: URL_SAFE_NO_PAD.encode(&payload),
            signature: URL_SAFE_NO_PAD.encode(key.sign(&payload).to_bytes()),
        };
        let profile = base_profile();
        assert!(
            envelope
                .decode(&key.verifying_key())
                .unwrap()
                .validate(100, &profile, 1)
                .is_ok()
        );
        let mut forged = envelope;
        forged.signature = URL_SAFE_NO_PAD.encode([0; 64]);
        assert!(forged.decode(&key.verifying_key()).is_err());
    }

    #[test]
    fn legacy_manifest_defaults_to_brainstem_release_and_policy_fields_deserialize() {
        let legacy_json = serde_json::json!({
            "manifestVersion": 1,
            "releaseId": "r-legacy",
            "channel": "stable",
            "sequence": 1,
            "version": "1.1.0-rust",
            "issuedAtMs": 1,
            "expiresAtMs": 1000,
            "model": "THUNDER 01",
            "hardwareRevision": "rev-a",
            "minimumBootloaderVersion": "1.0.0",
            "minimumBrainstemVersion": "1.0.0",
            "minimumBatteryPercent": 20,
            "artifactUrl": "https://example.test/r.tar.gz",
            "artifactSizeBytes": 10,
            "artifactSha256": Manifest::artifact_hash(b"0123456789"),
            "keyId": "k1",
            "rollbackPolicy": { "mode": "automatic", "healthTimeoutMs": 5000 }
        });
        let legacy: Manifest = serde_json::from_value(legacy_json.clone()).unwrap();
        assert_eq!(legacy.artifact_type, ArtifactType::BrainstemRelease);
        assert_eq!(legacy.policy_slot, None);
        assert_eq!(legacy.effective_shadow_frames(), 50);
        assert!(legacy.effective_activate());
        assert!(legacy.component.is_none());
        assert!(legacy.requires.is_empty());
        assert!(legacy.bundle_items.is_empty());
        assert!(legacy.delta.is_none());

        // Snake-case aliases accepted cleanly.
        let mut snake_json = legacy_json.clone();
        snake_json["artifact_type"] = serde_json::json!("policy_package");
        snake_json["policy_slot"] = serde_json::json!("flat");
        snake_json["shadow_frames"] = serde_json::json!(25);
        snake_json["activate"] = serde_json::json!(false);
        let policy: Manifest = serde_json::from_value(snake_json).unwrap();
        assert_eq!(policy.artifact_type, ArtifactType::PolicyPackage);
        assert_eq!(policy.policy_slot.as_deref(), Some("flat"));
        assert_eq!(policy.effective_shadow_frames(), 25);
        assert!(!policy.effective_activate());

        // CamelCase canonical fields accepted cleanly.
        let mut camel_json = legacy_json;
        camel_json["artifactType"] = serde_json::json!("policy_package");
        camel_json["policySlot"] = serde_json::json!("stairs");
        camel_json["shadowFrames"] = serde_json::json!(60);
        camel_json["activate"] = serde_json::json!(true);
        let camel: Manifest = serde_json::from_value(camel_json).unwrap();
        assert_eq!(camel.artifact_type, ArtifactType::PolicyPackage);
        assert_eq!(camel.policy_slot.as_deref(), Some("stairs"));
        assert_eq!(camel.effective_shadow_frames(), 60);
        assert!(camel.effective_activate());
    }

    #[test]
    fn validates_component_name_requires_and_delta_schema() {
        assert!(validate_component_name("cloud-link").is_ok());
        assert!(validate_component_name("brainstem").is_ok());
        assert!(validate_component_name("nav2").is_ok());
        assert!(validate_component_name("").is_err());
        assert!(validate_component_name("../evil").is_err());
        assert!(validate_component_name("CloudLink").is_err());
        assert!(validate_component_name("-bad").is_err());
        assert!(validate_component_name("bad-").is_err());
        assert!(validate_component_name("a/b").is_err());

        let (mut m, _) = fixture();
        m.artifact_type = ArtifactType::ComponentRelease;
        m.component = Some("cloud-link".into());
        m.systemd_unit = Some("doso-cloud-link.service".into());
        m.health_url = Some("http://127.0.0.1:8090/health".into());
        m.version = "0.2.0".into();
        m.requires.insert("brainstem".into(), "1.1.0".into());
        m.delta = Some(DeltaSpec {
            base_version: "0.1.0".into(),
            base_sha256: Manifest::artifact_hash(b"base"),
            patch_url: "https://example.test/patch.zst".into(),
            patch_size_bytes: 12,
            patch_sha256: Manifest::artifact_hash(b"patch"),
            codec: "zstd_patch".into(),
        });
        let profile = base_profile();
        assert!(m.validate(100, &profile, 1).is_ok());

        // Dependency too old is rejected.
        m.requires.insert("brainstem".into(), "2.0.0".into());
        assert!(m.validate(100, &profile, 1).is_err());

        // Missing dependency is rejected.
        m.requires.clear();
        m.requires.insert("voice".into(), "0.1.0".into());
        assert!(m.validate(100, &profile, 1).is_err());
    }

    #[test]
    fn validates_release_bundle_topological_dependencies() {
        let (mut m, _) = fixture();
        m.artifact_type = ArtifactType::ReleaseBundle;
        m.version = "2.0.0".into();
        m.bundle_items = vec![
            BundleItem {
                component: "brainstem".into(),
                version: "1.5.0".into(),
                artifact_url: "https://example.test/brainstem.tar.gz".into(),
                artifact_size_bytes: 10,
                artifact_sha256: Manifest::artifact_hash(b"0123456789"),
                systemd_unit: Some("doso-brainstem.service".into()),
                health_url: Some("http://127.0.0.1:5054/health".into()),
                requires: BTreeMap::new(),
                delta: None,
            },
            BundleItem {
                component: "cloud-link".into(),
                version: "0.5.0".into(),
                artifact_url: "https://example.test/cloud-link.tar.gz".into(),
                artifact_size_bytes: 10,
                artifact_sha256: Manifest::artifact_hash(b"abcdefghij"),
                systemd_unit: Some("doso-cloud-link.service".into()),
                health_url: None,
                requires: BTreeMap::from([("brainstem".into(), "1.5.0".into())]),
                delta: None,
            },
        ];
        let profile = base_profile();
        assert!(m.validate(100, &profile, 1).is_ok());

        // Reversing order breaks topological dependency (cloud-link needs brainstem >= 1.5.0).
        m.bundle_items.reverse();
        assert!(m.validate(100, &profile, 1).is_err());

        // Including `ota` inside a ReleaseBundle is rejected (must use standalone ComponentRelease).
        let mut ota_bundle = m.clone();
        ota_bundle.bundle_items = vec![BundleItem {
            component: "ota".into(),
            version: "1.1.0".into(),
            artifact_url: "https://example.test/ota.tar.gz".into(),
            artifact_size_bytes: 10,
            artifact_sha256: Manifest::artifact_hash(b"0123456789"),
            systemd_unit: None,
            health_url: None,
            requires: BTreeMap::new(),
            delta: None,
        }];
        assert!(ota_bundle.validate(100, &profile, 1).is_err());
    }
}
