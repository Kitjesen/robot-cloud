use crate::{Config, Grant, Media, loopback};
use anyhow::{Result, ensure};
use reqwest::Url;
use robot_cloud_common::now_ms;
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

pub(super) struct Demand {
    gateway: Option<(Url, String)>,
    pub warm: HashMap<(String, String), i64>,
    started: HashMap<String, i64>,
}

impl Demand {
    pub fn new(config: &Config) -> Result<Self> {
        let gateway = match (&config.source_gateway_url, &config.source_gateway_key) {
            (None, None) => None,
            (Some(url), Some(key)) => {
                let url = Url::parse(url)?;
                ensure!(
                    (url.scheme() == "https" || url.scheme() == "http" && loopback(&url))
                        && url.path() == "/"
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.query().is_none()
                        && url.fragment().is_none(),
                    "source Gateway requires an HTTPS or loopback origin"
                );
                ensure!(
                    (32..=256).contains(&key.len()) && key.bytes().all(|b| (33..=126).contains(&b)),
                    "invalid source Gateway credential"
                );
                Some((url, key.clone()))
            }
            _ => anyhow::bail!("source Gateway URL and credential must be configured together"),
        };
        Ok(Self {
            gateway,
            warm: HashMap::new(),
            started: HashMap::new(),
        })
    }
}

impl Media {
    pub(super) async fn warm_source(&self, grant: &Grant) -> Result<()> {
        let mut demand = self.demand.lock().await;
        if demand.gateway.is_none() {
            return Ok(());
        }
        demand.warm.insert(
            (grant.viewer_id.clone(), grant.device_id.clone()),
            grant.expires_at.min(now_ms() + 10_000),
        );
        drop(demand);
        self.sync_source(&grant.device_id).await
    }

    pub(super) async fn sync_source(&self, device: &str) -> Result<()> {
        // Serialize the decision and its HTTP call so a late stop cannot overtake a new viewer.
        let mut demand = self.demand.lock().await;
        let Some((gateway, key)) = demand.gateway.clone() else {
            return Ok(());
        };
        let now = now_ms();
        demand.warm.retain(|_, expires| *expires > now);
        let wanted = demand.warm.keys().any(|(_, id)| id == device)
            || self.sessions.lock().await.values().any(|session| {
                session.grant.device_id == device
                    && !session.closing
                    && session.grant.expires_at > now
            });
        let previous = demand.started.get(device).copied();
        if wanted && previous.is_some_and(|sent| now - sent < 5_000)
            || !wanted && previous.is_none()
        {
            return Ok(());
        }
        let response = self
            .client
            .put(gateway.join(&format!("api/v1/robots/{device}/video"))?)
            .bearer_auth(key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(&json!({"enabled": wanted}))?)
            .timeout(Duration::from_secs(2))
            .send()
            .await?;
        ensure!(
            response.status() == reqwest::StatusCode::ACCEPTED,
            "source control unavailable"
        );
        if wanted {
            demand.started.insert(device.into(), now);
        } else {
            demand.started.remove(device);
        }
        Ok(())
    }

    pub(super) async fn sync_sources(&self) {
        let mut devices: HashSet<_> = self
            .sessions
            .lock()
            .await
            .values()
            .map(|s| s.grant.device_id.clone())
            .collect();
        {
            let demand = self.demand.lock().await;
            devices.extend(demand.started.keys().cloned());
            devices.extend(demand.warm.keys().map(|(_, device)| device.clone()));
        }
        for device in devices {
            if let Err(error) = self.sync_source(&device).await {
                eprintln!("video_source_control_error device={device} reason={error}");
            }
        }
    }
}
