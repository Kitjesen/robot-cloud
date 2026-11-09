mod auto_check;
mod brainstem;
mod fixture;
mod simulated;
mod uplink;
mod video_check;

use anyhow::{Context, Result, ensure};
use brainstem::Brainstem;
use fixture::Fixture;
use reqwest::{Client, RequestBuilder, redirect::Policy};
use serde_json::{Value, json};
use std::{collections::BTreeMap, env, fs, path::Path, time::Duration};

struct Local {
    fixture: Fixture,
    brainstem: Brainstem,
    client: Client,
    device: String,
    url: String,
    api_key: String,
    link_env: BTreeMap<String, String>,
}

impl Local {
    async fn start(sync: Option<(&str, &str)>) -> Result<Self> {
        let link = fixture::binary("doso-cloud-link")?;
        let gateway = fixture::binary("doso-cloud-gateway")?;
        let brainstem = Brainstem::start()
            .await
            .context("bind SIMULATED Brainstem at 127.0.0.1:13146")?;
        let mut fixture = Fixture::new().await?;
        let device = uuid::Uuid::new_v4().to_string();
        let broker_port = fixture.broker(&device).await?;
        let port = fixture::free_port()?;
        let api_key = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let mut local = Self {
            fixture,
            brainstem,
            client: Client::builder()
                .no_proxy()
                .redirect(Policy::none())
                .timeout(Duration::from_secs(11))
                .build()?,
            url: format!("http://127.0.0.1:{port}/api/v1/robots/{device}/status"),
            device,
            api_key,
            link_env: BTreeMap::new(),
        };
        local.set_enabled(false)?;
        let mut gateway_env = local.fixture.gateway_env(broker_port, port, &local.api_key);
        if let Some((url, key)) = sync {
            gateway_env.insert("DOSO_REMOTE_SYNC_URL".to_owned(), url.to_owned());
            gateway_env.insert("DOSO_REMOTE_SYNC_KEY".to_owned(), key.to_owned());
        }
        let link_env = local.fixture.link_env(&local.device, broker_port)?;
        local.fixture.spawn(&gateway, &gateway_env, "gateway")?;
        local.fixture.spawn(&link, &link_env, "link")?;
        local.link_env = link_env;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        loop {
            local.check_processes()?;
            let connected = local
                .fixture
                .processes
                .last()
                .unwrap()
                .logs()
                .contains("cloud_connected");
            // A disabled request proves HTTP readiness without reading Brainstem.
            if let Ok((403, _)) = local.query(true).await
                && connected
            {
                break;
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "local startup timed out: {}",
                local
                    .fixture
                    .processes
                    .iter()
                    .map(|process| process.logs())
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        ensure!(
            local.brainstem.reads() == 0,
            "startup unexpectedly read Brainstem"
        );
        local.set_enabled(true)?;
        fs::write(
            local.fixture.path("client.json"),
            serde_json::to_vec_pretty(&json!({
                "simulated": true, "url": local.url,
                "latestUrl": format!("{}/latest", local.url), "apiKey": local.api_key,
            }))?,
        )?;
        Ok(local)
    }

    fn set_enabled(&self, enabled: bool) -> Result<()> {
        fs::write(
            self.fixture.path("devices.json"),
            serde_json::to_vec(&json!({"devices": [{
                "deviceId": self.device, "enabled": enabled, "publicKeyPath": "device-signing.pub",
            }]}))?,
        )?;
        Ok(())
    }

    fn check_processes(&mut self) -> Result<()> {
        self.brainstem.check()?;
        self.fixture.check()
    }

    async fn query(&self, authorized: bool) -> Result<(u16, Value)> {
        let mut request = self.client.post(&self.url);
        if authorized {
            request = request.bearer_auth(&self.api_key);
        }
        Self::response(request).await
    }

    async fn response(request: RequestBuilder) -> Result<(u16, Value)> {
        let mut response = request.send().await?;
        let status = response.status().as_u16();
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                body.len() + chunk.len() <= robot_cloud_common::MAX_MESSAGE_BYTES,
                "HTTP response exceeds 8 KiB"
            );
            body.extend_from_slice(&chunk);
        }
        Ok((status, serde_json::from_slice(&body)?))
    }

