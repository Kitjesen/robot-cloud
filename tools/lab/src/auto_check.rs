use crate::{Local, fixture, simulated::Simulated};
use anyhow::{Context, Result, ensure};
use robot_cloud_common::{now_ms, parse_time, timestamp};
use serde_json::{Value, json};
use std::{fs, path::Path, time::Duration};

pub async fn run(wire_path: Option<&Path>) -> Result<()> {
    let source = Simulated::start().await?;
    let mut local = Local::start(Some((&source.url, &source.key))).await?;
    println!(
        "SIMULATED business API + Brainstem; real Rust Gateway, TLS MQTT and cloud link. No real database or robot. Lab lease=8s/renew threshold=4s; production=30s/15s."
    );
    local
        .wait_for(|value| value["connectionId"].is_string())
        .await?;
    tokio::time::sleep(Duration::from_millis(2200)).await;
    ensure!(
        source.business.lock().unwrap().calls > 0,
        "automatic sync did not contact SIMULATED API"
    );
    ensure!(
        local.brainstem.reads() == 0 && local.latest().await?["latest"].is_null(),
        "default-off collected state"
    );
    println!("PASS automatic default-off: sync runs, zero Brainstem reads, no grant");

    source.business.lock().unwrap().choose(true);
    let first = local.wait_for(|value| value["fresh"] == true).await?;
    let first_deadline = source
        .business
        .lock()
        .unwrap()
        .issued
        .as_ref()
        .context("missing first grant")?
        .expires_at
        .clone();
    let first_session = first["permission"]["reportingSessionId"].clone();
    let first_connection = first["connectionId"].clone();
    local.brainstem.set_state(2);
    let remaining = parse_time(&first_deadline)? - now_ms();
    if remaining > 0 {
        tokio::time::sleep(Duration::from_millis(remaining as u64 + 300)).await;
    }
    local
        .wait_for(|value| {
            value["fresh"] == true
                && value["latest"]["report"]["status"]["state"]["kind"] == "CMS_STATE_KIND_STANDING"
                && value["latest"]["report"]["observedAt"]
                    .as_str()
                    .is_some_and(|time| time > first_deadline.as_str())
        })
        .await?;
    wait_saved(&mut local, &source, |saved| {
        saved["report"]["status"]["state"]["kind"] == "CMS_STATE_KIND_STANDING"
            && saved["report"]["observedAt"]
                .as_str()
                .is_some_and(|time| time > first_deadline.as_str())
    })
    .await?;
    ensure!(
        source.business.lock().unwrap().renewals > 0,
        "lease did not renew automatically"
    );
    ensure!(
        local.latest().await?["permission"]["reportingSessionId"] == first_session,
        "live renewal replaced session"
    );
    println!(
        "PASS one user enable: automatic renewal and SIMULATED latest save continue past first expiresAt={first_deadline}; no App keepalive or management PUT"
    );

    local.fixture.processes.last_mut().unwrap().stop()?;
    local.fixture.processes.pop();
    local.fixture.spawn(
        &fixture::binary("doso-cloud-link")?,
        &local.link_env,
        "link",
    )?;
    let reconnected = local
        .wait_for(|value| {
            value["connectionId"] != first_connection
                && value["fresh"] == true
                && value["permission"]["reportingSessionId"] != first_session
        })
        .await?;
    let new_session = reconnected["permission"]["reportingSessionId"].clone();
    wait_saved(&mut local, &source, |saved| {
        saved["report"]["reportingSessionId"] == new_session
    })
    .await?;
    ensure!(
        source
            .business
            .lock()
            .unwrap()
            .issued
            .as_ref()
            .unwrap()
            .consent_version
            == 1,
        "restart changed user decision version"
    );
    println!(
        "PASS cloud link restart: new connection nonce and automatic new session recover collection under the same user choice"
    );

    let frozen = {
        let mut business = source.business.lock().unwrap();
        business.unavailable = true;
        business.issued.clone().unwrap()
    };
    let until_expiry = parse_time(&frozen.expires_at)? - now_ms();
    if until_expiry > 0 {
        tokio::time::sleep(Duration::from_millis(until_expiry as u64 + 300)).await;
    }
    let stopped = local.brainstem.reads();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    local.check_processes()?;
    ensure!(
        local.brainstem.reads() == stopped && local.latest().await?["fresh"] == false,
        "source failure continued collection after expiry"
    );
    ensure!(
        source.business.lock().unwrap().issued.as_ref() == Some(&frozen),
        "source failure renewed permission"
    );
    println!(
        "PASS bounded source outage: HTTP 503 does not renew; existing deadline stops cloud link sampling"
    );
    source.business.lock().unwrap().unavailable = false;
    local
        .wait_for(|value| {
            value["fresh"] == true && value["permission"]["reportingSessionId"] != new_session
        })
        .await?;
    let saved = wait_saved(&mut local, &source, |saved| {
        saved["report"]["reportingSessionId"] != new_session
    })
    .await?;

    let (mut old_batch, rejected, issued_before_close) = {
        let mut business = source.business.lock().unwrap();
        let batch = business
            .last_batch
            .clone()
            .context("missing automatic batch")?;
        let rejected = business.rejected_reports;
        let issued = business.issued.clone().unwrap();
        business.choose(false);
        (serde_json::to_value(batch)?, rejected, issued)
    };
    old_batch["observedAt"] = json!(timestamp(now_ms()));
    let response = local
        .client
        .post(&source.url)
        .header("apikey", &source.key)
        .header("Content-Type", "application/json")
        .body(serde_json::to_vec(&old_batch)?)
        .send()
        .await?;
    ensure!(
        response.status().is_success(),
        "SIMULATED source disable check failed"
    );
    {
        let business = source.business.lock().unwrap();
        ensure!(
            business.rejected_reports > rejected,
            "source committed old state after user disable"
        );
        ensure!(
            serde_json::to_value(business.latest.as_ref().unwrap())? == saved,
            "disable changed saved latest"
        );
    }
    local
        .wait_for(|value| {
            value["permission"]["desiredEnabled"] == false
                && value["permission"]["deviceConfirmed"] == true
        })
        .await?;
    let stopped = local.brainstem.reads();
    let closed_grant = source.business.lock().unwrap().issued.clone().unwrap();
    tokio::time::sleep(Duration::from_millis(4300)).await;
    local.check_processes()?;
    ensure!(
        local.brainstem.reads() == stopped,
        "cloud link sampled after disable ACK"
    );
    ensure!(
        source.business.lock().unwrap().issued.as_ref() == Some(&closed_grant),
        "closed user choice renewed permission"
    );
    ensure!(
        closed_grant.consent_version > issued_before_close.consent_version && !closed_grant.enabled,
        "close did not advance consent"
    );
    println!(
        "PASS one user disable: source rejects old state immediately, Gateway/cloud link confirm stop, no renewed collection lease"
    );
    println!(
        "SIMULATED business latest JSON: {}",
        serde_json::to_string(&saved)?
    );
    if let Some(path) = wire_path {
        let wire = json!({"simulated":true,"leaseMs":crate::simulated::LEASE_MS,"samples":source.business.lock().unwrap().wire,"latest":saved});
        fs::write(path, serde_json::to_vec_pretty(&wire)?)
            .context("save credential-free automatic sync wire evidence")?;
        println!("SIMULATED sync wire saved {}", path.display());
    }
    local.fixture.close()?;
    println!(
        "PASS auto-check: all local behavior checks passed; private subprocesses and credentials cleaned"
    );
    Ok(())
}

async fn wait_saved(
    local: &mut Local,
    source: &Simulated,
    predicate: impl Fn(&Value) -> bool,
) -> Result<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        local.check_processes()?;
        let saved = serde_json::to_value(&source.business.lock().unwrap().latest)?;
        if predicate(&saved) {
            return Ok(saved);
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "SIMULATED business latest did not reach expected state: {saved}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
