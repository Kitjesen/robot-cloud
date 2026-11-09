//! Local simulated device and broker connected to a real cloud Gateway by SSH.
use crate::{
    brainstem::Brainstem,
    fixture::{self, Fixture},
};
use anyhow::{Context, Result, ensure};
use std::{fs, time::Duration};

const ROUTE_STEPS: u32 = 64;

fn route(step: u32) -> (f64, f64) {
    let angle = std::f64::consts::TAU * f64::from(step % ROUTE_STEPS) / f64::from(ROUTE_STEPS);
    let radius = 32.0 / 111_195.0;
    (
        31.2304 + radius * angle.sin(),
        121.4737 + radius * angle.cos() / 31.2304_f64.to_radians().cos(),
    )
}

fn location(fixture: &Fixture, step: u32) -> Result<()> {
    let (lat, lon) = route(step);
    fs::write(
        fixture.path("gnss-status.tmp"),
        serde_json::to_vec(&serde_json::json!({
            "location": {"lat":lat,"lon":lon,"fix_type":1,
                "ts":robot_cloud_common::now_ms() as f64 / 1000.0,"accuracy_m":null}
        }))?,
    )?;
    fs::rename(
        fixture.path("gnss-status.tmp"),
        fixture.path("gnss-status.json"),
    )?;
    Ok(())
}

pub async fn run(device: &str) -> Result<()> {
    ensure!(
        robot_cloud_common::is_uuid(device),
        "device ID must be a UUID"
    );
    let link = fixture::binary("doso-cloud-link")?;
    let brainstem = Brainstem::start()
        .await
        .context("bind SIMULATED Brainstem at 127.0.0.1:13146")?;
    let mut fixture = Fixture::new().await?;
    let broker_port = fixture.broker(device).await?;
    fs::write(
        fixture.path("devices.json"),
        serde_json::to_vec(&serde_json::json!({
            "devices": [{"deviceId":device,"enabled":true,"publicKeyPath":"device-signing.pub"}]
        }))?,
    )?;
    let api_key = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    fs::write(
        fixture.path("gateway.json"),
        serde_json::to_vec_pretty(&fixture.gateway_env(broker_port, 28130, &api_key))?,
    )?;
    let link_env = fixture.link_env(device, broker_port)?;
    location(&fixture, 0)?;
    fixture.spawn(&link, &link_env, "link")?;
    println!("SIMULATED device {device}; no local Gateway; real business permission required.");
    println!(
        "Private Gateway settings {}",
        fixture.path("gateway.json").display()
    );
    println!("MQTT tunnel target 127.0.0.1:{broker_port}");
    println!("State changes every 6 seconds. Ctrl+C stops the simulated device and broker.");
    println!("SIMULATED GNSS follows a 201m loop, advancing about 3.1m every 2 seconds.");
    let mut standing = false;
    let mut step = 0_u32;
    let mut changes = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(6),
        Duration::from_secs(6),
    );
    let mut checks = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(2),
        Duration::from_secs(2),
    );
    loop {
        tokio::select! {
            _ = changes.tick() => {
                standing = !standing;
                brainstem.set_state(if standing { 2 } else { 1 });
            }
            _ = checks.tick() => {
                step = step.wrapping_add(1);
                location(&fixture, step)?;
                brainstem.check()?;
                fixture.check()?;
                println!("SIMULATED reads={}", brainstem.reads());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_is_continuous_and_closes() {
        assert_eq!(route(0), route(ROUTE_STEPS));
        for step in 0..ROUTE_STEPS {
            let (lat, lon) = route(step);
            let (next_lat, next_lon) = route(step + 1);
            let north = (next_lat - lat) * 111_195.0;
            let east = (next_lon - lon) * 111_195.0 * lat.to_radians().cos();
            assert!((3.0..3.2).contains(&north.hypot(east)));
        }
    }
}
