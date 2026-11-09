use anyhow::{Context, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, SecondsFormat, Utc};
use ed25519_dalek::{
    Signature, Signer, SigningKey, VerifyingKey,
    pkcs8::{DecodePrivateKey, DecodePublicKey},
};
use rumqttc::{
    Transport,
    v5::{AsyncClient, EventLoop, MqttOptions},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{env, fs, time::Duration};

pub mod reporting;
pub mod status;
pub mod video;
pub use status::{Control, State, Status};

pub type Result<T> = anyhow::Result<T>;
pub const MAX_MESSAGE_BYTES: usize = 8192;

pub fn is_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| {
        id.to_string() == value
            && (1..=8).contains(&id.get_version_num())
            && id.get_variant() == uuid::Variant::RFC4122
    })
}

pub struct Topics {
    pub requests: String,
    pub replies: String,
    pub hello: String,
    pub control: String,
    pub acks: String,
    pub reports: String,
    pub video: String,
}

pub fn topics(device_id: &str) -> Result<Topics> {
    ensure!(is_uuid(device_id), "invalid_device");
    let base = format!("doso/v1/devices/{device_id}/status");
    Ok(Topics {
        requests: format!("{base}/requests"),
        replies: format!("{base}/replies"),
        hello: format!("doso/v1/devices/{device_id}/reporting/hello"),
        control: format!("doso/v1/devices/{device_id}/reporting/control"),
        acks: format!("doso/v1/devices/{device_id}/reporting/acks"),
        reports: format!("{base}/reports"),
        video: format!("doso/v1/devices/{device_id}/video/control"),
    })
}

pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

