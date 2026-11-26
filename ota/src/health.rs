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
    #[serde(
        default,
        alias = "motor_output_enabled",
        skip_serializing_if = "Option::is_none"
    )]
    pub motor_output_enabled: Option<bool>,
    #[serde(default, alias = "cms_state", skip_serializing_if = "Option::is_none")]
    pub cms_state: Option<String>,
    #[serde(
        default,
        alias = "online_mask",
        skip_serializing_if = "Option::is_none"
    )]
    pub online_mask: Option<u16>,
    #[serde(default, alias = "imu_fresh", skip_serializing_if = "Option::is_none")]
    pub imu_fresh: Option<bool>,
    #[serde(default, alias = "vbus_min_v", skip_serializing_if = "Option::is_none")]
    pub vbus_min_v: Option<f32>,
    #[serde(
        default,
        alias = "battery_percent",
        skip_serializing_if = "Option::is_none"
    )]
    pub battery_percent: Option<u8>,
}

pub trait HealthCheck: Send + Sync {
    fn check(&self) -> Result<HealthReport>;
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SafetyStatus {
    #[serde(default, alias = "motor_output_enabled")]
    pub motor_output_enabled: bool,
    #[serde(default, alias = "cms_state", skip_serializing_if = "Option::is_none")]
    pub cms_state: Option<String>,
    #[serde(default, alias = "vbus_min_v", skip_serializing_if = "Option::is_none")]
    pub vbus_min_v: Option<f32>,
    #[serde(
        default,
        alias = "battery_percent",
        skip_serializing_if = "Option::is_none"
    )]
    pub battery_percent: Option<u8>,
}

impl From<&HealthReport> for SafetyStatus {
    fn from(report: &HealthReport) -> Self {
        Self {
            motor_output_enabled: report.motor_output_enabled.unwrap_or(false),
            cms_state: report.cms_state.clone(),
            vbus_min_v: report.vbus_min_v,
            battery_percent: report.battery_percent,
        }
    }
}

pub trait SafetyPreCheck: Send + Sync {
    fn query_safety(&self) -> Result<SafetyStatus>;
}

#[derive(Clone)]
pub struct HttpSafetyPreCheck {
    pub health_url: String,
    pub verify_key: Option<VerifyingKey>,
}

impl HttpSafetyPreCheck {
    pub fn new(health_url: String) -> Self {
        Self {
            health_url,
            verify_key: None,
        }
    }

    pub fn from_env() -> Result<Self> {
        let health_url = std::env::var("DOSO_OTA_BRAINSTEM_HEALTH_URL")
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
        Ok(Self {
            health_url,
            verify_key,
        })
    }
}

impl SafetyPreCheck for HttpSafetyPreCheck {
    fn query_safety(&self) -> Result<SafetyStatus> {
        let report = fetch_health_report_for_safety(&self.health_url, self.verify_key.as_ref())?;
        Ok(SafetyStatus::from(&report))
    }
}

#[derive(Clone)]
pub struct SystemHealthCheck {
    pub boot_id_path: Option<PathBuf>,
    pub brainstem_url: String,
    pub strategy_marker: Option<PathBuf>,
    pub can_marker: Option<PathBuf>,
    pub verify_key: Option<VerifyingKey>,
    pub require_can_read_only: bool,
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
        let require_can_read_only = matches!(
            std::env::var("DOSO_OTA_REQUIRE_CAN_READ_ONLY")
                .ok()
                .as_deref()
                .map(str::trim),
            Some("1" | "true" | "TRUE" | "yes")
        );
        Ok(Self {
            boot_id_path,
            brainstem_url,
            strategy_marker,
            can_marker,
            verify_key,
            require_can_read_only,
        })
    }

    pub fn simple_http(brainstem_url: String) -> Self {
        Self {
            boot_id_path: None,
            brainstem_url,
            strategy_marker: None,
            can_marker: None,
            verify_key: None,
            require_can_read_only: false,
        }
    }

    pub fn with_require_can_read_only(mut self, require: bool) -> Self {
        self.require_can_read_only = require;
        self
    }
}

