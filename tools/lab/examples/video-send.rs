use anyhow::{Context, Result, ensure};
use std::{
    io::Read,
    process::{Command, Stdio},
    time::Duration,
};
use tokio::sync::mpsc;

fn take_frame(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let start = buffer.windows(2).position(|part| part == [0xff, 0xd8])?;
    let end = buffer[start + 2..]
        .windows(2)
        .position(|part| part == [0xff, 0xd9])?
        + start
        + 4;
    let frame = buffer[start..end].to_vec();
    buffer.drain(..end);
    Some(frame)
}

#[tokio::main]
async fn main() -> Result<()> {
    let port: u16 = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "19089".into())
        .parse()?;
    let mut encoder = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-re",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=5",
            "-c:v",
            "mjpeg",
            "-q:v",
            "5",
            "-f",
            "image2pipe",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("start ffmpeg")?;
    let mut stdout = encoder.stdout.take().context("ffmpeg stdout")?;
    let (tx, mut rx) = mpsc::channel(1);
    let reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 8192];
        while let Ok(count) = stdout.read(&mut chunk) {
            if count == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..count]);
            while let Some(frame) = take_frame(&mut buffer) {
                if tx.is_closed() {
                    return;
                }
                let _ = tx.try_send(frame);
            }
            if buffer.len() > 1024 * 1024 {
                return;
            }
        }
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()?;
    let url = format!("http://127.0.0.1:{port}/frame");
    println!("SIMULATED video source sending to {url}; Ctrl+C stops it");
    let result: Result<()> = async {
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => break,
                frame = rx.recv() => {
                    let Some(frame) = frame else { anyhow::bail!("video encoder stopped"); };
                    let response = client.post(&url).body(frame).send().await.context("send simulated frame")?;
                    ensure!(response.status().is_success(), "video relay rejected frame: {}", response.status());
                }
            }
        }
        Ok(())
    }.await;
    drop(rx);
    let _ = encoder.kill();
    let _ = encoder.wait();
    let _ = reader.join();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_complete_frames_from_a_stream() {
        let mut data = b"noise\xff\xd8first".to_vec();
        assert!(take_frame(&mut data).is_none());
        data.extend_from_slice(b"\xff\xd9\xff\xd8second\xff\xd9");
        assert_eq!(take_frame(&mut data).unwrap(), b"\xff\xd8first\xff\xd9");
        assert_eq!(take_frame(&mut data).unwrap(), b"\xff\xd8second\xff\xd9");
        assert!(data.is_empty());
    }
}
