use super::{Hub, serve};
use crate::{Gateway, app_with_control};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use ed25519_dalek::{
    SigningKey,
    pkcs8::{EncodePublicKey, spki::der::pem::LineEnding},
};
use robot_cloud_common::{
    control::{Grant, Proof, rpc},
    decode_signed, encode_signed, now_ms, timestamp,
};
use rumqttc::v5::{AsyncClient, EventLoop, MqttOptions};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    net::TcpListener,
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Code, Streaming, transport::Channel};
use tower::ServiceExt;
use uuid::Uuid;

type Client = rpc::remote_control_client::RemoteControlClient<Channel>;

struct Fixture {
    directory: PathBuf,
    hub: Arc<Hub>,
    device_id: String,
    device_key: SigningKey,
    client: Client,
    stop: watch::Sender<bool>,
    server: JoinHandle<Result<(), tonic::transport::Error>>,
    _mqtt: EventLoop,
}

impl Fixture {
    async fn new() -> Self {
        let directory = std::env::temp_dir().join(format!("doso-control-test-{}", Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let device_key = SigningKey::from_bytes(&[19; 32]);
        fs::write(
            directory.join("device.pub"),
            device_key
                .verifying_key()
                .to_public_key_pem(LineEnding::LF)
                .unwrap(),
        )
        .unwrap();
        let device_id = Uuid::new_v4().to_string();
        fs::write(
            directory.join("devices.json"),
            json!({"devices":[{"deviceId":device_id,"enabled":true,"publicKeyPath":"device.pub"}]})
                .to_string(),
        )
        .unwrap();
        let (mqtt, event_loop) =
            AsyncClient::new(MqttOptions::new("control-test", "127.0.0.1", 1883), 8);
        let gateway = Arc::new(
            Gateway::new(
                mqtt,
                SigningKey::from_bytes(&[21; 32]),
                directory.join("devices.json"),
            )
            .unwrap(),
        );
        let hub = Arc::new(Hub::new(gateway));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (stop, stopped) = watch::channel(false);
        let server = tokio::spawn(serve(hub.clone(), listener, stopped));
        let client = Client::connect(endpoint).await.unwrap();
        Self {
            directory,
            hub,
            device_id,
            device_key,
            client,
            stop,
            server,
            _mqtt: event_loop,
        }
    }

    async fn device(
        &self,
    ) -> (
        mpsc::Sender<rpc::DeviceEvent>,
        Streaming<rpc::DeviceCommand>,
        Vec<u8>,
    ) {
        let (sender, receiver) = mpsc::channel(8);
        let mut request = tonic::Request::new(ReceiverStream::new(receiver));
        request
            .metadata_mut()
            .insert("x-doso-device-id", self.device_id.parse().unwrap());
        let mut stream = self
            .client
            .clone()
            .device_link(request)
            .await
            .unwrap()
            .into_inner();
        let Some(rpc::device_command::Command::Challenge(challenge)) =
            stream.message().await.unwrap().unwrap().command
        else {
            panic!("challenge required");
        };
        let proof = Proof {
            v: 1,
            kind: "control.hello".into(),
            device_id: self.device_id.clone(),
            connection_id: Uuid::new_v4().to_string(),
            challenge,
            issued_at: timestamp(now_ms()),
        };
        let bytes = encode_signed(&proof, &self.device_key).unwrap();
        sender
            .send(rpc::DeviceEvent {
                event: Some(rpc::device_event::Event::Proof(bytes.clone())),
            })
            .await
            .unwrap();
        (sender, stream, bytes)
    }

    async fn issue(&self, operator: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let app = app_with_control(
                    self.hub.gateway.clone(),
                    &"k".repeat(32),
                    Some(self.hub.clone()),
                )
                .unwrap();
                let response = app
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri(format!(
                                "/api/v1/robots/{}/control/sessions",
                                self.device_id
                            ))
                            .header(header::AUTHORIZATION, format!("Bearer {}", "k".repeat(32)))
                            .body(Body::from(
                                json!({"operatorId":operator,"authorizationEpoch":1}).to_string(),
                            ))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                if response.status() == StatusCode::SERVICE_UNAVAILABLE {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    continue;
                }
                assert_eq!(response.status(), StatusCode::CREATED);
                return serde_json::from_slice(
                    &to_bytes(response.into_body(), 8192).await.unwrap(),
                )
                .unwrap();
            }
        })
        .await
        .unwrap()
    }

    async fn operate(
        &self,
        session: &Value,
    ) -> (
        mpsc::Sender<rpc::OperatorInput>,
        Streaming<rpc::ControlReply>,
    ) {
        let (sender, receiver) = mpsc::channel(8);
        sender
            .send(rpc::OperatorInput {
                input: Some(rpc::operator_input::Input::Grant(
                    session["grant"].as_str().unwrap().as_bytes().to_vec(),
                )),
            })
            .await
            .unwrap();
        let mut stream = self
            .client
            .clone()
            .operate(ReceiverStream::new(receiver))
            .await
            .unwrap()
            .into_inner();
        let ready = stream.message().await.unwrap().unwrap();
        assert_eq!(ready.sequence, 0);
        assert_eq!(ready.reason, "session_open");
        assert!(ready.accepted);
        (sender, stream)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.hub.shutdown();
        let _ = self.stop.send(true);
        self.server.abort();
        fs::remove_dir_all(&self.directory).unwrap();
    }
}

