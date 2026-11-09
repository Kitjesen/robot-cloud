use crate::{
    choice::Choice,
    device::Device,
    retry::{Backoff, Exit},
    session,
    source::Source,
};
use robot_cloud_common::shutdown_signal;

pub(super) async fn run(choice: &Choice) -> Result<(), &'static str> {
    let mut device = Device::load().map_err(|_| "local_config_error")?;
    let mut source = Source::load();
    let mut backoff = Backoff::new();
    loop {
        if !choice.enabled() {
            return Ok(());
        }
        let (exit, online) = session::run(&mut device, choice, &mut source)
            .await
            .map_err(|_| "local_config_error")?;
        let reason = match exit {
            Exit::Stopped => return Ok(()),
            Exit::Config(reason) => return Err(reason),
            Exit::Retry(reason) => reason,
        };
        let jitter = (uuid::Uuid::new_v4().as_u128() % 1000) as u64;
        let delay = backoff.next(online, jitter);
        eprintln!("cloud_retry reason={reason} retry_ms={}", delay.as_millis());
        let retry = tokio::time::sleep(delay);
        tokio::pin!(retry);
        loop {
            let video_deadline = source.deadline();
            tokio::select! {
                _ = shutdown_signal() => return Ok(()),
                _ = choice.closed() => return Ok(()),
                _ = &mut retry => break,
                _ = async { tokio::time::sleep_until(video_deadline.expect("pending video stop").into()).await }, if video_deadline.is_some() => {
                    if let Err(error) = source.request_stop() { eprintln!("video_stop_error reason={error}"); }
                }
            }
        }
    }
}
