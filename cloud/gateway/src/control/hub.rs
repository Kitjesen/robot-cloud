use crate::Gateway;
use ed25519_dalek::VerifyingKey;
use robot_cloud_common::{
    control::{Grant, SESSION_MS, rpc, validate_control_input},
    decode_signed, encode_signed, is_uuid, now_ms, parse_time,
    reporting::MAX_COUNTER,
    timestamp,
};
use serde::Serialize;
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{Semaphore, mpsc, watch},
    time::Instant,
};
use tonic::Status;
use uuid::Uuid;

type Commands = mpsc::Sender<Result<rpc::DeviceCommand, Status>>;
type Replies = mpsc::Sender<Result<rpc::ControlReply, Status>>;

const MAX_IN_FLIGHT_VELOCITY_SEQUENCES: usize = 128;

struct Pending {
    sequence: u64,
    action: rpc::Action,
    sent: Instant,
    timeout: Duration,
}

struct Session {
    grant: Grant,
    wire: Vec<u8>,
    expires: Instant,
    attached: bool,
    replies: Option<Replies>,
    last_sequence: u64,
    pending: Option<Pending>,
    velocity_sequences: VecDeque<u64>,
    ended: watch::Sender<bool>,
}

struct Device {
    connection_id: String,
    public_key: VerifyingKey,
    commands: Commands,
    session: Option<Session>,
    stop: watch::Sender<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IssuedSession {
    pub device_id: String,
    pub session_id: String,
    pub expires_at: String,
    pub grant: String,
}

pub struct Hub {
    pub(super) gateway: Arc<Gateway>,
    devices: Mutex<HashMap<String, Device>>,
    pub(super) authenticating: Arc<Semaphore>,
}

impl Hub {
    pub fn new(gateway: Arc<Gateway>) -> Self {
        Self {
            gateway,
            devices: Mutex::new(HashMap::new()),
            authenticating: Arc::new(Semaphore::new(16)),
        }
    }

    pub(super) fn register(
        &self,
        device_id: &str,
        connection_id: &str,
        public_key: VerifyingKey,
        commands: Commands,
        stop: watch::Sender<bool>,
    ) -> Result<(), Status> {
        let mut devices = self.devices.lock().unwrap();
        if devices.contains_key(device_id) {
            return Err(Status::already_exists("device_already_connected"));
        }
        devices.insert(
            device_id.to_owned(),
            Device {
                connection_id: connection_id.to_owned(),
                public_key,
                commands,
                session: None,
                stop,
            },
        );
        Ok(())
    }

    pub fn create_session(
        &self,
        device_id: &str,
        operator_id: &str,
        epoch: u64,
    ) -> Result<IssuedSession, Status> {
        if !is_uuid(device_id) || !is_uuid(operator_id) || !(1..=MAX_COUNTER).contains(&epoch) {
            return Err(Status::invalid_argument("invalid_control_identity"));
        }
        self.gateway
            .device_key(device_id)
            .map_err(|_| Status::permission_denied("device_disabled"))?;
        let mut devices = self.devices.lock().unwrap();
        let device = devices
            .get_mut(device_id)
            .ok_or_else(|| Status::unavailable("device_disconnected"))?;
        expire(device);
        if device.session.is_some() {
            return Err(Status::already_exists("control_session_active"));
        }
        let now = now_ms();
        let grant = Grant {
            v: 1,
            kind: "control.grant".into(),
            device_id: device_id.to_owned(),
            operator_id: operator_id.to_owned(),
            authorization_epoch: epoch,
            connection_id: device.connection_id.clone(),
            session_id: Uuid::new_v4().to_string(),
            issued_at: timestamp(now),
            expires_at: timestamp(now + SESSION_MS),
        };
        let wire = encode_signed(&grant, &self.gateway.signing_key)
            .map_err(|_| Status::internal("grant_signing_failed"))?;
        let issued = IssuedSession {
            device_id: grant.device_id.clone(),
            session_id: grant.session_id.clone(),
            expires_at: grant.expires_at.clone(),
            grant: String::from_utf8(wire.clone())
                .map_err(|_| Status::internal("grant_encoding_failed"))?,
        };
        let (ended, _) = watch::channel(false);
        device.session = Some(Session {
            grant,
            wire,
            expires: Instant::now() + Duration::from_millis(SESSION_MS as u64),
            attached: false,
            replies: None,
            last_sequence: 0,
            pending: None,
            velocity_sequences: VecDeque::new(),
            ended,
        });
        Ok(issued)
    }

