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
        components_root: None,
        keep_history: 2,
        require_can_read_only: false,
        min_vbus_v: 44.0,
        ota_unit: "doso-ota.service".into(),
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
        component: None,
        systemd_unit: None,
        health_url: None,
        requires: Default::default(),
        bundle_items: Vec::new(),
        delta: None,
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
        component: None,
        systemd_unit: None,
        health_url: None,
        requires: Default::default(),
        bundle_items: Vec::new(),
        delta: None,
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
            component: None,
            systemd_unit: None,
            health_url: None,
            requires: Default::default(),
            bundle_items: Vec::new(),
            delta: None,
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
                        component: None,
                        systemd_unit: None,
                        health_url: None,
                        requires: Default::default(),
                        bundle_items: Vec::new(),
                        delta: None,
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
        component: None,
        systemd_unit: None,
        health_url: None,
        requires: Default::default(),
        bundle_items: Vec::new(),
        delta: None,
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

struct UnitRecordingRestarter {
    units: StdMutex<Vec<String>>,
}

impl UnitRecordingRestarter {
    fn new() -> Self {
        Self {
            units: StdMutex::new(Vec::new()),
        }
    }

    fn units(&self) -> Vec<String> {
        self.units.lock().unwrap().clone()
    }

    fn count_for(&self, unit: &str) -> usize {
        self.units
            .lock()
            .unwrap()
            .iter()
            .filter(|u| u.as_str() == unit)
            .count()
    }
}

impl ServiceRestarter for UnitRecordingRestarter {
    fn restart(&self, unit: &str) -> anyhow::Result<()> {
        self.units.lock().unwrap().push(unit.to_owned());
        Ok(())
    }
}

