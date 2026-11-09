mod choice;
mod device;
mod gnss;
mod grpc;
mod link;
mod reporting;
mod retry;
mod session;
mod source;

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
    if let Err(err) = link::run(&choice).await {
        eprintln!("cloud_error reason={err}");
        std::process::exit(78);
    }
}