    pub fn revoke(
        &self,
        device_id: &str,
        session_id: &str,
        operator_id: &str,
        epoch: u64,
    ) -> Result<(), Status> {
        let mut devices = self.devices.lock().unwrap();
        let device = devices
            .get_mut(device_id)
            .ok_or_else(|| Status::not_found("control_session_missing"))?;
        let session = device
            .session
            .as_ref()
            .ok_or_else(|| Status::not_found("control_session_missing"))?;
        if session.grant.session_id != session_id
            || session.grant.operator_id != operator_id
            || session.grant.authorization_epoch != epoch
        {
            return Err(Status::permission_denied("control_session_mismatch"));
        }
        close_session(device, Some(Status::cancelled("control_session_revoked")));
        Ok(())
    }

    pub(super) fn attach(
        &self,
        bytes: &[u8],
        replies: Replies,
    ) -> Result<(Grant, watch::Receiver<bool>), Status> {
        let grant: Grant = decode_signed(bytes, &self.gateway.signing_key.verifying_key())
            .map_err(|_| Status::unauthenticated("invalid_control_grant"))?;
        grant
            .validate(&grant.device_id, &grant.connection_id, now_ms())
            .map_err(|_| Status::unauthenticated("expired_control_grant"))?;
        self.gateway
            .device_key(&grant.device_id)
            .map_err(|_| Status::permission_denied("device_disabled"))?;
        let mut devices = self.devices.lock().unwrap();
        let device = devices
            .get_mut(&grant.device_id)
            .ok_or_else(|| Status::unavailable("device_disconnected"))?;
        expire(device);
        let session = device
            .session
            .as_mut()
            .ok_or_else(|| Status::unauthenticated("control_session_missing"))?;
        if device.connection_id != grant.connection_id || session.wire != bytes {
            return Err(Status::permission_denied("control_session_mismatch"));
        }
        if session.attached {
            return Err(Status::already_exists("operator_already_connected"));
        }
        device
            .commands
            .try_send(Ok(rpc::DeviceCommand {
                command: Some(rpc::device_command::Command::Grant(bytes.to_vec())),
            }))
            .map_err(|_| Status::unavailable("device_send_unavailable"))?;
        session.attached = true;
        session.replies = Some(replies);
        Ok((grant, session.ended.subscribe()))
    }

    pub(super) fn input(&self, grant: &Grant, input: rpc::ControlInput) -> Result<(), Status> {
        self.gateway
            .device_key(&grant.device_id)
            .map_err(|_| Status::permission_denied("device_disabled"))?;
        let action = validate_control_input(&input)
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let mut devices = self.devices.lock().unwrap();
        let device = devices
            .get_mut(&grant.device_id)
            .ok_or_else(|| Status::unavailable("device_disconnected"))?;
        expire(device);
        let session = device
            .session
            .as_mut()
            .ok_or_else(|| Status::deadline_exceeded("control_session_ended"))?;
        if session.grant.session_id != grant.session_id || input.session_id != grant.session_id {
            return Err(Status::permission_denied("control_session_mismatch"));
        }
        if input.sequence == 0
            || input.sequence > MAX_COUNTER
            || input.sequence <= session.last_sequence
        {
            return Err(Status::invalid_argument("invalid_control_sequence"));
        }
        if session.pending.is_some() {
            return Err(Status::resource_exhausted("control_ack_pending"));
        }
        let sequence = input.sequence;
        let send_res = device.commands.try_send(Ok(rpc::DeviceCommand {
            command: Some(rpc::device_command::Command::Input(input)),
        }));
        if let Err(error) = send_res {
            return match error {
                mpsc::error::TrySendError::Full(_) if action == rpc::Action::Velocity => {
                    Err(Status::resource_exhausted("velocity_backpressure"))
                }
                _ => Err(Status::unavailable("device_send_unavailable")),
            };
        }
        session.last_sequence = sequence;
        if action == rpc::Action::Velocity {
            if session.velocity_sequences.len() >= MAX_IN_FLIGHT_VELOCITY_SEQUENCES {
                session.velocity_sequences.pop_front();
            }
            session.velocity_sequences.push_back(sequence);
        } else {
            session.pending = Some(Pending {
                sequence,
                action,
                sent: Instant::now(),
                timeout: action_timeout(action),
            });
        }
        Ok(())
    }

