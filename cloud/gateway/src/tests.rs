use super::*;
use axum::{
    body::Body,
    extract::Request,
    http::{StatusCode, header},
};
use ed25519_dalek::pkcs8::{EncodePublicKey, spki::der::pem::LineEnding};
use robot_cloud_common::reporting::{Ack, Hello, Permission, Report};
use robot_cloud_common::{Control, State as RobotState};
use rumqttc::v5::MqttOptions;
use serde_json::json;
use std::{
    future::Future,
    task::{Context, Waker},
};
use tower::ServiceExt;

#[tokio::test]
async fn video_requires_a_live_registered_device_and_no_commands() {
    let fixture = Fixture::new();
    assert!(matches!(
        fixture.gateway.set_video(&fixture.device_id, true),
        Err(Error::Unavailable)
    ));
    fixture.hello(&Uuid::new_v4().to_string(), now_ms());
    let accepted = fixture.gateway.set_video(&fixture.device_id, true).unwrap();
    let value = serde_json::to_value(accepted).unwrap();
    assert_eq!(value["accepted"], true);
    assert_eq!(value["enabled"], true);
    fixture
        .gateway
        .set_video(&fixture.device_id, false)
        .unwrap();
    fixture.set_enabled(false);
    assert!(matches!(
        fixture.gateway.set_video(&fixture.device_id, true),
        Err(Error::Disabled)
    ));
    fixture.set_enabled(true);
    let app = app(fixture.gateway.clone(), &"k".repeat(32)).unwrap();
    let request = Request::builder()
        .method("PUT")
        .uri(format!("/api/v1/robots/{}/video", fixture.device_id))
        .header(header::AUTHORIZATION, format!("Bearer {}", "k".repeat(32)))
        .body(Body::from(r#"{"enabled":true,"command":"anything"}"#))
        .unwrap();
    assert_eq!(
        app.oneshot(request).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
}

#[test]
fn registry_rejects_public_key_paths_outside_its_directory() {
    let fixture = Fixture::new();
    for public_key_path in [
        "../device.pub",
        fixture.directory.join("device.pub").to_str().unwrap(),
    ] {
        fs::write(
            fixture.directory.join("devices.json"),
            json!({ "devices": [{
                "deviceId": fixture.device_id,
                "enabled": true,
                "publicKeyPath": public_key_path
            }] })
            .to_string(),
        )
        .unwrap();
        assert!(matches!(
            fixture.gateway.device_key(&fixture.device_id),
            Err(Error::Unavailable)
        ));
    }
}

struct Fixture {
    directory: PathBuf,
    gateway: Arc<Gateway>,
    device_key: SigningKey,
    device_id: String,
    _mqtt: EventLoop,
}

impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!("doso-rust-gateway-{}", Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let device_key = SigningKey::from_bytes(&[2; 32]);
        fs::write(
            directory.join("device.pub"),
            device_key
                .verifying_key()
                .to_public_key_pem(LineEnding::LF)
                .unwrap(),
        )
        .unwrap();
        let device_id = Uuid::new_v4().to_string();
        let (client, mqtt) = AsyncClient::new(MqttOptions::new("test", "127.0.0.1", 1883), 32);
        let gateway = Arc::new(
            Gateway::new(
                client,
                SigningKey::from_bytes(&[1; 32]),
                directory.join("devices.json"),
            )
            .unwrap(),
        );
        let fixture = Self {
            directory,
            gateway,
            device_key,
            device_id,
            _mqtt: mqtt,
        };
        fixture.set_enabled(true);
        fixture
            .gateway
            .session
            .lock()
            .unwrap()
            .get_or_insert_default();
        fixture
    }

    fn queued(&self) -> usize {
        self.gateway
            .session
            .lock()
            .unwrap()
            .as_ref()
            .map_or(0, HashMap::len)
    }

    fn set_enabled(&self, enabled: bool) {
        fs::write(
            self.directory.join("devices.json"),
            json!({ "devices": [{
            "deviceId": self.device_id, "enabled": enabled, "publicKeyPath": "device.pub"
        }] })
            .to_string(),
        )
        .unwrap();
    }

    fn pending(&self) -> (ReadResult, oneshot::Receiver<Result<Snapshot, Error>>) {
        let request_id = Uuid::new_v4().to_string();
        let topic = topics(&self.device_id).unwrap();
        let (sender, receiver) = oneshot::channel();
        let permission = self
            .gateway
            .reporting
            .lock()
            .unwrap()
            .check_read(&self.device_id)
            .unwrap();
        self.gateway
            .session
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .insert(
                request_id.clone(),
                Pending {
                    device_id: self.device_id.clone(),
                    permission,
                    reply_topic: topic.replies,
                    public_key: self.device_key.verifying_key(),
                    expires_at: now_ms() + 8000,
                    result: sender,
                },
            );
        (
            ReadResult {
                v: 1,
                kind: "status.result".into(),
                device_id: self.device_id.clone(),
                request_id,
                observed_at: timestamp(now_ms()),
                status: Some(Status {
                    state: RobotState {
                        kind: "CMS_STATE_KIND_GROUNDED".into(),
                        transition: "CMS_TRANSITION_KIND_NONE".into(),
                    },
                    control: Control {
                        owner: "CONTROL_OWNER_NONE".into(),
                        motor_output_enabled: false,
                        critical_motor_fault: false,
                    },
                    transport: "local-insecure".into(),
                    location: None,
                }),
                error: None,
            },
            receiver,
        )
    }

    fn hello(&self, connection: &str, observed: i64) {
        let hello = Hello {
            v: 1,
            kind: "reporting.hello".into(),
            device_id: self.device_id.clone(),
            connection_id: connection.into(),
            observed_at: timestamp(observed),
        };
        self.gateway.accept_reporting(
            &topics(&self.device_id).unwrap().hello,
            &encode_signed(&hello, &self.device_key).unwrap(),
            false,
        );
    }

    fn grant(&self) -> Permission {
        let connection = Uuid::new_v4().to_string();
        self.hello(&connection, now_ms());
        Permission {
            authorization_epoch: 1,
            consent_version: 1,
            enabled: true,
            connection_id: Some(connection),
            reporting_session_id: Some(Uuid::new_v4().to_string()),
            expires_at: timestamp(now_ms() + 30_000),
        }
    }

    fn ack(&self, permission: &Permission) {
        let ack = Ack {
            v: 1,
            kind: "reporting.ack".into(),
            device_id: self.device_id.clone(),
            permission: permission.clone(),
            observed_at: timestamp(now_ms()),
        };
        self.gateway.accept_reporting(
            &topics(&self.device_id).unwrap().acks,
            &encode_signed(&ack, &self.device_key).unwrap(),
            false,
        );
    }

    fn report(&self, permission: &Permission, seq: u64, error: bool, observed: i64) {
        let report = Report {
            v: 1,
            kind: "status.report".into(),
            device_id: self.device_id.clone(),
            connection_id: permission.connection_id.clone().unwrap(),
            reporting_session_id: permission.reporting_session_id.clone().unwrap(),
            authorization_epoch: permission.authorization_epoch,
            consent_version: permission.consent_version,
            seq,
            observed_at: timestamp(observed),
            status: (!error).then(|| Status {
                state: RobotState {
                    kind: "CMS_STATE_KIND_GROUNDED".into(),
                    transition: "CMS_TRANSITION_KIND_NONE".into(),
                },
                control: Control {
                    owner: "CONTROL_OWNER_NONE".into(),
                    motor_output_enabled: false,
                    critical_motor_fault: false,
                },
                transport: "local-insecure".into(),
                location: None,
            }),
            error: error.then(|| "upstream_unavailable".into()),
        };
        self.gateway.accept_reporting(
            &topics(&self.device_id).unwrap().reports,
            &encode_signed(&report, &self.device_key).unwrap(),
            false,
        );
    }

    fn latest(&self) -> serde_json::Value {
        serde_json::to_value(self.gateway.latest(&self.device_id).unwrap()).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.directory.join("devices.json"));
        let _ = fs::remove_file(self.directory.join("device.pub"));
        let _ = fs::remove_file(self.directory.join("reporting.json"));
        let _ = fs::remove_dir(&self.directory);
    }
}

#[tokio::test]
async fn reply_permission() {
    let fixture = Fixture::new();
    let permission = fixture.grant();
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission.clone())
        .unwrap();
    fixture.ack(&permission);
    let (reply, receiver) = fixture.pending();
    let bytes = encode_signed(&reply, &fixture.device_key).unwrap();
    let topic = topics(&fixture.device_id).unwrap().replies;
    fixture.gateway.accept_reply(&topic, &bytes, false);
    assert_eq!(
        receiver.await.unwrap().unwrap().device_id,
        fixture.device_id
    );
    assert!(fixture.queued() == 0);
    let (reply, receiver) = fixture.pending();
    fixture.set_enabled(false);
    fixture.gateway.accept_reply(
        &topic,
        &encode_signed(&reply, &fixture.device_key).unwrap(),
        false,
    );
    assert!(matches!(receiver.await.unwrap(), Err(Error::Disabled)));
}