async fn send_input(
    sender: &mpsc::Sender<rpc::OperatorInput>,
    session: &Value,
    sequence: u64,
    action: rpc::Action,
) {
    sender
        .send(rpc::OperatorInput {
            input: Some(rpc::operator_input::Input::Command(rpc::ControlInput {
                session_id: session["sessionId"].as_str().unwrap().into(),
                sequence,
                action: action as i32,
            })),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn grpc_claim_release_preserves_device_confirmation() {
    let fixture = Fixture::new().await;
    let (device, mut commands, _) = fixture.device().await;
    let session = fixture.issue(&Uuid::new_v4().to_string()).await;
    let (operator, mut replies) = fixture.operate(&session).await;
    let Some(rpc::device_command::Command::Grant(bytes)) =
        commands.message().await.unwrap().unwrap().command
    else {
        panic!("grant required");
    };
    let grant: Grant =
        decode_signed(&bytes, &fixture.hub.gateway.signing_key.verifying_key()).unwrap();
    assert_eq!(grant.session_id, session["sessionId"]);
    for (sequence, action) in [(1, rpc::Action::Claim), (2, rpc::Action::Release)] {
        send_input(&operator, &session, sequence, action).await;
        let Some(rpc::device_command::Command::Input(input)) =
            commands.message().await.unwrap().unwrap().command
        else {
            panic!("input required");
        };
        assert_eq!(input.action, action as i32);
        assert_eq!(input.sequence, sequence);
        let acknowledgement = rpc::ControlReply {
            session_id: input.session_id,
            sequence,
            accepted: true,
            reason: "confirmed_by_fixture".into(),
            held_by_client: Some(action != rpc::Action::Release),
            final_output_confirmed: true,
            submission_unknown: false,
        };
        device
            .send(rpc::DeviceEvent {
                event: Some(rpc::device_event::Event::Reply(acknowledgement.clone())),
            })
            .await
            .unwrap();
        assert_eq!(replies.message().await.unwrap().unwrap(), acknowledgement);
    }
    assert!(matches!(
        commands.message().await.unwrap().unwrap().command,
        Some(rpc::device_command::Command::CloseSession(_))
    ));
    assert!(
        tokio::time::timeout(Duration::from_secs(1), replies.message())
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn http_revocation_checks_actor_and_closes_both_streams() {
    let fixture = Fixture::new().await;
    let (_device, mut commands, _) = fixture.device().await;
    let actor = Uuid::new_v4().to_string();
    let session = fixture.issue(&actor).await;
    let (_operator, mut replies) = fixture.operate(&session).await;
    commands.message().await.unwrap().unwrap();
    for (operator_id, credential, expected) in [
        (actor.clone(), false, StatusCode::UNAUTHORIZED),
        (Uuid::new_v4().to_string(), true, StatusCode::FORBIDDEN),
        (actor, true, StatusCode::OK),
    ] {
        let app = app_with_control(
            fixture.hub.gateway.clone(),
            &"k".repeat(32),
            Some(fixture.hub.clone()),
        )
        .unwrap();
        let mut request = Request::builder().method("DELETE").uri(format!(
            "/api/v1/robots/{}/control/sessions/{}",
            fixture.device_id,
            session["sessionId"].as_str().unwrap()
        ));
        if credential {
            request = request.header(header::AUTHORIZATION, format!("Bearer {}", "k".repeat(32)));
        }
        let response = app
            .oneshot(
                request
                    .body(Body::from(
                        json!({"operatorId":operator_id,"authorizationEpoch":1}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    assert_eq!(replies.message().await.unwrap_err().code(), Code::Cancelled);
    assert!(matches!(
        commands.message().await.unwrap().unwrap().command,
        Some(rpc::device_command::Command::CloseSession(_))
    ));
    let (sender, receiver) = mpsc::channel(1);
    sender
        .send(rpc::OperatorInput {
            input: Some(rpc::operator_input::Input::Grant(
                session["grant"].as_str().unwrap().as_bytes().to_vec(),
            )),
        })
        .await
        .unwrap();
    assert_eq!(
        fixture
            .client
            .clone()
            .operate(ReceiverStream::new(receiver))
            .await
            .unwrap_err()
            .code(),
        Code::Unauthenticated
    );
}

#[tokio::test]
async fn proof_replay_and_duplicate_connections_are_rejected() {
    let fixture = Fixture::new().await;
    let (_device, _commands, proof) = fixture.device().await;
    fixture.issue(&Uuid::new_v4().to_string()).await;
    let (_duplicate, mut replies, _) = fixture.device().await;
    assert_eq!(
        replies.message().await.unwrap_err().code(),
        Code::AlreadyExists
    );
    let (sender, receiver) = mpsc::channel(1);
    let mut request = tonic::Request::new(ReceiverStream::new(receiver));
    request
        .metadata_mut()
        .insert("x-doso-device-id", fixture.device_id.parse().unwrap());
    let mut replies = fixture
        .client
        .clone()
        .device_link(request)
        .await
        .unwrap()
        .into_inner();
    replies.message().await.unwrap().unwrap();
    sender
        .send(rpc::DeviceEvent {
            event: Some(rpc::device_event::Event::Proof(proof)),
        })
        .await
        .unwrap();
    assert_eq!(
        replies.message().await.unwrap_err().code(),
        Code::Unauthenticated
    );
}

#[tokio::test]
async fn device_disconnect_ends_operator_session() {
    let fixture = Fixture::new().await;
    let (device, mut commands, _) = fixture.device().await;
    let session = fixture.issue(&Uuid::new_v4().to_string()).await;
    let (_operator, mut replies) = fixture.operate(&session).await;
    commands.message().await.unwrap().unwrap();
    drop(device);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), replies.message())
            .await
            .unwrap()
            .unwrap_err()
            .code(),
        Code::Unavailable
    );
}

#[tokio::test]
async fn expiry_closes_device_and_operator_session() {
    let fixture = Fixture::new().await;
    let (_device, mut commands, _) = fixture.device().await;
    let session = fixture.issue(&Uuid::new_v4().to_string()).await;
    let (_operator, mut replies) = fixture.operate(&session).await;
    commands.message().await.unwrap().unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(31)).await;
    tokio::time::resume();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), replies.message())
            .await
            .unwrap()
            .unwrap_err()
            .code(),
        Code::DeadlineExceeded
    );
    assert!(matches!(
        commands.message().await.unwrap().unwrap().command,
        Some(rpc::device_command::Command::CloseSession(_))
    ));
}

#[tokio::test]
async fn pending_ack_rejects_a_second_command_and_operator_disconnect_releases() {
    let fixture = Fixture::new().await;
    let (_device, mut commands, _) = fixture.device().await;
    let session = fixture.issue(&Uuid::new_v4().to_string()).await;
    let (operator, mut replies) = fixture.operate(&session).await;
    commands.message().await.unwrap().unwrap();
    send_input(&operator, &session, 1, rpc::Action::Claim).await;
    assert!(matches!(
        commands.message().await.unwrap().unwrap().command,
        Some(rpc::device_command::Command::Input(_))
    ));
    send_input(&operator, &session, 2, rpc::Action::Hold).await;
    assert_eq!(
        replies.message().await.unwrap_err().code(),
        Code::ResourceExhausted
    );
    assert!(matches!(
        commands.message().await.unwrap().unwrap().command,
        Some(rpc::device_command::Command::CloseSession(_))
    ));
    let session = fixture.issue(&Uuid::new_v4().to_string()).await;
    let (operator, _replies) = fixture.operate(&session).await;
    commands.message().await.unwrap().unwrap();
    drop(operator);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), commands.message())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .command,
        Some(rpc::device_command::Command::CloseSession(_))
    ));
}

