mod config;
mod lease;

pub use config::Config;

use anyhow::{Context, Result, ensure};
use doso_sdk::{DosoClient, Teleop};
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
use tokio::sync::mpsc;
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
    let (events, receiver) = mpsc::channel(4);
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
    let mut teleop = None;
    loop {
        let message = tokio::select! {
            _ = tokio::time::sleep_until(lease.deadline) => return Ok(()),
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
                let result = tokio::select! {
                    _ = tokio::time::sleep_until(lease.deadline) => return Ok(()),
                    next = commands.message() => {
                        if let Some(DeviceCommand { command: Some(rpc::device_command::Command::CloseSession(id)) }) = next?
                            && id == lease.grant.session_id {
                            return Ok(());
                        }
                        anyhow::bail!("control_input_before_ack");
                    },
                    result = execute(&client, &mut teleop, action) => result,
                };
                let reply = outcome(&input, result);
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

fn validate_identity(info: &doso_sdk::rpc::SystemInfo, device_id: &str) -> Result<()> {
    ensure!(
        info.robot_id.as_deref() == Some(device_id),
        "local_sdk_device_mismatch"
    );
    Ok(())
}

async fn execute<'a>(
    client: &'a DosoClient,
    teleop: &mut Option<Teleop<'a>>,
    action: Action,
) -> doso_sdk::Result<doso_sdk::rpc::TeleopReply> {
    if action == Action::Claim
        && teleop
            .as_ref()
            .is_some_and(|session| session.control_state().is_err())
    {
        *teleop = None;
    }
    if teleop.is_none() {
        *teleop = Some(client.motion().teleop().await?);
    }
    let result = match action {
        Action::Claim => teleop.as_mut().expect("open teleop").claim(None).await,
        Action::Hold => teleop.as_mut().expect("open teleop").hold(None).await,
        Action::Release => teleop.take().expect("open teleop").close(None).await,
        Action::Unspecified => unreachable!("validated control action"),
    };
    if result.is_err() {
        *teleop = None;
    }
    result
}

fn outcome(
    input: &ControlInput,
    result: doso_sdk::Result<doso_sdk::rpc::TeleopReply>,
) -> ControlReply {
    match result {
        Ok(reply) => ControlReply {
            session_id: input.session_id.clone(),
            sequence: input.sequence,
            accepted: reply.accepted,
            reason: reply.reason,
            held_by_client: reply
                .control
                .filter(|state| state.fresh)
                .and_then(|state| state.held_by_client),
            final_output_confirmed: reply.final_output_confirmed,
            submission_unknown: false,
        },
        Err(error) => ControlReply {
            session_id: input.session_id.clone(),
            sequence: input.sequence,
            accepted: false,
            reason: error.reason_code().unwrap_or(&error.message).to_owned(),
            held_by_client: None,
            final_output_confirmed: false,
            submission_unknown: error.submission_unknown,
        },
    }
}

#[cfg(test)]
mod tests;
