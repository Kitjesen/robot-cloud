use anyhow::{Context, Result};
use doso_ota::{CloudPollConfig, SystemHealthCheck, Updater, UpdaterConfig, serve_with_cloud};
use std::{net::SocketAddr, path::PathBuf};

fn env_path_opt(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn env_str_opt(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn default_staging_dir() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        PathBuf::from("/ota")
            .is_dir()
            .then(|| PathBuf::from("/ota/staging"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let command = std::env::args().nth(1).unwrap_or_else(|| "inspect".into());
    let root = env_path_opt("DOSO_OTA_ROOT").unwrap_or_else(|| "/var/lib/doso-ota".into());
    let staging_dir = env_path_opt("DOSO_OTA_STAGING_DIR").or_else(default_staging_dir);
    let releases_dir = env_path_opt("DOSO_OTA_RELEASES_DIR");
    let current_link = env_path_opt("DOSO_OTA_CURRENT_LINK");
    let last_good_link = env_path_opt("DOSO_OTA_LAST_GOOD_LINK");

    let config = UpdaterConfig {
        root,
        device_id: std::env::var("DOSO_DEVICE_ID").context("DOSO_DEVICE_ID")?,
        model: std::env::var("DOSO_MODEL").context("DOSO_MODEL")?,
        hardware_revision: std::env::var("DOSO_HARDWARE_REVISION")
            .context("DOSO_HARDWARE_REVISION")?,
        bootloader_version: std::env::var("DOSO_BOOTLOADER_VERSION")
            .context("DOSO_BOOTLOADER_VERSION")?,
        brainstem_version: std::env::var("DOSO_BRAINSTEM_VERSION")
            .context("DOSO_BRAINSTEM_VERSION")?,
        current_version: std::env::var("DOSO_BRAINSTEM_RELEASE")
            .context("DOSO_BRAINSTEM_RELEASE")?,
        battery_percent: std::env::var("DOSO_BATTERY_PERCENT")
            .unwrap_or_else(|_| "100".into())
            .parse()?,
        online: env_bool("DOSO_DEVICE_ONLINE")?,
        task_running: env_bool("DOSO_TASK_RUNNING")?,
        boot_id_path: env_path_opt("DOSO_OTA_BOOT_ID")
            .unwrap_or_else(|| "/run/doso/brainstem.boot_id".into()),
        verify_key_path: std::env::var_os("DOSO_OTA_VERIFY_KEY")
            .map(PathBuf::from)
            .context("DOSO_OTA_VERIFY_KEY")?,
        brainstem_unit: std::env::var("DOSO_BRAINSTEM_UNIT")
            .unwrap_or_else(|_| "doso-brainstem.service".into()),
        staging_dir,
        releases_dir,
        current_link,
        last_good_link,
        policy_admin_url: env_str_opt("DOSO_POLICY_ADMIN_URL"),
        policy_admin_token: env_str_opt("DOSO_POLICY_ADMIN_TOKEN"),
        allow_insecure_http_loopback: std::env::var("DOSO_OTA_ALLOW_HTTP").as_deref() == Ok("1"),
    };
    let updater = Updater::new(config)?;
    match command.as_str() {
        "inspect" => println!("{}", serde_json::to_string_pretty(&updater.profile())?),
        "job" => println!("{}", serde_json::to_string_pretty(&updater.job()?)?),
        "serve" => {
            let bind = std::env::var("DOSO_OTA_BIND")
                .unwrap_or_else(|_| "127.0.0.1:13147".into())
                .parse::<SocketAddr>()?;
            let token = env_str_opt("DOSO_OTA_TOKEN");
            let health = SystemHealthCheck::from_env().ok();
            let cloud = CloudPollConfig::from_env();
            serve_with_cloud(updater, bind, token, health, cloud).await?;
        }
        _ => anyhow::bail!("commands: inspect | job | serve"),
    }
    Ok(())
}

fn env_bool(name: &str) -> Result<bool> {
    match std::env::var(name)?.as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => anyhow::bail!("{name} must be true or false"),
    }
}
