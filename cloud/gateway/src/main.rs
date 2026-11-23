use doso_cloud_gateway::{Gateway, SyncConfig, app_with_control, control, run_mqtt, run_sync};
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
    let hub = env::var("DOSO_CONTROL_PORT")
        .ok()
        .map(|value| -> anyhow::Result<_> {
            let port = value.parse::<u16>()?;
            anyhow::ensure!(port > 0, "invalid_control_port");
            Ok((Arc::new(control::Hub::new(gateway.clone())), port))
        })
        .transpose()?;
    let mut control_runtime = None;
    if let Some((hub, port)) = &hub {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", *port)).await?;
        let (shutdown, stopped) = tokio::sync::watch::channel(false);
        let hub = hub.clone();
        control_runtime = Some((
            shutdown,
            tokio::spawn(control::serve(hub, listener, stopped)),
        ));
    }
    let control_hub = hub.map(|(hub, _)| hub);
    let router = app_with_control(gateway.clone(), &api_key, control_hub.clone())?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let mqtt = tokio::spawn(run_mqtt(gateway.clone(), event_loop));
    let sync = sync_config.map(|config| tokio::spawn(run_sync(gateway.clone(), config)));
    let sync_abort = sync.as_ref().map(|task| task.abort_handle());
    println!("robot_cloud_gateway_listening");
    let shutdown_gateway = gateway.clone();
    let shutdown_control = control_hub.clone();
    let control_signal = control_runtime
        .as_ref()
        .map(|(shutdown, _)| shutdown.clone());
    let result = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            if let Some(sync) = sync_abort {
                sync.abort();
            }
            shutdown_gateway.disconnected();
            if let Some(hub) = shutdown_control {
                hub.shutdown();
            }
            if let Some(shutdown) = control_signal {
                let _ = shutdown.send(true);
            }
        })
        .await;
    gateway.disconnected();
    if let Some(hub) = control_hub {
        hub.shutdown();
    }
    if let Some((shutdown, task)) = control_runtime {
        let _ = shutdown.send(true);
        task.await??;
    }
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
