mod export;
mod http;
mod thingsboard;

use export::Worker;
use std::{env, time::Duration};

fn required(name: &str) -> anyhow::Result<String> {
    env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("invalid_telemetry_configuration"))
}

async fn run() -> anyhow::Result<()> {
    let worker = Worker::new(
        &required("DOSO_TELEMETRY_URL")?,
        &required("DOSO_TELEMETRY_KEY")?,
        &required("DOSO_THINGSBOARD_URL")?,
        &required("DOSO_THINGSBOARD_KEY")?,
    )?;
    let work = async {
        loop {
            match worker.step().await {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => eprintln!("telemetry_export_failed {error:#}"),
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    };
    tokio::select! {
        _ = robot_cloud_common::shutdown_signal() => Ok(()),
        _ = work => Ok(()),
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("telemetry_start_failed {error:#}");
        std::process::exit(1);
    }
}
