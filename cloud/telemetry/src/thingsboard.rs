use crate::{
    export::Item,
    http::{Http, endpoint},
};
use anyhow::{Context, Result, ensure};
use reqwest::{Method, StatusCode, Url};
use robot_cloud_common::{
    is_uuid, parse_time,
    reporting::{MAX_COUNTER, Report, valid_version},
};
use serde::Deserialize;
use serde_json::{Value, json};

pub struct ThingsBoard {
    http: Http,
    url: Url,
}

#[derive(Deserialize)]
struct Device {
    id: DeviceId,
    name: String,
}

#[derive(Deserialize)]
struct DeviceId {
    id: String,
}

impl ThingsBoard {
    pub fn new(url: &str, key: &str) -> Result<Self> {
        let url = endpoint(url)?;
        ensure!(url.path() == "/", "invalid_thingsboard_url");
        Ok(Self {
            http: Http::new("x-authorization", key)?,
            url,
        })
    }

    pub async fn device(&self, item: &Item) -> Result<String> {
        if let Some(id) = &item.tb_device_id {
            ensure!(is_uuid(id), "invalid_platform_device");
            return Ok(id.clone());
        }
        let name = format!("doso:{}:{}", item.device_id, item.authorization_epoch);
        if let Some(id) = self.find(&name).await? {
            return Ok(id);
        }
        let (status, body) = self
            .http
            .send(
                Method::POST,
                self.url.join("api/device")?,
                Some(json!({"name": name, "type": "DOSO"})),
            )
            .await?;
        if status.is_success() {
            return device_id(&body, &name);
        }
        // A previous attempt may have created the device before its response was lost.
        self.find(&name)
            .await
            .with_context(|| format!("platform_device_create http_status={}", status.as_u16()))?
            .ok_or_else(|| {
                anyhow::anyhow!("platform_device_create http_status={}", status.as_u16())
            })
    }

    async fn find(&self, name: &str) -> Result<Option<String>> {
        let mut url = self.url.join("api/tenant/device")?;
        url.query_pairs_mut().append_pair("deviceName", name);
        let (status, body) = self
            .http
            .send(Method::GET, url, None)
            .await
            .context("platform_device_find")?;
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        ensure!(
            status.is_success(),
            "platform_device_find http_status={}",
            status.as_u16()
        );
        device_id(&body, name).map(Some)
    }

    pub async fn attributes(&self, item: &Item, device: &str) -> Result<()> {
        self.post(
            &format!("api/plugins/telemetry/DEVICE/{device}/attributes/SERVER_SCOPE"),
            json!({
                "robotId": item.device_id, "authorizationEpoch": item.authorization_epoch,
                "simulated": item.simulated,
            }),
        )
        .await
    }

    pub async fn conflicts(&self, item: &Item, device: &str) -> Result<bool> {
        let value = projection(item)?;
        let ts = value["ts"].as_i64().unwrap();
        let keys = [
            "seq",
            "connectionId",
            "reportingSessionId",
            "authorizationEpoch",
            "consentVersion",
        ];
        let mut url = self.url.join(&format!(
            "api/plugins/telemetry/DEVICE/{device}/values/timeseries"
        ))?;
        url.query_pairs_mut()
            .append_pair("keys", &keys.join(","))
            .append_pair("startTs", &ts.to_string())
            .append_pair("endTs", &ts.saturating_add(1).to_string())
            .append_pair("agg", "NONE")
            .append_pair("limit", "2");
        let (status, body) = self
            .http
            .send(Method::GET, url, None)
            .await
            .context("platform_history")?;
        ensure!(
            status.is_success(),
            "platform_history http_status={}",
            status.as_u16()
        );
        let history: Value = serde_json::from_slice(&body)
            .map_err(|_| anyhow::anyhow!("invalid_platform_history"))?;
        let history = history
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("invalid_platform_history"))?;
        let mut found = 0;
        for key in keys {
            let Some(points) = history.get(key) else {
                continue;
            };
            let points = points
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("invalid_platform_history"))?;
            if points.is_empty() {
                continue;
            }
            ensure!(
                points.len() == 1 && points[0]["ts"].as_i64() == Some(ts),
                "invalid_platform_history"
            );
            let expected = &value["values"][key];
            let expected = expected
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| expected.to_string());
            if points[0]["value"].as_str() != Some(expected.as_str()) {
                return Ok(true);
            }
            found += 1;
        }
        Ok(found != 0 && found != keys.len())
    }

    pub async fn write(&self, item: &Item, device: &str) -> Result<()> {
        self.post(
            &format!("api/plugins/telemetry/DEVICE/{device}/timeseries/ANY"),
            projection(item)?,
        )
        .await
    }

    async fn post(&self, path: &str, body: Value) -> Result<()> {
        let (status, _) = self
            .http
            .send(Method::POST, self.url.join(path)?, Some(body))
            .await?;
        ensure!(
            status.is_success(),
            "platform_write_failed http_status={}",
            status.as_u16()
        );
        Ok(())
    }
}

fn device_id(body: &[u8], name: &str) -> Result<String> {
    let device: Device =
        serde_json::from_slice(body).map_err(|_| anyhow::anyhow!("invalid_platform_device"))?;
    ensure!(
        device.name == name && is_uuid(&device.id.id),
        "invalid_platform_device"
    );
    Ok(device.id.id)
}

pub fn projection(item: &Item) -> Result<Value> {
    let report: Report = serde_json::from_value(item.received.report.clone())?;
    ensure!(
        report.v == 1
            && report.kind == "status.report"
            && report.device_id == item.device_id
            && report.authorization_epoch == item.authorization_epoch
            && report.consent_version == item.consent_version
            && is_uuid(&report.device_id)
            && is_uuid(&report.connection_id)
            && is_uuid(&report.reporting_session_id)
            && valid_version(report.authorization_epoch, report.consent_version)
            && (1..=MAX_COUNTER).contains(&report.seq),
        "invalid_export_report"
    );
    let observed = parse_time(&report.observed_at)?;
    ensure!(observed >= 0, "invalid_export_time");
    parse_time(&item.received.received_at)?;
    let mut values = json!({
        "robotId": report.device_id, "authorizationEpoch": report.authorization_epoch,
        "consentVersion": report.consent_version, "connectionId": report.connection_id,
        "reportingSessionId": report.reporting_session_id, "seq": report.seq,
        "observedAt": report.observed_at, "receivedAt": item.received.received_at,
        "sourceReport": serde_json::to_string(&item.received.report)?,
    });
    match (&report.status, &report.error) {
        (Some(status), None) => {
            status.validate()?;
            values["sampleOk"] = json!(true);
            values["readError"] = json!("");
            values["stateKind"] = json!(status.state.kind);
            values["stateTransition"] = json!(status.state.transition);
            values["controlOwner"] = json!(status.control.owner);
            values["motorOutputEnabled"] = json!(status.control.motor_output_enabled);
            values["criticalMotorFault"] = json!(status.control.critical_motor_fault);
            values["transport"] = json!(status.transport);
        }
        (None, Some(error)) if error == "upstream_unavailable" => {
            values["sampleOk"] = json!(false);
            values["readError"] = json!(error);
        }
        _ => anyhow::bail!("invalid_export_report"),
    }
    Ok(json!({"ts": observed, "values": values}))
}
