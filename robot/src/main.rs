mod choice;
mod device;
mod gnss;
mod grpc;
mod link;
mod reporting;
mod retry;
mod session;
mod source;

use doso_cloud_link::control;

#[tokio::main]
async fn main() {
    let enabled = std::env::var_os("DOSO_CLOUD_ON")
        .or_else(|| std::env::var_os("DOSO_REMOTE_ENABLED"))
        .is_some_and(|value| value == "true");
    let choice = choice::Choice::from_env();
    if !enabled || !choice.enabled() {
        println!("cloud_off");
        return;
    }
    let result = match std::env::args().nth(1).as_deref() {
        None => link::run(&choice).await,
        Some("control") => match control::Config::from_env() {
            Ok(config) => tokio::select! {
                result = control::run(&config) => result.map_err(|_| "control_link_failed"),
                _ = choice.closed() => Ok(()),
                _ = robot_cloud_common::shutdown_signal() => Ok(()),
            },
            Err(_) => Err("control_config_error"),
        },
        Some(_) => Err("invalid_command"),
    };
    if let Err(err) = result {
        eprintln!("cloud_error reason={err}");
        std::process::exit(78);
    }
}