#[tokio::test]
async fn invalid_reply_and_disconnect() {
    let fixture = Fixture::new();
    let permission = fixture.grant();
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission.clone())
        .unwrap();
    fixture.ack(&permission);
    let (reply, receiver) = fixture.pending();
    let bytes = encode_signed(&reply, &SigningKey::from_bytes(&[3; 32])).unwrap();
    fixture
        .gateway
        .accept_reply(&topics(&fixture.device_id).unwrap().replies, &bytes, false);
    assert_eq!(fixture.queued(), 1);
    fixture.gateway.disconnected();
    assert!(receiver.await.is_err());
    assert!(fixture.gateway.session.lock().unwrap().is_none());
}

#[tokio::test]
async fn http_auth_and_body() {
    let fixture = Fixture::new();
    let key = "test-backend-service-key-32-characters";
    let router = app(fixture.gateway.clone(), key).unwrap();
    let uri = format!("/api/v1/robots/{}/status", fixture.device_id);
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&uri)
                .header(header::AUTHORIZATION, format!("Bearer {key}"))
                .body(Body::from("unexpected"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(fixture.queued() == 0);
}

#[tokio::test]
async fn device_signature_is_not_a_gateway_service_credential() {
    let fixture = Fixture::new();
    let permission = fixture.grant();
    let key = "test-backend-service-key-32-characters";
    let router = app(fixture.gateway.clone(), key).unwrap();
    let device_token =
        String::from_utf8(encode_signed(&permission, &fixture.device_key).unwrap()).unwrap();
    for (method, route, body) in [
        (
            "PUT",
            "reporting",
            serde_json::to_string(&permission).unwrap(),
        ),
        ("PUT", "video", r#"{"enabled":true}"#.into()),
        ("GET", "status/latest", String::new()),
        ("POST", "status", String::new()),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(format!("/api/v1/robots/{}/{route}", fixture.device_id))
                    .header(header::AUTHORIZATION, format!("Bearer {device_token}"))
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    assert!(fixture.latest()["permission"].is_null());
    assert!(!fixture.directory.join("reporting.json").exists());
    let response = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/v1/robots/{}/reporting", fixture.device_id))
                .header(header::AUTHORIZATION, format!("Bearer {key}"))
                .body(Body::from(serde_json::to_vec(&permission).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn device_messages_cannot_enable_a_cloud_disabled_permission() {
    let fixture = Fixture::new();
    let mut disabled = fixture.grant();
    disabled.enabled = false;
    fixture
        .gateway
        .set_reporting(&fixture.device_id, disabled.clone())
        .unwrap();
    fixture.ack(&disabled);
    let before = fixture.latest();
    let saved = fs::read(fixture.directory.join("reporting.json")).unwrap();
    let mut proposed = disabled;
    proposed.enabled = true;
    proposed.consent_version += 1;
    let control = robot_cloud_common::reporting::Control {
        v: 1,
        kind: "reporting.control".into(),
        device_id: fixture.device_id.clone(),
        permission: proposed.clone(),
    };
    fixture.gateway.accept_reporting(
        &topics(&fixture.device_id).unwrap().control,
        &encode_signed(&control, &fixture.device_key).unwrap(),
        false,
    );
    fixture.ack(&proposed);
    fixture.report(&proposed, 1, false, now_ms());
    assert_eq!(fixture.latest(), before);
    assert_eq!(
        fs::read(fixture.directory.join("reporting.json")).unwrap(),
        saved
    );

    fixture
        .gateway
        .set_reporting(&fixture.device_id, proposed.clone())
        .unwrap();
    fixture.ack(&proposed);
    fixture.report(&proposed, 1, false, now_ms());
    assert_eq!(fixture.latest()["fresh"], true);
}

#[tokio::test]
async fn service_credential_does_not_expose_cloud_motion_routes() {
    let fixture = Fixture::new();
    let key = "test-backend-service-key-32-characters";
    let router = app(fixture.gateway.clone(), key).unwrap();
    for (method, route) in [
        ("POST", "motion"),
        ("POST", "teleop"),
        ("PUT", "motors/enable"),
        ("POST", "grpc"),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(format!("/api/v1/robots/{}/{route}", fixture.device_id))
                    .header(header::AUTHORIZATION, format!("Bearer {key}"))
                    .body(Body::from(
                        r#"{"method":"/brainstem.api.v1.RobotControl/EnableMotors"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    assert!(fixture.queued() == 0);
    assert!(fixture.latest()["permission"].is_null());
    assert!(!fixture.directory.join("reporting.json").exists());
}

#[tokio::test]
async fn query_cleanup() {
    let fixture = Fixture::new();
    let permission = fixture.grant();
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission.clone())
        .unwrap();
    fixture.ack(&permission);
    let mut context = Context::from_waker(Waker::noop());
    let mut query = Box::pin(fixture.gateway.read_status(&fixture.device_id));
    assert!(query.as_mut().poll(&mut context).is_pending());
    assert_eq!(fixture.queued(), 1);
    drop(query);
    assert!(fixture.queued() == 0);

    let mut query = Box::pin(fixture.gateway.read_status(&fixture.device_id));
    assert!(query.as_mut().poll(&mut context).is_pending());
    fixture.gateway.disconnected();
    assert!(matches!(query.await, Err(Error::Unavailable)));
}

#[tokio::test]
async fn query_requires_confirmed_permission() {
    let fixture = Fixture::new();
    assert!(matches!(
        fixture.gateway.read_status(&fixture.device_id).await,
        Err(Error::Disabled)
    ));
    let permission = fixture.grant();
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission.clone())
        .unwrap();
    assert!(matches!(
        fixture.gateway.read_status(&fixture.device_id).await,
        Err(Error::Disabled)
    ));
    fixture.ack(&permission);
    let mut context = Context::from_waker(Waker::noop());
    let mut query = Box::pin(fixture.gateway.read_status(&fixture.device_id));
    assert!(query.as_mut().poll(&mut context).is_pending());
    assert_eq!(fixture.queued(), 1);
    drop(query);
    assert_eq!(fixture.queued(), 0);
}

#[tokio::test]
async fn expired_permission_rejects_an_inflight_reply() {
    let fixture = Fixture::new();
    let mut permission = fixture.grant();
    permission.expires_at = timestamp(now_ms() + 2_000);
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission.clone())
        .unwrap();
    fixture.ack(&permission);
    let (reply, receiver) = fixture.pending();
    tokio::time::sleep(Duration::from_millis(2_100)).await;
    fixture.gateway.accept_reply(
        &topics(&fixture.device_id).unwrap().replies,
        &encode_signed(&reply, &fixture.device_key).unwrap(),
        false,
    );
    assert!(matches!(receiver.await.unwrap(), Err(Error::Disabled)));
    assert!(matches!(
        fixture.gateway.read_status(&fixture.device_id).await,
        Err(Error::Disabled)
    ));
}

#[tokio::test]
async fn ownership_change_cancels_previous_query() {
    let fixture = Fixture::new();
    let mut permission = fixture.grant();
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission.clone())
        .unwrap();
    fixture.ack(&permission);
    let (_, receiver) = fixture.pending();
    permission.authorization_epoch = 2;
    permission.reporting_session_id = Some(Uuid::new_v4().to_string());
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission.clone())
        .unwrap();
    fixture.ack(&permission);
    assert!(matches!(receiver.await.unwrap(), Err(Error::Disabled)));
    assert_eq!(fixture.queued(), 0);
}

#[tokio::test]
async fn reporting_versions_sequence_and_read_error() {
    let fixture = Fixture::new();
    let permission = fixture.grant();
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission.clone())
        .unwrap();
    fixture.ack(&permission);
    let observed = now_ms() - 1000;
    fixture.report(&permission, 1, false, observed);
    assert_eq!(fixture.latest()["fresh"], true);
    let mut different_session = permission.clone();
    different_session.reporting_session_id = Some(Uuid::new_v4().to_string());
    assert!(matches!(
        fixture
            .gateway
            .set_reporting(&fixture.device_id, different_session),
        Err(Error::StalePermission)
    ));
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission.clone())
        .unwrap();
    fixture.ack(&permission);
    fixture.report(&permission, 1, true, now_ms());
    assert_eq!(fixture.latest()["latest"]["report"]["seq"], 1);
    assert!(fixture.latest()["latest"]["report"]["error"].is_null());
    fixture.report(&permission, 2, true, now_ms());
    let latest = fixture.latest();
    assert_eq!(latest["fresh"], false);
    assert_eq!(latest["latest"]["report"]["error"], "upstream_unavailable");
    assert_eq!(
        latest["lastSuccess"]["report"]["observedAt"],
        timestamp(observed)
    );
    let mut conflict = permission.clone();
    conflict.enabled = false;
    assert!(matches!(
        fixture.gateway.set_reporting(&fixture.device_id, conflict),
        Err(Error::StalePermission)
    ));
    fixture.gateway.disconnected();
    assert_eq!(fixture.latest()["fresh"], false);
    assert_eq!(fixture.latest()["reachable"], false);
}

#[tokio::test]
async fn signed_location_is_preserved_without_carrying_old_fix() {
    let fixture = Fixture::new();
    let permission = fixture.grant();
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission.clone())
        .unwrap();
    fixture.ack(&permission);
    let observed = now_ms();
    let location = serde_json::json!({"lat":31.2304,"lon":121.4737,"fixType":1,
        "observedAt":timestamp(observed - 1234),"accuracyM":null});
    for (seq, fix) in [(1, Some(location.clone())), (2, None)] {
        let mut status = serde_json::json!({"state":{"kind":"CMS_STATE_KIND_GROUNDED",
            "transition":"CMS_TRANSITION_KIND_NONE"},"control":{"owner":"CONTROL_OWNER_NONE",
            "motorOutputEnabled":false,"criticalMotorFault":false},"transport":"local-insecure"});
        if let Some(fix) = fix {
            status["location"] = fix;
        }
        let report = Report {
            v: 1,
            kind: "status.report".into(),
            device_id: fixture.device_id.clone(),
            connection_id: permission.connection_id.clone().unwrap(),
            reporting_session_id: permission.reporting_session_id.clone().unwrap(),
            authorization_epoch: permission.authorization_epoch,
            consent_version: permission.consent_version,
            seq,
            observed_at: timestamp(observed),
            status: Some(serde_json::from_value(status).unwrap()),
            error: None,
        };
        fixture.gateway.accept_reporting(
            &topics(&fixture.device_id).unwrap().reports,
            &encode_signed(&report, &fixture.device_key).unwrap(),
            false,
        );
        let saved = fixture.latest();
        let saved = &saved["latest"]["report"]["status"];
        if seq == 1 {
            assert_eq!(saved["location"], location);
        } else {
            assert!(saved.get("location").is_none());
        }
    }
}

#[tokio::test]
async fn disabled_fence_survives_restart_and_cancels_queries() {
    let fixture = Fixture::new();
    let mut permission = fixture.grant();
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission.clone())
        .unwrap();
    fixture.ack(&permission);
    let (_, query) = fixture.pending();
    permission.enabled = false;
    permission.consent_version = 2;
    permission.connection_id = None;
    permission.reporting_session_id = None;
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission.clone())
        .unwrap();
    assert!(matches!(query.await.unwrap(), Err(Error::Disabled)));
    assert!(matches!(
        fixture.gateway.read_status(&fixture.device_id).await,
        Err(Error::Disabled)
    ));
    let mut stale = permission.clone();
    stale.consent_version = 1;
    assert!(matches!(
        fixture.gateway.set_reporting(&fixture.device_id, stale),
        Err(Error::StalePermission)
    ));
    let (client, _mqtt) = AsyncClient::new(MqttOptions::new("restart", "127.0.0.1", 1883), 32);
    let restarted = Gateway::new(
        client,
        SigningKey::from_bytes(&[1; 32]),
        fixture.directory.join("devices.json"),
    )
    .unwrap();
    let state = serde_json::to_value(restarted.latest(&fixture.device_id).unwrap()).unwrap();
    assert_eq!(state["permission"]["desiredEnabled"], false);
    assert_eq!(state["permission"]["deviceConfirmed"], false);
    assert!(state["permission"]["expiresAt"].is_null());
    assert!(matches!(
        restarted.set_reporting(
            &fixture.device_id,
            Permission {
                enabled: true,
                ..permission
            }
        ),
        Err(Error::InvalidPermission)
    ));
}

