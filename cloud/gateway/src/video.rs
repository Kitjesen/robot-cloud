use crate::{Error, Gateway};
use robot_cloud_common::{
    encode_signed, now_ms, timestamp, topics,
    video::{Control, LEASE_MS},
};
use rumqttc::v5::mqttbytes::QoS;
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Accepted {
    accepted: bool,
    enabled: bool,
    expires_at: String,
}

impl Gateway {
    pub fn set_video(&self, id: &str, enabled: bool) -> Result<Accepted, Error> {
        let latest = self.latest(id)?;
        if !latest.reachable {
            return Err(Error::Unavailable);
        }
        let connection_id = latest.connection_id.ok_or(Error::Unavailable)?;
        let now = now_ms();
        let expires_at = timestamp(now + LEASE_MS);
        let mut sequences = self.video_seq.lock().unwrap();
        let previous = sequences.get(id).copied().unwrap_or(0);
        // shortcut: after clock rollback, renewals may wait for time to catch up; persist counters if clock resets are supported.
        let seq = (now.max(1) as u64).max(previous.checked_add(1).ok_or(Error::Unavailable)?);
        let control = Control {
            v: 1,
            kind: "video.control".into(),
            device_id: id.into(),
            connection_id,
            seq,
            enabled,
            expires_at: expires_at.clone(),
        };
        control
            .validate(id, &control.connection_id, now)
            .map_err(|_| Error::Unavailable)?;
        let payload = encode_signed(&control, &self.signing_key).map_err(|_| Error::Unavailable)?;
        self.client
            .try_publish(
                topics(id).map_err(|_| Error::InvalidDevice)?.video,
                QoS::AtLeastOnce,
                false,
                payload,
            )
            .map_err(|_| Error::Unavailable)?;
        sequences.insert(id.into(), seq);
        Ok(Accepted {
            accepted: true,
            enabled,
            expires_at,
        })
    }
}
