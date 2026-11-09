use robot_cloud_common::{
    Result, Status, parse_time,
    reporting::{Control, MAX_COUNTER, Permission, Report},
    timestamp,
};
use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

struct Lease {
    deadline: Instant,
    seq: u64,
}

pub(super) struct Reporting {
    pub connection_id: String,
    permission: Option<Permission>,
    lease: Option<Lease>,
    retired: HashSet<String>,
    clock: (i64, Instant),
}

impl Reporting {
    pub fn new() -> Self {
        Self {
            connection_id: String::new(),
            permission: None,
            lease: None,
            retired: HashSet::new(),
            clock: (0, Instant::now()),
        }
    }

    pub fn connect(&mut self, now: i64, instant: Instant) {
        self.connection_id = uuid::Uuid::new_v4().to_string();
        self.lease = None;
        self.retired.clear();
        self.clock = (now, instant);
    }

    fn effective_time(&mut self, now: i64, instant: Instant) -> i64 {
        let elapsed = instant.saturating_duration_since(self.clock.1).as_millis();
        let monotonic = self.clock.0.saturating_add(elapsed as i64);
        if now > monotonic {
            self.clock = (now, instant);
        }
        now.max(monotonic)
    }

    pub fn disconnect(&mut self) {
        self.lease = None;
    }

    pub fn expire(&mut self, now: i64, instant: Instant) -> bool {
        let now = self.effective_time(now, instant);
        let expired = self.lease.as_ref().is_some_and(|lease| {
            instant >= lease.deadline
                || self.permission.as_ref().is_some_and(|permission| {
                    parse_time(&permission.expires_at).is_ok_and(|expires| now >= expires)
                })
        });
        if expired {
            self.retire();
        }
        expired
    }

    fn retire(&mut self) {
        if self.lease.take().is_some()
            && let Some(session) = self
                .permission
                .as_ref()
                .and_then(|p| p.reporting_session_id.clone())
        {
            self.retired.insert(session);
        }
    }

    pub fn accept(
        &mut self,
        control: &Control,
        device: &str,
        now: i64,
        instant: Instant,
    ) -> Result<()> {
        self.expire(now, instant);
        let now = self.effective_time(now, instant);
        control.validate(device, &self.connection_id, now)?;
        let next = &control.permission;
        if let Some(current) = &self.permission
            && (next.version() < current.version()
                || (next.version() == current.version() && next.enabled != current.enabled)
                || (next.version() == current.version()
                    && self.lease.is_some()
                    && next.reporting_session_id != current.reporting_session_id)
                || (next.enabled
                    && next.reporting_session_id == current.reporting_session_id
                    && (next.version() != current.version()
                        || next.connection_id != current.connection_id)))
        {
            return Err(std::io::Error::other("stale_permission").into());
        }
        if next.enabled
            && next
                .reporting_session_id
                .as_ref()
                .is_some_and(|id| self.retired.contains(id))
        {
            return Err(std::io::Error::other("expired_session").into());
        }
        let continuing = self.lease.is_some()
            && self
                .permission
                .as_ref()
                .is_some_and(|current| next.enabled && current.same_session(next));
        if !continuing {
            self.retire();
        }
        if next.enabled {
            let deadline = self.clock.1
                + Duration::from_millis((parse_time(&next.expires_at)? - self.clock.0) as u64);
            match &mut self.lease {
                Some(lease) => lease.deadline = deadline,
                None => self.lease = Some(Lease { deadline, seq: 0 }),
            }
        }
        self.permission = Some(next.clone());
        Ok(())
    }

    pub fn disabled(&self) -> bool {
        self.permission
            .as_ref()
            .is_some_and(|permission| !permission.enabled)
    }

    pub fn active(&self) -> Option<&Permission> {
        self.lease.as_ref().and(self.permission.as_ref())
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.lease.as_ref().map(|lease| lease.deadline)
    }

    pub fn is_current(&self, sampled: &Permission) -> bool {
        self.active()
            .is_some_and(|permission| permission.same_session(sampled))
    }