    pub(super) fn reply(
        &self,
        device_id: &str,
        connection_id: &str,
        reply: rpc::ControlReply,
    ) -> Result<(), Status> {
        let mut devices = self.devices.lock().unwrap();
        let device = devices
            .get_mut(device_id)
            .ok_or_else(|| Status::unavailable("device_disconnected"))?;
        if device.connection_id != connection_id {
            return Err(Status::permission_denied("device_connection_mismatch"));
        }
        expire(device);
        let Some(session) = device.session.as_mut() else {
            return Ok(());
        };
        // A late acknowledgement from a closed session cannot affect its replacement.
        if reply.session_id != session.grant.session_id {
            return Ok(());
        }
        if reply.reason.len() > 512 {
            return Err(Status::invalid_argument("invalid_control_ack"));
        }
        // Unsolicited input-window refresh pushed by the robot before an explicit request.
        if reply.sequence == 0 && reply.accepted && reply.input_window != 0 {
            if let Some(replies) = &session.replies {
                let _ = replies.try_send(Ok(reply));
            }
            return Ok(());
        }
        if let Some(pos) = session
            .velocity_sequences
            .iter()
            .position(|&seq| seq == reply.sequence)
        {
            session.velocity_sequences.remove(pos);
            if let Some(replies) = &session.replies {
                let _ = replies.try_send(Ok(reply));
            }
            return Ok(());
        }
        let Some(pending) = session.pending.as_ref() else {
            return if reply.sequence <= session.last_sequence {
                Ok(())
            } else {
                Err(Status::invalid_argument("unexpected_control_ack"))
            };
        };
        if reply.sequence < pending.sequence {
            return Ok(());
        }
        if reply.sequence != pending.sequence {
            return Err(Status::invalid_argument("invalid_control_ack"));
        }
        let release = pending.action == rpc::Action::Release;
        let replies = session
            .replies
            .as_ref()
            .ok_or_else(|| Status::cancelled("operator_disconnected"))?;
        replies
            .try_send(Ok(reply))
            .map_err(|_| Status::cancelled("operator_send_unavailable"))?;
        session.pending = None;
        if release {
            close_session(device, None);
        }
        Ok(())
    }

    pub(super) fn maintain(&self, device_id: &str, connection_id: &str) -> bool {
        let public_key = match self.gateway.device_key(device_id) {
            Ok(key) => key,
            Err(_) => {
                self.disconnected(device_id, connection_id);
                return false;
            }
        };
        let mut devices = self.devices.lock().unwrap();
        let Some(device) = devices.get_mut(device_id) else {
            return false;
        };
        if device.connection_id != connection_id {
            return false;
        }
        if device.public_key != public_key {
            drop(devices);
            self.disconnected(device_id, connection_id);
            return false;
        }
        expire(device);
        true
    }

    pub(super) fn detach(&self, grant: &Grant) {
        let mut devices = self.devices.lock().unwrap();
        if let Some(device) = devices.get_mut(&grant.device_id)
            && device
                .session
                .as_ref()
                .is_some_and(|session| session.grant.session_id == grant.session_id)
        {
            close_session(device, None);
        }
    }

    pub(super) fn disconnected(&self, device_id: &str, connection_id: &str) {
        let mut devices = self.devices.lock().unwrap();
        if devices
            .get(device_id)
            .is_some_and(|device| device.connection_id == connection_id)
            && let Some(mut device) = devices.remove(device_id)
        {
            close_session(
                &mut device,
                Some(Status::unavailable("device_disconnected")),
            );
        }
    }

    pub fn shutdown(&self) {
        for (_, mut device) in self.devices.lock().unwrap().drain() {
            close_session(&mut device, Some(Status::unavailable("gateway_stopping")));
            let _ = device.stop.send(true);
        }
    }
}

fn action_timeout(action: rpc::Action) -> Duration {
    match action {
        rpc::Action::StandUp
        | rpc::Action::SitDown
        | rpc::Action::EnableMotors
        | rpc::Action::DisableMotors
        | rpc::Action::SwitchProfile => Duration::from_secs(16),
        _ => Duration::from_secs(3),
    }
}

fn expire(device: &mut Device) {
    if device.session.as_ref().is_some_and(|session| {
        Instant::now() >= session.expires
            || parse_time(&session.grant.expires_at).is_ok_and(|expires| now_ms() >= expires)
            || session
                .pending
                .as_ref()
                .is_some_and(|pending| pending.sent.elapsed() >= pending.timeout)
    }) {
        close_session(
            device,
            Some(Status::deadline_exceeded("control_session_expired")),
        );
    }
}

fn close_session(device: &mut Device, error: Option<Status>) {
    if let Some(session) = device.session.take() {
        let _ = session.ended.send(true);
        if let Some(replies) = session.replies
            && let Some(error) = error
        {
            let _ = replies.try_send(Err(error));
        }
        if session.attached
            && device
                .commands
                .try_send(Ok(rpc::DeviceCommand {
                    command: Some(rpc::device_command::Command::CloseSession(
                        session.grant.session_id,
                    )),
                }))
                .is_err()
        {
            // A full or closed device stream must end so its local disconnect handler releases authority.
            let _ = device.stop.send(true);
        }
    }
}
