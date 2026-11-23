use crate::{Result, is_uuid, parse_time};
use anyhow::ensure;
use serde::{Deserialize, Serialize};

pub mod rpc {
    tonic::include_proto!("doso.remote.v1");
}

pub const SESSION_MS: i64 = 30_000;
pub const PROOF_MS: i64 = 10_000;

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
}
