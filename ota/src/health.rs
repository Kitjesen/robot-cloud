use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, VerifyingKey, pkcs8::DecodePublicKey};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf, time::Duration};

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthReport {
    #[serde(default, alias = "boot_id")]
    pub boot_id: String,
    #[serde(default)]
    pub version: String,
    #[serde(default, alias = "release_id")]
    pub release_id: String,
    #[serde(default, alias = "brainstem_ready")]
    pub brainstem_ready: bool,
    #[serde(default = "default_true", alias = "strategy_ready")]
    pub strategy_ready: bool,
    #[serde(default = "default_true", alias = "can_read_only")]
    pub can_read_only: bool,
    #[serde(default = "default_true")]
    pub ok: bool,
}

pub trait HealthCheck: Send + Sync {
    fn check(&self) -> Result<HealthReport>;
}

#[derive(Clone)]
pub struct SystemHealthCheck {
    pub boot_id_path: Option<PathBuf>,
    pub brainstem_url: String,
    pub strategy_marker: Option<PathBuf>,
    pub can_marker: Option<PathBuf>,
    pub verify_key: Option<VerifyingKey>,
}

impl SystemHealthCheck {
    pub fn from_env() -> Result<Self> {
        let brainstem_url = std::env::var("DOSO_OTA_BRAINSTEM_HEALTH_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "http://127.0.0.1:5054/health".into());
        let verify_key = match std::env::var_os("DOSO_OTA_HEALTH_VERIFY_KEY")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
        {
            Some(key_path) => Some(VerifyingKey::from_public_key_pem(&fs::read_to_string(
                key_path,
            )?)?),
            None => None,
        };
        let boot_id_path = std::env::var_os("DOSO_OTA_BOOT_ID")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        let strategy_marker = std::env::var_os("DOSO_OTA_STRATEGY_MARKER")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        let can_marker = std::env::var_os("DOSO_OTA_CAN_MARKER")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        Ok(Self {
            boot_id_path,
            brainstem_url,
            strategy_marker,
            can_marker,
            verify_key,
        })
    }

    pub fn simple_http(brainstem_url: String) -> Self {
        Self {
            boot_id_path: None,
            brainstem_url,
            strategy_marker: None,
            can_marker: None,
            verify_key: None,
        }
    }
}

impl HealthCheck for SystemHealthCheck {
    fn check(&self) -> Result<HealthReport> {
        let url = self.brainstem_url.clone();
        // Run blocking HTTP client on a dedicated OS thread so callers inside Tokio never panic.
        let body = std::thread::spawn(move || -> Result<Vec<u8>> {
            let mut builder = reqwest::blocking::Client::builder().timeout(Duration::from_secs(3));
            if is_loopback_url(&url) {
                builder = builder.no_proxy();
            }
            let response = builder.build()?.get(&url).send()?;
            ensure!(response.status().is_success(), "brainstem_health_failed");
            Ok(response.bytes()?.to_vec())
        })
        .join()
        .map_err(|_| anyhow::anyhow!("health_check_thread_panicked"))??;

        let mut report = if let Some(key) = &self.verify_key {
            let envelope: HealthEnvelope =
                serde_json::from_slice(&body).context("health_envelope_json")?;
            decode_health(envelope, key)?
        } else {
            serde_json::from_slice::<HealthReport>(&body).context("health_report_json")?
        };

        ensure!(report.ok, "brainstem_health_not_ok");
        ensure!(report.brainstem_ready, "brainstem_not_ready");

        if let Some(boot_id_path) = &self.boot_id_path
            && boot_id_path.exists()
        {
            let boot_id = fs::read_to_string(boot_id_path)?.trim().to_owned();
            ensure!(!boot_id.is_empty(), "boot_id_missing");
            if !report.boot_id.is_empty() {
                ensure!(report.boot_id == boot_id, "health_boot_id_mismatch");
            } else {
                report.boot_id = boot_id;
            }
        }
        if let Some(strategy_marker) = &self.strategy_marker {
            let strategy_ready = fs::read_to_string(strategy_marker)
                .map(|value| value.trim() == "ready")
                .unwrap_or(false);
            ensure!(strategy_ready, "strategy_not_ready");
            report.strategy_ready = strategy_ready;
        } else {
            ensure!(report.strategy_ready, "strategy_not_ready");
        }
        if let Some(can_marker) = &self.can_marker {
            let can_read_only = fs::read_to_string(can_marker)
                .map(|value| value.trim() == "ok")
                .unwrap_or(false);
            ensure!(can_read_only, "can_read_only_failed");
            report.can_read_only = can_read_only;
        } else {
            ensure!(report.can_read_only, "can_read_only_failed");
        }
        Ok(report)
    }
}

fn is_loopback_url(raw: &str) -> bool {
    url::Url::parse(raw)
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| matches!(h, "127.0.0.1" | "localhost" | "::1"))
        })
        .unwrap_or(false)
}

fn decode_health(envelope: HealthEnvelope, key: &VerifyingKey) -> Result<HealthReport> {
    let payload = URL_SAFE_NO_PAD.decode(envelope.payload)?;
    let signature = Signature::from_slice(&URL_SAFE_NO_PAD.decode(envelope.signature)?)?;
    key.verify_strict(&payload, &signature)
        .context("health_signature_invalid")?;
    Ok(serde_json::from_slice(&payload)?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HealthEnvelope {
    payload: String,
    signature: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn rejects_forged_health_proof() {
        let key = SigningKey::from_bytes(&[3; 32]);
        let report = HealthReport {
            boot_id: "boot-2".into(),
            version: "1.2.0".into(),
            release_id: "release-2".into(),
            brainstem_ready: true,
            strategy_ready: true,
            can_read_only: true,
            ok: true,
        };
        let payload = serde_json::to_vec(&report).unwrap();
        let valid = HealthEnvelope {
            payload: URL_SAFE_NO_PAD.encode(&payload),
            signature: URL_SAFE_NO_PAD.encode(key.sign(&payload).to_bytes()),
        };
        assert_eq!(
            decode_health(valid, &key.verifying_key()).unwrap().boot_id,
            "boot-2"
        );
        let forged = HealthEnvelope {
            payload: URL_SAFE_NO_PAD.encode(b"{}"),
            signature: URL_SAFE_NO_PAD.encode(key.sign(&payload).to_bytes()),
        };
        assert!(decode_health(forged, &key.verifying_key()).is_err());
    }

    #[test]
    fn parses_snake_case_5054_health_json() {
        let raw = r#"{
            "ok": true,
            "boot_id": "b-123",
            "version": "v1.0.1-rust",
            "release_id": "rel-1",
            "brainstem_ready": true,
            "strategy_ready": true,
            "can_read_only": true,
            "active_profile": "flat_w5",
            "uptime_s": 42
        }"#;
        let report: HealthReport = serde_json::from_str(raw).unwrap();
        assert!(report.ok);
        assert!(report.brainstem_ready);
        assert!(report.strategy_ready);
        assert!(report.can_read_only);
        assert_eq!(report.boot_id, "b-123");
        assert_eq!(report.version, "v1.0.1-rust");
        assert_eq!(report.release_id, "rel-1");
    }
}
