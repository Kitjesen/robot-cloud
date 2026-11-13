use crate::reporting::Reporting;
use ed25519_dalek::{SigningKey, VerifyingKey};
use robot_cloud_common::{
    ReadRequest, ReadResult, Result, Status, Topics, decode_signed, encode_signed,
    load_signing_key, load_verifying_key, parse_time,
    reporting::{Ack, Control as ReportingControl, Hello, Permission},
    timestamp, topics,
    video::Control as VideoControl,
};
use std::{collections::HashMap, env, time::Instant};

pub(super) struct Device {
    pub id: String,
    pub topics: Topics,
    pub reporting: Reporting,
    signer: SigningKey,
    verifier: VerifyingKey,
    seen: HashMap<String, i64>,
}

impl Device {
    pub fn load() -> Result<Self> {
        let id = env::var("DOSO_DEVICE_ID")?;
        Ok(Self {
            topics: topics(&id)?,
            id,
            signer: load_signing_key(&env::var("DOSO_DEVICE_SIGNING_KEY")?)?,
            verifier: load_verifying_key(&env::var("DOSO_GATEWAY_PUBLIC_KEY")?)?,
            seen: HashMap::new(),
            reporting: Reporting::new(),
        })
    }

    pub fn accept(&mut self, bytes: &[u8], now: i64) -> Result<Option<ReadRequest>> {
        if self.reporting.active().is_none() {
            return Ok(None);
        }
        let request: ReadRequest = decode_signed(bytes, &self.verifier)?;
        request.validate(&self.id, now)?;
        self.seen.retain(|_, expires| *expires > now);
        if self.seen.contains_key(&request.request_id) || self.seen.len() >= 512 {
            return Ok(None);
        }
        self.seen
            .insert(request.request_id.clone(), parse_time(&request.expires_at)?);
        Ok(Some(request))
    }

    pub fn reply(
        &self,
        request: ReadRequest,
        status: Result<Status>,
        now: i64,
    ) -> Result<Option<Vec<u8>>> {
        if self.reporting.active().is_none() || request.validate(&self.id, now).is_err() {
            return Ok(None);
        }
        let (status, error) = match status {
            Ok(status) => (Some(status), None),
            Err(_) => (None, Some("upstream_unavailable".into())),
        };
        encode_signed(
            &ReadResult {
                v: 1,
                kind: "status.result".into(),
                device_id: self.id.clone(),
                request_id: request.request_id,
                observed_at: timestamp(now),
                status,
                error,
            },
            &self.signer,
        )
        .map(Some)
    }

    pub fn hello(&self, now: i64) -> Result<Vec<u8>> {
        encode_signed(
            &Hello {
                v: 1,
                kind: "reporting.hello".into(),
                device_id: self.id.clone(),
                connection_id: self.reporting.connection_id.clone(),
                observed_at: timestamp(now),
            },
            &self.signer,
        )
    }

    pub fn control(&mut self, bytes: &[u8], now: i64, instant: Instant) -> Result<Vec<u8>> {
        let control: ReportingControl = decode_signed(bytes, &self.verifier)?;
        self.reporting.accept(&control, &self.id, now, instant)?;
        encode_signed(
            &Ack {
                v: 1,
                kind: "reporting.ack".into(),
                device_id: self.id.clone(),
                permission: control.permission,
                observed_at: timestamp(now),
            },
            &self.signer,
        )
    }

    pub fn video_control(&self, bytes: &[u8], now: i64) -> Result<VideoControl> {
        let control: VideoControl = decode_signed(bytes, &self.verifier)?;
        control.validate(&self.id, &self.reporting.connection_id, now)?;
        Ok(control)
    }

