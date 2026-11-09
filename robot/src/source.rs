use robot_cloud_common::{Result, now_ms, parse_time, video::Control};
use serde::Deserialize;
use std::{
    env, fs,
    path::PathBuf,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::process::{Child, Command};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    program: PathBuf,
    args: Vec<String>,
}

pub(super) struct Source {
    config: Option<Config>,
    child: Option<Child>,
    deadline: Option<Instant>,
    seq: u64,
    stopping: bool,
}

impl Source {
    pub fn load() -> Self {
        let config = env::var_os("DOSO_VIDEO_CONFIG")
            .map(|path| -> Result<Config> {
                let config: Config = serde_json::from_slice(&fs::read(path)?)?;
                anyhow::ensure!(config.program.is_absolute(), "invalid_video_program");
                Ok(config)
            })
            .transpose();
        let config = match config {
            Ok(config) => config,
            Err(error) => {
                eprintln!("video_source_unavailable reason={error}");
                None
            }
        };
        Self {
            config,
            child: None,
            deadline: None,
            seq: 0,
            stopping: false,
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub fn connected(&mut self) {
        self.seq = 0;
    }

    pub async fn apply(&mut self, control: Control) -> Result<()> {
        anyhow::ensure!(control.seq > self.seq, "stale_video_control");
        self.seq = control.seq;
        if !control.enabled {
            return self.request_stop().map(|_| ());
        }
        anyhow::ensure!(!self.stopping, "video_stop_pending");
        let remaining = parse_time(&control.expires_at)? - now_ms();
        anyhow::ensure!(remaining > 0, "expired_video_control");
        let config = self
            .config
            .as_ref()
            .ok_or_else(|| std::io::Error::other("video_not_configured"))?;
        if let Some(child) = self.child.as_mut()
            && child.try_wait()?.is_some()
        {
            self.child = None;
        }
        if self.child.is_none() {
            let mut command = Command::new(&config.program);
            command
                .args(&config.args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            #[cfg(windows)]
            command.creation_flags(0x08000000);
            self.child = Some(command.spawn()?);
        }
        self.deadline = Some(Instant::now() + Duration::from_millis(remaining as u64));
        Ok(())
    }

    fn stopped(&mut self) {
        self.child = None;
        self.deadline = None;
        self.stopping = false;
    }

    pub fn request_stop(&mut self) -> Result<bool> {
        self.stopping = true;
        let result = (|| -> Result<bool> {
            if let Some(child) = self.child.as_mut()
                && child.try_wait()?.is_none()
            {
                child.start_kill()?;
                return Ok(false);
            }
            Ok(true)
        })();
        match &result {
            Ok(true) => self.stopped(),
            Ok(false) => self.deadline = Some(Instant::now() + Duration::from_millis(50)),
            Err(_) => self.deadline = Some(Instant::now() + Duration::from_secs(1)),
        }
        result
    }

    pub async fn stop(&mut self) -> Result<()> {
        if self.request_stop()? {
            return Ok(());
        }
        let result: Result<()> = async {
            tokio::time::timeout(
                Duration::from_secs(1),
                self.child.as_mut().expect("stopping source").wait(),
            )
            .await??;
            Ok(())
        }
        .await;
        if result.is_ok() {
            self.stopped();
        } else {
            self.deadline = Some(Instant::now() + Duration::from_secs(1));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use robot_cloud_common::{timestamp, video::LEASE_MS};

    fn control(seq: u64, enabled: bool) -> Control {
        Control {
            v: 1,
            kind: "video.control".into(),
            device_id: uuid::Uuid::new_v4().to_string(),
            connection_id: uuid::Uuid::new_v4().to_string(),
            seq,
            enabled,
            expires_at: timestamp(now_ms() + LEASE_MS),
        }
    }

    #[test]
    #[ignore = "subprocess fixture"]
    fn source_process() {
        std::thread::sleep(Duration::from_secs(60));
    }

    #[tokio::test]
    async fn renews_one_process_and_stops_it() {
        let mut source = Source {
            config: Some(Config {
                program: env::current_exe().unwrap(),
                args: vec![
                    "--exact".into(),
                    "source::tests::source_process".into(),
                    "--ignored".into(),
                ],
            }),
            child: None,
            deadline: None,
            seq: 0,
            stopping: false,
        };
        source.apply(control(1, true)).await.unwrap();
        let pid = source.child.as_ref().unwrap().id();
        source.apply(control(2, true)).await.unwrap();
        assert_eq!(source.child.as_ref().unwrap().id(), pid);
        assert!(source.apply(control(1, true)).await.is_err());
        source.apply(control(3, false)).await.unwrap();
        source.stop().await.unwrap();
        assert!(source.child.is_none());
        assert!(source.deadline.is_none());
        assert!(source.apply(control(2, true)).await.is_err());
        source.apply(control(4, true)).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), source.stop())
            .await
            .unwrap()
            .unwrap();
        assert!(source.child.is_none());
    }

    #[tokio::test]
    async fn missing_source_and_expired_controls_do_not_start() {
        let mut source = Source {
            config: None,
            child: None,
            deadline: None,
            seq: 0,
            stopping: false,
        };
        assert!(source.apply(control(1, true)).await.is_err());
        let mut expired = control(2, true);
        expired.expires_at = timestamp(now_ms() - 1);
        assert!(source.apply(expired).await.is_err());
        assert!(source.child.is_none());
    }

    #[tokio::test]
    async fn pending_stop_prevents_another_source_until_cleanup() {
        let mut source = Source {
            config: None,
            child: None,
            deadline: Some(Instant::now() + Duration::from_secs(1)),
            seq: 10,
            stopping: true,
        };
        source.connected();
        assert!(
            source
                .apply(control(1, true))
                .await
                .unwrap_err()
                .to_string()
                .contains("video_stop_pending")
        );
        source.stop().await.unwrap();
        assert!(!source.stopping);
        assert!(source.deadline.is_none());
    }
}