#[tokio::test]
async fn reconnect_rejects_old_session_and_ack() {
    let fixture = Fixture::new();
    let mut permission = fixture.grant();
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission.clone())
        .unwrap();
    fixture.ack(&permission);
    fixture.report(&permission, 1, false, now_ms());
    let new_connection = Uuid::new_v4().to_string();
    fixture.hello(&new_connection, now_ms() + 1);
    fixture.ack(&permission);
    fixture.report(&permission, 99, false, now_ms());
    assert_eq!(fixture.latest()["latest"]["report"]["seq"], 1);
    assert_eq!(fixture.latest()["fresh"], false);
    permission.connection_id = Some(new_connection);
    assert!(matches!(
        fixture
            .gateway
            .set_reporting(&fixture.device_id, permission.clone()),
        Err(Error::StalePermission)
    ));
    permission.reporting_session_id = Some(Uuid::new_v4().to_string());
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission.clone())
        .unwrap();
    fixture.ack(&permission);
    assert_eq!(fixture.latest()["fresh"], false);
    fixture.report(&permission, 1, false, now_ms());
    assert_eq!(fixture.latest()["fresh"], true);
    permission.authorization_epoch = 2;
    assert!(matches!(
        fixture
            .gateway
            .set_reporting(&fixture.device_id, permission.clone()),
        Err(Error::StalePermission)
    ));
    permission.reporting_session_id = Some(Uuid::new_v4().to_string());
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission)
        .unwrap();
    assert!(fixture.latest()["latest"].is_null());
    assert!(fixture.latest()["lastSuccess"].is_null());
}

