use anyhow::{Result, ensure};
use robot_cloud_common::{
    control::{
        Grant,
        rpc::{Action, ControlInput},
    },
    parse_time,
};
use std::time::Duration;
use tokio::time::Instant;

pub(super) struct Lease {
    pub grant: Grant,
    pub deadline: Instant,
    sequence: u64,
}

impl Lease {
    pub fn new(grant: Grant, device: &str, connection: &str, now: i64) -> Result<Self> {
        grant.validate(device, connection, now)?;
        let remaining = parse_time(&grant.expires_at)? - now;
        Ok(Self {
            grant,
            deadline: Instant::now() + Duration::from_millis(remaining as u64),
            sequence: 0,
        })
    }

    pub fn accept(&mut self, input: &ControlInput, now: i64) -> Result<Action> {
        ensure!(Instant::now() < self.deadline, "expired_control_session");
        self.grant
            .validate(&self.grant.device_id, &self.grant.connection_id, now)?;
        ensure!(
            input.session_id == self.grant.session_id
                && input.sequence > self.sequence
                && input.sequence <= robot_cloud_common::reporting::MAX_COUNTER,
            "invalid_control_sequence"
        );
        let action = Action::try_from(input.action)?;
        ensure!(action != Action::Unspecified, "unsupported_control_action");
        self.sequence = input.sequence;
        Ok(action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use robot_cloud_common::{control::SESSION_MS, timestamp};

    fn grant(now: i64) -> Grant {
        Grant {
            v: 1,
            kind: "control.grant".into(),
            device_id: uuid::Uuid::new_v4().to_string(),
            operator_id: uuid::Uuid::new_v4().to_string(),
            authorization_epoch: 1,
            connection_id: uuid::Uuid::new_v4().to_string(),
            session_id: uuid::Uuid::new_v4().to_string(),
            issued_at: timestamp(now),
            expires_at: timestamp(now + SESSION_MS),
        }
    }

    #[tokio::test]
    async fn rejects_replay_identity_changes_and_expiry() {
        let now = 1_793_616_000_000;
        let grant = grant(now);
        let mut lease =
            Lease::new(grant.clone(), &grant.device_id, &grant.connection_id, now).unwrap();
        let mut input = ControlInput {
            session_id: grant.session_id,
            sequence: 1,
            action: Action::Claim as i32,
        };
        assert_eq!(lease.accept(&input, now).unwrap(), Action::Claim);
        assert!(lease.accept(&input, now).is_err());
        input.sequence = 2;
        input.session_id = uuid::Uuid::new_v4().to_string();
        assert!(lease.accept(&input, now).is_err());
        input.session_id = lease.grant.session_id.clone();
        assert!(lease.accept(&input, now + SESSION_MS).is_err());
        lease.deadline = Instant::now();
        assert!(lease.accept(&input, now).is_err());
    }

    #[tokio::test]
    async fn unsupported_action_does_not_advance_sequence() {
        let now = 1_793_616_000_000;
        let grant = grant(now);
        let mut lease =
            Lease::new(grant.clone(), &grant.device_id, &grant.connection_id, now).unwrap();
        let mut input = ControlInput {
            session_id: grant.session_id,
            sequence: 1,
            action: 99,
        };
        assert!(lease.accept(&input, now).is_err());
        input.action = Action::Release as i32;
        assert_eq!(lease.accept(&input, now).unwrap(), Action::Release);
    }
}