    async fn latest(&self) -> Result<Value> {
        let (status, value) = Self::response(
            self.client
                .get(format!("{}/latest", self.url))
                .bearer_auth(&self.api_key),
        )
        .await?;
        ensure!(status == 200, "latest status failed ({status}): {value}");
        Ok(value)
    }

    async fn permission(&self, value: &Value) -> Result<u16> {
        let url = format!("{}/reporting", self.url.trim_end_matches("/status"));
        let (status, _) = Self::response(
            self.client
                .put(url)
                .bearer_auth(&self.api_key)
                .header("Content-Type", "application/json")
                .body(serde_json::to_vec(value)?),
        )
        .await?;
        Ok(status)
    }

    async fn wait_for(&mut self, predicate: impl Fn(&Value) -> bool) -> Result<Value> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        loop {
            self.check_processes()?;
            let value = self.latest().await?;
            if predicate(&value) {
                return Ok(value);
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "reporting did not reach expected state: {value}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn reporting(&mut self) -> Result<()> {
        use robot_cloud_common::{now_ms, timestamp};

        self.set_enabled(true)?;
        let state = self
            .wait_for(|value| value["connectionId"].is_string())
            .await?;
        let connection = state["connectionId"].as_str().unwrap().to_owned();
        let before = self.brainstem.reads();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        ensure!(
            self.brainstem.reads() == before,
            "periodic reading started without permission"
        );
        let mut permission = json!({"authorizationEpoch":1,"consentVersion":1,"enabled":true,
            "connectionId":connection,"reportingSessionId":uuid::Uuid::new_v4().to_string(),
            "expiresAt":timestamp(now_ms()+15_000)});
        ensure!(
            self.permission(&permission).await? == 202,
            "reporting grant was not accepted"
        );
        let first = self
            .wait_for(|value| {
                value["permission"]["deviceConfirmed"] == true && value["fresh"] == true
            })
            .await?;
        let (status, snapshot) = self.query(true).await?;
        ensure!(
            status == 200,
            "authorized status expected 200, got {status}: {snapshot}"
        );
        ensure!(snapshot["deviceId"] == self.device, "wrong device ID");
        ensure!(
            snapshot["requestId"]
                .as_str()
                .is_some_and(robot_cloud_common::is_uuid),
            "invalid request ID"
        );
        let observed = robot_cloud_common::parse_time(
            snapshot["observedAt"]
                .as_str()
                .context("missing observedAt")?,
        )?;
        ensure!(
            (robot_cloud_common::now_ms() - observed).abs() < 5000,
            "stale observedAt"
        );
        ensure!(
            snapshot["state"]
                == json!({"kind": "CMS_STATE_KIND_GROUNDED", "transition": "CMS_TRANSITION_KIND_NONE"}),
            "incorrect simulated state"
        );
        ensure!(
            snapshot["control"]
                == json!({"owner": "CONTROL_OWNER_NONE", "motorOutputEnabled": false, "criticalMotorFault": false}),
            "incorrect simulated control"
        );
        ensure!(
            snapshot["transport"] == "local-insecure" && snapshot.get("battery").is_none(),
            "incorrect status projection"
        );
        println!("PASS confirmed permission allows signed status HTTP 200");
        let first_seq = first["latest"]["report"]["seq"]
            .as_u64()
            .context("missing report sequence")?;
        self.brainstem.set_state(2);
        let standing = self
            .wait_for(|value| {
                value["latest"]["report"]["status"]["state"]["kind"] == "CMS_STATE_KIND_STANDING"
                    && value["latest"]["report"]["seq"]
                        .as_u64()
                        .is_some_and(|seq| seq > first_seq)
            })
            .await?;
        ensure!(
            standing["reachable"] == true && standing["fresh"] == true,
            "fresh report was not available"
        );
        println!("PASS reporting: device confirmed; GROUNDED -> STANDING; seq increases");
        println!(
            "SIMULATED latest: {}",
            serde_json::to_string(&standing["latest"])?
        );

        self.brainstem.fail(true);
        let failed = self
            .wait_for(|value| value["latest"]["report"]["error"] == "upstream_unavailable")
            .await?;
        ensure!(
            failed["fresh"] == false && failed["reachable"] == true,
            "read failure was confused with connection state"
        );
        let success_time = failed["lastSuccess"]["report"]["observedAt"].clone();
        ensure!(
            success_time.is_string() && success_time != failed["latest"]["report"]["observedAt"],
            "old success was stamped as new"
        );
        let failed_seq = failed["latest"]["report"]["seq"].as_u64().unwrap();
        let next_failure = self
            .wait_for(|value| {
                value["latest"]["report"]["seq"]
                    .as_u64()
                    .is_some_and(|seq| seq > failed_seq)
            })
            .await?;
        ensure!(
            next_failure["lastSuccess"]["report"]["observedAt"] == success_time,
            "failure changed last successful collection time"
        );
        println!(
            "PASS read failure: last success keeps its original time; fresh=false, reachable=true"
        );
        self.brainstem.fail(false);

        let mut disabled = permission.clone();
        disabled["consentVersion"] = json!(2);
        disabled["enabled"] = json!(false);
        disabled["connectionId"] = Value::Null;
        disabled["reportingSessionId"] = Value::Null;
        disabled["expiresAt"] = json!(timestamp(now_ms() + 15_000));
        ensure!(
            self.permission(&disabled).await? == 202,
            "disable was not accepted"
        );
        self.wait_for(|value| {
            value["permission"]["desiredEnabled"] == false
                && value["permission"]["deviceConfirmed"] == true
        })
        .await?;
        let stopped = self.brainstem.reads();
        tokio::time::sleep(Duration::from_millis(1300)).await;
        ensure!(
            self.brainstem.reads() == stopped,
            "sampling continued after disable confirmation"
        );
        permission["expiresAt"] = json!(timestamp(now_ms() + 15_000));
        ensure!(
            self.permission(&permission).await? == 409,
            "old consent reopened reporting"
        );
        ensure!(
            self.query(true).await?.0 == 403,
            "disabled consent still allowed on-demand reads"
        );
        println!(
            "PASS disable: confirmed, sampling stopped, old grant rejected (409), query rejected (403)"
        );

        permission["consentVersion"] = json!(3);
        permission["reportingSessionId"] = json!(uuid::Uuid::new_v4().to_string());
        permission["expiresAt"] = json!(timestamp(now_ms() + 3000));
        ensure!(
            self.permission(&permission).await? == 202,
            "short lease was not accepted"
        );
        self.wait_for(|value| {
            value["permission"]["deviceConfirmed"] == true && value["fresh"] == true
        })
        .await?;
        tokio::time::sleep(Duration::from_millis(3200)).await;
        let expired_reads = self.brainstem.reads();
        tokio::time::sleep(Duration::from_millis(1300)).await;
        ensure!(
            self.brainstem.reads() == expired_reads && self.latest().await?["fresh"] == false,
            "expired lease continued sampling"
        );
        permission["expiresAt"] = json!(timestamp(now_ms() + 15_000));
        ensure!(
            self.permission(&permission).await? == 409,
            "expired reporting session was reused"
        );
        println!("PASS expiry: sampling stopped; expired session cannot be renewed");

        self.fixture.processes.last_mut().unwrap().stop()?;
        self.fixture.processes.pop();
        self.fixture
            .spawn(&fixture::binary("doso-cloud-link")?, &self.link_env, "link")?;
        self.wait_for(|value| {
            value["connectionId"]
                .as_str()
                .is_some_and(|id| id != connection)
        })
        .await?;
        permission["consentVersion"] = json!(4);
        permission["reportingSessionId"] = json!(uuid::Uuid::new_v4().to_string());
        ensure!(
            self.permission(&permission).await? >= 400,
            "old connection accepted a new grant"
        );
        let reconnected_reads = self.brainstem.reads();
        tokio::time::sleep(Duration::from_millis(1300)).await;
        ensure!(
            self.brainstem.reads() == reconnected_reads,
            "reconnected cloud link sampled without a new grant"
        );
        println!("PASS reconnect: new connection ID; old grant rejected; no automatic collection");
        Ok(())
    }

    async fn verify(&mut self) -> Result<()> {
        let (status, _) = self.query(true).await?;
        ensure!(
            status == 403 && self.brainstem.reads() == 0,
            "query without user permission must not read Brainstem"
        );
        println!("PASS no user permission HTTP 403, reads=0");

        let (status, _) = self.query(false).await?;
        ensure!(
            status == 401 && self.brainstem.reads() == 0,
            "missing API credential must return 401 without another read"
        );
        println!("PASS missing credential HTTP 401, reads=0");

        self.set_enabled(false)?;
        let (status, body) = self.query(true).await?;
        ensure!(
            status == 403
                && body == json!({"error": {"code": "disabled"}})
                && self.brainstem.reads() == 0,
            "disabled device must return 403 without another read"
        );
        println!("PASS disabled registration HTTP 403, reads=0");
        self.reporting().await?;
        self.verify_video().await?;
        self.check_processes()?;
        let connection = self.latest().await?["connectionId"].clone();
        let permission = json!({
            "authorizationEpoch": 1, "consentVersion": 5, "enabled": true,
            "connectionId": connection,
            "reportingSessionId": uuid::Uuid::new_v4().to_string(),
            "expiresAt": robot_cloud_common::timestamp(robot_cloud_common::now_ms() + 15_000),
        });
        ensure!(
            self.permission(&permission).await? == 202,
            "local-off fixture grant failed"
        );
        self.wait_for(|value| value["fresh"] == true).await?;
        fs::write(self.fixture.path("cloud-choice"), "false\n")?;
        self.fixture
            .processes
            .last_mut()
            .unwrap()
            .wait_success()
            .await?;
        self.fixture.processes.pop();
        let stopped = self.brainstem.reads();
        let video_stopped = self.video_heartbeat();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        ensure!(self.brainstem.reads() == stopped, "local off kept sampling");
        ensure!(
            self.video_heartbeat() == video_stopped,
            "local off kept video source running"
        );
        self.fixture
            .spawn(&fixture::binary("doso-cloud-link")?, &self.link_env, "link")?;
        self.fixture
            .processes
            .last_mut()
            .unwrap()
            .wait_success()
            .await?;
        let logs = self.fixture.processes.pop().unwrap().logs();
        ensure!(
            logs.contains("cloud_off") && !logs.contains("cloud_connected"),
            "restart ignored saved local off"
        );
        println!(
            "PASS local off: active sampling and connection stopped; saved off survives restart"
        );
        self.check_processes()
    }
}

async fn demo() -> Result<()> {
    let source = simulated::Simulated::start().await?;
    let mut local = Local::start(Some((&source.url, &source.key))).await?;
    source.business.lock().unwrap().choose(true);
    local.wait_for(|value| value["fresh"] == true).await?;
    println!("SIMULATED robot + business API. Real Rust cloud link, TLS MQTT and Gateway.");
    println!(
        "Private client settings {}",
        local.fixture.path("client.json").display()
    );
    println!(
        "State changes every 6 seconds. Ctrl+C stops this demo and removes its private settings."
    );
    let mut standing = false;
    let mut changes = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(6),
        Duration::from_secs(6),
    );
    let mut reads = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            _ = changes.tick() => {
                standing = !standing;
                local.brainstem.set_state(if standing { 2 } else { 1 });
            }
            _ = reads.tick() => {
                local.check_processes()?;
                let value = local.latest().await?;
                let report = &value["latest"]["report"];
                println!("{}", json!({
                    "simulated": true,
                    "deviceId": local.device,
                    "fresh": value["fresh"],
                    "seq": report["seq"],
                    "observedAt": report["observedAt"],
                    "state": report["status"]["state"]["kind"],
                }));
            }
        }
    }
}

