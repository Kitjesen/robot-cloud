mod http;
mod reporting;
mod sync;
mod video;
pub use http::app;
pub use reporting::Latest;
pub use sync::{SyncConfig, run_sync};

use ed25519_dalek::{SigningKey, VerifyingKey};
use robot_cloud_common::{
    ReadRequest, ReadResult, Status, decode_signed, encode_signed, is_uuid, load_verifying_key,
    now_ms, reporting::Permission, timestamp, topics,
};
use rumqttc::v5::{
    AsyncClient, Event, EventLoop,
    mqttbytes::{
        QoS,
        v5::{Filter, Packet, SubscribeReasonCode},
    },
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Semaphore, oneshot};
use uuid::Uuid;

const QUERY_TIMEOUT: Duration = Duration::from_secs(8);
const REPLY_FILTER: &str = "doso/v1/devices/+/status/replies";
const FILTERS: [&str; 4] = [
    REPLY_FILTER,
    "doso/v1/devices/+/reporting/hello",
    "doso/v1/devices/+/reporting/acks",
    "doso/v1/devices/+/status/reports",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Disabled,
    Unavailable,
    Timeout,
    UpstreamUnavailable,
    Busy,
    InvalidDevice,
    InvalidPermission,
    StalePermission,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub device_id: String,
    pub request_id: String,
    pub observed_at: String,
    #[serde(flatten)]
    pub status: Status,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    devices: Vec<RegisteredDevice>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RegisteredDevice {
    device_id: String,
    #[serde(default)]
    enabled: bool,
    public_key_path: String,
}

struct Pending {
    device_id: String,
    permission: Permission,
    reply_topic: String,
    public_key: VerifyingKey,
    expires_at: i64,
    result: oneshot::Sender<Result<Snapshot, Error>>,
}

pub struct Gateway {
    client: AsyncClient,
    signing_key: SigningKey,
    registry_path: PathBuf,
    reporting: Mutex<reporting::Reporting>,
    // None means MQTT is offline; dropping a session cancels all its queries.
    session: Mutex<Option<HashMap<String, Pending>>>,
    limit: Semaphore,
    video_seq: Mutex<HashMap<String, u64>>,
}

struct PendingGuard<'a> {
    gateway: &'a Gateway,
    request_id: &'a str,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        if let Some(pending) = self.gateway.session.lock().unwrap().as_mut() {
            pending.remove(self.request_id);
        }
    }
}

impl Gateway {
    pub fn new(
        client: AsyncClient,
        signing_key: SigningKey,
        registry_path: PathBuf,
    ) -> anyhow::Result<Self> {
        let state_path = registry_path.with_file_name("reporting.json");
        Self::with_reporting_state(client, signing_key, registry_path, state_path)
    }

    pub fn with_reporting_state(
        client: AsyncClient,
        signing_key: SigningKey,
        registry_path: PathBuf,
        state_path: PathBuf,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(registry_path != state_path, "invalid_reporting_state");
        Ok(Self {
            client,
            signing_key,
            registry_path,
            reporting: Mutex::new(reporting::Reporting::load(state_path)?),
            session: Mutex::new(None),
            limit: Semaphore::new(16),
            video_seq: Mutex::new(HashMap::new()),
        })
    }

    fn registry(&self) -> Result<Registry, Error> {
        let bytes = fs::read(&self.registry_path).map_err(|_| Error::Unavailable)?;
        let registry: Registry = serde_json::from_slice(&bytes).map_err(|_| Error::Unavailable)?;
        let mut ids = HashSet::new();
        if registry
            .devices
            .iter()
            .any(|device| !is_uuid(&device.device_id) || !ids.insert(&device.device_id))
        {
            return Err(Error::Unavailable);
        }
        Ok(registry)
    }

    fn device_key(&self, device_id: &str) -> Result<VerifyingKey, Error> {
        let registry = self.registry()?;
        let device = registry
            .devices
            .iter()
            .find(|device| device.device_id == device_id)
            .ok_or(Error::Disabled)?;
        if !device.enabled {
            return Err(Error::Disabled);
        }
        let key_path = Path::new(&device.public_key_path);
        if key_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
            || key_path.as_os_str().is_empty()
        {
            return Err(Error::Unavailable);
        }
        let path = self
            .registry_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(key_path);
        load_verifying_key(path.to_str().ok_or(Error::Unavailable)?).map_err(|_| Error::Unavailable)
    }

    pub fn disconnected(&self) {
        let mut reporting = self.reporting.lock().unwrap();
        reporting.disconnect();
        self.session.lock().unwrap().take();
    }