#[tokio::test]
async fn expired_session_needs_new_identifier() {
    let fixture = Fixture::new();
    let mut permission = fixture.grant();
    permission.expires_at = timestamp(now_ms() + 1000);
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission.clone())
        .unwrap();
    fixture.ack(&permission);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(fixture.latest()["permission"]["deviceConfirmed"], false);
    permission.expires_at = timestamp(now_ms() + 30_000);
    assert!(matches!(
        fixture
            .gateway
            .set_reporting(&fixture.device_id, permission.clone()),
        Err(Error::StalePermission)
    ));
    permission.reporting_session_id = Some(Uuid::new_v4().to_string());
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission)
        .unwrap();
}

#[tokio::test]
async fn failed_fence_write_does_not_accept_permission() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.directory.join("reporting.json")).unwrap();
    let mut permission = fixture.grant();
    permission.enabled = false;
    assert!(matches!(
        fixture
            .gateway
            .set_reporting(&fixture.device_id, permission),
        Err(Error::Unavailable)
    ));
    assert!(fixture.latest()["permission"].is_null());
    fs::remove_dir(fixture.directory.join("reporting.json")).unwrap();
}

#[tokio::test]
async fn full_mqtt_queue_allows_permission_retry() {
    let mut fixture = Fixture::new();
    let mut permission = fixture.grant();
    permission.expires_at = timestamp(now_ms() + 20_000);
    for _ in 0..32 {
        fixture
            .gateway
            .client
            .try_publish("test", QoS::AtLeastOnce, false, vec![0])
            .unwrap();
    }
    assert!(matches!(
        fixture
            .gateway
            .set_reporting(&fixture.device_id, permission.clone()),
        Err(Error::Unavailable)
    ));
    fixture._mqtt.clean();
    fixture._mqtt.pending.clear();
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission.clone())
        .unwrap();
    fixture.ack(&permission);
    fixture.report(&permission, 1, false, now_ms());
    permission.expires_at = timestamp(now_ms() + 30_000);
    for _ in 0..31 {
        fixture
            .gateway
            .client
            .try_publish("test", QoS::AtLeastOnce, false, vec![0])
            .unwrap();
    }
    assert!(matches!(
        fixture
            .gateway
            .set_reporting(&fixture.device_id, permission.clone()),
        Err(Error::Unavailable)
    ));
    assert_eq!(fixture.latest()["fresh"], true);
    fixture._mqtt.clean();
    fixture._mqtt.pending.clear();
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission.clone())
        .unwrap();
    fixture.ack(&permission);
    fixture.report(&permission, 1, true, now_ms());
    assert!(fixture.latest()["latest"]["report"]["error"].is_null());
}

