use crate::{Error, Gateway};
use robot_cloud_common::reporting::{
    Ack, Control, FRESH_MS, Hello, LEASE_MS, Permission, Report, SEEN_MS, valid_version,
};
use robot_cloud_common::{
    decode_signed, encode_signed, is_uuid, now_ms, parse_time, timestamp, topics,
};
use rumqttc::v5::mqttbytes::QoS;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Fence {
    authorization_epoch: u64,
    consent_version: u64,
    enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncertain_storage_blocks_enabled_fence() {
        let id = uuid::Uuid::new_v4().to_string();
        let reporting = Reporting {
            path: PathBuf::new(),
            storage_failed: true,
            fences: HashMap::from([(
                id.clone(),
                Fence {
                    authorization_epoch: 1,
                    consent_version: 2,
                    enabled: true,
                },
            )]),
            devices: HashMap::new(),
        };
        assert_eq!(reporting.check_read(&id), Err(Error::Unavailable));
        assert_eq!(reporting.available(), Err(Error::Unavailable));
    }
}

impl Fence {
    fn version(&self) -> (u64, u64) {
        (self.authorization_epoch, self.consent_version)
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Received {
    pub report: Report,
    pub received_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    authorization_epoch: u64,
    consent_version: u64,
    desired_enabled: bool,
    device_confirmed: bool,
    reporting_session_id: Option<String>,
    expires_at: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Latest {
    pub device_id: String,
    pub connection_id: Option<String>,
    pub last_seen_at: Option<String>,
    pub reachable: bool,
    pub permission: Option<Decision>,
    pub latest: Option<Received>,
    pub last_success: Option<Received>,
    pub fresh: bool,
}

#[derive(Default)]
struct Device {
    hello: Option<Hello>,
    hello_received: i64,
    last_seen: Option<String>,
    desired: Option<Permission>,
    confirmed: bool,
    live: bool,
    sessions: HashSet<String>,
    seq: u64,
    latest: Option<Received>,
    last_success: Option<Received>,
}

impl Device {
    fn active(&self, now: i64) -> bool {
        self.live
            && self.desired.as_ref().is_some_and(|permission| {
                permission.enabled
                    && parse_time(&permission.expires_at).is_ok_and(|expires| expires > now)
                    && self.hello.as_ref().is_some_and(|hello| {
                        permission.connection_id.as_ref() == Some(&hello.connection_id)
                    })
            })
    }

    fn seen(&mut self, observed: &str) {
        if self.last_seen.as_deref().is_none_or(|last| observed > last) {
            self.last_seen = Some(observed.into());
        }
    }

    fn renews(&self, permission: &Permission, now: i64) -> bool {
        self.active(now)
            && self
                .desired
                .as_ref()
                .is_some_and(|old| old.same_session(permission))
    }
}

pub(crate) struct Reporting {
    path: PathBuf,
    storage_failed: bool,
    fences: HashMap<String, Fence>,
    devices: HashMap<String, Device>,
}

impl Reporting {
    pub fn load(path: PathBuf) -> anyhow::Result<Self> {
        let fences: HashMap<String, Fence> = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(error) => return Err(error.into()),
        };
        for (id, fence) in &fences {
            anyhow::ensure!(
                is_uuid(id) && valid_version(fence.authorization_epoch, fence.consent_version),
                "invalid_reporting_state"
            );
        }
        Ok(Self {
            path,
            storage_failed: false,
            fences,
            devices: HashMap::new(),
        })
    }

    fn persist(&mut self, id: &str, permission: &Permission) -> Result<(), Error> {
        self.available()?;
        if self.fences.get(id).is_some_and(|fence| {
            fence.version() == permission.version() && fence.enabled == permission.enabled
        }) {
            return Ok(());
        }
        let mut fences = self.fences.clone();
        fences.insert(
            id.into(),
            Fence {
                authorization_epoch: permission.authorization_epoch,
                consent_version: permission.consent_version,
                enabled: permission.enabled,
            },
        );
        let temporary = self
            .path
            .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let mut renamed = false;
        let result = (|| -> anyhow::Result<()> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(&serde_json::to_vec(&fences)?)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, &self.path)?;
            renamed = true;
            #[cfg(unix)]
            fs::File::open(self.path.parent().unwrap_or(std::path::Path::new(".")))?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
            // A failed directory sync leaves a replaced file with uncertain durability.
            if renamed {
                self.storage_failed = true;
                self.fences = fences;
                self.disconnect();
                for device in self.devices.values_mut() {
                    device.desired = None;
                    device.latest = None;
                    device.last_success = None;
                }
            }
            return Err(Error::Unavailable);
        }
        self.fences = fences;
        Ok(())
    }

    pub fn available(&self) -> Result<(), Error> {
        if self.storage_failed {
            Err(Error::Unavailable)
        } else {
            Ok(())
        }
    }

    pub fn check_read(&self, id: &str) -> Result<Permission, Error> {
        self.available()?;
        self.devices
            .get(id)
            .filter(|device| device.confirmed && device.active(now_ms()))
            .and_then(|device| device.desired.clone())
            .ok_or(Error::Disabled)
    }

    pub fn disconnect(&mut self) {
        for device in self.devices.values_mut() {
            device.live = false;
            device.confirmed = false;
            // Keep the hello watermark to reject replay when MQTT reconnects.
            device.hello_received = 0;
        }
    }

    fn latest(&self, id: &str, online: bool, now: i64) -> Latest {
        let device = self.devices.get(id);
        let permission = self.fences.get(id).map(|fence| {
            let desired = device.and_then(|device| device.desired.as_ref());
            Decision {
                authorization_epoch: fence.authorization_epoch,
                consent_version: fence.consent_version,
                desired_enabled: fence.enabled,
                device_confirmed: device.is_some_and(|device| {
                    device.confirmed && (!fence.enabled || device.active(now))
                }),
                reporting_session_id: desired.and_then(|value| value.reporting_session_id.clone()),
                expires_at: desired.map(|value| value.expires_at.clone()),
            }
        });
        let fresh = online
            && device.is_some_and(|device| {
                device.active(now)
                    && device.confirmed
                    && device.latest.as_ref().is_some_and(|latest| {
                        latest.report.status.is_some()
                            && device
                                .desired
                                .as_ref()
                                .is_some_and(|permission| permission.matches_report(&latest.report))
                            && parse_time(&latest.report.observed_at)
                                .is_ok_and(|observed| now - observed <= FRESH_MS)
                            && parse_time(&latest.received_at)
                                .is_ok_and(|received| now - received <= FRESH_MS)
                    })
            });
        Latest {
            device_id: id.into(),
            connection_id: device.and_then(|device| {
                device
                    .hello
                    .as_ref()
                    .map(|hello| hello.connection_id.clone())
            }),
            last_seen_at: device.and_then(|device| device.last_seen.clone()),
            reachable: online
                && device
                    .and_then(|device| device.last_seen.as_ref())
                    .is_some_and(|seen| parse_time(seen).is_ok_and(|seen| now - seen <= SEEN_MS)),
            permission,
            latest: device.and_then(|device| device.latest.clone()),
            last_success: device.and_then(|device| device.last_success.clone()),
            fresh,
        }
    }
}

impl Gateway {
    pub(crate) fn sync_snapshot(&self, id: &str) -> Result<(Latest, bool), Error> {
        let reporting = self.reporting.lock().unwrap();
        reporting.available()?;
        let online = self.session.lock().unwrap().is_some();
        let now = now_ms();
        let reusable = online
            && reporting
                .devices
                .get(id)
                .is_some_and(|device| device.active(now));
        Ok((reporting.latest(id, online, now), reusable))
    }

