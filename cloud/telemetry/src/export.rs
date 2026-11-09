use crate::{
    http::{Http, endpoint},
    thingsboard::{ThingsBoard, projection},
};
use anyhow::{Context, Result, ensure};
use reqwest::{Method, Url};
use robot_cloud_common::{is_uuid, reporting::valid_version};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Received {
    pub report: Value,
    pub received_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Item {
    pub id: String,
    pub lease_token: String,
    pub device_id: String,
    pub owner_id: String,
    pub simulated: bool,
    pub authorization_epoch: u64,
    pub consent_version: u64,
    pub tb_device_id: Option<String>,
    pub received: Received,
}

impl Item {
    fn validate(&self) -> Result<()> {
        ensure!(
            [&self.id, &self.lease_token, &self.device_id, &self.owner_id]
                .into_iter()
                .all(|id| is_uuid(id))
                && self.tb_device_id.as_deref().is_none_or(is_uuid)
                && valid_version(self.authorization_epoch, self.consent_version),
            "invalid_export_item"
        );
        projection(self)?;
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Claim {
    v: u8,
    items: Vec<Item>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Check {
    v: u8,
    allowed: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Finish {
    v: u8,
    accepted: bool,
}

pub struct Worker {
    http: Http,
    url: Url,
    platform: ThingsBoard,
}

enum Delivery {
    Delivered,
    Cancelled,
    Conflict,
}

impl Worker {
    pub fn new(url: &str, key: &str, platform_url: &str, platform_key: &str) -> Result<Self> {
        Ok(Self {
            http: Http::new("apikey", key)?,
            url: endpoint(url)?,
            platform: ThingsBoard::new(platform_url, platform_key)?,
        })
    }

    async fn call<T: serde::de::DeserializeOwned>(&self, body: Value) -> Result<T> {
        let (status, bytes) = self
            .http
            .send(Method::POST, self.url.clone(), Some(body))
            .await?;
        ensure!(
            status.is_success(),
            "export_source_failed http_status={}",
            status.as_u16()
        );
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid_export_response"))
    }

    async fn check(&self, item: &Item, device: Option<&str>) -> Result<bool> {
        let result: Check = self.call(json!({"v":1,"action":"check","id":item.id,"leaseToken":item.lease_token,"tbDeviceId":device})).await.context("export_check")?;
        ensure!(result.v == 1, "invalid_export_response");
        Ok(result.allowed)
    }

    async fn finish(&self, item: &Item, outcome: &str) -> Result<()> {
        let result: Finish = self.call(json!({"v":1,"action":"finish","id":item.id,"leaseToken":item.lease_token,"outcome":outcome})).await.context("export_finish")?;
        ensure!(result.v == 1 && result.accepted, "export_finish_failed");
        Ok(())
    }

    async fn deliver(&self, item: &Item) -> Result<Delivery> {
        if !self.check(item, None).await? {
            return Ok(Delivery::Cancelled);
        }
        let device = self
            .platform
            .device(item)
            .await
            .context("platform_device")?;
        if !self.check(item, Some(&device)).await? {
            return Ok(Delivery::Cancelled);
        }
        if self.platform.conflicts(item, &device).await? {
            return Ok(Delivery::Conflict);
        }
        self.platform
            .attributes(item, &device)
            .await
            .context("platform_attributes")?;
        if !self.check(item, Some(&device)).await? {
            return Ok(Delivery::Cancelled);
        }
        self.platform
            .write(item, &device)
            .await
            .context("platform_sample")?;
        Ok(Delivery::Delivered)
    }

    pub async fn step(&self) -> Result<bool> {
        let mut claim: Claim = self
            .call(json!({"v":1,"action":"claim"}))
            .await
            .context("export_claim")?;
        ensure!(
            claim.v == 1 && claim.items.len() <= 1,
            "invalid_export_response"
        );
        let Some(item) = claim.items.pop() else {
            return Ok(false);
        };
        if item.validate().is_err() {
            if is_uuid(&item.id) && is_uuid(&item.lease_token) {
                self.finish(&item, "conflict").await?;
            }
            anyhow::bail!("invalid_export_item");
        }
        match self.deliver(&item).await {
            Ok(Delivery::Delivered) => self.finish(&item, "delivered").await?,
            Ok(Delivery::Conflict) => self.finish(&item, "conflict").await?,
            Ok(Delivery::Cancelled) => return Ok(true),
            Err(error) => {
                self.finish(&item, "retry")
                    .await
                    .with_context(|| format!("export_retry_after {error:#}"))?;
                return Err(error.context("export_delivery_failed"));
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