#[tokio::test]
async fn confirmed_identical_permission_keeps_ack_and_does_not_publish() {
    let fixture = Fixture::new();
    let permission = fixture.grant();
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission.clone())
        .unwrap();
    fixture.ack(&permission);
    for _ in 0..31 {
        fixture
            .gateway
            .client
            .try_publish("test", QoS::AtLeastOnce, false, vec![0])
            .unwrap();
    }
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission)
        .unwrap();
    assert_eq!(fixture.latest()["permission"]["deviceConfirmed"], true);
    assert!(
        fixture
            .gateway
            .client
            .try_publish("test", QoS::AtLeastOnce, false, vec![0])
            .is_err()
    );
}

#[tokio::test]
async fn confirmed_stop_normalizes_current_connection() {
    let fixture = Fixture::new();
    let mut permission = fixture.grant();
    let connection = permission.connection_id.take();
    permission.enabled = false;
    permission.reporting_session_id = None;
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission.clone())
        .unwrap();
    let mut bound = permission.clone();
    bound.connection_id = connection;
    fixture.ack(&bound);
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission)
        .unwrap();
    assert_eq!(fixture.latest()["permission"]["deviceConfirmed"], true);
}

#[tokio::test]
async fn expired_sync_grant_does_not_block_another_stop() {
    let fixture = Fixture::new();
    let other = Uuid::new_v4().to_string();
    fs::write(
        &fixture.gateway.registry_path,
        serde_json::to_vec(&json!({"devices":[
            {"deviceId":fixture.device_id,"enabled":true,"publicKeyPath":"device.pub"},
            {"deviceId":other,"enabled":true,"publicKeyPath":"device.pub"}
        ]}))
        .unwrap(),
    )
    .unwrap();
    let mut expired = Permission {
        authorization_epoch: 1,
        consent_version: 1,
        enabled: false,
        connection_id: None,
        reporting_session_id: None,
        expires_at: timestamp(now_ms() - 1),
    };
    let stale = expired.clone();
    expired.expires_at = timestamp(now_ms() + 20_000);
    let body = json!({"v":1,"permissions":[
        {"deviceId":fixture.device_id,"permission":stale},
        {"deviceId":other,"permission":expired}
    ]});
    let router = axum::Router::new().fallback(move |_: axum::Json<serde_json::Value>| {
        let body = body.clone();
        async move { axum::Json(body) }
    });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let url = format!("http://{}/sync", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    SyncConfig::new(&url, &"x".repeat(32))
        .unwrap()
        .sync_batch(
            &fixture.gateway,
            &[fixture.device_id.clone(), other.clone()],
        )
        .await
        .unwrap();
    assert!(fixture.latest()["permission"].is_null());
    let stopped = serde_json::to_value(fixture.gateway.latest(&other).unwrap()).unwrap();
    assert_eq!(stopped["permission"]["desiredEnabled"], false);
    server.abort();
}

#[tokio::test]
async fn sync_source_failure_does_not_apply_and_request_matches_contract() {
    let fixture = Fixture::new();
    let mut permission = fixture.grant();
    permission.expires_at = timestamp(now_ms() + 10_000);
    fixture
        .gateway
        .set_reporting(&fixture.device_id, permission.clone())
        .unwrap();
    let before = fixture.latest();
    permission.expires_at = timestamp(now_ms() + 20_000);
    let valid =
        json!({"v": 1, "permissions": [{"deviceId": fixture.device_id, "permission": permission}]})
            .to_string();
    let invalid = json!({"v": 1, "permissions": [
        {"deviceId": fixture.device_id, "permission": permission},
        {"deviceId": Uuid::new_v4().to_string(), "permission": null}
    ]})
    .to_string();
    for (status, response) in [
        (StatusCode::UNAUTHORIZED, valid.clone()),
        (StatusCode::FOUND, valid.clone()),
        (StatusCode::OK, invalid),
        (StatusCode::OK, " ".repeat(128 * 1024 + 1)),
    ] {
        let (sender, mut requests) = tokio::sync::mpsc::channel(1);
        let router = axum::Router::new().fallback(move |request: Request<Body>| {
            let sender = sender.clone();
            let response = response.clone();
            async move {
                let (parts, body) = request.into_parts();
                let body = axum::body::to_bytes(body, 128 * 1024).await.unwrap();
                sender.send((parts, body)).await.unwrap();
                (status, [(header::LOCATION, "/redirected")], response)
            }
        });
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let url = format!("http://{}/sync", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let config = SyncConfig::new(&url, &"x".repeat(32)).unwrap();
        assert!(
            config
                .sync_batch(&fixture.gateway, std::slice::from_ref(&fixture.device_id))
                .await
                .is_err()
        );
        let (request, body) = requests.recv().await.unwrap();
        assert_eq!(request.method, axum::http::Method::POST);
        assert_eq!(request.headers["apikey"], "x".repeat(32));
        assert!(!request.headers.contains_key(header::AUTHORIZATION));
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["v"], 1);
        assert!(robot_cloud_common::parse_time(body["observedAt"].as_str().unwrap()).is_ok());
        let device = &body["devices"][0];
        assert_eq!(device.as_object().unwrap().len(), 7);
        assert_eq!(device["deviceId"], fixture.device_id);
        assert_eq!(device["sessionReusable"], true);
        assert_eq!(device["permission"]["deviceConfirmed"], false);
        for field in [
            "connectionId",
            "lastSeenAt",
            "reachable",
            "permission",
            "latest",
        ] {
            assert_eq!(device[field], before[field]);
        }
        assert_eq!(fixture.latest()["permission"], before["permission"]);
        server.abort();
    }
}

#[tokio::test]
async fn reporting_http_boundaries_and_offline_stop() {
    let fixture = Fixture::new();
    let key = "test-backend-service-key-32-characters";
    let router = app(fixture.gateway.clone(), key).unwrap();
    let uri = format!("/api/v1/robots/{}/reporting", fixture.device_id);
    for (method, body, expected) in [
        ("PUT", " ".repeat(2049), StatusCode::PAYLOAD_TOO_LARGE),
        ("PUT", "{}".into(), StatusCode::BAD_REQUEST),
        ("POST", String::new(), StatusCode::METHOD_NOT_ALLOWED),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(&uri)
                    .header(header::AUTHORIZATION, format!("Bearer {key}"))
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    let latest_uri = format!("/api/v1/robots/{}/status/latest", fixture.device_id);
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(latest_uri)
                .header(header::AUTHORIZATION, format!("Bearer {key}"))
                .body(Body::from("x"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    fixture.gateway.disconnected();
    let permission = Permission {
        authorization_epoch: 1,
        consent_version: 1,
        enabled: false,
        connection_id: None,
        reporting_session_id: None,
        expires_at: timestamp(now_ms() + 30_000),
    };
    let response = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(uri)
                .header(header::AUTHORIZATION, format!("Bearer {key}"))
                .body(Body::from(serde_json::to_vec(&permission).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let latest = fixture.latest();
    assert_eq!(latest["permission"]["desiredEnabled"], false);
    assert_eq!(latest["permission"]["deviceConfirmed"], false);
}
