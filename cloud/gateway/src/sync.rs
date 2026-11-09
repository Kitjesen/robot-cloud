use crate::{
    Gateway,
    reporting::{Decision, Received},
};
use reqwest::{
    Client, Url,
    header::{HeaderMap, HeaderValue},
};
use robot_cloud_common::{now_ms, reporting::Permission, timestamp};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, env, net::IpAddr, sync::Arc, time::Duration};

const BODY_LIMIT: usize = 128 * 1024;
const BATCH_SIZE: usize = 32;

pub struct SyncConfig {
    client: Client,
    url: Url,
}

impl SyncConfig {
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let url = env::var("DOSO_REMOTE_SYNC_URL");
        let key = env::var("DOSO_REMOTE_SYNC_KEY");
        match (url, key) {
            (Err(env::VarError::NotPresent), Err(env::VarError::NotPresent)) => {
                Self::configured(None, None)
            }
            (Ok(url), Ok(key)) => Self::configured(Some(&url), Some(&key)),
            _ => anyhow::bail!("invalid_sync_configuration"),
        }
    }

    fn configured(url: Option<&str>, key: Option<&str>) -> anyhow::Result<Option<Self>> {
        match (url, key) {
            (None, None) => Ok(None),
            (Some(url), Some(key)) => Self::new(url, key).map(Some),
            _ => anyhow::bail!("invalid_sync_configuration"),
        }
    }

    pub(crate) fn new(url: &str, key: &str) -> anyhow::Result<Self> {
        let url = Url::parse(url).map_err(|_| anyhow::anyhow!("invalid_sync_configuration"))?;
        let loopback = url.host_str().is_some_and(|host| {
            host.trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
        });
        anyhow::ensure!(
            url.host_str().is_some()
                && (url.scheme() == "https" || (url.scheme() == "http" && loopback))
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none()
                && url.query().is_none()
                && (32..=256).contains(&key.len())
                && key.bytes().all(|byte| (33..=126).contains(&byte)),
            "invalid_sync_configuration"
        );
        let mut headers = HeaderMap::new();
        let mut key = HeaderValue::from_str(key)
            .map_err(|_| anyhow::anyhow!("invalid_sync_configuration"))?;
        key.set_sensitive(true);
        headers.insert("apikey", key);
        let client = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|_| anyhow::anyhow!("invalid_sync_configuration"))?;
        Ok(Self { client, url })
    }

    pub(crate) async fn sync_batch(&self, gateway: &Gateway, ids: &[String]) -> anyhow::Result<()> {
        anyhow::ensure!(ids.len() <= BATCH_SIZE, "invalid_sync_batch");
        let devices = ids
            .iter()
            .map(|id| {
                let (latest, session_reusable) = gateway
                    .sync_snapshot(id)
                    .map_err(|_| anyhow::anyhow!("sync_snapshot_failed"))?;
                Ok(DeviceSnapshot {
                    device_id: latest.device_id,
                    connection_id: latest.connection_id,
                    last_seen_at: latest.last_seen_at,
                    reachable: latest.reachable,
                    permission: latest.permission,
                    latest: latest.latest,
                    session_reusable,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let body = serde_json::to_vec(&Batch {
            v: 1,
            observed_at: timestamp(now_ms()),
            devices,
        })?;
        anyhow::ensure!(body.len() <= BODY_LIMIT, "sync_body_too_large");
        let mut response = self
            .client
            .post(self.url.clone())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await?;
        anyhow::ensure!(response.status().is_success(), "sync_source_failed");
        anyhow::ensure!(
            response
                .content_length()
                .is_none_or(|length| length <= BODY_LIMIT as u64),
            "sync_body_too_large"
        );
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            anyhow::ensure!(
                chunk.len() <= BODY_LIMIT - body.len(),
                "sync_body_too_large"
            );
            body.extend_from_slice(&chunk);
        }
        let response = parse_response(&body, ids)?;
        for item in response.permissions {
            if let Some(permission) = item.permission
                && gateway.set_reporting(&item.device_id, permission).is_err()
            {
                eprintln!("robot_cloud_sync_permission_failed");
            }
        }
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Batch {
    v: u8,
    observed_at: String,
    devices: Vec<DeviceSnapshot>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceSnapshot {
    device_id: String,
    connection_id: Option<String>,
    last_seen_at: Option<String>,
    reachable: bool,
    permission: Option<Decision>,
    latest: Option<Received>,
    session_reusable: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    v: u8,
    permissions: Vec<Grant>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Grant {
    device_id: String,
    permission: Option<Permission>,
}

fn parse_response(body: &[u8], ids: &[String]) -> anyhow::Result<Response> {
    anyhow::ensure!(body.len() <= BODY_LIMIT, "sync_body_too_large");
    let response: Response = serde_json::from_slice(body)?;
    anyhow::ensure!(
        response.v == 1 && response.permissions.len() <= ids.len(),
        "invalid_sync_response"
    );
    let mut seen = HashSet::new();
    for grant in &response.permissions {
        anyhow::ensure!(
            ids.contains(&grant.device_id) && seen.insert(&grant.device_id),
            "invalid_sync_response"
        );
    }
    Ok(response)
}

pub async fn run_sync(gateway: Arc<Gateway>, config: SyncConfig) {
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let Ok(registry) = gateway.registry() else {
            eprintln!("robot_cloud_sync_registry_failed");
            continue;
        };
        let ids: Vec<_> = registry
            .devices
            .into_iter()
            .filter(|device| device.enabled)
            .map(|device| device.device_id)
            .collect();
        for batch in ids.chunks(BATCH_SIZE) {
            if config.sync_batch(&gateway, batch).await.is_err() {
                eprintln!("robot_cloud_sync_failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_url_and_credential() {
        let key = "x".repeat(32);
        assert!(SyncConfig::configured(None, None).unwrap().is_none());
        assert!(SyncConfig::configured(Some("https://example.com/"), None).is_err());
        assert!(SyncConfig::configured(None, Some(&key)).is_err());
        for url in [
            "https://example.com/functions/v1/remote-state-sync",
            "http://127.0.0.1:8080/",
            "http://[::1]:8080/",
        ] {
            assert!(SyncConfig::new(url, &key).is_ok(), "{url}");
        }
        for url in [
            "http://example.com/",
            "http://localhost/",
            "https://user:pass@example.com/",
            "https://example.com/#secret",
            "https://example.com/?secret=x",
            "file:///tmp/x",
        ] {
            assert!(SyncConfig::new(url, &key).is_err(), "{url}");
        }
        for key in [
            "".into(),
            "x".repeat(31),
            "x".repeat(257),
            format!("{}\r\n", "x".repeat(32)),
            " ".repeat(32),
        ] {
            assert!(SyncConfig::new("https://example.com/", &key).is_err());
        }
    }

    #[test]
    fn validates_whole_response_before_apply() {
        let id = uuid::Uuid::new_v4().to_string();
        let ids = vec![id.clone()];
        for value in [
            json!({"v": 1, "permissions": [{"deviceId": id, "permission": null}]}),
            json!({"v": 1, "permissions": [{"deviceId": id}]}),
            json!({"v": 1, "permissions": []}),
        ] {
            assert!(parse_response(&serde_json::to_vec(&value).unwrap(), &ids).is_ok());
        }
        for value in [
            json!({"v": 2, "permissions": []}),
            json!({"v": 1, "permissions": [], "extra": true}),
            json!({"v": 1, "permissions": [{"deviceId": "other", "permission": null}]}),
            json!({"v": 1, "permissions": [{"deviceId": id}, {"deviceId": id}]}),
            json!({"v": 1, "permissions": [{"deviceId": id, "permission": {}}]}),
        ] {
            assert!(parse_response(&serde_json::to_vec(&value).unwrap(), &ids).is_err());
        }
        assert!(parse_response(&vec![b' '; BODY_LIMIT + 1], &ids).is_err());
    }
}
