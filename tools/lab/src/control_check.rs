use anyhow::{Context, Result, ensure};
use robot_cloud_common::{
    control::{
        SESSION_MS,
        rpc::{
            Action, ControlInput, ControlReply, OperatorInput, operator_input,
            remote_control_client::RemoteControlClient,
        },
    },
    is_uuid, now_ms, parse_time,
};
use serde::Deserialize;
use std::{path::Path, time::Duration};
use tokio::sync::mpsc;
use tonic::{
    codegen::tokio_stream::wrappers::ReceiverStream,
    transport::{Channel, ClientTlsConfig, Endpoint},
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Session {
    device_id: String,
    session_id: String,
    expires_at: String,
    grant: String,
    endpoint: String,
}

async fn channel(url: &str) -> Result<Channel> {
    let uri = url::Url::parse(url)?;
    ensure!(
        uri.username().is_empty()
            && uri.password().is_none()
            && uri.query().is_none()
            && uri.fragment().is_none()
            && matches!(uri.path(), "" | "/"),
        "invalid control endpoint"
    );
    let mut endpoint =
        Endpoint::from_shared(url.to_owned())?.connect_timeout(Duration::from_secs(5));
    if uri.scheme() == "https" {
        endpoint = endpoint.tls_config(ClientTlsConfig::new().with_native_roots())?;
    } else {
        ensure!(
            uri.scheme() == "http"
                && matches!(uri.host_str(), Some("127.0.0.1" | "[::1]" | "localhost")),
            "control endpoint requires HTTPS"
        );
    }
    Ok(endpoint.connect().await?)
}

pub async fn run(path: &Path) -> Result<()> {
    let bytes = std::fs::read(path).context("read private control-session file")?;
    ensure!(
        bytes.len() <= robot_cloud_common::MAX_MESSAGE_BYTES,
        "session file exceeds 8 KiB"
    );
    let session: Session =
        serde_json::from_slice(&bytes).context("invalid control-session file")?;
    let remaining = parse_time(&session.expires_at)? - now_ms();
    ensure!(
        is_uuid(&session.device_id)
            && is_uuid(&session.session_id)
            && (1..=SESSION_MS).contains(&remaining),
        "control session is invalid or expired"
    );
    let mut client = RemoteControlClient::new(channel(&session.endpoint).await?)
        .max_decoding_message_size(robot_cloud_common::MAX_MESSAGE_BYTES)
        .max_encoding_message_size(robot_cloud_common::MAX_MESSAGE_BYTES);
    let (sender, receiver) = mpsc::channel(1);
    sender
        .send(OperatorInput {
            input: Some(operator_input::Input::Grant(session.grant.into_bytes())),
        })
        .await?;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(remaining as u64);
    let mut replies =
        tokio::time::timeout_at(deadline, client.operate(ReceiverStream::new(receiver)))
            .await??
            .into_inner();
    let opened = tokio::time::timeout_at(deadline, replies.message())
        .await??
        .context("control stream ended before session confirmation")?;
    ensure!(
        opened.session_id == session.session_id && opened.sequence == 0 && opened.accepted,
        "control session was not opened"
    );
    for (index, action) in [Action::Claim, Action::Hold, Action::Release]
        .into_iter()
        .enumerate()
    {
        let sequence = index as u64 + 1;
        sender
            .send(OperatorInput {
                input: Some(operator_input::Input::Command(ControlInput {
                    session_id: session.session_id.clone(),
                    sequence,
                    action: action as i32,
                    ..Default::default()
                })),
            })
            .await?;
        let reply: ControlReply = tokio::time::timeout_at(deadline, replies.message())
            .await??
            .context("control stream ended before device reply")?;
        ensure!(
            reply.session_id == session.session_id && reply.sequence == sequence,
            "control reply identity mismatch"
        );
        println!(
            "{}",
            serde_json::json!({
                "action": action.as_str_name(), "accepted": reply.accepted,
            "reason": reply.reason, "heldByClient": reply.held_by_client,
                "finalOutputConfirmed": reply.final_output_confirmed,
                "submissionUnknown": reply.submission_unknown,
            })
        );
        ensure!(
            reply.accepted && !reply.submission_unknown,
            "device operation was not confirmed"
        );
        if action == Action::Release {
            ensure!(
                reply.held_by_client == Some(false)
                    && (reply.final_output_confirmed || reply.reason == "already_idle"),
                "device release has no idle or final-output confirmation"
            );
        }
    }
    Ok(())
}
