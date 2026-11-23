use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use doso_ota::{
    ArtifactType, CloudClient, JobPhase, Manifest, ManifestEnvelope, RollbackPolicy,
    ServiceRestarter, SystemHealthCheck, Updater, UpdaterConfig, atomic_swap_link, cloud,
    read_link_target,
};
use ed25519_dalek::{Signer, SigningKey, pkcs8::EncodePublicKey};
use std::{
    fs,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

struct CountingRestarter {
    calls: AtomicUsize,
}

impl CountingRestarter {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }

    fn count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl ServiceRestarter for CountingRestarter {
    fn restart(&self, _unit: &str) -> anyhow::Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn make_tar_gz(src_dir: &Path, dest_archive: &Path) -> Vec<u8> {
    let status = Command::new("tar")
        .arg("-czf")
        .arg(dest_archive)
        .arg("-C")
        .arg(src_dir)
        .arg(".")
        .status()
        .expect("run tar -czf");
    assert!(status.success(), "tar -czf failed");
    fs::read(dest_archive).unwrap()
}

fn sign_manifest(manifest: &Manifest, key: &SigningKey) -> ManifestEnvelope {
    let payload = serde_json::to_vec(manifest).unwrap();
    ManifestEnvelope {
        payload: URL_SAFE_NO_PAD.encode(&payload),
        signature: URL_SAFE_NO_PAD.encode(key.sign(&payload).to_bytes()),
    }
}

fn setup_release_updater(
    root: &Path,
    key: &SigningKey,
    policy_admin_url: Option<String>,
    policy_admin_token: Option<String>,
    restarter: Arc<dyn ServiceRestarter>,
) -> (Updater, PathBuf, PathBuf, PathBuf) {
    let key_path = root.join("release-signing.pub.pem");
    fs::write(
        &key_path,
        key.verifying_key()
            .to_public_key_pem(Default::default())
            .unwrap(),
    )
    .unwrap();
    let releases_dir = root.join("opt/doso/releases");
    let v1_dir = releases_dir.join("1.0.0-rust");
    fs::create_dir_all(v1_dir.join("bin")).unwrap();
    fs::write(v1_dir.join("bin/doso-brainstem"), b"brainstem-v1").unwrap();

    let current_link = root.join("opt/doso/current");
    let last_good_link = root.join("opt/doso/last-known-good");
    atomic_swap_link(&current_link, &v1_dir).unwrap();
    atomic_swap_link(&last_good_link, &v1_dir).unwrap();

    let boot_id_path = root.join("run/doso/brainstem.boot_id");
    fs::create_dir_all(boot_id_path.parent().unwrap()).unwrap();
    fs::write(&boot_id_path, "boot-v1\n").unwrap();

    let cfg = UpdaterConfig {
        root: root.join("var/lib/doso-ota"),
        device_id: "dev-s100p-01".into(),
        model: "THUNDER 01".into(),
        hardware_revision: "rdk-s100p".into(),
        bootloader_version: "1.0.0".into(),
        brainstem_version: "1.0.0-rust".into(),
        current_version: "1.0.0-rust".into(),
        battery_percent: 95,
        online: true,
        task_running: false,
        boot_id_path,
        verify_key_path: key_path,
        brainstem_unit: "doso-brainstem.service".into(),
        staging_dir: Some(root.join("ota/staging")),
        releases_dir: Some(releases_dir.clone()),
        current_link: Some(current_link.clone()),
        last_good_link: Some(last_good_link.clone()),
        policy_admin_url,
        policy_admin_token,
        allow_insecure_http_loopback: true,
    };
    let updater = Updater::with_restarter(cfg, restarter).unwrap();
    (updater, releases_dir, current_link, last_good_link)
}

fn build_release_bundle(tmp: &Path, version_bytes: &[u8]) -> Vec<u8> {
    let src = tmp.join(format!("rel_src_{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(src.join("bin")).unwrap();
    fs::create_dir_all(src.join("lib")).unwrap();
    fs::write(src.join("bin/doso-brainstem"), version_bytes).unwrap();
    fs::write(src.join("lib/libdoso_shm_ipc.so"), b"fake-so").unwrap();
    let h_bin = Manifest::artifact_hash(version_bytes);
    let h_lib = Manifest::artifact_hash(b"fake-so");
    fs::write(
        src.join("SHA256SUMS"),
        format!("{h_bin}  bin/doso-brainstem\n{h_lib}  lib/libdoso_shm_ipc.so\n"),
    )
    .unwrap();
    let archive = tmp.join(format!("rel_{}.tar.gz", uuid::Uuid::new_v4()));
    make_tar_gz(&src, &archive)
}

fn build_policy_bundle(tmp: &Path, onnx_bytes: &[u8]) -> Vec<u8> {
    let src = tmp.join(format!("pol_src_{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&src).unwrap();
    let sha = Manifest::artifact_hash(onnx_bytes);
    fs::write(
        src.join("policy.yaml"),
        format!(
            "schema_version: 2\nname: flat_v2\nmodel:\n  file: \"model.onnx\"\n  sha256: \"{sha}\"\n"
        ),
    )
    .unwrap();
    fs::write(src.join("model.onnx"), onnx_bytes).unwrap();
    fs::write(
        src.join("golden.json"),
        r#"{"max_abs_tolerance":1e-4,"steps":[]}"#,
    )
    .unwrap();
    let archive = tmp.join(format!("pol_{}.tar.gz", uuid::Uuid::new_v4()));
    make_tar_gz(&src, &archive)
}

#[tokio::test]
async fn channel_a_release_install_flips_current_symlink_and_commits_on_healthy_5054() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[21; 32]);
    let restarter = Arc::new(CountingRestarter::new());
    let (updater, releases_dir, current_link, last_good_link) =
        setup_release_updater(dir.path(), &key, None, None, restarter.clone());

    let bundle_bytes = Arc::new(build_release_bundle(dir.path(), b"brainstem-v1.1.0-elf"));
    let bundle_for_srv = bundle_bytes.clone();
    let restarter_for_health = restarter.clone();

    let app = Router::new()
        .route(
            "/release.tar.gz",
            get(move || {
                let bytes = bundle_for_srv.clone();
                async move { bytes.as_ref().clone() }
            }),
        )
        .route(
            "/health",
            get(move || {
                let restarts = restarter_for_health.count();
                async move {
                    // Simulate Brainstem coming back healthy on new release after restart.
                    if restarts >= 1 {
                        (
                            StatusCode::OK,
                            Json(serde_json::json!({
                                "ok": true,
                                "boot_id": "boot-v1.1.0",
                                "version": "v1.1.0-rust",
                                "release_id": "rel-1.1.0",
                                "brainstem_ready": true,
                                "strategy_ready": true,
                                "can_read_only": true,
                                "active_profile": "flat_w5",
                                "uptime_s": 2
                            })),
                        )
                    } else {
                        (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(serde_json::json!({ "ok": false })),
                        )
                    }
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let now = now_ms();
    let manifest = Manifest {
        manifest_version: 1,
        release_id: "rel-1.1.0".into(),
        channel: "qa".into(),
        sequence: 2,
        version: "1.1.0-rust".into(),
        issued_at_ms: now - 1000,
        expires_at_ms: now + 60_000,
        model: "THUNDER 01".into(),
        hardware_revision: "rdk-s100p".into(),
        minimum_bootloader_version: "1.0.0".into(),
        minimum_brainstem_version: "1.0.0".into(),
        minimum_battery_percent: 20,
        artifact_url: format!("http://{addr}/release.tar.gz"),
        artifact_size_bytes: bundle_bytes.len() as u64,
        artifact_sha256: Manifest::artifact_hash(&bundle_bytes),
        key_id: "k1".into(),
        rollback_policy: RollbackPolicy {
            mode: "automatic".into(),
            health_timeout_ms: 2000,
        },
        artifact_type: ArtifactType::BrainstemRelease,
        policy_slot: None,
        shadow_frames: None,
        activate: None,
    };
    let envelope = sign_manifest(&manifest, &key);
    updater.schedule(&envelope, "idem-chan-a-ok").unwrap();

    let health = Arc::new(SystemHealthCheck::simple_http(format!(
        "http://{addr}/health"
    )));
    let mut phases = Vec::new();
    let final_job = updater
        .execute_active_job(Some(health), Duration::from_millis(25), |j| {
            phases.push(j.phase);
        })
        .await
        .unwrap()
        .unwrap();

    assert_eq!(final_job.phase, JobPhase::Succeeded);
    assert_eq!(restarter.count(), 1);
    assert_eq!(updater.profile().current_version, "1.1.0-rust");
    let expected_target = releases_dir.join("1.1.0-rust");
    assert_eq!(read_link_target(&current_link).unwrap(), expected_target);
    assert_eq!(read_link_target(&last_good_link).unwrap(), expected_target);
    assert_eq!(
        fs::read(expected_target.join("bin/doso-brainstem")).unwrap(),
        b"brainstem-v1.1.0-elf"
    );
    assert!(phases.contains(&JobPhase::Downloading));
    assert!(phases.contains(&JobPhase::HealthChecking));
    assert_eq!(*phases.last().unwrap(), JobPhase::Succeeded);

    server.abort();
}

#[tokio::test]
async fn channel_a_failed_health_automatically_rolls_back_to_last_known_good() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[22; 32]);
    let restarter = Arc::new(CountingRestarter::new());
    let (updater, releases_dir, current_link, _last_good_link) =
        setup_release_updater(dir.path(), &key, None, None, restarter.clone());

    let bundle_bytes = Arc::new(build_release_bundle(dir.path(), b"bad-brainstem-v1.2.0"));
    let bundle_for_srv = bundle_bytes.clone();
    let restarter_for_health = restarter.clone();

    let app = Router::new()
        .route(
            "/bad-release.tar.gz",
            get(move || {
                let bytes = bundle_for_srv.clone();
                async move { bytes.as_ref().clone() }
            }),
        )
        .route(
            "/health",
            get(move || {
                let restarts = restarter_for_health.count();
                async move {
                    // Restart #1 (bad release) fails health; Restart #2 (rolled back to 1.0.0-rust) is healthy!
                    if restarts == 1 {
                        (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(serde_json::json!({ "ok": false, "brainstem_ready": false })),
                        )
                    } else {
                        (
                            StatusCode::OK,
                            Json(serde_json::json!({
                                "ok": true,
                                "boot_id": "boot-v1-restored",
                                "version": "1.0.0-rust",
                                "release_id": "rel-1.0.0",
                                "brainstem_ready": true,
                                "strategy_ready": true,
                                "can_read_only": true
                            })),
                        )
                    }
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let now = now_ms();
    let manifest = Manifest {
        manifest_version: 1,
        release_id: "rel-1.2.0".into(),
        channel: "qa".into(),
        sequence: 3,
        version: "1.2.0-rust".into(),
        issued_at_ms: now - 1000,
        expires_at_ms: now + 60_000,
        model: "THUNDER 01".into(),
        hardware_revision: "rdk-s100p".into(),
        minimum_bootloader_version: "1.0.0".into(),
        minimum_brainstem_version: "1.0.0".into(),
        minimum_battery_percent: 20,
        artifact_url: format!("http://{addr}/bad-release.tar.gz"),
        artifact_size_bytes: bundle_bytes.len() as u64,
        artifact_sha256: Manifest::artifact_hash(&bundle_bytes),
        key_id: "k1".into(),
        rollback_policy: RollbackPolicy {
            mode: "automatic".into(),
            health_timeout_ms: 150,
        },
        artifact_type: ArtifactType::BrainstemRelease,
        policy_slot: None,
        shadow_frames: None,
        activate: None,
    };
    let envelope = sign_manifest(&manifest, &key);
    updater.schedule(&envelope, "idem-chan-a-rb").unwrap();

    let health = Arc::new(SystemHealthCheck::simple_http(format!(
        "http://{addr}/health"
    )));
    let final_job = updater
        .execute_active_job(Some(health), Duration::from_millis(25), |_| {})
        .await
        .unwrap()
        .unwrap();

    assert_eq!(final_job.phase, JobPhase::RolledBack);
    assert_eq!(restarter.count(), 2);
    // Symlink must be atomically restored to 1.0.0-rust.
    assert_eq!(
        read_link_target(&current_link).unwrap(),
        releases_dir.join("1.0.0-rust")
    );
    assert_eq!(updater.profile().current_version, "1.0.0-rust");

    server.abort();
}

#[derive(Clone)]
struct MockPolicyState {
    expected_token: String,
    fail_activate: Arc<AtomicBool>,
    fail_golden: Arc<AtomicBool>,
    push_calls: Arc<StdMutex<Vec<serde_json::Value>>>,
    rollback_slots: Arc<StdMutex<Vec<String>>>,
}

#[tokio::test]
async fn channel_b_policy_hot_push_zero_restart_and_failure_auto_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[23; 32]);
    let restarter = Arc::new(CountingRestarter::new());
    let onnx_bytes = b"fake-onnx-model-payload-v2";
    let policy_bundle = Arc::new(build_policy_bundle(dir.path(), onnx_bytes));
    let bundle_for_srv = policy_bundle.clone();

    let mock_state = MockPolicyState {
        expected_token: "admin-secret-5054".into(),
        fail_activate: Arc::new(AtomicBool::new(false)),
        fail_golden: Arc::new(AtomicBool::new(false)),
        push_calls: Arc::new(StdMutex::new(Vec::new())),
        rollback_slots: Arc::new(StdMutex::new(Vec::new())),
    };

    let app = Router::new()
        .route(
            "/policy.tar.gz",
            get(move || {
                let bytes = bundle_for_srv.clone();
                async move { bytes.as_ref().clone() }
            }),
        )
        .route(
            "/v1/policies/push",
            post(
                |State(st): State<MockPolicyState>,
                 headers: HeaderMap,
                 Json(body): Json<serde_json::Value>| async move {
                    let auth = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("");
                    if auth != format!("Bearer {}", st.expected_token) {
                        return (
                            StatusCode::UNAUTHORIZED,
                            Json(serde_json::json!({"ok": false, "error": "unauthorized"})),
                        );
                    }
                    st.push_calls.lock().unwrap().push(body);
                    if st.fail_golden.load(Ordering::SeqCst) {
                        return (
                            StatusCode::UNPROCESSABLE_ENTITY,
                            Json(serde_json::json!({
                                "ok": false,
                                "stage": "golden_or_schema_gate",
                                "error": "golden_mismatch"
                            })),
                        );
                    }
                    if st.fail_activate.load(Ordering::SeqCst) {
                        return (
                            StatusCode::CONFLICT,
                            Json(serde_json::json!({
                                "ok": false,
                                "stage": "activate",
                                "error": "cannot_switch_while_walking"
                            })),
                        );
                    }
                    (
                        StatusCode::OK,
                        Json(serde_json::json!({
                            "ok": true,
                            "name": "flat_v2",
                            "active_profile": "flat_v2"
                        })),
                    )
                },
            ),
        )
        .route(
            "/v1/policies/rollback",
            post(
                |State(st): State<MockPolicyState>,
                 headers: HeaderMap,
                 Json(body): Json<serde_json::Value>| async move {
                    let auth = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("");
                    if auth != format!("Bearer {}", st.expected_token) {
                        return (
                            StatusCode::UNAUTHORIZED,
                            Json(serde_json::json!({"ok": false, "error": "unauthorized"})),
                        );
                    }
                    let slot = body["slot"].as_str().unwrap_or("").to_owned();
                    st.rollback_slots.lock().unwrap().push(slot.clone());
                    (
                        StatusCode::OK,
                        Json(serde_json::json!({
                            "ok": true,
                            "slot": slot,
                            "restored_policy": "flat_w5"
                        })),
                    )
                },
            ),
        )
        .with_state(mock_state.clone());

    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let (updater, _releases_dir, _current_link, _last_good_link) = setup_release_updater(
        dir.path(),
        &key,
        Some(format!("http://{addr}")),
        Some("admin-secret-5054".into()),
        restarter.clone(),
    );

    let make_policy_manifest = |seq: u64, rel_id: &str| {
        let now = now_ms();
        Manifest {
            manifest_version: 1,
            release_id: rel_id.into(),
            channel: "qa".into(),
            sequence: seq,
            version: "1.0.0".into(),
            issued_at_ms: now - 1000,
            expires_at_ms: now + 60_000,
            model: "THUNDER 01".into(),
            hardware_revision: "rdk-s100p".into(),
            minimum_bootloader_version: "1.0.0".into(),
            minimum_brainstem_version: "1.0.0".into(),
            minimum_battery_percent: 20,
            artifact_url: format!("http://{addr}/policy.tar.gz"),
            artifact_size_bytes: policy_bundle.len() as u64,
            artifact_sha256: Manifest::artifact_hash(&policy_bundle),
            key_id: "k1".into(),
            rollback_policy: RollbackPolicy {
                mode: "automatic".into(),
                health_timeout_ms: 1000,
            },
            artifact_type: ArtifactType::PolicyPackage,
            policy_slot: Some("flat".into()),
            shadow_frames: Some(50),
            activate: Some(true),
        }
    };

    // 1) Happy path: zero-restart hot push succeeds.
    let env1 = sign_manifest(&make_policy_manifest(10, "pol-ok"), &key);
    updater.schedule(&env1, "idem-pol-ok").unwrap();
    let job1 = updater
        .execute_active_job::<SystemHealthCheck, _>(None, Duration::from_millis(20), |_| {})
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job1.phase, JobPhase::Succeeded);
    assert_eq!(
        restarter.count(),
        0,
        "policy OTA must NEVER restart brainstem"
    );
    {
        let pushes = mock_state.push_calls.lock().unwrap();
        assert_eq!(pushes.len(), 1);
        assert_eq!(pushes[0]["slot"], "flat");
        assert_eq!(pushes[0]["shadow_frames"], 50);
        assert_eq!(pushes[0]["activate"], true);
        assert!(
            pushes[0]["manifest_yaml"]
                .as_str()
                .unwrap()
                .contains("flat_v2")
        );
        assert!(pushes[0]["golden_json"].as_str().is_some());
    }

    // 2) Activation failure: triggers automatic POST /v1/policies/rollback -> RolledBack, still 0 restarts!
    mock_state.fail_activate.store(true, Ordering::SeqCst);
    let env2 = sign_manifest(&make_policy_manifest(11, "pol-rb"), &key);
    updater.schedule(&env2, "idem-pol-rb").unwrap();
    let job2 = updater
        .execute_active_job::<SystemHealthCheck, _>(None, Duration::from_millis(20), |_| {})
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job2.phase, JobPhase::RolledBack);
    assert_eq!(restarter.count(), 0);
    assert_eq!(
        *mock_state.rollback_slots.lock().unwrap(),
        vec!["flat".to_string()]
    );

    // 3) Pre-mutation Golden gate rejection (422): fails cleanly WITHOUT touching previous good slot!
    mock_state.fail_activate.store(false, Ordering::SeqCst);
    mock_state.fail_golden.store(true, Ordering::SeqCst);
    let env3 = sign_manifest(&make_policy_manifest(12, "pol-bad-golden"), &key);
    updater.schedule(&env3, "idem-pol-bad-golden").unwrap();
    let job3 = updater
        .execute_active_job::<SystemHealthCheck, _>(None, Duration::from_millis(20), |_| {})
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job3.phase, JobPhase::Failed);
    assert_eq!(restarter.count(), 0);
    assert_eq!(mock_state.rollback_slots.lock().unwrap().len(), 1);

    server.abort();
}

#[tokio::test]
async fn cloud_poll_executes_and_reports_progress_and_recovers_interrupted_boot() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[24; 32]);
    let restarter = Arc::new(CountingRestarter::new());
    let policy_bundle = Arc::new(build_policy_bundle(dir.path(), b"onnx-cloud-pol"));
    let bundle_for_srv = policy_bundle.clone();

    let reports: Arc<StdMutex<Vec<serde_json::Value>>> = Arc::new(StdMutex::new(Vec::new()));
    let reports_srv = reports.clone();
    let served_once = Arc::new(AtomicBool::new(false));
    let served_srv = served_once.clone();
    let key_clone = key.clone();

    // Start mock policy :5054 + artifact server first so we know its port.
    let pol_app = Router::new()
        .route(
            "/artifact.tar.gz",
            get(move || {
                let bytes = bundle_for_srv.clone();
                async move { bytes.as_ref().clone() }
            }),
        )
        .route(
            "/v1/policies/push",
            post(|| async { (StatusCode::OK, Json(serde_json::json!({"ok": true}))) }),
        );
    let pol_listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let pol_addr = pol_listener.local_addr().unwrap();
    let pol_srv = tokio::spawn(async move { axum::serve(pol_listener, pol_app).await.unwrap() });

    let bundle_len = policy_bundle.len() as u64;
    let bundle_sha = Manifest::artifact_hash(&policy_bundle);
    let cloud_app = Router::new()
        .route(
            "/v1/agent/devices/{device_id}/next-job",
            get(move || {
                let served = served_srv.clone();
                let key = key_clone.clone();
                let sha = bundle_sha.clone();
                async move {
                    if served.swap(true, Ordering::SeqCst) {
                        return (StatusCode::NO_CONTENT, Json(serde_json::Value::Null));
                    }
                    let now = now_ms();
                    let manifest = Manifest {
                        manifest_version: 1,
                        release_id: "cloud-pol-1".into(),
                        channel: "stable".into(),
                        sequence: 5,
                        version: "1.0.0".into(),
                        issued_at_ms: now - 1000,
                        expires_at_ms: now + 60_000,
                        model: "THUNDER 01".into(),
                        hardware_revision: "rdk-s100p".into(),
                        minimum_bootloader_version: "1.0.0".into(),
                        minimum_brainstem_version: "1.0.0".into(),
                        minimum_battery_percent: 20,
                        artifact_url: format!("http://{pol_addr}/artifact.tar.gz"),
                        artifact_size_bytes: bundle_len,
                        artifact_sha256: sha,
                        key_id: "k-cloud".into(),
                        rollback_policy: RollbackPolicy {
                            mode: "automatic".into(),
                            health_timeout_ms: 1000,
                        },
                        artifact_type: ArtifactType::PolicyPackage,
                        policy_slot: Some("stairs".into()),
                        shadow_frames: Some(50),
                        activate: Some(true),
                    };
                    let envelope = sign_manifest(&manifest, &key);
                    (
                        StatusCode::OK,
                        Json(serde_json::json!({
                            "jobId": "job-cloud-99",
                            "idempotencyKey": "idem-cloud-99",
                            "manifest": envelope
                        })),
                    )
                }
            }),
        )
        .route(
            "/v1/agent/devices/{device_id}/jobs/{job_id}/report",
            post(move |Json(body): Json<serde_json::Value>| {
                let reports = reports_srv.clone();
                async move {
                    reports.lock().unwrap().push(body);
                    (StatusCode::OK, Json(serde_json::json!({"ok": true})))
                }
            }),
        );
    let cloud_listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let cloud_addr = cloud_listener.local_addr().unwrap();
    let cloud_srv =
        tokio::spawn(async move { axum::serve(cloud_listener, cloud_app).await.unwrap() });

    let (updater, _releases_dir, _current_link, _last_good_link) = setup_release_updater(
        dir.path(),
        &key,
        Some(format!("http://{pol_addr}")),
        None,
        restarter.clone(),
    );
    let updater = Arc::new(Mutex::new(updater));
    let client = CloudClient::new(format!("http://{cloud_addr}"), Some("tok".into())).unwrap();

    let executed = cloud::poll_and_execute_once::<SystemHealthCheck>(
        &updater,
        None,
        &client,
        Duration::from_millis(20),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(executed.job_id, "job-cloud-99");
    assert_eq!(executed.phase, JobPhase::Succeeded);
    assert_eq!(restarter.count(), 0);

    {
        let posted = reports.lock().unwrap();
        assert!(posted.iter().any(|r| r["phase"] == "downloading"));
        assert_eq!(posted.last().unwrap()["phase"], "succeeded");
    }

    // Second poll gets 204 No Content -> Ok(None).
    let second = cloud::poll_and_execute_once::<SystemHealthCheck>(
        &updater,
        None,
        &client,
        Duration::from_millis(20),
    )
    .await
    .unwrap();
    assert!(second.is_none());

    pol_srv.abort();
    cloud_srv.abort();
}

#[tokio::test]
async fn startup_recovery_finishes_interrupted_rebooting_job_and_rolls_back_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[25; 32]);
    let restarter = Arc::new(CountingRestarter::new());
    let (updater, releases_dir, current_link, _last_good_link) =
        setup_release_updater(dir.path(), &key, None, None, restarter.clone());

    // Simulate a crash right after Channel A symlink swap + restart (persisted Rebooting job).
    let broken_v2 = releases_dir.join("1.3.0-rust");
    fs::create_dir_all(broken_v2.join("bin")).unwrap();
    fs::write(broken_v2.join("bin/doso-brainstem"), b"broken-v2").unwrap();
    atomic_swap_link(&current_link, &broken_v2).unwrap();

    let now = now_ms();
    let manifest = Manifest {
        manifest_version: 1,
        release_id: "rel-1.3.0".into(),
        channel: "qa".into(),
        sequence: 9,
        version: "1.3.0-rust".into(),
        issued_at_ms: now - 1000,
        expires_at_ms: now + 60_000,
        model: "THUNDER 01".into(),
        hardware_revision: "rdk-s100p".into(),
        minimum_bootloader_version: "1.0.0".into(),
        minimum_brainstem_version: "1.0.0".into(),
        minimum_battery_percent: 20,
        artifact_url: "https://example.test/v1.3.0.tar.gz".into(),
        artifact_size_bytes: 16,
        artifact_sha256: Manifest::artifact_hash(b"0123456789abcdef"),
        key_id: "k1".into(),
        rollback_policy: RollbackPolicy {
            mode: "automatic".into(),
            health_timeout_ms: 120,
        },
        artifact_type: ArtifactType::BrainstemRelease,
        policy_slot: None,
        shadow_frames: None,
        activate: None,
    };
    let persisted = doso_ota::Job {
        job_id: "job-crash-rec".into(),
        idempotency_key: "idem-crash-rec".into(),
        device_id: "dev-s100p-01".into(),
        release_id: "rel-1.3.0".into(),
        version: "1.3.0-rust".into(),
        manifest,
        phase: JobPhase::Rebooting,
        downloaded_bytes: 16,
        total_bytes: 16,
        active_slot: doso_ota::Slot::A,
        last_known_good_slot: doso_ota::Slot::A,
        last_known_good_version: "1.0.0-rust".into(),
        progress_sequence: 3,
        boot_id_before: Some("boot-v1".into()),
        boot_id_after: None,
        error: None,
        previous_release_path: Some(
            releases_dir
                .join("1.0.0-rust")
                .to_string_lossy()
                .into_owned(),
        ),
        installed_release_path: Some(broken_v2.to_string_lossy().into_owned()),
    };
    fs::write(
        updater.config().root.join("job.json"),
        serde_json::to_vec_pretty(&persisted).unwrap(),
    )
    .unwrap();

    // Mock health server: unhealthy before rollback restart, healthy on 1.0.0-rust after rollback.
    let restarter_for_srv = restarter.clone();
    let app = Router::new().route(
        "/health",
        get(move || {
            let r = restarter_for_srv.count();
            async move {
                if r == 0 {
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({ "ok": false })),
                    )
                } else {
                    (
                        StatusCode::OK,
                        Json(serde_json::json!({
                            "ok": true,
                            "boot_id": "boot-v1-recovered",
                            "version": "1.0.0-rust",
                            "brainstem_ready": true,
                            "strategy_ready": true,
                            "can_read_only": true
                        })),
                    )
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let srv = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let health = Arc::new(SystemHealthCheck::simple_http(format!(
        "http://{addr}/health"
    )));
    let shared = Arc::new(Mutex::new(updater));
    let recovered =
        cloud::recover_on_startup(&shared, Some(health), None, Duration::from_millis(20))
            .await
            .unwrap()
            .unwrap();

    assert_eq!(recovered.phase, JobPhase::RolledBack);
    assert_eq!(restarter.count(), 1);
    assert_eq!(
        read_link_target(&current_link).unwrap(),
        releases_dir.join("1.0.0-rust")
    );

    srv.abort();
}
