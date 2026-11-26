use super::*;
use ed25519_dalek::SigningKey;
use robot_cloud_common::control::SESSION_MS;

#[test]
fn local_sdk_identity_must_match_the_authenticated_device() {
    let device_id = uuid::Uuid::new_v4().to_string();
    let mut info = doso_sdk::rpc::SystemInfo::default();
    assert!(validate_identity(&info, &device_id).is_err());
    info.robot_id = Some(uuid::Uuid::new_v4().to_string());
    assert!(validate_identity(&info, &device_id).is_err());
    info.robot_id = Some(device_id.clone());
    validate_identity(&info, &device_id).unwrap();
}

fn grant(now: i64) -> Grant {
    Grant {
        v: 1,
        kind: "control.grant".into(),
        device_id: uuid::Uuid::new_v4().to_string(),
        operator_id: uuid::Uuid::new_v4().to_string(),
        authorization_epoch: 2,
        connection_id: uuid::Uuid::new_v4().to_string(),
        session_id: uuid::Uuid::new_v4().to_string(),
        issued_at: timestamp(now),
        expires_at: timestamp(now + SESSION_MS),
    }
}

#[tokio::test]
async fn signed_grants_reject_replay_and_old_authority() {
    let now = 1_793_616_000_000;
    let key = SigningKey::from_bytes(&[31; 32]);
    let mut grant = grant(now);
    let mut seen = HashMap::new();
    let mut epoch = 0;
    let wire = encode_signed(&grant, &key).unwrap();
    let accept = |bytes: &[u8], seen: &mut HashMap<_, _>, epoch: &mut u64| {
        accept_grant(
            bytes,
            &key.verifying_key(),
            &grant.device_id,
            &grant.connection_id,
            now,
            seen,
            epoch,
        )
    };
    accept(&wire, &mut seen, &mut epoch).unwrap();
    assert!(accept(&wire, &mut seen, &mut epoch).is_err());
    assert_eq!(epoch, 2);
    grant.session_id = uuid::Uuid::new_v4().to_string();
    grant.authorization_epoch = 1;
    let wire = encode_signed(&grant, &key).unwrap();
    assert!(
        accept_grant(
            &wire,
            &key.verifying_key(),
            &grant.device_id,
            &grant.connection_id,
            now,
            &mut seen,
            &mut epoch,
        )
        .is_err()
    );
    assert!(
        accept_grant(
            &wire,
            &SigningKey::from_bytes(&[32; 32]).verifying_key(),
            &grant.device_id,
            &grant.connection_id,
            now,
            &mut seen,
            &mut epoch,
        )
        .is_err()
    );
}

#[test]
fn sdk_reply_preserves_execution_evidence() {
    let input = ControlInput {
        session_id: uuid::Uuid::new_v4().to_string(),
        sequence: 8,
        action: Action::Release as i32,
        ..Default::default()
    };
    let reply = outcome(
        &input,
        Ok(doso_sdk::rpc::TeleopReply {
            accepted: false,
            reason: "native_release_unconfirmed".into(),
            final_output_confirmed: false,
            input_window: 5,
            ingress_accepted: false,
            ..Default::default()
        }),
    );
    assert_eq!(reply.session_id, input.session_id);
    assert_eq!(reply.sequence, input.sequence);
    assert!(!reply.accepted);
    assert_eq!(reply.held_by_client, None);
    assert!(!reply.final_output_confirmed);
    assert_eq!(reply.input_window, 5);
    assert!(!reply.ingress_accepted);
    assert_eq!(reply.reason, "native_release_unconfirmed");
    let reply = outcome(
        &input,
        Ok(doso_sdk::rpc::TeleopReply {
            accepted: true,
            final_output_confirmed: true,
            input_window: 6,
            ingress_accepted: true,
            ..Default::default()
        }),
    );
    assert!(reply.accepted);
    assert!(reply.final_output_confirmed);
    assert_eq!(reply.input_window, 6);
    assert!(reply.ingress_accepted);
}

#[test]
fn ownership_keeps_unknown_and_stale_state_distinct() {
    let input = ControlInput {
        session_id: uuid::Uuid::new_v4().to_string(),
        sequence: 1,
        action: Action::Claim as i32,
        ..Default::default()
    };
    for (fresh, held, expected) in [
        (true, Some(true), Some(true)),
        (true, Some(false), Some(false)),
        (true, None, None),
        (false, Some(true), None),
        (false, Some(false), None),
    ] {
        let reply = outcome(
            &input,
            Ok(doso_sdk::rpc::TeleopReply {
                accepted: true,
                control: Some(doso_sdk::rpc::ControlState {
                    fresh,
                    held_by_client: held,
                    ..Default::default()
                }),
                ..Default::default()
            }),
        );
        assert_eq!(reply.held_by_client, expected);
        assert!(reply.accepted);
    }
}

#[test]
fn receipt_outcome_preserves_acceptance_and_unknown_submission() {
    let input = ControlInput {
        session_id: uuid::Uuid::new_v4().to_string(),
        sequence: 11,
        action: Action::StandUp as i32,
        ..Default::default()
    };
    let ok = receipt_outcome(
        &input,
        Ok(doso_sdk::CommandReceipt {
            ok: true,
            accepted: Some(true),
            reason: "started".into(),
            reason_code: "ok".into(),
            ..Default::default()
        }),
        4,
    );
    assert!(ok.accepted);
    assert_eq!(ok.reason, "ok");
    assert!(!ok.final_output_confirmed);
    assert!(!ok.submission_unknown);
    assert_eq!(ok.input_window, 4);

    let rejected = receipt_outcome(
        &input,
        Ok(doso_sdk::CommandReceipt {
            ok: false,
            accepted: Some(false),
            reason: "busy".into(),
            reason_code: String::new(),
            ..Default::default()
        }),
        4,
    );
    assert!(!rejected.accepted);
    assert_eq!(rejected.reason, "busy");
    assert!(!rejected.submission_unknown);

    let err = error_to_reply(&input, "deadline exceeded".into(), true, 4);
    assert!(!err.accepted);
    assert!(err.submission_unknown);
    assert_eq!(err.input_window, 4);
    assert_eq!(err.reason, "deadline exceeded");
}