    pub fn latest(&self, id: &str) -> Result<Latest, Error> {
        if !is_uuid(id) {
            return Err(Error::InvalidDevice);
        }
        self.device_key(id)?;
        let reporting = self.reporting.lock().unwrap();
        reporting.available()?;
        let online = self.session.lock().unwrap().is_some();
        Ok(reporting.latest(id, online, now_ms()))
    }

    pub fn set_reporting(&self, id: &str, permission: Permission) -> Result<Latest, Error> {
        if !is_uuid(id) {
            return Err(Error::InvalidDevice);
        }
        self.device_key(id)?;
        let mut reporting = self.reporting.lock().unwrap();
        reporting.available()?;
        let mut session = self.session.lock().unwrap();
        let now = now_ms();
        permission
            .validate(now)
            .map_err(|_| Error::InvalidPermission)?;
        if reporting.fences.get(id).is_some_and(|fence| {
            permission.version() < fence.version()
                || (permission.version() == fence.version() && permission.enabled != fence.enabled)
        }) {
            return Err(Error::StalePermission);
        }
        if permission.enabled {
            let device = reporting.devices.get(id).ok_or(Error::Unavailable)?;
            if session.is_none()
                || now - device.hello_received > SEEN_MS
                || !device.hello.as_ref().is_some_and(|hello| {
                    permission.connection_id.as_ref() == Some(&hello.connection_id)
                })
            {
                return Err(Error::Unavailable);
            }
            let session_id = permission.reporting_session_id.as_ref().unwrap();
            let same_session = device.renews(&permission, now);
            if device.sessions.contains(session_id) && !same_session {
                return Err(Error::StalePermission);
            }
            if device.active(now)
                && device
                    .desired
                    .as_ref()
                    .is_some_and(|old| old.version() == permission.version())
                && !same_session
            {
                return Err(Error::StalePermission);
            }
        }
        let mut bound = permission.clone();
        if let Some(device) = reporting.devices.get(id) {
            if let Some(hello) = device
                .hello
                .as_ref()
                .filter(|_| session.is_some() && now - device.hello_received <= SEEN_MS)
            {
                bound.connection_id = Some(hello.connection_id.clone());
            }
            if device.confirmed
                && device.desired.as_ref() == Some(&bound)
                && (!permission.enabled || device.active(now))
            {
                return Ok(reporting.latest(id, session.is_some(), now));
            }
        }
        let epoch_changed = reporting
            .fences
            .get(id)
            .is_some_and(|old| old.authorization_epoch != permission.authorization_epoch);
        reporting.persist(id, &permission)?;
        let device = reporting.devices.entry(id.into()).or_default();
        if epoch_changed {
            device.latest = None;
            device.last_success = None;
        }
        let same_session = device.renews(&permission, now);
        if !same_session {
            device.seq = 0;
        }
        let previous = device.desired.clone();
        let was_confirmed = device.confirmed;
        device.live = permission.enabled;
        device.confirmed = false;
        device.desired = Some(permission.clone());
        if (!permission.enabled || !same_session)
            && let Some(pending) = session.as_mut()
        {
            let cancelled: Vec<_> = pending
                .iter()
                .filter(|(_, query)| query.device_id == id)
                .map(|(key, _)| key.clone())
                .collect();
            for key in cancelled {
                if let Some(query) = pending.remove(&key) {
                    let _ = query.result.send(Err(Error::Disabled));
                }
            }
        }
        let control = device
            .hello
            .as_ref()
            .filter(|_| session.is_some() && now - device.hello_received <= SEEN_MS)
            .map(|hello| {
                let mut bound = permission.clone();
                bound.connection_id = Some(hello.connection_id.clone());
                bound
            });
        if let Some(bound) = control {
            device.desired = Some(bound.clone());
            if self.publish_control(id, bound).is_err() && permission.enabled {
                device.live = same_session;
                device.confirmed = same_session && was_confirmed;
                device.desired = if same_session { previous } else { None };
                return Err(Error::Unavailable);
            }
        }
        if permission.enabled
            && let Some(session_id) = &permission.reporting_session_id
        {
            device.sessions.insert(session_id.clone());
        }
        Ok(reporting.latest(id, session.is_some(), now_ms()))
    }

