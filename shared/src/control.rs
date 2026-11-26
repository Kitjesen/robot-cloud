use crate::{Result, is_uuid, parse_time};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub mod rpc {
    tonic::include_proto!("doso.remote.v1");
}

pub const SESSION_MS: i64 = 30_000;
pub const PROOF_MS: i64 = 10_000;
pub const MAX_ABS_VX_M_S: f64 = 5.0;
pub const MAX_ABS_VY_M_S: f64 = 3.0;
pub const MAX_ABS_YAW_RAD_S: f64 = 5.0;
pub const MAX_PROFILE_NAME_LEN: usize = 64;
pub const DEFAULT_VELOCITY_TIMEOUT_MS: u64 = 300;
pub const MIN_VELOCITY_TIMEOUT_MS: u64 = 100;
pub const MAX_VELOCITY_TIMEOUT_MS: u64 = 1000;

pub fn validate_control_input(input: &rpc::ControlInput) -> Result<rpc::Action> {
    let action = rpc::Action::try_from(input.action)
        .ok()
        .filter(|action| *action != rpc::Action::Unspecified)
        .context("unsupported_control_action")?;
    match action {
        rpc::Action::Velocity => {
            ensure!(
                input.profile_name.is_empty(),
                "unexpected_control_profile_name"
            );
            let velocity = input
                .velocity
                .as_ref()
                .context("missing_velocity_command")?;
            ensure!(
                velocity.vx_m_s.is_finite()
                    && velocity.vy_m_s.is_finite()
                    && velocity.yaw_rad_s.is_finite(),
                "invalid_velocity_command"
            );
            ensure!(
                velocity.vx_m_s.abs() <= MAX_ABS_VX_M_S
                    && velocity.vy_m_s.abs() <= MAX_ABS_VY_M_S
                    && velocity.yaw_rad_s.abs() <= MAX_ABS_YAW_RAD_S,
                "velocity_out_of_bounds"
            );
            ensure!(velocity.input_window > 0, "missing_input_window");
        }
        rpc::Action::SwitchProfile => {
            ensure!(input.velocity.is_none(), "unexpected_velocity_command");
            ensure!(
                !input.profile_name.is_empty()
                    && input.profile_name.len() <= MAX_PROFILE_NAME_LEN
                    && input
                        .profile_name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')),
                "invalid_control_profile_name"
            );
        }
        _ => {
            ensure!(input.velocity.is_none(), "unexpected_velocity_command");
            ensure!(
                input.profile_name.is_empty(),
                "unexpected_control_profile_name"
            );
        }
    }
    Ok(action)
}