#[tokio::test]
async fn duplicate_operator_and_replayed_input_cannot_take_control() {
    let fixture = Fixture::new().await;
    let (device, mut commands, _) = fixture.device().await;
    let session = fixture.issue(&Uuid::new_v4().to_string()).await;
    let (operator, mut replies) = fixture.operate(&session).await;
    commands.message().await.unwrap().unwrap();
    let (duplicate, receiver) = mpsc::channel(1);
    duplicate
        .send(rpc::OperatorInput {
            input: Some(rpc::operator_input::Input::Grant(
                session["grant"].as_str().unwrap().as_bytes().to_vec(),
            )),
        })
        .await
        .unwrap();
    assert_eq!(
        fixture
            .client
            .clone()
            .operate(ReceiverStream::new(receiver))
            .await
            .unwrap_err()
            .code(),
        Code::AlreadyExists
    );
    send_input(&operator, &session, 1, rpc::Action::Claim).await;
    let Some(rpc::device_command::Command::Input(input)) =
        commands.message().await.unwrap().unwrap().command
    else {
        panic!("input required");
    };
    device
        .send(rpc::DeviceEvent {
            event: Some(rpc::device_event::Event::Reply(rpc::ControlReply {
                session_id: input.session_id,
                sequence: 1,
                accepted: true,
                ..Default::default()
            })),
        })
        .await
        .unwrap();
    assert!(replies.message().await.unwrap().unwrap().accepted);
    send_input(&operator, &session, 1, rpc::Action::Claim).await;
    assert_eq!(
        replies.message().await.unwrap_err().code(),
        Code::InvalidArgument
    );
    assert!(matches!(
        commands.message().await.unwrap().unwrap().command,
        Some(rpc::device_command::Command::CloseSession(_))
    ));
}

