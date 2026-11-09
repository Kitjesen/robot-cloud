use anyhow::{Context, Result, ensure};
use reqwest::{Certificate, Url, blocking::Client, redirect::Policy};
use robot_cloud_common::{MAX_MESSAGE_BYTES, is_uuid};
use serde::Deserialize;
use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    base_url: String,
    api_key: String,
    ca_file: PathBuf,
}

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let path = PathBuf::from(
        args.next()
            .context("usage: query <config.json> <device-uuid>")?,
    );
    let device = args.next().context("missing device UUID")?;
    ensure!(
        args.next().is_none() && is_uuid(&device),
        "invalid device UUID or extra argument"
    );
    let config: Config =
        serde_json::from_slice(&fs::read(&path).context("read query configuration")?)?;
    let mut url = Url::parse(&config.base_url)?;
    ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && matches!(url.path(), "" | "/"),
        "baseUrl must be an HTTPS origin"
    );
    ensure!(!config.api_key.is_empty(), "missing API key");
    url.set_path(&format!("/api/v1/robots/{device}/status"));
    let ca = fs::read(path.parent().unwrap_or(Path::new(".")).join(config.ca_file))?;
    let response = Client::builder()
        .https_only(true)
        .redirect(Policy::none())
        .timeout(Duration::from_secs(12))
        .add_root_certificate(Certificate::from_pem(&ca)?)
        .build()?
        .post(url)
        .bearer_auth(config.api_key)
        .send()
        .context("HTTPS status query")?;
    let status = response.status();
    println!("HTTP {}", status.as_u16());
    let mut body = String::new();
    response
        .take((MAX_MESSAGE_BYTES + 1) as u64)
        .read_to_string(&mut body)?;
    ensure!(body.len() <= MAX_MESSAGE_BYTES, "response exceeds 8 KiB");
    let value: serde_json::Value = serde_json::from_str(&body).context("response is not JSON")?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    ensure!(status.is_success(), "status query failed ({status})");
    Ok(())
}
