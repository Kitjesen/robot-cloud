use doso_cloud_gateway::{Gateway, SyncConfig, app, run_mqtt, run_sync};
use robot_cloud_common::{connect_mqtt, load_signing_key, shutdown_signal};
use std::{env, path::PathBuf, sync::Arc};

fn required(name: &str) -> anyhow::Result<String> {
    let value = env::var(name).unwrap_or_default();
    anyhow::ensure!(!value.trim().is_empty(), "invalid_configuration");
    Ok(value)
}

async fn run() -> anyhow::Result<()> {
    let sync_config = SyncConfig::from_env()?;
    let registry_path = PathBuf::from(required("DOSO_DEVICE_REGISTRY")?);
    let key = load_signing_key(&required("DOSO_GATEWAY_SIGNING_KEY")?)?;
    let api_key = required("DOSO_GATEWAY_API_KEY")?;
    let port = env::var("DOSO_GATEWAY_PORT")
        .unwrap_or_else(|_| "8130".into())
        .parse::<u16>()?;
    anyhow::ensure!(port > 0, "invalid_configuration");
    let client_id = env::var("DOSO_MQTT_CLIENT_ID").unwrap_or_else(|_| "doso-gateway".into());
    let (client, event_loop) = connect_mqtt(&client_id)?;
    let reporting_path = env::var_os("DOSO_REPORTING_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| registry_path.with_file_name("reporting.json"));
    let gateway = Arc::new(Gateway::with_reporting_state(
        client,
        key,
        registry_path,
        reporting_path,
    )?);
    let router = app(gateway.clone(), &api_key)?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let mqtt = tokio::spawn(run_mqtt(gateway.clone(), event_loop));
    let sync = sync_config.map(|config| tokio::spawn(run_sync(gateway.clone(), config)));
    let sync_abort = sync.as_ref().map(|task| task.abort_handle());
    println!("robot_cloud_gateway_listening");
    let shutdown_gateway = gateway.clone();
    let result = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            if let Some(sync) = sync_abort {
                sync.abort();
            }
            shutdown_gateway.disconnected();
        })
        .await;
    gateway.disconnected();
    if let Some(sync) = sync {
        sync.abort();
        let _ = sync.await;
    }
    mqtt.abort();
    result?;
    Ok(())
}

#[tokio::main]
async fn main() {
    if run().await.is_err() {
        eprintln!("robot_cloud_gateway_start_failed");
        std::process::exit(1);
    }
}