#[tokio::test]
async fn missing_ack_and_registry_disable_end_control() {
    let fixture = Fixture::new().await;
    let (_device, mut commands, _) = fixture.device().await;
    let session = fixture.issue(&Uuid::new_v4().to_string()).await;
    let (operator, mut replies) = fixture.operate(&session).await;
    commands.message().await.unwrap().unwrap();
    send_input(&operator, &session, 1, rpc::Action::Claim).await;
    commands.message().await.unwrap().unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(4)).await;
    tokio::time::resume();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), replies.message())
            .await
            .unwrap()
            .unwrap_err()
            .code(),
        Code::DeadlineExceeded
    );
    assert!(matches!(
        commands.message().await.unwrap().unwrap().command,
        Some(rpc::device_command::Command::CloseSession(_))
    ));
    let session = fixture.issue(&Uuid::new_v4().to_string()).await;
    let (_operator, mut replies) = fixture.operate(&session).await;
    commands.message().await.unwrap().unwrap();
    fs::write(fixture.directory.join("devices.json"), json!({"devices":[{"deviceId":fixture.device_id,"enabled":false,"publicKeyPath":"device.pub"}]}).to_string()).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), replies.message())
            .await
            .unwrap()
            .unwrap_err()
            .code(),
        Code::Unavailable
    );
    assert!(matches!(
        commands.message().await.unwrap().unwrap().command,
        Some(rpc::device_command::Command::CloseSession(_))
    ));
}

#[tokio::test]
async fn late_ack_does_not_affect_the_replacement_session() {
    let fixture = Fixture::new().await;
    let (device, mut commands, _) = fixture.device().await;
    let actor = Uuid::new_v4().to_string();
    let old = fixture.issue(&actor).await;
    let (old_operator, mut old_replies) = fixture.operate(&old).await;
    commands.message().await.unwrap().unwrap();
    send_input(&old_operator, &old, 1, rpc::Action::Claim).await;
    commands.message().await.unwrap().unwrap();
    fixture
        .hub
        .revoke(
            &fixture.device_id,
            old["sessionId"].as_str().unwrap(),
            &actor,
            1,
        )
        .unwrap();
    assert_eq!(
        old_replies.message().await.unwrap_err().code(),
        Code::Cancelled
    );
    commands.message().await.unwrap().unwrap();
    let current = fixture.issue(&actor).await;
    let (operator, mut replies) = fixture.operate(&current).await;
    commands.message().await.unwrap().unwrap();
    device
        .send(rpc::DeviceEvent {
            event: Some(rpc::device_event::Event::Reply(rpc::ControlReply {
                session_id: old["sessionId"].as_str().unwrap().into(),
                sequence: 1,
                accepted: true,
                ..Default::default()
            })),
        })
        .await
        .unwrap();
    for (sequence, action) in [(1, rpc::Action::Claim), (2, rpc::Action::Release)] {
        send_input(&operator, &current, sequence, action).await;
        let Some(rpc::device_command::Command::Input(input)) =
            commands.message().await.unwrap().unwrap().command
        else {
            panic!("input required");
        };
        let acknowledgement = rpc::ControlReply {
            session_id: input.session_id,
            sequence,
            accepted: true,
            ..Default::default()
        };
        device
            .send(rpc::DeviceEvent {
                event: Some(rpc::device_event::Event::Reply(acknowledgement.clone())),
            })
            .await
            .unwrap();
        assert_eq!(replies.message().await.unwrap().unwrap(), acknowledgement);
        if sequence == 1 {
            device
                .send(rpc::DeviceEvent {
                    event: Some(rpc::device_event::Event::Reply(acknowledgement)),
                })
                .await
                .unwrap();
        }
    }
}