pub fn parse_velocity_timeout(value: Option<&str>) -> Result<Duration> {
    match value {
        None => Ok(Duration::from_millis(DEFAULT_VELOCITY_TIMEOUT_MS)),
        Some(raw) => {
            let ms = raw
                .trim()
                .parse::<u64>()
                .context("invalid_control_velocity_timeout")?;
            ensure!(
                (MIN_VELOCITY_TIMEOUT_MS..=MAX_VELOCITY_TIMEOUT_MS).contains(&ms),
                "invalid_control_velocity_timeout"
            );
            Ok(Duration::from_millis(ms))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Proof {
    pub v: u8,
    pub kind: String,
    pub device_id: String,
    pub connection_id: String,
    pub challenge: String,
    pub issued_at: String,
}

impl Proof {
    pub fn validate(&self, challenge: &str, now: i64) -> Result<()> {
        ensure!(
            self.v == 1
                && self.kind == "control.hello"
                && is_uuid(&self.device_id)
                && is_uuid(&self.connection_id)
                && is_uuid(&self.challenge)
                && self.challenge == challenge,
            "invalid_control_proof"
        );
        let issued = parse_time(&self.issued_at)?;
        ensure!(
            issued <= now + 2000 && now - issued <= PROOF_MS,
            "expired_control_proof"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Grant {
    pub v: u8,
    pub kind: String,
    pub device_id: String,
    pub operator_id: String,
    pub authorization_epoch: u64,
    pub connection_id: String,
    pub session_id: String,
    pub issued_at: String,
    pub expires_at: String,
}

impl Grant {
    pub fn validate(&self, device: &str, connection: &str, now: i64) -> Result<()> {
        ensure!(
            self.v == 1
                && self.kind == "control.grant"
                && is_uuid(&self.device_id)
                && self.device_id == device
                && is_uuid(&self.connection_id)
                && self.connection_id == connection
                && is_uuid(&self.operator_id)
                && is_uuid(&self.session_id)
                && (1..=crate::reporting::MAX_COUNTER).contains(&self.authorization_epoch),
            "invalid_control_grant"
        );
        let issued = parse_time(&self.issued_at)?;
        let expires = parse_time(&self.expires_at)?;
        ensure!(
            issued <= now + 2000
                && expires > now
                && expires > issued
                && expires - issued <= SESSION_MS,
            "expired_control_grant"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_signed, encode_signed, timestamp};
    use ed25519_dalek::SigningKey;

    #[test]
    fn proof_requires_the_current_challenge() {
        let now = 1_793_616_000_000;
        let proof = Proof {
            v: 1,
            kind: "control.hello".into(),
            device_id: uuid::Uuid::new_v4().to_string(),
            connection_id: uuid::Uuid::new_v4().to_string(),
            challenge: uuid::Uuid::new_v4().to_string(),
            issued_at: timestamp(now),
        };
        proof.validate(&proof.challenge, now).unwrap();
        assert!(
            proof
                .validate(&uuid::Uuid::new_v4().to_string(), now)
                .is_err()
        );
        assert!(
            proof
                .validate(&proof.challenge, now + PROOF_MS + 1)
                .is_err()
        );
    }

    #[test]
    fn grant_is_signed_device_bound_and_short_lived() {
        let now = 1_793_616_000_000;
        let key = SigningKey::from_bytes(&[23; 32]);
        let mut grant = Grant {
            v: 1,
            kind: "control.grant".into(),
            device_id: uuid::Uuid::new_v4().to_string(),
            operator_id: uuid::Uuid::new_v4().to_string(),
            authorization_epoch: 1,
            connection_id: uuid::Uuid::new_v4().to_string(),
            session_id: uuid::Uuid::new_v4().to_string(),
            issued_at: timestamp(now),
            expires_at: timestamp(now + SESSION_MS),
        };
        let bytes = encode_signed(&grant, &key).unwrap();
        let decoded: Grant = decode_signed(&bytes, &key.verifying_key()).unwrap();
        decoded
            .validate(&grant.device_id, &grant.connection_id, now)
            .unwrap();
        assert!(
            decoded
                .validate(&grant.device_id, &uuid::Uuid::new_v4().to_string(), now)
                .is_err()
        );
        assert!(
            decoded
                .validate(&grant.device_id, &grant.connection_id, now + SESSION_MS)
                .is_err()
        );
        assert!(
            decode_signed::<Grant>(&bytes, &SigningKey::from_bytes(&[24; 32]).verifying_key())
                .is_err()
        );
        grant.expires_at = timestamp(now + SESSION_MS + 1);
        assert!(
            grant
                .validate(&grant.device_id, &grant.connection_id, now)
                .is_err()
        );
    }

    #[test]
    fn validates_velocity_profile_and_discrete_control_inputs() {
        let sid = uuid::Uuid::new_v4().to_string();
        let valid_vel = rpc::ControlInput {
            session_id: sid.clone(),
            sequence: 1,
            action: rpc::Action::Velocity as i32,
            velocity: Some(rpc::VelocityCommand {
                vx_m_s: 1.5,
                vy_m_s: -0.5,
                yaw_rad_s: 2.0,
                deadman: true,
                manual_mode: false,
                input_window: 7,
            }),
            ..Default::default()
        };
        assert_eq!(
            validate_control_input(&valid_vel).unwrap(),
            rpc::Action::Velocity
        );

        for bad_vel in [
            rpc::VelocityCommand {
                input_window: 0,
                ..valid_vel.velocity.unwrap()
            },
            rpc::VelocityCommand {
                vx_m_s: f64::NAN,
                ..valid_vel.velocity.unwrap()
            },
            rpc::VelocityCommand {
                vy_m_s: f64::INFINITY,
                ..valid_vel.velocity.unwrap()
            },
            rpc::VelocityCommand {
                vx_m_s: MAX_ABS_VX_M_S + 0.1,
                ..valid_vel.velocity.unwrap()
            },
            rpc::VelocityCommand {
                vy_m_s: -(MAX_ABS_VY_M_S + 0.1),
                ..valid_vel.velocity.unwrap()
            },
            rpc::VelocityCommand {
                yaw_rad_s: MAX_ABS_YAW_RAD_S + 0.1,
                ..valid_vel.velocity.unwrap()
            },
        ] {
            let input = rpc::ControlInput {
                velocity: Some(bad_vel),
                ..valid_vel.clone()
            };
            assert!(validate_control_input(&input).is_err());
        }
        assert!(
            validate_control_input(&rpc::ControlInput {
                velocity: None,
                ..valid_vel.clone()
            })
            .is_err()
        );
        assert!(
            validate_control_input(&rpc::ControlInput {
                profile_name: "flat".into(),
                ..valid_vel.clone()
            })
            .is_err()
        );

        let valid_profile = rpc::ControlInput {
            session_id: sid.clone(),
            sequence: 2,
            action: rpc::Action::SwitchProfile as i32,
            profile_name: "flat_w5.v1-test".into(),
            ..Default::default()
        };
        assert_eq!(
            validate_control_input(&valid_profile).unwrap(),
            rpc::Action::SwitchProfile
        );
        for bad_name in [
            String::new(),
            "bad space".into(),
            "bad/slash".into(),
            "a".repeat(MAX_PROFILE_NAME_LEN + 1),
        ] {
            assert!(
                validate_control_input(&rpc::ControlInput {
                    profile_name: bad_name,
                    ..valid_profile.clone()
                })
                .is_err()
            );
        }

        for action in [
            rpc::Action::Claim,
            rpc::Action::Hold,
            rpc::Action::Release,
            rpc::Action::StandUp,
            rpc::Action::SitDown,
            rpc::Action::StopMotion,
            rpc::Action::EmergencyStop,
            rpc::Action::ClearEmergencyStop,
            rpc::Action::Resume,
            rpc::Action::EnableMotors,
            rpc::Action::DisableMotors,
        ] {
            let ok = rpc::ControlInput {
                session_id: sid.clone(),
                sequence: 3,
                action: action as i32,
                ..Default::default()
            };
            assert_eq!(validate_control_input(&ok).unwrap(), action);
            assert!(
                validate_control_input(&rpc::ControlInput {
                    profile_name: "flat".into(),
                    ..ok.clone()
                })
                .is_err()
            );
            assert!(
                validate_control_input(&rpc::ControlInput {
                    velocity: valid_vel.velocity,
                    ..ok
                })
                .is_err()
            );
        }
    }

    #[test]
    fn velocity_timeout_defaults_and_bounds() {
        assert_eq!(
            parse_velocity_timeout(None).unwrap(),
            Duration::from_millis(300)
        );
        assert_eq!(
            parse_velocity_timeout(Some("100")).unwrap(),
            Duration::from_millis(100)
        );
        assert_eq!(
            parse_velocity_timeout(Some("1000")).unwrap(),
            Duration::from_millis(1000)
        );
        for bad in ["", "0", "99", "1001", "abc"] {
            assert!(parse_velocity_timeout(Some(bad)).is_err(), "{bad}");
        }
    }
}