    pub fn report(
        &mut self,
        device: &str,
        sampled: &Permission,
        status: Option<Status>,
        error: Option<String>,
        observed: i64,
    ) -> Option<Report> {
        if !self.is_current(sampled) {
            return None;
        }
        let permission = self.active()?;
        let session = permission.reporting_session_id.clone()?;
        let lease = self.lease.as_mut()?;
        if lease.seq >= MAX_COUNTER {
            return None;
        }
        lease.seq += 1;
        Some(Report {
            v: 1,
            kind: "status.report".into(),
            device_id: device.into(),
            connection_id: self.connection_id.clone(),
            reporting_session_id: session,
            authorization_epoch: sampled.authorization_epoch,
            consent_version: sampled.consent_version,
            seq: lease.seq,
            observed_at: timestamp(observed),
            status,
            error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_793_616_000_000;
    const DEVICE: &str = "00000000-0000-4000-8000-000000000001";

    fn fixture() -> (Reporting, Control, Instant) {
        let instant = Instant::now();
        let mut reporting = Reporting::new();
        reporting.connect(NOW, instant);
        let control = Control {
            v: 1,
            kind: "reporting.control".into(),
            device_id: DEVICE.into(),
            permission: Permission {
                authorization_epoch: 1,
                consent_version: 1,
                enabled: true,
                connection_id: Some(reporting.connection_id.clone()),
                reporting_session_id: Some(uuid::Uuid::new_v4().to_string()),
                expires_at: timestamp(NOW + 10_000),
            },
        };
        (reporting, control, instant)
    }

    #[test]
    fn disable_and_connection_fence() {
        let (mut reporting, mut control, instant) = fixture();
        reporting.accept(&control, DEVICE, NOW, instant).unwrap();
        let old = control.clone();
        control.permission.consent_version = 2;
        control.permission.enabled = false;
        reporting.accept(&control, DEVICE, NOW, instant).unwrap();
        assert!(reporting.disabled());
        assert!(reporting.active().is_none());
        assert!(reporting.accept(&old, DEVICE, NOW, instant).is_err());
        reporting.connect(NOW, instant);
        assert!(reporting.disabled());
        assert!(reporting.accept(&control, DEVICE, NOW, instant).is_err());
        control.permission.connection_id = Some(reporting.connection_id.clone());
        reporting.accept(&control, DEVICE, NOW, instant).unwrap();
        control.permission.enabled = true;
        assert!(reporting.accept(&control, DEVICE, NOW, instant).is_err());
    }

    #[test]
    fn renewal_preserves_seq_and_expired_sessions_stay_retired() {
        let (mut reporting, mut control, instant) = fixture();
        reporting.accept(&control, DEVICE, NOW, instant).unwrap();
        let sampled = control.permission.clone();
        assert_eq!(
            reporting
                .report(
                    DEVICE,
                    &sampled,
                    None,
                    Some("upstream_unavailable".into()),
                    NOW
                )
                .unwrap()
                .seq,
            1
        );
        control.permission.expires_at = timestamp(NOW + 20_000);
        reporting
            .accept(
                &control,
                DEVICE,
                NOW + 1000,
                instant + Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(
            reporting
                .report(
                    DEVICE,
                    &sampled,
                    None,
                    Some("upstream_unavailable".into()),
                    NOW + 1000
                )
                .unwrap()
                .seq,
            2
        );
        assert!(reporting.expire(NOW, instant + Duration::from_secs(20)));
        assert!(!reporting.disabled());
        control.permission.expires_at = timestamp(NOW + 30_000);
        assert!(
            reporting
                .accept(&control, DEVICE, NOW, instant + Duration::from_secs(20))
                .is_err()
        );
        control.permission.reporting_session_id = Some(uuid::Uuid::new_v4().to_string());
        reporting
            .accept(&control, DEVICE, NOW, instant + Duration::from_secs(20))
            .unwrap();
        let sampled = control.permission.clone();
        assert_eq!(
            reporting
                .report(
                    DEVICE,
                    &sampled,
                    None,
                    Some("upstream_unavailable".into()),
                    NOW + 20_000
                )
                .unwrap()
                .seq,
            1
        );
    }

    #[test]
    fn rollback_and_duplicates_cannot_extend_deadline() {
        let (mut reporting, control, instant) = fixture();
        reporting.accept(&control, DEVICE, NOW, instant).unwrap();
        reporting
            .accept(
                &control,
                DEVICE,
                NOW - 5000,
                instant + Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(
            reporting.deadline(),
            Some(instant + Duration::from_secs(10))
        );
        assert!(reporting.expire(NOW - 10_000, instant + Duration::from_secs(10)));
        assert!(
            reporting
                .accept(
                    &control,
                    DEVICE,
                    NOW - 10_000,
                    instant + Duration::from_secs(10)
                )
                .is_err()
        );
    }

    #[test]
    fn frequent_controls_keep_monotonic_elapsed_time() {
        let (mut reporting, control, instant) = fixture();
        reporting.accept(&control, DEVICE, NOW, instant).unwrap();
        for step in 1..20_000 {
            let current = instant + Duration::from_micros(step * 500);
            reporting
                .accept(&control, DEVICE, NOW - 5000, current)
                .unwrap();
        }
        assert!(reporting.expire(NOW - 5000, instant + Duration::from_secs(10)));
    }

    #[test]
    fn session_is_bound_to_version_and_connection() {
        let (mut reporting, mut control, instant) = fixture();
        reporting.accept(&control, DEVICE, NOW, instant).unwrap();
        let original = control.clone();
        control.permission.connection_id = Some(uuid::Uuid::new_v4().to_string());
        assert!(reporting.accept(&control, DEVICE, NOW, instant).is_err());
        control = original.clone();
        control.permission.reporting_session_id = Some(uuid::Uuid::new_v4().to_string());
        assert!(reporting.accept(&control, DEVICE, NOW, instant).is_err());
        control = original.clone();
        control.permission.consent_version += 1;
        assert!(reporting.accept(&control, DEVICE, NOW, instant).is_err());
        reporting.disconnect();
        assert!(reporting.active().is_none());
        reporting.connect(NOW, instant);
        control = original;
        control.permission.connection_id = Some(reporting.connection_id.clone());
        assert!(reporting.accept(&control, DEVICE, NOW, instant).is_err());
        control.permission.reporting_session_id = Some(uuid::Uuid::new_v4().to_string());
        reporting.accept(&control, DEVICE, NOW, instant).unwrap();
        assert!(reporting.active().is_some());
    }
}
