mod config;
mod lease;
mod teleop;

pub use config::Config;

use anyhow::{Context, Result, ensure};
use doso_sdk::DosoClient;
use ed25519_dalek::VerifyingKey;
use lease::Lease;
use robot_cloud_common::{
    control::{
        Grant, PROOF_MS, Proof,
        rpc::{self, Action, ControlInput, ControlReply, DeviceCommand, DeviceEvent},
    },
    decode_signed, encode_signed, is_uuid, now_ms, parse_time, timestamp,
};
use std::{collections::HashMap, time::Duration};
use teleop::TeleopBridge;
use tokio::{sync::mpsc, time::Instant};
use tonic::{Request, Streaming, codegen::tokio_stream::wrappers::ReceiverStream};

/// Reconnect with a new device connection identity after every transport failure.
pub async fn run(config: &Config) -> Result<()> {
    config.validate()?;
    loop {
        if connect(config).await.is_err() {
            eprintln!("control_retry reason=connection_closed");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// One device link, used by the production reconnect loop and local acceptance lab.
pub async fn connect(config: &Config) -> Result<()> {
    config.validate()?;
    let channel = config.endpoint()?.connect().await?;
    let mut client = rpc::remote_control_client::RemoteControlClient::new(channel)
        .max_decoding_message_size(robot_cloud_common::MAX_MESSAGE_BYTES)
        .max_encoding_message_size(robot_cloud_common::MAX_MESSAGE_BYTES);
    let (events, receiver) = mpsc::channel(16);
    let mut request = Request::new(ReceiverStream::new(receiver));
    request
        .metadata_mut()
        .insert("x-doso-device-id", config.device_id.parse()?);
    let mut commands = client.device_link(request).await?.into_inner();
    let challenge =
        tokio::time::timeout(Duration::from_millis(PROOF_MS as u64), commands.message())
            .await??
            .context("control_link_closed")?;
    let Some(rpc::device_command::Command::Challenge(challenge)) = challenge.command else {
        anyhow::bail!("missing_control_challenge");
    };
    ensure!(is_uuid(&challenge), "invalid_control_challenge");
    let connection = uuid::Uuid::new_v4().to_string();
    let proof = Proof {
        v: 1,
        kind: "control.hello".into(),
        device_id: config.device_id.clone(),
        connection_id: connection.clone(),
        challenge,
        issued_at: timestamp(now_ms()),
    };
    events
        .send(DeviceEvent {
            event: Some(rpc::device_event::Event::Proof(encode_signed(
                &proof,
                &config.signing_key,
            )?)),
        })
        .await?;
    let mut seen = HashMap::new();
    let mut epoch = 0;
    while let Some(command) = commands.message().await? {
        let bytes = match command.command {
            Some(rpc::device_command::Command::Grant(bytes)) => bytes,
            Some(rpc::device_command::Command::CloseSession(id)) if seen.contains_key(&id) => {
                continue;
            }
            _ => anyhow::bail!("expected_control_grant"),
        };
        let now = now_ms();
        let grant = accept_grant(
            &bytes,
            &config.gateway_key,
            &config.device_id,
            &connection,
            now,
            &mut seen,
            &mut epoch,
        )?;
        let lease = Lease::new(grant, &config.device_id, &connection, now)?;
        session(config, lease, &mut commands, &events).await?;
    }
    Ok(())
}

fn accept_grant(
    bytes: &[u8],
    key: &VerifyingKey,
    device: &str,
    connection: &str,
    now: i64,
    seen: &mut HashMap<String, tokio::time::Instant>,
    epoch: &mut u64,
) -> Result<Grant> {
    let grant: Grant = decode_signed(bytes, key)?;
    grant.validate(device, connection, now)?;
    let instant = tokio::time::Instant::now();
    seen.retain(|_, expires| *expires > instant);
    ensure!(
        !seen.contains_key(&grant.session_id) && seen.len() < 128,
        "replayed_control_session"
    );
    ensure!(
        grant.authorization_epoch >= *epoch,
        "stale_control_authorization"
    );
    *epoch = grant.authorization_epoch;
    seen.insert(
        grant.session_id.clone(),
        instant + Duration::from_millis((parse_time(&grant.expires_at)? - now) as u64),
    );
    Ok(grant)
}

async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

async fn session(
    config: &Config,
    mut lease: Lease,
    commands: &mut Streaming<DeviceCommand>,
    events: &mpsc::Sender<DeviceEvent>,
) -> Result<()> {
    let client = tokio::time::timeout_at(
        lease.deadline,
        DosoClient::with_config(config.sdk(&lease.grant.session_id)),
    )
    .await??;
    validate_identity(client.system().info(), &config.device_id)?;
    let mut teleop: Option<TeleopBridge> = None;
    let mut velocity_watchdog: Option<Instant> = None;
    loop {
        let message = tokio::select! {
            _ = tokio::time::sleep_until(lease.deadline) => return Ok(()),
            _ = sleep_until_opt(velocity_watchdog) => {
                velocity_watchdog = None;
                eprintln!("control_velocity_watchdog_hold");
                if let Some(bridge) = teleop.as_mut().filter(|b| b.is_healthy()) {
                    let hold_deadline = lease.deadline.min(Instant::now() + Duration::from_secs(1));
                    if !matches!(tokio::time::timeout_at(hold_deadline, bridge.hold()).await, Ok(Ok(_))) {
                        teleop = None;
                    }
                } else {
                    teleop = None;
                }
                continue;
            }
            message = commands.message() => message?.context("control_link_closed")?,
        };
        match message.command {
            Some(rpc::device_command::Command::CloseSession(id))
                if id == lease.grant.session_id =>
            {
                return Ok(());
            }
            Some(rpc::device_command::Command::Input(input)) => {
                let action = lease.accept(&input, now_ms())?;
                if action == Action::Velocity {
                    let velocity = input.velocity.as_ref().expect("validated velocity command");
                    let reply_err = match ensure_teleop(config, &lease, &mut teleop, events).await?
                    {
                        Ok(()) => {
                            let bridge = teleop.as_mut().expect("opened teleop bridge");
                            match bridge.publish_velocity(input.sequence, velocity) {
                                Ok(()) => {
                                    let active = velocity.deadman
                                        || velocity.vx_m_s != 0.0
                                        || velocity.vy_m_s != 0.0
                                        || velocity.yaw_rad_s != 0.0;
                                    velocity_watchdog =
                                        active.then(|| Instant::now() + config.velocity_timeout);
                                    None
                                }
                                Err(err) => {
                                    velocity_watchdog = None;
                                    let win = bridge.latest_window();
                                    if !bridge.is_healthy() {
                                        teleop = None;
                                    }
                                    Some((err.reason, err.submission_unknown, win))
                                }
                            }
                        }
                        Err(reason) => {
                            velocity_watchdog = None;
                            Some((reason, false, 0))
                        }
                    };
                    if let Some((reason, submission_unknown, input_window)) = reply_err {
                        let reply =
                            error_to_reply(&input, reason, submission_unknown, input_window);
                        tokio::time::timeout_at(
                            lease.deadline,
                            events.send(DeviceEvent {
                                event: Some(rpc::device_event::Event::Reply(reply)),
                            }),
                        )
                        .await??;
                    }
                    continue;
                }

                velocity_watchdog = None;
                let reply = tokio::select! {
                    _ = tokio::time::sleep_until(lease.deadline) => return Ok(()),
                    next = commands.message() => {
                        if let Some(DeviceCommand { command: Some(rpc::device_command::Command::CloseSession(id)) }) = next?
                            && id == lease.grant.session_id {
                            return Ok(());
                        }
                        anyhow::bail!("control_input_before_ack");
                    },
                    reply = execute_non_velocity(config, &lease, &client, &mut teleop, events, &input, action) => reply?,
                };
                tokio::time::timeout_at(
                    lease.deadline,
                    events.send(DeviceEvent {
                        event: Some(rpc::device_event::Event::Reply(reply)),
                    }),
                )
                .await??;
                if action == Action::Release {
                    return Ok(());
                }
            }
            _ => anyhow::bail!("invalid_control_session_message"),
        }
    }
}

async fn ensure_teleop(
    config: &Config,
    lease: &Lease,
    teleop: &mut Option<TeleopBridge>,
    events: &mpsc::Sender<DeviceEvent>,
) -> Result<std::result::Result<(), String>> {
    if teleop.as_ref().is_some_and(|bridge| !bridge.is_healthy()) {
        *teleop = None;
    }
    if teleop.is_none() {
        let opened = tokio::time::timeout_at(
            lease.deadline,
            TeleopBridge::open(config, &lease.grant.session_id, events.clone()),
        )
        .await
        .map_err(|_| anyhow::anyhow!("control_session_expired"))?;
        match opened {
            Ok(bridge) => *teleop = Some(bridge),
            Err(error) => return Ok(Err(error.to_string())),
        }
    }
    Ok(Ok(()))
}

fn validate_identity(info: &doso_sdk::rpc::SystemInfo, device_id: &str) -> Result<()> {
    ensure!(
        info.robot_id.as_deref() == Some(device_id),
        "local_sdk_device_mismatch"
    );
    Ok(())
}

async fn execute_non_velocity(
    config: &Config,
    lease: &Lease,
    client: &DosoClient,
    teleop: &mut Option<TeleopBridge>,
    events: &mpsc::Sender<DeviceEvent>,
    input: &ControlInput,
    action: Action,
) -> Result<ControlReply> {
    let win = teleop.as_ref().map_or(0, TeleopBridge::latest_window);
    match action {
        Action::Claim | Action::Hold | Action::Release => {
            if let Err(reason) = ensure_teleop(config, lease, teleop, events).await? {
                return Ok(error_to_reply(input, reason, false, 0));
            }
            let res = match action {
                Action::Claim => teleop.as_mut().expect("opened teleop").claim().await,
                Action::Hold => teleop.as_mut().expect("opened teleop").hold().await,
                Action::Release => teleop.take().expect("opened teleop").close().await,
                _ => unreachable!(),
            };
            Ok(match res {
                Ok(reply) => control_reply_from_sdk(&input.session_id, input.sequence, reply),
                Err(err) => {
                    let err_win = teleop.as_ref().map_or(win, TeleopBridge::latest_window);
                    if !teleop.as_ref().is_some_and(TeleopBridge::is_healthy) {
                        *teleop = None;
                    }
                    error_to_reply(input, err.reason, err.submission_unknown, err_win)
                }
            })
        }
        Action::StandUp => Ok(receipt_outcome(
            input,
            client.motion().stand_up(None).await,
            win,
        )),
        Action::SitDown => Ok(receipt_outcome(
            input,
            client.motion().sit_down(None).await,
            win,
        )),
        Action::StopMotion => Ok(receipt_outcome(
            input,
            client.motion().stop(None).await,
            win,
        )),
        Action::EmergencyStop => Ok(receipt_outcome(
            input,
            client.motion().emergency_stop(None).await,
            win,
        )),
        Action::ClearEmergencyStop => Ok(receipt_outcome(
            input,
            client.motion().clear_emergency_stop(None).await,
            win,
        )),
        Action::Resume => Ok(receipt_outcome(
            input,
            client.motion().resume(None).await,
            win,
        )),
        Action::EnableMotors => Ok(receipt_outcome(
            input,
            client.motion().enable_motors(None).await,
            win,
        )),
        Action::DisableMotors => Ok(receipt_outcome(
            input,
            client.motion().disable_motors(None).await,
            win,
        )),
        Action::SwitchProfile => Ok(receipt_outcome(
            input,
            client
                .motion()
                .switch_profile(&input.profile_name, None)
                .await,
            win,
        )),
        Action::Velocity | Action::Unspecified => unreachable!("validated non-velocity action"),
    }
}

pub(super) fn control_reply_from_sdk(
    session_id: &str,
    sequence: u64,
    reply: doso_sdk::rpc::TeleopReply,
) -> ControlReply {
    ControlReply {
        session_id: session_id.to_owned(),
        sequence,
        accepted: reply.accepted,
        reason: reply.reason,
        held_by_client: reply
            .control
            .filter(|state| state.fresh)
            .and_then(|state| state.held_by_client),
        final_output_confirmed: reply.final_output_confirmed,
        submission_unknown: false,
        input_window: reply.input_window,
        ingress_accepted: reply.ingress_accepted,
    }
}

fn error_to_reply(
    input: &ControlInput,
    reason: String,
    submission_unknown: bool,
    input_window: u64,
) -> ControlReply {
    ControlReply {
        session_id: input.session_id.clone(),
        sequence: input.sequence,
        accepted: false,
        reason,
        held_by_client: None,
        final_output_confirmed: false,
        submission_unknown,
        input_window,
        ingress_accepted: false,
    }
}

#[cfg(test)]
fn outcome(
    input: &ControlInput,
    result: doso_sdk::Result<doso_sdk::rpc::TeleopReply>,
) -> ControlReply {
    match result {
        Ok(reply) => control_reply_from_sdk(&input.session_id, input.sequence, reply),
        Err(error) => error_to_reply(
            input,
            error.reason_code().unwrap_or(&error.message).to_owned(),
            error.submission_unknown,
            0,
        ),
    }
}

fn receipt_outcome(
    input: &ControlInput,
    result: doso_sdk::Result<doso_sdk::CommandReceipt>,
    input_window: u64,
) -> ControlReply {
    match result {
        Ok(receipt) => {
            let accepted = receipt.ok && receipt.accepted.unwrap_or(true);
            let mut reason = if !receipt.reason_code.is_empty() {
                receipt.reason_code
            } else {
                receipt.reason
            };
            if !accepted && reason.is_empty() {
                reason = "rejected".into();
            }
            ControlReply {
                session_id: input.session_id.clone(),
                sequence: input.sequence,
                accepted,
                reason,
                held_by_client: None,
                final_output_confirmed: false,
                submission_unknown: false,
                input_window,
                ingress_accepted: false,
            }
        }
        Err(error) => error_to_reply(
            input,
            error.reason_code().unwrap_or(&error.message).to_owned(),
            error.submission_unknown,
            input_window,
        ),
    }
}

#[cfg(test)]
mod tests;