async fn public(config: &Path) -> Result<()> {
    let values: BTreeMap<String, String> = serde_json::from_slice(&fs::read(config)?)
        .context("link.json must be a DOSO_* string environment object")?;
    ensure!(
        values.keys().all(|name| name.starts_with("DOSO_")),
        "link.json only accepts DOSO_* environment names"
    );
    ensure!(
        !values.contains_key("DOSO_GATEWAY_API_KEY")
            && !values.contains_key("DOSO_REMOTE_SYNC_KEY"),
        "link.json must not contain Gateway or business sync service keys"
    );
    ensure!(
        values
            .get("DOSO_CLOUD_ON")
            .or_else(|| values.get("DOSO_REMOTE_ENABLED"))
            .map(String::as_str)
            == Some("true"),
        "link.json requires DOSO_CLOUD_ON=true (legacy DOSO_REMOTE_ENABLED is accepted when absent)"
    );
    ensure!(
        values
            .get("DOSO_CLOUD_CHOICE")
            .is_some_and(|path| Path::new(path).is_file()),
        "link.json requires DOSO_CLOUD_CHOICE pointing to this run's private simulated local-choice file"
    );
    let link = fixture::binary("doso-cloud-link")?;
    let brainstem = Brainstem::start()
        .await
        .context("bind SIMULATED Brainstem at 127.0.0.1:13146")?;
    let mut fixture = Fixture::new().await?;
    fixture.spawn(&link, &values, "link")?;
    println!(
        "SIMULATED Brainstem only. Real Rust cloud link connects to configured MQTT. Ctrl+C stops this lab."
    );
    let mut printed = 0;
    loop {
        brainstem.check()?;
        fixture.check()?;
        let logs = fixture.processes[0].logs();
        if let Some(new) = logs.get(printed..) {
            print!("{new}");
        }
        printed = logs.len();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn run() -> Result<()> {
    let arguments: Vec<_> = env::args().skip(1).collect();
    match arguments.as_slice() {
        [mode, heartbeat] if mode == "video-source" => {
            video_check::source(Path::new(heartbeat)).await
        }
        [mode] if mode == "auth-check" => {
            let mut fixture = Fixture::new().await?;
            let device = uuid::Uuid::new_v4().to_string();
            let port = fixture.broker(&device).await?;
            fixture.check_auth(port, &device).await?;
            fixture.close()
        }
        [mode] if mode == "check" || mode == "local" => {
            let mut local = Local::start(None).await?;
            println!("SIMULATED Brainstem. Real Rust Gateway + TLS MQTT + Rust cloud link.");
            if mode == "check" {
                local.verify().await?;
                return local.fixture.close();
            }
            println!(
                "Private client settings {}",
                local.fixture.path("client.json").display()
            );
            println!("Ctrl+C stops only this lab and removes its private settings.");
            loop {
                local.check_processes()?;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        [mode] if mode == "auto-check" => auto_check::run(None).await,
        [mode, wire] if mode == "auto-check" => auto_check::run(Some(Path::new(wire))).await,
        [mode] if mode == "demo" => demo().await,
        [mode, config] if mode == "public" => public(Path::new(config)).await,
        [mode, device] if mode == "uplink" => uplink::run(device).await,
        _ => anyhow::bail!(
            "usage: lab auth-check | lab check | lab auto-check [wire.json] | lab demo | lab local | lab public <link.json> | lab uplink <device-UUID>"
        ),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Cancelling startup/check drops their owned fixtures and subprocesses too.
    tokio::select! {
        result = run() => result,
        signal = tokio::signal::ctrl_c() => signal.context("wait for Ctrl+C"),
    }
}
