use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactType {
    #[default]
    BrainstemRelease,
    PolicyPackage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RollbackPolicy {
    pub mode: String,
    pub health_timeout_ms: u64,
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
        ensure!(self.manifest_version == 1, "manifest_version_unsupported");
        ensure!(
            !self.release_id.is_empty() && !self.key_id.is_empty(),
            "manifest_identity_invalid"
        );
        ensure!(
            matches!(self.channel.as_str(), "dev" | "qa" | "stable"),
            "release_channel_invalid"
        );
        ensure!(
            self.issued_at_ms <= now_ms && self.expires_at_ms > now_ms,
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
            self.hardware_revision == profile.hardware_revision,
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
        match self.artifact_type {
            ArtifactType::BrainstemRelease => {
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
                            && slot
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)),
                        "policy_slot_invalid"
                    );
                }
            }
        }
        ensure!(profile.online, "device_offline");
        ensure!(!profile.task_running, "task_in_progress");
        ensure!(
            profile.battery_percent >= self.minimum_battery_percent,
            "battery_too_low"
        );
        ensure!(self.sequence > last_sequence, "release_sequence_not_new");
        ensure!(self.artifact_size_bytes > 0, "artifact_size_invalid");
        ensure!(
            self.artifact_sha256.len() == 64
                && self.artifact_sha256.bytes().all(|b| b.is_ascii_hexdigit()),
            "artifact_hash_invalid"
        );
        let url = url::Url::parse(&self.artifact_url).context("artifact_url_invalid")?;
        ensure!(
            url.scheme() == "https"
                || (matches!(url.host_str(), Some("127.0.0.1" | "localhost"))
                    && allow_http_loopback),
            "artifact_url_must_be_https"
        );
        Ok(())
    }

    pub fn artifact_hash(bytes: &[u8]) -> String {
        let mut hash = Sha256::new();
        hash.update(bytes);
        format!("{:x}", hash.finalize())
    }
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

fn compare_versions(left: &str, right: &str) -> Result<std::cmp::Ordering> {
    Ok(parse_version(left)?.cmp(&parse_version(right)?))
}

/// Compares major.minor.patch only so build-flavor suffixes like `1.0.0-rust` satisfy
/// minimum compatibility gates like `minimum_brainstem_version: "1.0.0"`.
fn compare_core_versions(left: &str, right: &str) -> Result<std::cmp::Ordering> {
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
            },
            SigningKey::from_bytes(&[7; 32]),
        )
    }

    #[test]
    fn signed_manifest_is_verified_before_decode() {
        let (manifest, key) = fixture();
        let payload = serde_json::to_vec(&manifest).unwrap();
        let envelope = ManifestEnvelope {
            payload: URL_SAFE_NO_PAD.encode(&payload),
            signature: URL_SAFE_NO_PAD.encode(key.sign(&payload).to_bytes()),
        };
        let profile = crate::DeviceProfile {
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
        };
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
}