    fn publish_control(&self, id: &str, permission: Permission) -> Result<(), Error> {
        let control = Control {
            v: 1,
            kind: "reporting.control".into(),
            device_id: id.into(),
            permission,
        };
        let bytes = encode_signed(&control, &self.signing_key).map_err(|_| Error::Unavailable)?;
        self.client
            .try_publish(
                topics(id).map_err(|_| Error::InvalidDevice)?.control,
                QoS::AtLeastOnce,
                false,
                bytes,
            )
            .map_err(|_| Error::Unavailable)
    }

    pub(crate) fn accept_reporting(&self, topic: &str, payload: &[u8], retained: bool) {
        if retained || payload.len() > robot_cloud_common::MAX_MESSAGE_BYTES {
            return;
        }
        let Some(id) = topic
            .strip_prefix("doso/v1/devices/")
            .and_then(|rest| rest.split('/').next())
            .filter(|id| is_uuid(id))
        else {
            return;
        };
        let Ok(key) = self.device_key(id) else {
            return;
        };
        let Ok(topics) = topics(id) else {
            return;
        };
        let mut reporting = self.reporting.lock().unwrap();
        if reporting.available().is_err() {
            return;
        }
        if self.session.lock().unwrap().is_none() {
            return;
        }
        let now = now_ms();
        if topic == topics.hello {
            let Ok(hello) = decode_signed::<Hello>(payload, &key) else {
                return;
            };
            if hello.validate(id, now).is_err() {
                return;
            }
            let disabled = reporting
                .fences
                .get(id)
                .filter(|fence| !fence.enabled)
                .cloned();
            let device = reporting.devices.entry(id.into()).or_default();
            if device
                .hello
                .as_ref()
                .is_some_and(|old| hello.observed_at <= old.observed_at)
            {
                return;
            }
            let changed = device
                .hello
                .as_ref()
                .is_none_or(|old| old.connection_id != hello.connection_id);
            if changed {
                device.live = false;
                device.confirmed = false;
            }
            device.hello_received = now;
            device.seen(&hello.observed_at);
            device.hello = Some(hello.clone());
            if let Some(fence) = disabled
                && (changed || !device.confirmed)
            {
                let permission = Permission {
                    authorization_epoch: fence.authorization_epoch,
                    consent_version: fence.consent_version,
                    enabled: false,
                    connection_id: Some(hello.connection_id),
                    reporting_session_id: None,
                    expires_at: timestamp(now + LEASE_MS),
                };
                device.desired = Some(permission.clone());
                device.confirmed = false;
                let _ = self.publish_control(id, permission);
            }
        } else if topic == topics.acks {
            let Ok(ack) = decode_signed::<Ack>(payload, &key) else {
                return;
            };
            if ack.validate(id, now).is_err() {
                return;
            }
            let Some(device) = reporting.devices.get_mut(id) else {
                return;
            };
            if device.desired.as_ref() != Some(&ack.permission)
                || device.hello_received == 0
                || !device.hello.as_ref().is_some_and(|hello| {
                    ack.permission.connection_id.as_ref() == Some(&hello.connection_id)
                })
                || (ack.permission.enabled && !device.active(now))
            {
                return;
            }
            device.confirmed = true;
        } else if topic == topics.reports {
            let Ok(report) = decode_signed::<Report>(payload, &key) else {
                return;
            };
            if report.validate(id, now).is_err() {
                return;
            }
            if !reporting.fences.get(id).is_some_and(|fence| {
                fence.enabled
                    && fence.version() == (report.authorization_epoch, report.consent_version)
            }) {
                return;
            }
            let Some(device) = reporting.devices.get_mut(id) else {
                return;
            };
            if !device.active(now) || !device.confirmed {
                return;
            }
            let permission = device.desired.as_ref().unwrap();
            if !permission.matches_report(&report) || report.seq <= device.seq {
                return;
            }
            device.seq = report.seq;
            device.seen(&report.observed_at);
            let received = Received {
                report,
                received_at: timestamp(now),
            };
            if received.report.status.is_some() {
                device.last_success = Some(received.clone());
            }
            device.latest = Some(received);
        }
    }
}
