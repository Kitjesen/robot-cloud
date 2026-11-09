//! SIMULATED business API. Memory only, with no account service or real database.
use anyhow::{Result, ensure};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::post,
};
use robot_cloud_common::{
    now_ms, parse_time,
    reporting::{Permission, Report, recent},
    timestamp,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

// Accelerated lab lease. Production uses 30 seconds and a 15-second renewal threshold.
pub const LEASE_MS: i64 = 8_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Received {
    pub report: Report,
    pub received_at: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Decision {
    authorization_epoch: u64,
    consent_version: u64,
    desired_enabled: bool,
    device_confirmed: bool,
    reporting_session_id: Option<String>,
    expires_at: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Device {
    pub device_id: String,
    pub connection_id: Option<String>,
    last_seen_at: Option<String>,
    reachable: bool,
    permission: Option<Decision>,
    pub latest: Option<Received>,
    session_reusable: bool,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Batch {
    v: u8,
    observed_at: String,
    pub devices: Vec<Device>,
}

#[derive(Default)]
pub struct Business {
    pub desired: bool,
    version: u64,
    pub issued: Option<Permission>,
    frozen: bool,
    watermark: i64,
    pub latest: Option<Received>,
    pub last_batch: Option<Batch>,
    pub calls: usize,
    pub rejected_reports: usize,
    pub renewals: usize,
    pub unavailable: bool,
    pub wire: BTreeMap<String, serde_json::Value>,
}

impl Business {
    pub fn choose(&mut self, enabled: bool) {
        self.desired = enabled;
        self.version += 1;
        self.issued = None;
        self.frozen = false;
    }

    fn save(&mut self, device: &Device, now: i64) -> Result<()> {
        let Some(received) = &device.latest else {
            return Ok(());
        };
        let report = &received.report;
        let allowed = self.desired
            && self.issued.as_ref().is_some_and(|issued| {
                issued.matches_report(report)
                    && parse_time(&issued.expires_at).is_ok_and(|end| end > now)
            });
        if !allowed {
            self.rejected_reports += 1;
            return Ok(());
        }
        report.validate(&device.device_id, now)?;
        recent(&received.received_at, now)?;
        ensure!(
            parse_time(&report.observed_at)? <= parse_time(&received.received_at)?,
            "invalid report times"
        );
        if report.status.is_some()
            && self.latest.as_ref().is_none_or(|old| {
                old.report.reporting_session_id != report.reporting_session_id
                    || old.report.seq < report.seq
            })
        {
            self.latest = Some(received.clone());
        }
        Ok(())
    }

    fn permission(&mut self, device: &Device, now: i64) -> Result<Option<Permission>> {
        if self.version == 0 {
            return Ok(None);
        }
        if !self.desired {
            let permission = self.issued.get_or_insert_with(|| Permission {
                authorization_epoch: 1,
                consent_version: self.version,
                enabled: false,
                connection_id: None,
                reporting_session_id: None,
                expires_at: timestamp(now + LEASE_MS),
            });
            if parse_time(&permission.expires_at)? <= now {
                permission.expires_at = timestamp(now + LEASE_MS);
            }
            return Ok(Some(permission.clone()));
        }
        let Some(connection) = device.connection_id.as_ref().filter(|_| device.reachable) else {
            return Ok(None);
        };
        if let Some(issued) = &mut self.issued
            && issued.connection_id.as_ref() == Some(connection)
        {
            let end = parse_time(&issued.expires_at)?;
            let matching = device.permission.as_ref().is_some_and(|projection| {
                projection.authorization_epoch == issued.authorization_epoch
                    && projection.consent_version == issued.consent_version
                    && projection.desired_enabled
                    && projection.reporting_session_id == issued.reporting_session_id
            });
            self.frozen |= matching && !device.session_reusable;
            if now < end {
                let exact = matching
                    && device
                        .permission
                        .as_ref()
                        .is_some_and(|p| p.expires_at.as_ref() == Some(&issued.expires_at));
                if self.frozen {
                    return Ok(None);
                }
                if exact
                    && device
                        .permission
                        .as_ref()
                        .is_some_and(|p| p.device_confirmed)
                    && end - now <= LEASE_MS / 2
                {
                    issued.expires_at = timestamp(now + LEASE_MS);
                    self.renewals += 1;
                }
                return Ok(Some(issued.clone()));
            }
        }
        self.frozen = false;
        let permission = Permission {
            authorization_epoch: 1,
            consent_version: self.version,
            enabled: true,
            connection_id: Some(connection.clone()),
            reporting_session_id: Some(uuid::Uuid::new_v4().to_string()),
            expires_at: timestamp(now + LEASE_MS),
        };
        permission.validate(now)?;
        self.issued = Some(permission.clone());
        Ok(Some(permission))
    }

    fn sync(&mut self, batch: Batch, now: i64) -> Result<serde_json::Value> {
        ensure!(
            batch.v == 1 && batch.devices.len() == 1,
            "SIMULATED API expects one device"
        );
        recent(&batch.observed_at, now)?;
        let observed = parse_time(&batch.observed_at)?;
        ensure!(observed >= self.watermark, "stale batch");
        self.watermark = observed;
        let device = &batch.devices[0];
        ensure!(
            robot_cloud_common::is_uuid(&device.device_id),
            "invalid device"
        );
        self.save(device, now)?;
        let permission = self.permission(device, now)?;
        let response = serde_json::json!({"v":1,"permissions":[{"deviceId":device.device_id,"permission":permission}]});
        let phase = if self.version == 0 {
            "defaultOff"
        } else if !self.desired {
            "closed"
        } else if self.renewals > 0 {
            "renewed"
        } else if device.latest.is_some() {
            "confirmed"
        } else {
            "enabled"
        };
        self.wire
            .entry(phase.to_owned())
            .or_insert_with(|| serde_json::json!({"request":batch,"response":response}));
        self.last_batch = Some(batch);
        Ok(response)
    }
}

#[derive(Clone)]
struct Api {
    key: String,
    business: Arc<Mutex<Business>>,
}

async fn sync(
    State(api): State<Api>,
    headers: HeaderMap,
    Json(batch): Json<Batch>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if headers.get("apikey").and_then(|value| value.to_str().ok()) != Some(&api.key) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let mut business = api.business.lock().unwrap();
    business.calls += 1;
    if business.unavailable {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    business
        .sync(batch, now_ms())
        .map(Json)
        .map_err(|_| StatusCode::BAD_REQUEST)
}

pub struct Simulated {
    pub url: String,
    pub key: String,
    pub business: Arc<Mutex<Business>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Simulated {
    pub async fn start() -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!(
            "http://{}/functions/v1/remote-state-sync",
            listener.local_addr()?
        );
        let key = uuid::Uuid::new_v4().to_string();
        let business = Arc::new(Mutex::new(Business::default()));
        let app = Router::new()
            .route("/functions/v1/remote-state-sync", post(sync))
            .layer(DefaultBodyLimit::max(128 * 1024))
            .with_state(Api {
                key: key.clone(),
                business: business.clone(),
            });
        let task = tokio::spawn(async move { axum::serve(listener, app).await });
        Ok(Self {
            url,
            key,
            business,
            task,
        })
    }
}

impl Drop for Simulated {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> Device {
        Device {
            device_id: uuid::Uuid::new_v4().to_string(),
            connection_id: Some(uuid::Uuid::new_v4().to_string()),
            last_seen_at: None,
            reachable: true,
            permission: None,
            latest: None,
            session_reusable: false,
        }
    }

    fn project(device: &mut Device, permission: &Permission, reusable: bool) {
        device.permission = Some(Decision {
            authorization_epoch: permission.authorization_epoch,
            consent_version: permission.consent_version,
            desired_enabled: permission.enabled,
            device_confirmed: false,
            reporting_session_id: permission.reporting_session_id.clone(),
            expires_at: Some(permission.expires_at.clone()),
        });
        device.session_reusable = reusable;
    }

    #[test]
    fn exact_projection_and_maximum_deadline_bound_renewal() {
        let now = now_ms();
        let mut business = Business::default();
        let mut device = device();
        assert!(business.permission(&device, now).unwrap().is_none());
        business.choose(true);
        let first = business.permission(&device, now).unwrap().unwrap();
        assert_eq!(
            business.permission(&device, now + 5_000).unwrap(),
            Some(first.clone())
        );
        project(&mut device, &first, true);
        assert_eq!(
            business.permission(&device, now + 5_000).unwrap(),
            Some(first.clone())
        );
        device.permission.as_mut().unwrap().device_confirmed = true;
        let renewed = business.permission(&device, now + 5_000).unwrap().unwrap();
        assert_eq!(
            business.permission(&device, now + 6_000).unwrap(),
            Some(renewed.clone())
        );
        project(&mut device, &first, false);
        assert!(business.permission(&device, now + 9_000).unwrap().is_none());
        assert!(
            business
                .permission(&device, now + 12_999)
                .unwrap()
                .is_none()
        );
        let replacement = business.permission(&device, now + 13_000).unwrap().unwrap();
        assert_ne!(replacement.reporting_session_id, first.reporting_session_id);
        device.connection_id = Some(uuid::Uuid::new_v4().to_string());
        let restarted = business.permission(&device, now + 13_001).unwrap().unwrap();
        assert_ne!(
            restarted.reporting_session_id,
            replacement.reporting_session_id
        );
        assert_eq!(restarted.consent_version, first.consent_version);
    }
}