pub fn timestamp(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .expect("timestamp within the system clock range")
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn parse_time(value: &str) -> Result<i64> {
    let parsed = DateTime::parse_from_rfc3339(value).context("invalid_time")?;
    let ms = parsed.timestamp_millis();
    ensure!(timestamp(ms) == value, "invalid_time");
    Ok(ms)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadRequest {
    pub v: u8,
    pub kind: String,
    pub device_id: String,
    pub request_id: String,
    pub issued_at: String,
    pub expires_at: String,
}

impl ReadRequest {
    pub fn validate(&self, device_id: &str, now: i64) -> Result<()> {
        ensure!(
            self.v == 1
                && self.kind == "status.read"
                && is_uuid(&self.device_id)
                && self.device_id == device_id
                && is_uuid(&self.request_id),
            "invalid_request"
        );
        let issued = parse_time(&self.issued_at)?;
        let expires = parse_time(&self.expires_at)?;
        ensure!(
            issued <= now + 2000 && expires > now && expires > issued && expires - issued <= 10000,
            "expired_request"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadResult {
    pub v: u8,
    pub kind: String,
    pub device_id: String,
    pub request_id: String,
    pub observed_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ReadResult {
    pub fn validate(&self, device_id: &str, request_id: &str, now: i64) -> Result<()> {
        ensure!(
            self.v == 1
                && self.kind == "status.result"
                && is_uuid(&self.device_id)
                && is_uuid(&self.request_id)
                && self.device_id == device_id
                && self.request_id == request_id,
            "invalid_result"
        );
        let observed = parse_time(&self.observed_at)?;
        ensure!(
            now - observed <= 15000 && observed - now <= 2000,
            "stale_result"
        );
        validate_outcome(&self.status, &self.error)
    }
}

fn validate_outcome(status: &Option<Status>, error: &Option<String>) -> Result<()> {
    match (status, error) {
        (Some(status), None) => status.validate(),
        (None, Some(error)) if error == "upstream_unavailable" => Ok(()),
        _ => anyhow::bail!("invalid_result"),
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    payload: String,
    signature: String,
}

pub fn encode_signed<T: Serialize>(value: &T, key: &SigningKey) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value)?;
    let signature = key.sign(&bytes);
    let envelope = Envelope {
        payload: URL_SAFE_NO_PAD.encode(bytes),
        signature: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
    };
    let wire = serde_json::to_vec(&envelope)?;
    ensure!(wire.len() <= MAX_MESSAGE_BYTES, "message_too_large");
    Ok(wire)
}

pub fn decode_signed<T: DeserializeOwned>(bytes: &[u8], key: &VerifyingKey) -> Result<T> {
    ensure!(bytes.len() <= MAX_MESSAGE_BYTES, "message_too_large");
    let envelope: Envelope = serde_json::from_slice(bytes)?;
    let payload = URL_SAFE_NO_PAD.decode(envelope.payload)?;
    let signature = Signature::from_slice(&URL_SAFE_NO_PAD.decode(envelope.signature)?)?;
    key.verify_strict(&payload, &signature)
        .context("invalid_signature")?;
    Ok(serde_json::from_slice(&payload)?)
}

pub fn load_signing_key(path: &str) -> Result<SigningKey> {
    Ok(SigningKey::from_pkcs8_pem(&fs::read_to_string(path)?)?)
}

pub fn load_verifying_key(path: &str) -> Result<VerifyingKey> {
    Ok(VerifyingKey::from_public_key_pem(&fs::read_to_string(
        path,
    )?)?)
}

pub fn connect_mqtt(client_id: &str) -> Result<(AsyncClient, EventLoop)> {
    let url = url::Url::parse(&env::var("DOSO_MQTT_URL")?)?;
    ensure!(
        url.scheme() == "mqtts"
            && url.username().is_empty()
            && url.password().is_none()
            && matches!(url.path(), "" | "/")
            && url.query().is_none()
            && url.fragment().is_none(),
        "invalid_mqtt_url"
    );
    let host = url.host_str().context("missing_mqtt_host")?;
    let ca = fs::read(env::var("DOSO_MQTT_CA")?)?;
    let cert = fs::read(env::var("DOSO_MQTT_CERT")?)?;
    let key = fs::read(env::var("DOSO_MQTT_KEY")?)?;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut options = MqttOptions::new(client_id, host, url.port().unwrap_or(8883));
    options.set_transport(Transport::tls(ca, Some((cert, key)), None));
    options.set_clean_start(true);
    options.set_keep_alive(Duration::from_secs(15));
    options.set_max_packet_size(Some(MAX_MESSAGE_BYTES as u32));
    Ok(AsyncClient::new(options, 32))
}

pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut terminate) = signal(SignalKind::terminate()) {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_queries_are_device_bound_and_expire() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let now = 1_793_600_000_000;
        let request = ReadRequest {
            v: 1,
            kind: "status.read".into(),
            device_id: uuid::Uuid::new_v4().to_string(),
            request_id: uuid::Uuid::new_v4().to_string(),
            issued_at: timestamp(now),
            expires_at: timestamp(now + 8000),
        };
        let wire = encode_signed(&request, &key).unwrap();
        let decoded: ReadRequest = decode_signed(&wire, &key.verifying_key()).unwrap();
        decoded.validate(&request.device_id, now).unwrap();
        assert!(
            decoded
                .validate(&uuid::Uuid::new_v4().to_string(), now)
                .is_err()
        );
        assert!(decoded.validate(&request.device_id, now + 8000).is_err());
        assert!(
            decode_signed::<ReadRequest>(&wire, &SigningKey::from_bytes(&[8; 32]).verifying_key())
                .is_err()
        );
        let mut changed: serde_json::Value = serde_json::from_slice(&wire).unwrap();
        changed["payload"] = serde_json::json!(URL_SAFE_NO_PAD.encode(b"{}"));
        assert!(
            decode_signed::<ReadRequest>(
                &serde_json::to_vec(&changed).unwrap(),
                &key.verifying_key()
            )
            .is_err()
        );
    }

    #[test]
    fn unknown_fields_and_invented_status_are_rejected() {
        let status = serde_json::json!({"state":{"kind":"CMS_STATE_KIND_GROUNDED","transition":"CMS_TRANSITION_KIND_NONE"},
            "control":{"owner":"CONTROL_OWNER_NONE","motorOutputEnabled":false,"criticalMotorFault":false},"transport":"local-insecure"});
        let value: Status = serde_json::from_value(status.clone()).unwrap();
        value.validate().unwrap();
        let mut extra = status;
        extra["battery"] = serde_json::json!(82);
        assert!(serde_json::from_value::<Status>(extra).is_err());
    }

    #[test]
    fn signed_fixture_is_compatible() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/signed-request.json")).unwrap();
        let key =
            VerifyingKey::from_public_key_pem(fixture["publicKey"].as_str().unwrap()).unwrap();
        let request: ReadRequest =
            decode_signed(&serde_json::to_vec(&fixture["envelope"]).unwrap(), &key).unwrap();
        request
            .validate(
                "00000000-0000-4000-8000-000000000001",
                fixture["now"].as_i64().unwrap(),
            )
            .unwrap();
    }
}