fn build_component_bundle(tmp: &Path, bin_name: &str, payload: &[u8]) -> Vec<u8> {
    let src = tmp.join(format!("comp_src_{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(src.join("bin")).unwrap();
    fs::write(src.join("bin").join(bin_name), payload).unwrap();
    let hash = Manifest::artifact_hash(payload);
    fs::write(src.join("SHA256SUMS"), format!("{hash}  bin/{bin_name}\n")).unwrap();
    let archive = tmp.join(format!("comp_{}.tar.gz", uuid::Uuid::new_v4()));
    make_tar_gz(&src, &archive)
}

#[tokio::test]
async fn multi_component_install_never_restarts_brainstem_rolls_back_and_runs_gc() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[31; 32]);
    let key_path = dir.path().join("pub.pem");
    fs::write(
        &key_path,
        key.verifying_key()
            .to_public_key_pem(Default::default())
            .unwrap(),
    )
    .unwrap();

    let components_root = dir.path().join("opt/doso/components");
    let cl_releases = components_root.join("cloud-link/releases");
    for old_ver in ["0.0.1", "0.0.2", "0.0.3", "0.0.4"] {
        let p = cl_releases.join(old_ver);
        fs::create_dir_all(p.join("bin")).unwrap();
        fs::write(p.join("bin/doso-cloud-link"), old_ver.as_bytes()).unwrap();
    }
    let cl_current = components_root.join("cloud-link/current");
    let cl_last_good = components_root.join("cloud-link/last-known-good");
    atomic_swap_link(&cl_current, &cl_releases.join("0.0.4")).unwrap();
    atomic_swap_link(&cl_last_good, &cl_releases.join("0.0.4")).unwrap();

    let restarter = Arc::new(UnitRecordingRestarter::new());
    let cfg = UpdaterConfig {
        root: dir.path().join("var/lib/doso-ota"),
        device_id: "dev-mc-01".into(),
        model: "THUNDER 01".into(),
        hardware_revision: "rdk-s100p".into(),
        bootloader_version: "1.0.0".into(),
        brainstem_version: "1.0.0-rust".into(),
        current_version: "1.0.0-rust".into(),
        battery_percent: 90,
        online: true,
        task_running: false,
        boot_id_path: dir.path().join("run/boot_id"),
        verify_key_path: key_path,
        brainstem_unit: "doso-brainstem.service".into(),
        staging_dir: Some(dir.path().join("ota/staging")),
        releases_dir: None,
        current_link: None,
        last_good_link: None,
        components_root: Some(components_root.clone()),
        keep_history: 2,
        require_can_read_only: false,
        min_vbus_v: 44.0,
        ota_unit: "doso-ota.service".into(),
        policy_admin_url: None,
        policy_admin_token: None,
        allow_insecure_http_loopback: true,
    };
    let updater = Updater::with_restarter(cfg, restarter.clone()).unwrap();

    let good_bundle = Arc::new(build_component_bundle(
        dir.path(),
        "doso-cloud-link",
        b"cloud-link-0.1.0",
    ));
    let bad_bundle = Arc::new(build_component_bundle(
        dir.path(),
        "doso-cloud-link",
        b"cloud-link-0.2.0-broken",
    ));
    let good_srv = good_bundle.clone();
    let bad_srv = bad_bundle.clone();
    let fail_health = Arc::new(AtomicBool::new(false));
    let fail_health_srv = fail_health.clone();
    let restarter_srv = restarter.clone();

    let app = Router::new()
        .route(
            "/cloud-link-good.tar.gz",
            get(move || {
                let b = good_srv.clone();
                async move { b.as_ref().clone() }
            }),
        )
        .route(
            "/cloud-link-bad.tar.gz",
            get(move || {
                let b = bad_srv.clone();
                async move { b.as_ref().clone() }
            }),
        )
        .route(
            "/cl-health",
            get(move || {
                let fail = fail_health_srv.load(Ordering::SeqCst);
                let cl_restarts = restarter_srv.count_for("doso-cloud-link.service");
                async move {
                    if fail && cl_restarts == 2 {
                        (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(serde_json::json!({ "ok": false })),
                        )
                    } else if cl_restarts >= 3 {
                        (
                            StatusCode::OK,
                            Json(serde_json::json!({ "ok": true, "version": "0.1.0" })),
                        )
                    } else {
                        (
                            StatusCode::OK,
                            Json(serde_json::json!({ "ok": true, "version": "0.1.0", "releaseId": "cl-0.1.0" })),
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

    let now = now_ms();
    let mut m1 = Manifest {
        manifest_version: 1,
        release_id: "cl-0.1.0".into(),
        channel: "qa".into(),
        sequence: 1,
        version: "0.1.0".into(),
        issued_at_ms: now - 1000,
        expires_at_ms: now + 60_000,
        model: "THUNDER 01".into(),
        hardware_revision: "rdk-s100p".into(),
        minimum_bootloader_version: "1.0.0".into(),
        minimum_brainstem_version: "1.0.0".into(),
        minimum_battery_percent: 20,
        artifact_url: format!("http://{addr}/cloud-link-good.tar.gz"),
        artifact_size_bytes: good_bundle.len() as u64,
        artifact_sha256: Manifest::artifact_hash(&good_bundle),
        key_id: "k1".into(),
        rollback_policy: RollbackPolicy {
            mode: "automatic".into(),
            health_timeout_ms: 200,
        },
        artifact_type: ArtifactType::ComponentRelease,
        policy_slot: None,
        shadow_frames: None,
        activate: None,
        component: Some("cloud-link".into()),
        systemd_unit: Some("doso-cloud-link.service".into()),
        health_url: Some(format!("http://{addr}/cl-health")),
        requires: std::collections::BTreeMap::from([("brainstem".into(), "1.0.0".into())]),
        bundle_items: Vec::new(),
        delta: None,
    };
    updater
        .schedule(&sign_manifest(&m1, &key), "idem-cl-1")
        .unwrap();
    let job1 = updater
        .execute_active_job::<SystemHealthCheck, _>(None, Duration::from_millis(20), |_| {})
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job1.phase, JobPhase::Succeeded);
    assert_eq!(restarter.count_for("doso-cloud-link.service"), 1);
    assert_eq!(
        restarter.count_for("doso-brainstem.service"),
        0,
        "upgrading cloud-link must NEVER restart doso-brainstem"
    );
    assert_eq!(
        updater.component_version("cloud-link").as_deref(),
        Some("0.1.0")
    );
    assert_eq!(
        read_link_target(&cl_current).unwrap(),
        cl_releases.join("0.1.0")
    );
    // GC kept current/last-good (0.1.0) + 2 newest history (0.0.4, 0.0.3) and deleted 0.0.1, 0.0.2!
    assert!(cl_releases.join("0.1.0").is_dir());
    assert!(cl_releases.join("0.0.4").is_dir());
    assert!(cl_releases.join("0.0.3").is_dir());
    assert!(!cl_releases.join("0.0.2").exists());
    assert!(!cl_releases.join("0.0.1").exists());
    // Staging artifact cleaned up after commit.
    assert!(
        !dir.path()
            .join(format!("ota/staging/{}.artifact", job1.job_id))
            .exists()
    );

    // 2) Bad cloud-link release fails health -> auto-rolls back to 0.1.0, still 0 brainstem restarts!
    fail_health.store(true, Ordering::SeqCst);
    m1.release_id = "cl-0.2.0".into();
    m1.sequence = 2;
    m1.version = "0.2.0".into();
    m1.artifact_url = format!("http://{addr}/cloud-link-bad.tar.gz");
    m1.artifact_size_bytes = bad_bundle.len() as u64;
    m1.artifact_sha256 = Manifest::artifact_hash(&bad_bundle);
    updater
        .schedule(&sign_manifest(&m1, &key), "idem-cl-2")
        .unwrap();
    let job2 = updater
        .execute_active_job::<SystemHealthCheck, _>(None, Duration::from_millis(20), |_| {})
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job2.phase, JobPhase::RolledBack);
    assert_eq!(restarter.count_for("doso-cloud-link.service"), 3);
    assert_eq!(restarter.count_for("doso-brainstem.service"), 0);
    assert_eq!(
        read_link_target(&cl_current).unwrap(),
        cl_releases.join("0.1.0")
    );

    srv.abort();
}

#[tokio::test]
async fn real_robot_waiting_safe_window_blocks_while_walking_then_succeeds_with_can_read_only_false()
 {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[32; 32]);
    let restarter = Arc::new(CountingRestarter::new());
    let (mut updater, releases_dir, current_link, _last_good_link) =
        setup_release_updater(dir.path(), &key, None, None, restarter.clone());

    let bundle_bytes = Arc::new(build_release_bundle(dir.path(), b"brainstem-real-v1.1.0"));
    let bundle_srv = bundle_bytes.clone();
    let is_walking = Arc::new(AtomicBool::new(true));
    let walking_srv = is_walking.clone();
    let restarter_srv = restarter.clone();

    let app = Router::new()
        .route(
            "/real-brainstem.tar.gz",
            get(move || {
                let b = bundle_srv.clone();
                async move { b.as_ref().clone() }
            }),
        )
        .route(
            "/health",
            get(move || {
                let walking = walking_srv.load(Ordering::SeqCst);
                let restarts = restarter_srv.count();
                async move {
                    if walking {
                        Json(serde_json::json!({
                            "ok": true,
                            "boot_id": "boot-v1",
                            "version": "1.0.0-rust",
                            "release_id": "rel-1.0.0",
                            "brainstem_ready": true,
                            "strategy_ready": true,
                            "can_read_only": false,
                            "motor_output_enabled": true,
                            "cms_state": "walking",
                            "vbus_min_v": 48.2,
                            "battery_percent": 88
                        }))
                    } else if restarts == 0 {
                        Json(serde_json::json!({
                            "ok": true,
                            "boot_id": "boot-v1",
                            "version": "1.0.0-rust",
                            "release_id": "rel-1.0.0",
                            "brainstem_ready": true,
                            "strategy_ready": true,
                            "can_read_only": false,
                            "motor_output_enabled": false,
                            "cms_state": "grounded",
                            "vbus_min_v": 48.2,
                            "battery_percent": 88
                        }))
                    } else {
                        // Real robot post-upgrade health: can_read_only=false, motors disabled!
                        Json(serde_json::json!({
                            "ok": true,
                            "boot_id": "boot-v1.1.0",
                            "version": "v1.1.0-rust",
                            "release_id": "rel-real-1.1.0",
                            "brainstem_ready": true,
                            "strategy_ready": true,
                            "can_read_only": false,
                            "motor_output_enabled": false,
                            "cms_state": "grounded",
                            "online_mask": 65535,
                            "imu_fresh": true,
                            "vbus_min_v": 48.0,
                            "battery_percent": 87
                        }))
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let srv = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let health_url = format!("http://{addr}/health");
    updater.set_safety_check(Some(Arc::new(doso_ota::HttpSafetyPreCheck::new(
        health_url.clone(),
    ))));

    let now = now_ms();
    let manifest = Manifest {
        manifest_version: 1,
        release_id: "rel-real-1.1.0".into(),
        channel: "stable".into(),
        sequence: 2,
        version: "1.1.0-rust".into(),
        issued_at_ms: now - 1000,
        expires_at_ms: now + 60_000,
        model: "THUNDER 01".into(),
        hardware_revision: "rdk-s100p".into(),
        minimum_bootloader_version: "1.0.0".into(),
        minimum_brainstem_version: "1.0.0".into(),
        minimum_battery_percent: 30,
        artifact_url: format!("http://{addr}/real-brainstem.tar.gz"),
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
        component: None,
        systemd_unit: None,
        health_url: None,
        requires: Default::default(),
        bundle_items: Vec::new(),
        delta: None,
    };
    updater
        .schedule(&sign_manifest(&manifest, &key), "idem-real-safe")
        .unwrap();

    let health = Arc::new(SystemHealthCheck::simple_http(health_url));
    // Tick 1: robot is walking -> downloads in background, parks cleanly in WaitingSafeWindow!
    let tick1 = updater
        .execute_active_job(Some(health.clone()), Duration::from_millis(20), |_| {})
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tick1.phase, JobPhase::WaitingSafeWindow);
    assert_eq!(
        restarter.count(),
        0,
        "must NOT restart brainstem while robot is walking"
    );

    // Robot sits down & disables motors -> Tick 2 installs, restarts, and passes with can_read_only=false!
    is_walking.store(false, Ordering::SeqCst);
    let tick2 = updater
        .execute_active_job(Some(health), Duration::from_millis(20), |_| {})
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tick2.phase, JobPhase::Succeeded);
    assert_eq!(restarter.count(), 1);
    assert_eq!(
        read_link_target(&current_link).unwrap(),
        releases_dir.join("1.1.0-rust")
    );
    assert_eq!(updater.profile().current_version, "1.1.0-rust");

    srv.abort();
}

#[tokio::test]
async fn release_bundle_installs_in_order_and_rolls_back_all_items_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[33; 32]);
    let key_path = dir.path().join("pub.pem");
    fs::write(
        &key_path,
        key.verifying_key()
            .to_public_key_pem(Default::default())
            .unwrap(),
    )
    .unwrap();

    let components_root = dir.path().join("opt/doso/components");
    let bs_v1 = components_root.join("brainstem/releases/1.0.0");
    fs::create_dir_all(bs_v1.join("bin")).unwrap();
    fs::write(bs_v1.join("bin/doso-brainstem"), b"bs-v1").unwrap();
    atomic_swap_link(&components_root.join("brainstem/current"), &bs_v1).unwrap();
    atomic_swap_link(&components_root.join("brainstem/last-known-good"), &bs_v1).unwrap();

    let cl_v1 = components_root.join("cloud-link/releases/0.1.0");
    fs::create_dir_all(cl_v1.join("bin")).unwrap();
    fs::write(cl_v1.join("bin/doso-cloud-link"), b"cl-v1").unwrap();
    atomic_swap_link(&components_root.join("cloud-link/current"), &cl_v1).unwrap();
    atomic_swap_link(&components_root.join("cloud-link/last-known-good"), &cl_v1).unwrap();

    let bs_pkg = Arc::new(build_release_bundle(dir.path(), b"bs-v1.2.0"));
    let cl_pkg = Arc::new(build_component_bundle(
        dir.path(),
        "doso-cloud-link",
        b"cl-v0.2.0",
    ));
    let bs_srv = bs_pkg.clone();
    let cl_srv = cl_pkg.clone();
    let fail_cl = Arc::new(AtomicBool::new(true));
    let fail_cl_srv = fail_cl.clone();

    let app = Router::new()
        .route(
            "/bs.tar.gz",
            get(move || {
                let b = bs_srv.clone();
                async move { b.as_ref().clone() }
            }),
        )
        .route(
            "/cl.tar.gz",
            get(move || {
                let b = cl_srv.clone();
                async move { b.as_ref().clone() }
            }),
        )
        .route(
            "/bs-health",
            get(|| async {
                Json(serde_json::json!({
                    "ok": true,
                    "version": "1.2.0",
                    "brainstem_ready": true,
                    "strategy_ready": true,
                    "can_read_only": false,
                    "motor_output_enabled": false
                }))
            }),
        )
        .route(
            "/cl-health",
            get(move || {
                let fail = fail_cl_srv.load(Ordering::SeqCst);
                async move {
                    if fail {
                        (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(serde_json::json!({ "ok": false })),
                        )
                    } else {
                        (
                            StatusCode::OK,
                            Json(serde_json::json!({ "ok": true, "version": "0.2.0" })),
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

    let restarter = Arc::new(UnitRecordingRestarter::new());
    let cfg = UpdaterConfig {
        root: dir.path().join("var/lib/doso-ota"),
        device_id: "dev-bundle-01".into(),
        model: "THUNDER 01".into(),
        hardware_revision: "rdk-s100p".into(),
        bootloader_version: "1.0.0".into(),
        brainstem_version: "1.0.0".into(),
        current_version: "1.0.0".into(),
        battery_percent: 95,
        online: true,
        task_running: false,
        boot_id_path: dir.path().join("run/boot_id"),
        verify_key_path: key_path,
        brainstem_unit: "doso-brainstem.service".into(),
        staging_dir: Some(dir.path().join("ota/staging")),
        releases_dir: None,
        current_link: None,
        last_good_link: None,
        components_root: Some(components_root.clone()),
        keep_history: 0,
        require_can_read_only: false,
        min_vbus_v: 44.0,
        ota_unit: "doso-ota.service".into(),
        policy_admin_url: None,
        policy_admin_token: None,
        allow_insecure_http_loopback: true,
    };
    let updater = Updater::with_restarter(cfg, restarter.clone()).unwrap();

    let now = now_ms();
    let make_bundle_manifest = |seq: u64| Manifest {
        manifest_version: 1,
        release_id: format!("bundle-{seq}"),
        channel: "qa".into(),
        sequence: seq,
        version: format!("2.0.{seq}"),
        issued_at_ms: now - 1000,
        expires_at_ms: now + 60_000,
        model: "THUNDER 01".into(),
        hardware_revision: "rdk-s100p".into(),
        minimum_bootloader_version: "1.0.0".into(),
        minimum_brainstem_version: "1.0.0".into(),
        minimum_battery_percent: 20,
        artifact_url: String::new(),
        artifact_size_bytes: 0,
        artifact_sha256: String::new(),
        key_id: "k1".into(),
        rollback_policy: RollbackPolicy {
            mode: "automatic".into(),
            health_timeout_ms: 150,
        },
        artifact_type: ArtifactType::ReleaseBundle,
        policy_slot: None,
        shadow_frames: None,
        activate: None,
        component: None,
        systemd_unit: None,
        health_url: None,
        requires: Default::default(),
        bundle_items: vec![
            doso_ota::BundleItem {
                component: "brainstem".into(),
                version: "1.2.0".into(),
                artifact_url: format!("http://{addr}/bs.tar.gz"),
                artifact_size_bytes: bs_pkg.len() as u64,
                artifact_sha256: Manifest::artifact_hash(&bs_pkg),
                systemd_unit: Some("doso-brainstem.service".into()),
                health_url: None,
                requires: Default::default(),
                delta: None,
            },
            doso_ota::BundleItem {
                component: "cloud-link".into(),
                version: "0.2.0".into(),
                artifact_url: format!("http://{addr}/cl.tar.gz"),
                artifact_size_bytes: cl_pkg.len() as u64,
                artifact_sha256: Manifest::artifact_hash(&cl_pkg),
                systemd_unit: Some("doso-cloud-link.service".into()),
                health_url: Some(format!("http://{addr}/cl-health")),
                requires: std::collections::BTreeMap::from([("brainstem".into(), "1.2.0".into())]),
                delta: None,
            },
        ],
        delta: None,
    };

    let bs_health = Arc::new(SystemHealthCheck::simple_http(format!(
        "http://{addr}/bs-health"
    )));

    // 1) Second item fails health -> both cloud-link and brainstem roll back,
    //    last-known-good links stay intact, and previous release dirs survive on disk even with keep_history=0!
    updater
        .schedule(&sign_manifest(&make_bundle_manifest(1), &key), "idem-bun-1")
        .unwrap();
    let j1 = updater
        .execute_active_job(Some(bs_health.clone()), Duration::from_millis(20), |_| {})
        .await
        .unwrap()
        .unwrap();
    assert_eq!(j1.phase, JobPhase::RolledBack);
    assert!(
        bs_v1.is_dir(),
        "prev brainstem release must NOT be GC'd on mid-bundle rollback"
    );
    assert!(
        cl_v1.is_dir(),
        "prev cloud-link release must NOT be GC'd on mid-bundle rollback"
    );
    assert_eq!(
        read_link_target(&components_root.join("brainstem/current")).unwrap(),
        bs_v1
    );
    assert_eq!(
        read_link_target(&components_root.join("brainstem/last-known-good")).unwrap(),
        bs_v1
    );
    assert_eq!(
        read_link_target(&components_root.join("cloud-link/current")).unwrap(),
        cl_v1
    );
    assert_eq!(
        read_link_target(&components_root.join("cloud-link/last-known-good")).unwrap(),
        cl_v1
    );
    assert_eq!(
        updater.component_version("brainstem").as_deref(),
        Some("1.0.0")
    );

    // 2) Fix cloud-link health -> bundle succeeds cleanly!
    fail_cl.store(false, Ordering::SeqCst);
    updater
        .schedule(&sign_manifest(&make_bundle_manifest(2), &key), "idem-bun-2")
        .unwrap();
    let j2 = updater
        .execute_active_job(Some(bs_health), Duration::from_millis(20), |_| {})
        .await
        .unwrap()
        .unwrap();
    assert_eq!(j2.phase, JobPhase::Succeeded);
    assert_eq!(
        updater.component_version("brainstem").as_deref(),
        Some("1.2.0")
    );
    assert_eq!(
        updater.component_version("cloud-link").as_deref(),
        Some("0.2.0")
    );
    assert_eq!(
        restarter.units().last().map(String::as_str),
        Some("doso-cloud-link.service")
    );

    srv.abort();
}
