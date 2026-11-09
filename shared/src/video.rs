use crate::{Result, is_uuid, parse_time};
use anyhow::ensure;
use serde::{Deserialize, Serialize};

pub const LEASE_MS: i64 = 15_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Control {
    pub v: u8,
    pub kind: String,
    pub device_id: String,
    pub connection_id: String,
    pub seq: u64,
    pub enabled: bool,
    pub expires_at: String,
}

impl Control {
    pub fn validate(&self, device: &str, connection: &str, now: i64) -> Result<()> {
        ensure!(
            self.v == 1
                && self.kind == "video.control"
                && is_uuid(device)
                && self.device_id == device
                && is_uuid(connection)
                && self.connection_id == connection
                && (1..=crate::reporting::MAX_COUNTER).contains(&self.seq),
            "invalid_video_control"
        );
        let expires = parse_time(&self.expires_at)?;
        ensure!(
            expires > now && expires - now <= LEASE_MS,
            "expired_video_control"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binds_device_connection_and_deadline() {
        let now = 1_793_616_000_000;
        let device = uuid::Uuid::new_v4().to_string();
        let connection = uuid::Uuid::new_v4().to_string();
        let mut control = Control {
            v: 1,
            kind: "video.control".into(),
            device_id: device.clone(),
            connection_id: connection.clone(),
            seq: 1,
            enabled: true,
            expires_at: crate::timestamp(now + LEASE_MS),
        };
        control.validate(&device, &connection, now).unwrap();
        assert!(
            control
                .validate(&device, &uuid::Uuid::new_v4().to_string(), now)
                .is_err()
        );
        assert!(
            control
                .validate(&uuid::Uuid::new_v4().to_string(), &connection, now)
                .is_err()
        );
        assert!(
            control
                .validate(&device, &connection, now + LEASE_MS)
                .is_err()
        );
        control.expires_at = crate::timestamp(now + LEASE_MS + 1);
        assert!(control.validate(&device, &connection, now).is_err());
    }
}