#[tokio::test]
async fn server_shutdown_closes_live_control_streams() {
    let mut fixture = Fixture::new().await;
    let (_device, mut commands, _) = fixture.device().await;
    let session = fixture.issue(&Uuid::new_v4().to_string()).await;
    let (_operator, mut replies) = fixture.operate(&session).await;
    commands.message().await.unwrap().unwrap();
    fixture.stop.send(true).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), replies.message())
            .await
            .unwrap()
            .unwrap_err()
            .code(),
        Code::Unavailable
    );
    assert!(matches!(
        commands.message().await.unwrap().unwrap().command,
        Some(rpc::device_command::Command::CloseSession(_))
    ));
    tokio::time::timeout(Duration::from_secs(2), &mut fixture.server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        fixture
            .hub
            .create_session(&fixture.device_id, &Uuid::new_v4().to_string(), 1)
            .err()
            .unwrap()
            .code(),
        Code::Unavailable
    );
}

#[tokio::test]
async fn unattached_revoke_and_expiry_preserve_the_device_connection() {
    let fixture = Fixture::new().await;
    let (device, mut commands, _) = fixture.device().await;
    let actor = Uuid::new_v4().to_string();
    for expired in [false, true] {
        let unused = fixture.issue(&actor).await;
        if expired {
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(31)).await;
            tokio::time::resume();
        } else {
            fixture
                .hub
                .revoke(
                    &fixture.device_id,
                    unused["sessionId"].as_str().unwrap(),
                    &actor,
                    1,
                )
                .unwrap();
        }
        let session = fixture.issue(&actor).await;
        let (operator, mut replies) = fixture.operate(&session).await;
        let Some(rpc::device_command::Command::Grant(bytes)) =
            commands.message().await.unwrap().unwrap().command
        else {
            panic!("an unattached session must not send a close command");
        };
        let grant: Grant =
            decode_signed(&bytes, &fixture.hub.gateway.signing_key.verifying_key()).unwrap();
        assert_eq!(grant.session_id, session["sessionId"]);
        send_input(&operator, &session, 1, rpc::Action::Release).await;
        let Some(rpc::device_command::Command::Input(input)) =
            commands.message().await.unwrap().unwrap().command
        else {
            panic!("release required");
        };
        device
            .send(rpc::DeviceEvent {
                event: Some(rpc::device_event::Event::Reply(rpc::ControlReply {
                    session_id: input.session_id,
                    sequence: input.sequence,
                    accepted: true,
                    held_by_client: Some(false),
                    final_output_confirmed: true,
                    ..Default::default()
                })),
            })
            .await
            .unwrap();
        assert!(
            replies
                .message()
                .await
                .unwrap()
                .unwrap()
                .final_output_confirmed
        );
        assert!(matches!(
            commands.message().await.unwrap().unwrap().command,
            Some(rpc::device_command::Command::CloseSession(_))
        ));
        assert!(replies.message().await.unwrap().is_none());
    }
}

#[tokio::test]
async fn replacing_the_registered_key_closes_the_authenticated_connection() {
    let fixture = Fixture::new().await;
    let (_device, mut commands, _) = fixture.device().await;
    let session = fixture.issue(&Uuid::new_v4().to_string()).await;
    let (_operator, mut replies) = fixture.operate(&session).await;
    commands.message().await.unwrap().unwrap();
    let replacement = SigningKey::from_bytes(&[22; 32]);
    fs::write(
        fixture.directory.join("replacement.pub"),
        replacement
            .verifying_key()
            .to_public_key_pem(LineEnding::LF)
            .unwrap(),
    )
    .unwrap();
    fs::rename(
        fixture.directory.join("replacement.pub"),
        fixture.directory.join("device.pub"),
    )
    .unwrap();
    assert_eq!(
        fixture.hub.gateway.device_key(&fixture.device_id).unwrap(),
        replacement.verifying_key()
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), replies.message())
            .await
            .unwrap()
            .unwrap_err()
            .code(),
        Code::Unavailable,
    );
    assert!(matches!(
        commands.message().await.unwrap().unwrap().command,
        Some(rpc::device_command::Command::CloseSession(_)),
    ));
    assert!(
        tokio::time::timeout(Duration::from_secs(1), commands.message())
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fixture
            .hub
            .create_session(&fixture.device_id, &Uuid::new_v4().to_string(), 1)
            .err()
            .unwrap()
            .code(),
        Code::Unavailable,
    );
}
