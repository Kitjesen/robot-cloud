use anyhow::{Context, Result};
use doso_cloud_media::{Config, Media};
use std::{env, fs, sync::Arc};

#[tokio::main]
async fn main() -> Result<()> {
    let path = env::args()
        .nth(1)
        .context("usage: doso-cloud-media <config.json>")?;
    let config: Config = serde_json::from_slice(&fs::read(path)?)?;
    let port = config.port;
    let media = Arc::new(Media::new(config)?);
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
    println!("video signalling on 127.0.0.1:{port}; authorization backend required");
    let cleanup = tokio::spawn(media.clone().cleanup());
    let result = axum::serve(listener, media.router())
        .with_graceful_shutdown(robot_cloud_common::shutdown_signal())
        .await;
    cleanup.abort();
    media.close_all().await;
    result?;
    Ok(())
}