    pub fn report(
        &mut self,
        sampled: &Permission,
        status: Result<Status>,
        observed: i64,
    ) -> Result<Option<Vec<u8>>> {
        let (status, error) = match status {
            Ok(status) => (Some(status), None),
            Err(_) => (None, Some("upstream_unavailable".into())),
        };
        self.reporting
            .report(&self.id, sampled, status, error, observed)
            .map(|report| encode_signed(&report, &self.signer))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use robot_cloud_common::{Control, State};

    const NOW: i64 = 1_793_616_000_000;

    fn fixture() -> (Device, SigningKey, ReadRequest) {
        let cloud = SigningKey::from_bytes(&[1; 32]);
        let id = "00000000-0000-4000-8000-000000000001".to_owned();
        let request = ReadRequest {
            v: 1,
            kind: "status.read".into(),
            device_id: id.clone(),
            request_id: "00000000-0000-4000-8000-000000000002".into(),
            issued_at: timestamp(NOW),
            expires_at: timestamp(NOW + 8000),
        };
        (
            Device {
                topics: topics(&id).unwrap(),
                id,
                signer: SigningKey::from_bytes(&[2; 32]),
                verifier: cloud.verifying_key(),
                seen: HashMap::new(),
                reporting: Reporting::new(),
            },
            cloud,
            request,
        )
    }

    fn enable(device: &mut Device, cloud: &SigningKey) {
        let instant = Instant::now();
        device.reporting.connect(NOW, instant);
        let control = ReportingControl {
            v: 1,
            kind: "reporting.control".into(),
            device_id: device.id.clone(),
            permission: Permission {
                authorization_epoch: 1,
                consent_version: 1,
                enabled: true,
                connection_id: Some(device.reporting.connection_id.clone()),
                reporting_session_id: Some(uuid::Uuid::new_v4().to_string()),
                expires_at: timestamp(NOW + 15_000),
            },
        };
        device
            .control(&encode_signed(&control, cloud).unwrap(), NOW, instant)
            .unwrap();
    }

    #[test]
    fn request_checks() {
        let (mut device, cloud, request) = fixture();
        let bytes = encode_signed(&request, &cloud).unwrap();
        assert!(device.accept(&bytes, NOW).unwrap().is_none());
        enable(&mut device, &cloud);
        assert!(device.accept(&bytes, NOW).unwrap().is_some());
        assert!(device.accept(&bytes, NOW).unwrap().is_none());
        assert!(device.accept(&bytes, NOW + 8000).is_err());
        let wrong_key = encode_signed(&request, &device.signer).unwrap();
        assert!(device.accept(&wrong_key, NOW).is_err());
    }

    #[test]
    fn cloud_motion_requests_are_rejected_before_read_admission() {
        let (mut device, cloud, mut request) = fixture();
        enable(&mut device, &cloud);
        assert!(device.reporting.active().is_some());
        for kind in [
            "motion",
            "teleop",
            "enable_motors",
            "/brainstem.api.v1.RobotControl/EnableMotors",
            "status.get",
        ] {
            request.kind = kind.into();
            let signed = encode_signed(&request, &cloud).unwrap();
            assert!(device.accept(&signed, NOW).is_err(), "accepted {kind}");
            assert!(device.seen.is_empty());
        }
        request.kind = "status.read".into();
        for field in ["method", "arguments", "command"] {
            let mut value = serde_json::to_value(&request).unwrap();
            value[field] = serde_json::json!("/brainstem.api.v1.RobotControl/EnableMotors");
            let signed = encode_signed(&value, &cloud).unwrap();
            assert!(device.accept(&signed, NOW).is_err(), "accepted {field}");
            assert!(device.seen.is_empty());
        }
        assert!(
            device
                .accept(&encode_signed(&request, &cloud).unwrap(), NOW)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn video_requires_signed_current_connection_without_status_permission() {
        let (mut device, cloud, _) = fixture();
        device.reporting.connect(NOW, Instant::now());
        let mut control = VideoControl {
            v: 1,
            kind: "video.control".into(),
            device_id: device.id.clone(),
            connection_id: device.reporting.connection_id.clone(),
            seq: 1,
            enabled: true,
            expires_at: timestamp(NOW + robot_cloud_common::video::LEASE_MS),
        };
        assert!(
            device
                .video_control(&encode_signed(&control, &cloud).unwrap(), NOW)
                .is_ok()
        );
        assert!(
            device
                .video_control(&encode_signed(&control, &device.signer).unwrap(), NOW)
                .is_err()
        );
        control.connection_id = uuid::Uuid::new_v4().to_string();
        assert!(
            device
                .video_control(&encode_signed(&control, &cloud).unwrap(), NOW)
                .is_err()
        );
    }

    #[test]
    fn expired_permission_blocks_one_shot_reads_and_replies() {
        let (mut device, cloud, request) = fixture();
        enable(&mut device, &cloud);
        device.reporting.expire(
            NOW + 15_000,
            Instant::now() + std::time::Duration::from_secs(16),
        );
        assert!(
            device
                .accept(&encode_signed(&request, &cloud).unwrap(), NOW)
                .unwrap()
                .is_none()
        );
        assert!(
            device
                .reply(
                    request,
                    Err(std::io::Error::other("unavailable").into()),
                    NOW
                )
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn signed_reply() {
        let (mut device, cloud, request) = fixture();
        enable(&mut device, &cloud);
        let status = Status {
            state: State {
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
        };
        let bytes = device
            .reply(request.clone(), Ok(status.clone()), NOW)
            .unwrap()
            .unwrap();
        let reply: ReadResult = decode_signed(&bytes, &device.signer.verifying_key()).unwrap();
        reply
            .validate(&device.id, &request.request_id, NOW)
            .unwrap();
        assert_eq!(reply.status, Some(status));
        let bytes = device
            .reply(
                request.clone(),
                Err(std::io::Error::other("private error detail").into()),
                NOW,
            )
            .unwrap()
            .unwrap();
        let reply: ReadResult = decode_signed(&bytes, &device.signer.verifying_key()).unwrap();
        assert_eq!(reply.error.as_deref(), Some("upstream_unavailable"));
    }

    #[test]
    fn late_reply() {
        let (mut device, cloud, request) = fixture();
        enable(&mut device, &cloud);
        assert!(
            device
                .reply(
                    request,
                    Err(std::io::Error::other("timeout").into()),
                    NOW + 8000
                )
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn signed_control_echoes_permission_and_disable_blocks_reads() {
        let (mut device, cloud, request) = fixture();
        let instant = Instant::now();
        device.reporting.connect(NOW, instant);
        let hello: Hello =
            decode_signed(&device.hello(NOW).unwrap(), &device.signer.verifying_key()).unwrap();
        hello.validate(&device.id, NOW).unwrap();
        assert_eq!(hello.connection_id, device.reporting.connection_id);
        let mut control = ReportingControl {
            v: 1,
            kind: "reporting.control".into(),
            device_id: device.id.clone(),
            permission: Permission {
                authorization_epoch: 1,
                consent_version: 1,
                enabled: true,
                connection_id: Some(hello.connection_id),
                reporting_session_id: Some(uuid::Uuid::new_v4().to_string()),
                expires_at: timestamp(NOW + 10_000),
            },
        };
        let forged = encode_signed(&control, &device.signer).unwrap();
        assert!(device.control(&forged, NOW, instant).is_err());
        assert!(device.reporting.active().is_none());
        let bytes = encode_signed(&control, &cloud).unwrap();
        let ack: Ack = decode_signed(
            &device.control(&bytes, NOW, instant).unwrap(),
            &device.signer.verifying_key(),
        )
        .unwrap();
        ack.validate(&device.id, NOW).unwrap();
        assert_eq!(ack.permission, control.permission);
        let bytes = device
            .report(
                &control.permission,
                Err(std::io::Error::other("private details").into()),
                NOW,
            )
            .unwrap()
            .unwrap();
        let report: robot_cloud_common::reporting::Report =
            decode_signed(&bytes, &device.signer.verifying_key()).unwrap();
        report.validate(&device.id, NOW).unwrap();
        assert_eq!(report.error.as_deref(), Some("upstream_unavailable"));
        assert_eq!(report.observed_at, timestamp(NOW));
        assert_eq!(report.seq, 1);
        control.permission.consent_version = 2;
        control.permission.enabled = false;
        let bytes = encode_signed(&control, &cloud).unwrap();
        device.control(&bytes, NOW, instant).unwrap();
        assert!(
            device
                .accept(&encode_signed(&request, &cloud).unwrap(), NOW)
                .unwrap()
                .is_none()
        );
        assert!(
            device
                .reply(request, Err(std::io::Error::other("cancelled").into()), NOW)
                .unwrap()
                .is_none()
        );
    }
}