impl SafetyPreCheck for SystemHealthCheck {
    fn query_safety(&self) -> Result<SafetyStatus> {
        let report = fetch_health_report_for_safety(&self.brainstem_url, self.verify_key.as_ref())?;
        Ok(SafetyStatus::from(&report))
    }
}

impl HealthCheck for SystemHealthCheck {
    fn check(&self) -> Result<HealthReport> {
        let mut report = fetch_health_report(&self.brainstem_url, self.verify_key.as_ref())?;

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
        } else if self.require_can_read_only {
            ensure!(report.can_read_only, "can_read_only_failed");
        } else {
            ensure!(
                !report.motor_output_enabled.unwrap_or(false),
                "motor_output_enabled_during_health_check"
            );
        }
        Ok(report)
    }
}

/// Lightweight HTTP health checker for non-brainstem components (e.g. `cloud-link`, `voice`, `nav`).
#[derive(Clone)]
pub struct ComponentHttpHealthCheck {
    pub url: String,
}

impl ComponentHttpHealthCheck {
    pub fn new(url: String) -> Self {
        Self { url }
    }
}

impl HealthCheck for ComponentHttpHealthCheck {
    fn check(&self) -> Result<HealthReport> {
        let body = http_get_blocking(&self.url)?;
        let mut version = String::new();
        let mut release_id = String::new();
        let mut boot_id = String::new();
        if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&body) {
            if json.get("ok").and_then(|v| v.as_bool()) == Some(false) {
                anyhow::bail!("component_health_not_ok");
            }
            if json.get("ready").and_then(|v| v.as_bool()) == Some(false) {
                anyhow::bail!("component_not_ready");
            }
            if let Some(v) = json
                .get("version")
                .or_else(|| json.get("agentVersion"))
                .and_then(|v| v.as_str())
            {
                version = v.to_owned();
            }
            if let Some(r) = json
                .get("releaseId")
                .or_else(|| json.get("release_id"))
                .and_then(|v| v.as_str())
            {
                release_id = r.to_owned();
            }
            if let Some(b) = json
                .get("bootId")
                .or_else(|| json.get("boot_id"))
                .and_then(|v| v.as_str())
            {
                boot_id = b.to_owned();
            }
        }
        Ok(HealthReport {
            boot_id,
            version,
            release_id,
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

fn http_get_blocking(url: &str) -> Result<Vec<u8>> {
    http_get_for_health(url, false)
}

fn http_get_for_health(url: &str, allow_service_unavailable: bool) -> Result<Vec<u8>> {
    let url = url.to_owned();
    // Run blocking HTTP client on a dedicated OS thread so callers inside Tokio never panic.
    std::thread::spawn(move || -> Result<Vec<u8>> {
        let mut builder = reqwest::blocking::Client::builder().timeout(Duration::from_secs(3));
        if is_loopback_url(&url) {
            builder = builder.no_proxy();
        }
        let response = builder.build()?.get(&url).send()?;
        let status = response.status();
        ensure!(
            status.is_success()
                || (allow_service_unavailable
                    && status == reqwest::StatusCode::SERVICE_UNAVAILABLE),
            "brainstem_health_failed"
        );
        Ok(response.bytes()?.to_vec())
    })
    .join()
    .map_err(|_| anyhow::anyhow!("health_check_thread_panicked"))?
}

fn fetch_health_report(url: &str, verify_key: Option<&VerifyingKey>) -> Result<HealthReport> {
    let body = http_get_for_health(url, false)?;
    decode_health_payload(&body, verify_key)
}

fn fetch_health_report_for_safety(
    url: &str,
    verify_key: Option<&VerifyingKey>,
) -> Result<HealthReport> {
    let body = http_get_for_health(url, true)?;
    decode_health_payload(&body, verify_key)
}

fn decode_health_payload(body: &[u8], verify_key: Option<&VerifyingKey>) -> Result<HealthReport> {
    if let Some(key) = verify_key {
        decode_signed_health(body, key)
    } else {
        serde_json::from_slice::<HealthReport>(body).context("health_report_json")
    }
}

/// Verifies either the legacy `{ payload, signature }` `HealthEnvelope` OR `brainstem`'s flat
/// Ed25519-signed `/health` JSON response (`HealthCorePayload` + `algorithm: "ed25519"` +
/// `signature`).
pub fn decode_signed_health(body: &[u8], key: &VerifyingKey) -> Result<HealthReport> {
    if let Ok(envelope) = serde_json::from_slice::<HealthEnvelope>(body) {
        return decode_health(envelope, key);
    }
    let flat: FlatSignedHealthResponse =
        serde_json::from_slice(body).context("health_envelope_json")?;
    if let Some(ref alg) = flat.algorithm {
        ensure!(alg == "ed25519", "health_signature_algorithm_unsupported");
    }
    let sig_bytes = URL_SAFE_NO_PAD
        .decode(&flat.signature)
        .context("health_signature_base64")?;
    let signature = Signature::from_slice(&sig_bytes).context("health_signature_length")?;
    let canonical = serde_json::to_vec(&flat.core).context("health_core_canonical")?;
    key.verify_strict(&canonical, &signature)
        .context("health_signature_invalid")?;
    Ok(HealthReport {
        boot_id: flat.core.boot_id,
        version: flat.core.version,
        release_id: flat.core.release_id,
        brainstem_ready: flat.core.brainstem_ready,
        strategy_ready: flat.core.strategy_ready,
        can_read_only: flat.core.can_read_only,
        ok: flat.core.ok,
        motor_output_enabled: Some(flat.core.motor_output_enabled),
        cms_state: Some(flat.core.cms_state),
        online_mask: Some(flat.core.online_mask),
        imu_fresh: Some(flat.core.imu_fresh),
        vbus_min_v: Some(flat.core.vbus_min_v),
        battery_percent: None,
    })
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

/// Exact mirror of `brainstem/src/hot_deploy.rs::HealthCorePayload` field order & types for
/// canonical Ed25519 signature verification of flat `/health` responses.
#[derive(Debug, Serialize, Deserialize)]
struct BrainstemHealthCore {
    ok: bool,
    boot_id: String,
    version: String,
    release_id: String,
    brainstem_ready: bool,
    strategy_ready: bool,
    can_read_only: bool,
    active_profile: String,
    uptime_s: u64,
    motor_output_enabled: bool,
    cms_state: String,
    online_mask: u16,
    imu_fresh: bool,
    vbus_min_v: f32,
}

#[derive(Debug, Deserialize)]
struct FlatSignedHealthResponse {
    #[serde(flatten)]
    core: BrainstemHealthCore,
    #[serde(default)]
    algorithm: Option<String>,
    signature: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, routing::get};
    use ed25519_dalek::{Signer, SigningKey};
    use std::{net::Ipv4Addr, sync::Arc};

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
            motor_output_enabled: Some(false),
            cms_state: Some("grounded".into()),
            online_mask: Some(0xffff),
            imu_fresh: Some(true),
            vbus_min_v: Some(48.2),
            battery_percent: Some(92),
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
    fn parses_snake_case_5054_health_json_and_real_robot_fields() {
        let raw = r#"{
            "ok": true,
            "boot_id": "b-123",
            "version": "v1.0.1-rust",
            "release_id": "rel-1",
            "brainstem_ready": true,
            "strategy_ready": true,
            "can_read_only": false,
            "motor_output_enabled": false,
            "cms_state": "grounded",
            "online_mask": 65535,
            "imu_fresh": true,
            "vbus_min_v": 48.5,
            "battery_percent": 87,
            "active_profile": "flat_w5",
            "uptime_s": 42
        }"#;
        let report: HealthReport = serde_json::from_str(raw).unwrap();
        assert!(report.ok);
        assert!(report.brainstem_ready);
        assert!(report.strategy_ready);
        assert!(!report.can_read_only);
        assert_eq!(report.motor_output_enabled, Some(false));
        assert_eq!(report.cms_state.as_deref(), Some("grounded"));
        assert_eq!(report.online_mask, Some(0xffff));
        assert_eq!(report.imu_fresh, Some(true));
        assert_eq!(report.vbus_min_v, Some(48.5));
        assert_eq!(report.battery_percent, Some(87));
        assert_eq!(report.boot_id, "b-123");
        assert_eq!(report.version, "v1.0.1-rust");
        assert_eq!(report.release_id, "rel-1");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn real_robot_health_passes_when_can_read_only_is_false_and_motors_disabled() {
        let app = Router::new()
            .route(
                "/health-real",
                get(|| async {
                    Json(serde_json::json!({
                        "ok": true,
                        "boot_id": "boot-real",
                        "version": "v0.0.2",
                        "release_id": "rel-0.0.2",
                        "brainstem_ready": true,
                        "strategy_ready": true,
                        "can_read_only": false,
                        "motor_output_enabled": false,
                        "cms_state": "grounded",
                        "vbus_min_v": 48.1,
                        "battery_percent": 90
                    }))
                }),
            )
            .route(
                "/health-motors-on",
                get(|| async {
                    Json(serde_json::json!({
                        "ok": true,
                        "boot_id": "boot-real",
                        "version": "v0.0.2",
                        "release_id": "rel-0.0.2",
                        "brainstem_ready": true,
                        "strategy_ready": true,
                        "can_read_only": false,
                        "motor_output_enabled": true,
                        "cms_state": "standing"
                    }))
                }),
            );
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        // Default require_can_read_only = false -> passes real robot with can_read_only=false!
        let checker = SystemHealthCheck::simple_http(format!("http://{addr}/health-real"));
        assert!(!checker.require_can_read_only);
        let report = checker.check().unwrap();
        assert!(!report.can_read_only);
        assert_eq!(report.motor_output_enabled, Some(false));

        // Opt-in require_can_read_only = true -> rejects can_read_only=false (test bench mode).
        let strict_checker = checker.with_require_can_read_only(true);
        assert!(strict_checker.check().is_err());

        // Real robot with motor_output_enabled=true fails health check even when require_can_read_only=false.
        let motors_on = SystemHealthCheck::simple_http(format!("http://{addr}/health-motors-on"));
        assert!(motors_on.check().is_err());
        // Safety query still succeeds so caller can inspect motor_output_enabled=true.
        let safety = motors_on.query_safety().unwrap();
        assert!(safety.motor_output_enabled);
        assert_eq!(safety.cms_state.as_deref(), Some("standing"));

        srv.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn verifies_flat_ed25519_signed_brainstem_health_payload_and_rejects_tampering() {
        let key = SigningKey::from_bytes(&[42; 32]);
        let core = BrainstemHealthCore {
            ok: true,
            boot_id: "boot-flat-01".into(),
            version: "v0.0.1".into(),
            release_id: "rel-0.0.1".into(),
            brainstem_ready: true,
            strategy_ready: true,
            can_read_only: false,
            active_profile: "flat_w5".into(),
            uptime_s: 120,
            motor_output_enabled: false,
            cms_state: "grounded".into(),
            online_mask: 0xffff,
            imu_fresh: true,
            vbus_min_v: 48.25,
        };
        let canonical = serde_json::to_vec(&core).unwrap();
        let sig_b64 = URL_SAFE_NO_PAD.encode(key.sign(&canonical).to_bytes());
        let mut flat_json = serde_json::to_value(&core).unwrap();
        flat_json["algorithm"] = serde_json::json!("ed25519");
        flat_json["signature"] = serde_json::json!(sig_b64);
        let signed_bytes = serde_json::to_vec(&flat_json).unwrap();

        // Direct decode verifies exact brainstem flat signed JSON format.
        let decoded = decode_signed_health(&signed_bytes, &key.verifying_key()).unwrap();
        assert!(decoded.ok);
        assert_eq!(decoded.boot_id, "boot-flat-01");
        assert!(!decoded.can_read_only);
        assert_eq!(decoded.motor_output_enabled, Some(false));
        assert_eq!(decoded.cms_state.as_deref(), Some("grounded"));
        assert_eq!(decoded.online_mask, Some(0xffff));
        assert_eq!(decoded.imu_fresh, Some(true));
        assert_eq!(decoded.vbus_min_v, Some(48.25));

        // Tampering any signed field (e.g. motor_output_enabled or vbus_min_v) MUST fail!
        let mut tampered = flat_json.clone();
        tampered["motor_output_enabled"] = serde_json::json!(true);
        let tampered_bytes = serde_json::to_vec(&tampered).unwrap();
        assert!(decode_signed_health(&tampered_bytes, &key.verifying_key()).is_err());

        // End-to-end HTTP check with SystemHealthCheck + verify_key passes flat signed /health!
        let served = Arc::new(signed_bytes);
        let served_srv = served.clone();
        let app = Router::new().route(
            "/health",
            get(move || {
                let bytes = served_srv.clone();
                async move { bytes.as_ref().clone() }
            }),
        );
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut checker = SystemHealthCheck::simple_http(format!("http://{addr}/health"));
        checker.verify_key = Some(key.verifying_key());
        let live_report = checker.check().unwrap();
        assert_eq!(live_report.boot_id, "boot-flat-01");
        assert_eq!(live_report.cms_state.as_deref(), Some("grounded"));

        // HttpSafetyPreCheck with verify_key also verifies flat signed /health!
        let safety_checker = HttpSafetyPreCheck {
            health_url: format!("http://{addr}/health"),
            verify_key: Some(key.verifying_key()),
        };
        let safety = safety_checker.query_safety().unwrap();
        assert!(!safety.motor_output_enabled);
        assert_eq!(safety.vbus_min_v, Some(48.25));

        srv.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn safety_pre_check_reads_http_503_grounded_and_blocks_503_motors_on() {
        let app = Router::new()
            .route(
                "/health-503-grounded",
                get(|| async {
                    (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({
                            "ok": false,
                            "brainstem_ready": false,
                            "strategy_ready": false,
                            "motor_output_enabled": false,
                            "cms_state": "grounded"
                        })),
                    )
                }),
            )
            .route(
                "/health-503-motors-on",
                get(|| async {
                    (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({
                            "ok": false,
                            "brainstem_ready": false,
                            "strategy_ready": false,
                            "motor_output_enabled": true,
                            "cms_state": "standing"
                        })),
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let safe_503 = HttpSafetyPreCheck::new(format!("http://{addr}/health-503-grounded"));
        let s1 = safe_503.query_safety().unwrap();
        assert!(!s1.motor_output_enabled);
        assert_eq!(s1.cms_state.as_deref(), Some("grounded"));

        // SystemHealthCheck::check() (post-install gate) MUST still reject HTTP 503!
        assert!(
            SystemHealthCheck::simple_http(format!("http://{addr}/health-503-grounded"))
                .check()
                .is_err()
        );

        let unsafe_503 = HttpSafetyPreCheck::new(format!("http://{addr}/health-503-motors-on"));
        let s2 = unsafe_503.query_safety().unwrap();
        assert!(s2.motor_output_enabled);
        assert_eq!(s2.cms_state.as_deref(), Some("standing"));

        srv.abort();
    }
}
