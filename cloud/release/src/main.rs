use anyhow::{Context, Result};
use doso_ota_release::{Config, run};
use std::{env, net::SocketAddr, path::PathBuf};

fn required(name: &str) -> Result<String> {
    let value = env::var(name).context(name.to_owned())?;
    anyhow::ensure!(!value.trim().is_empty(), "{name}_empty");
    Ok(value)
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config {
        root: env::var_os("DOSO_RELEASE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| "/var/lib/doso-ota-release".into()),
        device_registry: PathBuf::from(required("DOSO_RELEASE_DEVICE_REGISTRY")?),
        verify_api_token: required("DOSO_RELEASE_API_TOKEN")?,
        signing_key_path: PathBuf::from(required("DOSO_RELEASE_SIGNING_KEY")?),
        signing_key_id: required("DOSO_RELEASE_SIGNING_KEY_ID")?,
        oss_bucket: required("DOSO_OSS_BUCKET")?,
        oss_endpoint: required("DOSO_OSS_ENDPOINT")?,
        oss_access_key_id: required("DOSO_OSS_ACCESS_KEY_ID")?,
        oss_access_key_secret: required("DOSO_OSS_ACCESS_KEY_SECRET")?,
        signed_url_ttl_seconds: env::var("DOSO_OSS_SIGNED_URL_TTL_SECONDS")
            .unwrap_or_else(|_| "1800".into())
            .parse()?,
    };
    let bind = env::var("DOSO_RELEASE_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8140".into())
        .parse::<SocketAddr>()?;
    run(config, bind).await
}
