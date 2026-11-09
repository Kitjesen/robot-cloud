use crate::{Result, Status, is_uuid, parse_time};
use anyhow::ensure;
use serde::{Deserialize, Serialize};

pub const LEASE_MS: i64 = 30_000;
pub const REPORT_MS: u64 = 1_000;
pub const HELLO_MS: u64 = 5_000;
pub const FRESH_MS: i64 = 5_000;
pub const SEEN_MS: i64 = 15_000;
pub const MAX_COUNTER: u64 = 9_007_199_254_740_991;

pub fn valid_version(epoch: u64, consent: u64) -> bool {
    (1..=MAX_COUNTER).contains(&epoch) && (1..=MAX_COUNTER).contains(&consent)
}

pub fn recent(observed: &str, now: i64) -> Result<()> {
    let observed = parse_time(observed)?;
    ensure!(
        now - observed <= SEEN_MS && observed - now <= 2000,
        "stale_report"
    );
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Permission {
    pub authorization_epoch: u64,
    pub consent_version: u64,
    pub enabled: bool,
    pub connection_id: Option<String>,
    pub reporting_session_id: Option<String>,
    pub expires_at: String,
}

impl Permission {
    pub fn version(&self) -> (u64, u64) {
        (self.authorization_epoch, self.consent_version)
    }

    // Identity only; signatures, device IDs, deadlines and confirmation remain caller checks.
    pub fn same_session(&self, other: &Self) -> bool {
        self.version() == other.version()
            && self.connection_id.is_some()
            && self.reporting_session_id.is_some()
            && self.connection_id == other.connection_id
            && self.reporting_session_id == other.reporting_session_id
    }

    pub fn matches_report(&self, report: &Report) -> bool {
        self.version() == (report.authorization_epoch, report.consent_version)
            && self.connection_id.as_deref() == Some(report.connection_id.as_str())
            && self.reporting_session_id.as_deref() == Some(report.reporting_session_id.as_str())
    }

    pub fn validate(&self, now: i64) -> Result<()> {
        ensure!(
            valid_version(self.authorization_epoch, self.consent_version),
            "invalid_permission"
        );
        let expires = parse_time(&self.expires_at)?;
        ensure!(
            expires > now && expires - now <= LEASE_MS,
            "expired_permission"
        );
        ensure!(
            self.connection_id.as_deref().is_none_or(is_uuid)
                && self.reporting_session_id.as_deref().is_none_or(is_uuid),
            "invalid_session"
        );
        ensure!(
            !self.enabled || (self.connection_id.is_some() && self.reporting_session_id.is_some()),
            "missing_session"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Hello {
    pub v: u8,
    pub kind: String,
    pub device_id: String,
    pub connection_id: String,
    pub observed_at: String,
}

impl Hello {
    pub fn validate(&self, device: &str, now: i64) -> Result<()> {
        ensure!(
            self.v == 1
                && self.kind == "reporting.hello"
                && self.device_id == device
                && is_uuid(device)
                && is_uuid(&self.connection_id),
            "invalid_hello"
        );
        recent(&self.observed_at, now)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Control {
    pub v: u8,
    pub kind: String,
    pub device_id: String,
    pub permission: Permission,
}

impl Control {
    pub fn validate(&self, device: &str, connection: &str, now: i64) -> Result<()> {
        ensure!(
            self.v == 1
                && self.kind == "reporting.control"
                && self.device_id == device
                && is_uuid(device)
                && self.permission.connection_id.as_deref() == Some(connection),
            "invalid_control"
        );
        self.permission.validate(now)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Ack {
    pub v: u8,
    pub kind: String,
    pub device_id: String,
    pub permission: Permission,
    pub observed_at: String,
}

impl Ack {
    pub fn validate(&self, device: &str, now: i64) -> Result<()> {
        ensure!(
            self.v == 1
                && self.kind == "reporting.ack"
                && self.device_id == device
                && is_uuid(device),
            "invalid_ack"
        );
        self.permission.validate(now)?;
        recent(&self.observed_at, now)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Report {
    pub v: u8,
    pub kind: String,
    pub device_id: String,
    pub connection_id: String,
    pub reporting_session_id: String,
    pub authorization_epoch: u64,
    pub consent_version: u64,
    pub seq: u64,
    pub observed_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Report {
    pub fn validate(&self, device: &str, now: i64) -> Result<()> {
        ensure!(
            self.v == 1
                && self.kind == "status.report"
                && self.device_id == device
                && is_uuid(device)
                && is_uuid(&self.connection_id)
                && is_uuid(&self.reporting_session_id)
                && self.seq > 0
                && self.seq <= MAX_COUNTER,
            "invalid_report"
        );
        ensure!(
            valid_version(self.authorization_epoch, self.consent_version),
            "invalid_permission"
        );
        recent(&self.observed_at, now)?;
        crate::validate_outcome(&self.status, &self.error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timestamp;

    #[test]
    fn bounded_permission() {
        let now = 1_793_700_000_000;
        let mut permission = Permission {
            authorization_epoch: 1,
            consent_version: 1,
            enabled: true,
            connection_id: Some(uuid::Uuid::new_v4().to_string()),
            reporting_session_id: Some(uuid::Uuid::new_v4().to_string()),
            expires_at: timestamp(now + LEASE_MS),
        };
        permission.validate(now).unwrap();
        assert!(permission.validate(now + LEASE_MS).is_err());
        permission.expires_at = timestamp(now + LEASE_MS + 1);
        assert!(permission.validate(now).is_err());
        permission.expires_at = timestamp(now + LEASE_MS);
        permission.connection_id = None;
        assert!(permission.validate(now).is_err());
        permission.enabled = false;
        permission.validate(now).unwrap();
        permission.consent_version = 0;
        assert!(permission.validate(now).is_err());
    }

    #[test]
    fn session_identity() {
        let permission = Permission {
            authorization_epoch: 1,
            consent_version: 2,
            enabled: true,
            connection_id: Some(uuid::Uuid::new_v4().to_string()),
            reporting_session_id: Some(uuid::Uuid::new_v4().to_string()),
            expires_at: timestamp(1_793_700_030_000),
        };
        let mut renewed = permission.clone();
        renewed.expires_at = timestamp(1_793_700_040_000);
        assert!(permission.same_session(&renewed));
        assert_ne!(permission, renewed); // An old ACK must not confirm this renewal.
        for field in [
            "authorizationEpoch",
            "consentVersion",
            "connectionId",
            "reportingSessionId",
        ] {
            let mut changed = serde_json::to_value(&permission).unwrap();
            changed[field] = if field.ends_with("Id") {
                serde_json::json!(uuid::Uuid::new_v4().to_string())
            } else {
                serde_json::json!(3)
            };
            let changed: Permission = serde_json::from_value(changed).unwrap();
            assert!(!permission.same_session(&changed));
        }
        let mut report = Report {
            v: 1,
            kind: "status.report".into(),
            device_id: uuid::Uuid::new_v4().to_string(),
            connection_id: permission.connection_id.clone().unwrap(),
            reporting_session_id: permission.reporting_session_id.clone().unwrap(),
            authorization_epoch: 1,
            consent_version: 2,
            seq: 1,
            observed_at: timestamp(1_793_700_000_000),
            status: None,
            error: Some("upstream_unavailable".into()),
        };
        assert!(permission.matches_report(&report));
        report.consent_version += 1;
        assert!(!permission.matches_report(&report));
        renewed.connection_id = None;
        assert!(!renewed.same_session(&renewed));
        assert!(valid_version(MAX_COUNTER, MAX_COUNTER));
        assert!(!valid_version(MAX_COUNTER + 1, 1));
    }
}
