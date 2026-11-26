use super::{control_check, control_fixture, fixture::Fixture};
use anyhow::{Context, Result, ensure};
use doso_cloud_gateway::{
    Gateway, app_with_control,
    control::{Hub, serve},
};
use ed25519_dalek::{
    SigningKey,
    pkcs8::{EncodePublicKey, spki::der::pem::LineEnding},
};
use robot_cloud_common::control::rpc;
use std::{
    fs,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tonic::codegen::tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};

struct Local {
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Local {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

pub async fn run() -> Result<()> {
    let mut fixture = Fixture::new().await?;
    let device = uuid::Uuid::new_v4().to_string();
    let operator = uuid::Uuid::new_v4().to_string();
    let key = SigningKey::from_bytes(&[31; 32]);
    let gateway_key = SigningKey::from_bytes(&[32; 32]);
    fs::write(
        fixture.path("device.pub"),
        key.verifying_key().to_public_key_pem(LineEnding::LF)?,
    )?;
    fs::write(
        fixture.path("devices.json"),
        serde_json::to_vec(&serde_json::json!({
            "devices": [{"deviceId":device,"enabled":true,"publicKeyPath":"device.pub"}]
        }))?,
    )?;
    let (mqtt, _) = rumqttc::v5::AsyncClient::new(
        rumqttc::v5::MqttOptions::new("SIMULATED-unused", "127.0.0.1", 1883),
        1,
    );
    let gateway = Arc::new(Gateway::new(
        mqtt,
        gateway_key.clone(),
        fixture.path("devices.json"),
    )?);
    let hub = Arc::new(Hub::new(gateway.clone()));
    let cloud = TcpListener::bind("127.0.0.1:0").await?;
    let cloud_url = format!("http://{}", cloud.local_addr()?);
    let http = TcpListener::bind("127.0.0.1:0").await?;
    let http_url = format!(
        "http://{}/api/v1/robots/{device}/control/sessions",
        http.local_addr()?
    );
    let sdk = TcpListener::bind("127.0.0.1:0").await?;
    let sdk_url = format!("http://{}", sdk.local_addr()?);
    let state = control_fixture::State::new(device.clone());
    let server_state = state.clone();
    let mut local = Local { tasks: Vec::new() };
    let (_stop, stopped) = watch::channel(false);
    let relay = hub.clone();
    local.tasks.push(tokio::spawn(async move {
        let _ = serve(relay, cloud, stopped).await;
    }));
    let api_key = "lab-private-service-key-12345678901234567890";
    let router = app_with_control(gateway, api_key, Some(hub.clone()))?;
    local.tasks.push(tokio::spawn(async move {
        let _ = axum::serve(http, router).await;
    }));
    local.tasks.push(tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(control_fixture::System(server_state.clone()))
            .add_service(control_fixture::Motion(server_state))
            .serve_with_incoming(TcpListenerStream::new(sdk))
            .await;
    }));
    let config = doso_cloud_link::control::Config {
        device_id: device.clone(),
        signing_key: key,
        gateway_key: gateway_key.verifying_key(),
        cloud_url: cloud_url.clone(),
        cloud_ca: None,
        local_endpoint: sdk_url,
        local_api_key: "lab-control-key".into(),
        local_ca: None,
        velocity_timeout: Duration::from_millis(300),
    };
    local.tasks.push(tokio::spawn(async move {
        let _ = doso_cloud_link::control::connect(&config).await;
    }));
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()?;
    let unauthenticated = client.post(&http_url).send().await?;
    ensure!(
        unauthenticated.status() == 401,
        "unauthenticated session creation was accepted"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut session = loop {
        let response = client
            .post(&http_url)
            .bearer_auth(api_key)
            .json(&serde_json::json!({"operatorId":operator,"authorizationEpoch":1}))
            .send()
            .await?;
        if response.status().is_success() {
            break response.json::<serde_json::Value>().await?;
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "robot control link did not become ready"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    session["endpoint"] = cloud_url.clone().into();
    let path = fixture.path("control-session.json");
    fs::write(&path, serde_json::to_vec(&session)?)?;
    control_check::run(&path).await?;
    ensure!(
        !state.held.load(Ordering::SeqCst),
        "SIMULATED local authority was not released"
    );
    ensure!(
        state.disconnects.load(Ordering::SeqCst) > 0,
        "SIMULATED local stream did not close"
    );
    let issued = hub.create_session(&device, &operator, 1)?;
    let mut operator_client =
        rpc::remote_control_client::RemoteControlClient::connect(cloud_url.clone()).await?;
    let (inputs, receiver) = mpsc::channel(1);
    inputs
        .send(rpc::OperatorInput {
            input: Some(rpc::operator_input::Input::Grant(issued.grant.into_bytes())),
        })
        .await?;
    let mut replies = operator_client
        .operate(ReceiverStream::new(receiver))
        .await?
        .into_inner();
    ensure!(
        replies
            .message()
            .await?
            .is_some_and(|reply| reply.sequence == 0 && reply.accepted),
        "second operator session was not opened"
    );
    tokio::time::sleep(Duration::from_secs(4)).await;
    ensure!(
        state.opens.load(Ordering::SeqCst) == 1,
        "operator idle time opened a local Teleop prematurely"
    );
    inputs
        .send(rpc::OperatorInput {
            input: Some(rpc::operator_input::Input::Command(rpc::ControlInput {
                session_id: issued.session_id.clone(),
                sequence: 1,
                action: rpc::Action::Claim as i32,
                ..Default::default()
            })),
        })
        .await?;
    let claimed = tokio::time::timeout(Duration::from_secs(3), replies.message()).await??;
    ensure!(
        claimed.is_some_and(|reply| reply.accepted && reply.held_by_client == Some(true)),
        "delayed first claim failed"
    );
    tokio::time::sleep(Duration::from_secs(4)).await;
    ensure!(
        !state.held.load(Ordering::SeqCst),
        "SIMULATED idle stream did not release"
    );
    inputs
        .send(rpc::OperatorInput {
            input: Some(rpc::operator_input::Input::Command(rpc::ControlInput {
                session_id: issued.session_id,
                sequence: 2,
                action: rpc::Action::Claim as i32,
                ..Default::default()
            })),
        })
        .await?;
    let reclaimed = tokio::time::timeout(Duration::from_secs(3), replies.message()).await??;
    ensure!(
        reclaimed.is_some_and(|reply| reply.accepted && reply.held_by_client == Some(true))
            && state.opens.load(Ordering::SeqCst) == 3,
        "explicit claim did not reopen the idle stream"
    );
    drop(inputs);
    drop(replies);
    tokio::time::timeout(Duration::from_secs(3), async {
        while state.held.load(Ordering::SeqCst) || state.disconnects.load(Ordering::SeqCst) < 3 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;

    // Third session verifies discrete motion actions, verbatim input_window, 50Hz velocity, and watchdog hold.
    let issued3 = hub.create_session(&device, &operator, 1)?;
    let mut operator_client3 =
        rpc::remote_control_client::RemoteControlClient::connect(cloud_url).await?;
    let (inputs3, receiver3) = mpsc::channel(16);
    inputs3
        .send(rpc::OperatorInput {
            input: Some(rpc::operator_input::Input::Grant(
                issued3.grant.into_bytes(),
            )),
        })
        .await?;
    let mut replies3 = operator_client3
        .operate(ReceiverStream::new(receiver3))
        .await?
        .into_inner();
    ensure!(
        replies3
            .message()
            .await?
            .is_some_and(|reply| reply.sequence == 0 && reply.accepted),
        "third operator session was not opened"
    );

    let mut seq = 0u64;
    for (action, profile) in [
        (rpc::Action::StandUp, ""),
        (rpc::Action::SitDown, ""),
        (rpc::Action::SwitchProfile, "flat"),
        (rpc::Action::StopMotion, ""),
        (rpc::Action::EmergencyStop, ""),
        (rpc::Action::ClearEmergencyStop, ""),
        (rpc::Action::Resume, ""),
        (rpc::Action::EnableMotors, ""),
        (rpc::Action::DisableMotors, ""),
    ] {
        seq += 1;
        inputs3
            .send(rpc::OperatorInput {
                input: Some(rpc::operator_input::Input::Command(rpc::ControlInput {
                    session_id: issued3.session_id.clone(),
                    sequence: seq,
                    action: action as i32,
                    profile_name: profile.to_owned(),
                    ..Default::default()
                })),
            })
            .await?;
        let reply = tokio::time::timeout(Duration::from_secs(3), replies3.message())
            .await??
            .context("missing discrete action reply")?;
        ensure!(
            reply.sequence == seq
                && reply.accepted
                && !reply.final_output_confirmed
                && !reply.submission_unknown,
            "discrete action {:?} failed: {:?}",
            action,
            reply
        );
    }
    ensure!(
        state.discrete_calls.lock().unwrap().as_slice()
            == [
                "StandUp",
                "SitDown",
                "SwitchProfile:flat",
                "StopMotion",
                "Stop",
                "ClearEmergencyStop",
                "Resume",
                "EnableMotors",
                "DisableMotors",
            ],
        "unexpected discrete SDK calls: {:?}",
        *state.discrete_calls.lock().unwrap()
    );

    seq += 1;
    inputs3
        .send(rpc::OperatorInput {
            input: Some(rpc::operator_input::Input::Command(rpc::ControlInput {
                session_id: issued3.session_id.clone(),
                sequence: seq,
                action: rpc::Action::Claim as i32,
                ..Default::default()
            })),
        })
        .await?;
    let claim3 = tokio::time::timeout(Duration::from_secs(3), replies3.message())
        .await??
        .context("missing third session claim reply")?;
    ensure!(
        claim3.sequence == seq && claim3.accepted && claim3.input_window == 1,
        "expected initial input_window == 1 on claim: {:?}",
        claim3
    );

    // Advance robot window and verify async window update reaches operator.
    state.window_tx.send(2)?;
    let window_push = tokio::time::timeout(Duration::from_secs(3), replies3.message())
        .await??
        .context("missing async window update")?;
    ensure!(
        window_push.sequence == 0 && window_push.accepted && window_push.input_window == 2,
        "unexpected window update: {:?}",
        window_push
    );

    // Stale operator window must reach robot verbatim and be rejected cleanly.
    seq += 1;
    let stale_seq = seq;
    inputs3
        .send(rpc::OperatorInput {
            input: Some(rpc::operator_input::Input::Command(rpc::ControlInput {
                session_id: issued3.session_id.clone(),
                sequence: stale_seq,
                action: rpc::Action::Velocity as i32,
                velocity: Some(rpc::VelocityCommand {
                    vx_m_s: 0.3,
                    vy_m_s: 0.0,
                    yaw_rad_s: 0.0,
                    deadman: true,
                    manual_mode: false,
                    input_window: 1,
                }),
                ..Default::default()
            })),
        })
        .await?;
    let stale_reply = tokio::time::timeout(Duration::from_secs(3), replies3.message())
        .await??
        .context("missing stale velocity reply")?;
    ensure!(
        stale_reply.sequence == stale_seq
            && !stale_reply.accepted
            && stale_reply.reason == "stale_input_window"
            && state.last_velocity_window.load(Ordering::SeqCst) == 1
            && state.velocity_rejected.load(Ordering::SeqCst) == 1,
        "stale input window was not preserved verbatim: {:?}",
        stale_reply
    );

    // Stream 5 fresh 50Hz velocity frames with input_window == 2.
    let mut fresh_seqs = Vec::new();
    for _ in 0..5 {
        seq += 1;
        fresh_seqs.push(seq);
        inputs3
            .send(rpc::OperatorInput {
                input: Some(rpc::operator_input::Input::Command(rpc::ControlInput {
                    session_id: issued3.session_id.clone(),
                    sequence: seq,
                    action: rpc::Action::Velocity as i32,
                    velocity: Some(rpc::VelocityCommand {
                        vx_m_s: 0.4,
                        vy_m_s: 0.0,
                        yaw_rad_s: 0.1,
                        deadman: true,
                        manual_mode: false,
                        input_window: 2,
                    }),
                    ..Default::default()
                })),
            })
            .await?;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    for expected_seq in fresh_seqs {
        let vel_reply = tokio::time::timeout(Duration::from_secs(3), replies3.message())
            .await??
            .context("missing fresh velocity reply")?;
        ensure!(
            vel_reply.sequence == expected_seq
                && vel_reply.accepted
                && vel_reply.ingress_accepted
                && vel_reply.input_window == 2,
            "fresh velocity frame failed: {:?}",
            vel_reply
        );
    }
    ensure!(
        state.velocity_accepted.load(Ordering::SeqCst) >= 5,
        "expected >= 5 accepted velocity frames"
    );

    // Silence after deadman velocity must trigger bridge-side Hold watchdog.
    let holds_before = state.holds.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(450)).await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while state.holds.load(Ordering::SeqCst) <= holds_before {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;

    seq += 1;
    inputs3
        .send(rpc::OperatorInput {
            input: Some(rpc::operator_input::Input::Command(rpc::ControlInput {
                session_id: issued3.session_id,
                sequence: seq,
                action: rpc::Action::Release as i32,
                ..Default::default()
            })),
        })
        .await?;
    let rel3 = tokio::time::timeout(Duration::from_secs(3), replies3.message())
        .await??
        .context("missing third session release reply")?;
    ensure!(
        rel3.sequence == seq && rel3.accepted && rel3.held_by_client == Some(false),
        "third session release failed: {:?}",
        rel3
    );
    drop(inputs3);
    drop(replies3);
    tokio::time::timeout(Duration::from_secs(3), async {
        while state.held.load(Ordering::SeqCst) || state.disconnects.load(Ordering::SeqCst) < 4 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;

    hub.shutdown();
    drop(local);
    fixture.close()?;
    println!(
        "{}",
        serde_json::json!({
            "simulated": true,
            "passed": true,
            "path": "HTTP session -> gRPC relay -> Rust device bridge -> pinned Rust SDK -> SIMULATED Motion",
            "actions": [
                rpc::Action::Claim.as_str_name(),
                rpc::Action::Hold.as_str_name(),
                rpc::Action::Release.as_str_name(),
                rpc::Action::Velocity.as_str_name(),
                rpc::Action::StandUp.as_str_name(),
                rpc::Action::SitDown.as_str_name(),
                rpc::Action::StopMotion.as_str_name(),
                rpc::Action::EmergencyStop.as_str_name(),
                rpc::Action::ClearEmergencyStop.as_str_name(),
                rpc::Action::Resume.as_str_name(),
                rpc::Action::EnableMotors.as_str_name(),
                rpc::Action::DisableMotors.as_str_name(),
                rpc::Action::SwitchProfile.as_str_name(),
            ]
        })
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn control_crosses_the_real_relay_and_sdk() {
        super::run().await.unwrap();
    }
}
