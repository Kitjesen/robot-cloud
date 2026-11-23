use super::{control_check, control_fixture, fixture::Fixture};
use anyhow::{Result, ensure};
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
        rpc::remote_control_client::RemoteControlClient::connect(cloud_url).await?;
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
    hub.shutdown();
    drop(local);
    fixture.close()?;
    println!(
        "{}",
        serde_json::json!({"simulated":true,"passed":true,
        "path":"HTTP session -> gRPC relay -> Rust device bridge -> pinned Rust SDK -> SIMULATED Motion",
        "actions":[rpc::Action::Claim.as_str_name(),rpc::Action::Hold.as_str_name(),rpc::Action::Release.as_str_name()]})
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
