use super::*;

pub(super) async fn source(path: &Path) -> Result<()> {
    for count in 1u64.. {
        fs::write(path, count.to_string())?;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

impl Local {
    pub(super) fn video_heartbeat(&self) -> Option<u64> {
        fs::read_to_string(self.fixture.path("video-heartbeat"))
            .ok()?
            .parse()
            .ok()
    }

    async fn video(&self, enabled: bool) -> Result<()> {
        let (status, response) = Self::response(
            self.client
                .put(format!("{}/video", self.url.trim_end_matches("/status")))
                .bearer_auth(&self.api_key)
                .header("Content-Type", "application/json")
                .body(serde_json::to_vec(&json!({"enabled": enabled}))?),
        )
        .await?;
        ensure!(
            status == 202 && response["accepted"] == true,
            "video control failed: {response}"
        );
        Ok(())
    }

    pub(super) async fn verify_video(&mut self) -> Result<()> {
        ensure!(
            self.video_heartbeat().is_none(),
            "video source started without a viewer"
        );
        self.video(true).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
        while self.video_heartbeat().is_none_or(|count| count < 2) {
            ensure!(
                tokio::time::Instant::now() < deadline,
                "simulated video source did not start"
            );
            self.check_processes()?;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        self.video(false).await?;
        tokio::time::sleep(Duration::from_millis(250)).await;
        let stopped = self.video_heartbeat();
        tokio::time::sleep(Duration::from_millis(250)).await;
        ensure!(
            self.video_heartbeat() == stopped,
            "simulated video source did not stop"
        );
        self.video(true).await?;
        println!("PASS video: signed MQTT starts and stops fixed simulated source");
        Ok(())
    }
}