    pub async fn read_status(&self, device_id: &str) -> Result<Snapshot, Error> {
        let _permit = self.limit.try_acquire().map_err(|_| Error::Busy)?;
        if !is_uuid(device_id) {
            return Err(Error::InvalidDevice);
        }
        if self.session.lock().unwrap().is_none() {
            return Err(Error::Unavailable);
        }
        let key = self.device_key(device_id)?;
        let request_id = Uuid::new_v4().to_string();
        let issued_at = now_ms();
        let expires_at = issued_at + QUERY_TIMEOUT.as_millis() as i64;
        let topic = topics(device_id).map_err(|_| Error::InvalidDevice)?;
        let request = ReadRequest {
            v: 1,
            kind: "status.read".into(),
            device_id: device_id.into(),
            request_id: request_id.clone(),
            issued_at: timestamp(issued_at),
            expires_at: timestamp(expires_at),
        };
        let payload = encode_signed(&request, &self.signing_key).map_err(|_| Error::Unavailable)?;
        let (sender, receiver) = oneshot::channel();
        {
            let reporting = self.reporting.lock().unwrap();
            let permission = reporting.check_read(device_id)?;
            let mut session = self.session.lock().unwrap();
            let pending = session.as_mut().ok_or(Error::Unavailable)?;
            pending.insert(
                request_id.clone(),
                Pending {
                    device_id: device_id.into(),
                    permission,
                    reply_topic: topic.replies,
                    public_key: key,
                    expires_at,
                    result: sender,
                },
            );
            // Queue while holding the pending lock so disconnect cleanup also drains this request.
            if self
                .client
                .try_publish(topic.requests, QoS::AtLeastOnce, false, payload)
                .is_err()
            {
                pending.remove(&request_id);
                return Err(Error::Unavailable);
            }
        }
        let _guard = PendingGuard {
            gateway: self,
            request_id: &request_id,
        };
        // Queueing transport work is not success; only a signed device reply completes this read.
        tokio::time::timeout(QUERY_TIMEOUT, receiver)
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(|_| Error::Unavailable)?
    }

    fn accept_reply(&self, topic: &str, payload: &[u8], retained: bool) {
        if retained || payload.len() > robot_cloud_common::MAX_MESSAGE_BYTES {
            return;
        }
        let reporting = self.reporting.lock().unwrap();
        let mut session = self.session.lock().unwrap();
        let Some(pending) = session.as_mut() else {
            return;
        };
        for (request_id, expected) in pending
            .iter()
            .filter(|(_, query)| query.reply_topic == topic)
        {
            let Ok(reply) = decode_signed::<ReadResult>(payload, &expected.public_key) else {
                continue;
            };
            if reply
                .validate(&expected.device_id, request_id, now_ms())
                .is_err()
            {
                continue;
            }
            let result = match (
                self.device_key(&expected.device_id),
                reporting.check_read(&expected.device_id),
            ) {
                (Err(error), _) | (_, Err(error)) => Err(error),
                (Ok(key), Ok(permission))
                    if key == expected.public_key
                        && permission.same_session(&expected.permission) =>
                {
                    if now_ms() >= expected.expires_at {
                        Err(Error::Timeout)
                    } else if reply.error.is_some() {
                        Err(Error::UpstreamUnavailable)
                    } else {
                        reply
                            .status
                            .ok_or(Error::Unavailable)
                            .map(|status| Snapshot {
                                device_id: reply.device_id,
                                request_id: reply.request_id,
                                observed_at: reply.observed_at,
                                status,
                            })
                    }
                }
                _ => Err(Error::Disabled),
            };
            let request_id = request_id.clone();
            if let Some(pending) = pending.remove(&request_id) {
                let _ = pending.result.send(result);
            }
            return;
        }
    }
}

pub async fn run_mqtt(gateway: Arc<Gateway>, mut event_loop: EventLoop) {
    loop {
        match event_loop.poll().await {
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                gateway.disconnected();
                if gateway
                    .client
                    .try_subscribe_many(FILTERS.map(|topic| Filter::new(topic, QoS::AtLeastOnce)))
                    .is_err()
                {
                    event_loop.clean();
                    event_loop.pending.clear();
                }
            }
            Ok(Event::Incoming(Packet::SubAck(ack))) => {
                let accepted = ack.return_codes.len() == FILTERS.len()
                    && ack
                        .return_codes
                        .iter()
                        .all(|code| matches!(code, SubscribeReasonCode::Success(_)));
                if accepted {
                    gateway.session.lock().unwrap().get_or_insert_default();
                } else {
                    gateway.disconnected();
                }
            }
            Ok(Event::Incoming(Packet::Publish(publish))) => {
                if let Ok(topic) = std::str::from_utf8(&publish.topic) {
                    gateway.accept_reply(topic, &publish.payload, publish.retain);
                    gateway.accept_reporting(topic, &publish.payload, publish.retain);
                }
            }
            Ok(Event::Incoming(Packet::Disconnect(_))) | Err(_) => {
                gateway.disconnected();
                event_loop.clean();
                // Queries have short lifetimes; reconnect never deliberately resends them.
                event_loop.pending.clear();
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
