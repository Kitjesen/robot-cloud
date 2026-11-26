pub mod rollout;

pub use rollout::{
    CircuitBreakerConfig, CircuitBreakerState, CreateRolloutRequest, RolloutCampaign,
    RolloutDeviceState, RolloutStage, RolloutStageCounts, RolloutStatus, RolloutView,
    device_bucket,
};

use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use doso_ota::{ArtifactType, Manifest, ManifestEnvelope};
use ed25519_dalek::{Signer, SigningKey, pkcs8::DecodePrivateKey};
use hmac::{Hmac, Mac};
use rollout::{
    build_rollout_view, eligible_stage_for_device, evaluate_circuit_breaker, load_rollouts,
    maybe_auto_promote, persist_rollout, validate_rollout_id, validate_rollout_request,
};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;
use url::form_urlencoded;
use uuid::Uuid;

type HmacSha1 = Hmac<Sha1>;
const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;

fn default_ring() -> String {
    "prod".into()
}

#[derive(Clone)]
pub struct Config {
    pub root: PathBuf,
    pub device_registry: PathBuf,
    pub verify_api_token: String,
    pub signing_key_path: PathBuf,
    pub signing_key_id: String,
    pub oss_bucket: String,
    pub oss_endpoint: String,
    pub oss_access_key_id: String,
    pub oss_access_key_secret: String,
    pub signed_url_ttl_seconds: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceRecord {
    pub model: String,
    pub hardware_revision: String,
    pub enabled: bool,
    #[serde(default = "default_ring")]
    pub ring: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReleaseTemplate {
    pub manifest: Manifest,
    #[serde(default)]
    pub object_key: String,
    #[serde(default)]
    pub bundle_object_keys: BTreeMap<String, String>,
    #[serde(default)]
    pub delta_object_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseSummary {
    pub release_id: String,
    pub version: String,
    pub sequence: u64,
    pub channel: String,
    pub model: String,
    pub hardware_revision: String,
    pub artifact_size_bytes: u64,
    pub artifact_sha256: String,
    #[serde(default)]
    pub artifact_type: ArtifactType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_slot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartRequest {
    pub release_id: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentReportRequest {
    pub phase: String,
    #[serde(default, alias = "bytes_downloaded")]
    pub bytes_downloaded: Option<u64>,
    #[serde(default, alias = "current_version")]
    pub current_version: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub job_id: String,
    pub device_id: String,
    pub release_id: String,
    pub idempotency_key: String,
    pub phase: String,
    pub manifest: ManifestEnvelope,
    pub created_at_ms: i64,
    #[serde(default)]
    pub updated_at_ms: i64,
    #[serde(default)]
    pub bytes_downloaded: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollout_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AndroidAppReleaseTemplate {
    pub channel: String,
    pub version_name: String,
    pub version_code: u64,
    #[serde(default)]
    pub min_supported_code: u64,
    #[serde(default)]
    pub mandatory: bool,
    #[serde(default)]
    pub changelog: String,
    pub apk_sha256: String,
    pub apk_size_bytes: u64,
    pub object_key: String,
    #[serde(default)]
    pub published_at_ms: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AndroidAppPublishRequest {
    #[serde(default = "default_android_channel")]
    pub channel: String,
    pub version_name: String,
    pub version_code: u64,
    #[serde(default)]
    pub min_supported_code: u64,
    #[serde(default)]
    pub mandatory: bool,
    #[serde(default)]
    pub changelog: String,
    pub apk_sha256: String,
    pub apk_size_bytes: u64,
    pub object_key: String,
}

fn default_android_channel() -> String {
    "prod".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct AndroidAppLatestQuery {
    #[serde(default = "default_android_channel")]
    pub channel: String,
    #[serde(default)]
    pub version_code: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AndroidAppLatestResponse {
    pub channel: String,
    pub version_name: String,
    pub version_code: u64,
    pub min_supported_code: u64,
    pub mandatory: bool,
    pub update_available: bool,
    pub force_update: bool,
    pub changelog: String,
    pub apk_sha256: String,
    pub apk_size_bytes: u64,
    pub download_url: String,
    pub published_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DevicePolicyInfo {
    pub name: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default, alias = "warmup_us")]
    pub warmup_us: u64,
    #[serde(default, alias = "golden_steps")]
    pub golden_steps: usize,
    #[serde(default, alias = "golden_max_err")]
    pub golden_max_err: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRtLoopInfo {
    #[serde(default, alias = "max_tick_us")]
    pub max_tick_us: u64,
    #[serde(default, alias = "max_jitter_us")]
    pub max_jitter_us: u64,
    #[serde(default, alias = "max_mutex_wait_us")]
    pub max_mutex_wait_us: u64,
    #[serde(default)]
    pub ticks: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStatusReportRequest {
    #[serde(default, alias = "boot_id")]
    pub boot_id: Option<String>,
    #[serde(default, alias = "firmware_version", alias = "version")]
    pub firmware_version: Option<String>,
    #[serde(default, alias = "release_id")]
    pub release_id: Option<String>,
    #[serde(default, alias = "brainstem_ready")]
    pub brainstem_ready: bool,
    #[serde(default, alias = "strategy_ready")]
    pub strategy_ready: bool,
    #[serde(default, alias = "can_read_only")]
    pub can_read_only: bool,
    #[serde(default, alias = "active_profile")]
    pub active_profile: Option<String>,
    #[serde(default)]
    pub slots: BTreeMap<String, String>,
    #[serde(default)]
    pub policies: Vec<DevicePolicyInfo>,
    #[serde(default, alias = "rt_loop", skip_serializing_if = "Option::is_none")]
    pub rt_loop: Option<DeviceRtLoopInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStatusRecord {
    pub device_id: String,
    pub reported_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_age_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_id: Option<String>,
    pub brainstem_ready: bool,
    pub strategy_ready: bool,
    pub can_read_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_profile: Option<String>,
    #[serde(default)]
    pub slots: BTreeMap<String, String>,
    #[serde(default)]
    pub policies: Vec<DevicePolicyInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rt_loop: Option<DeviceRtLoopInfo>,
}

#[derive(Clone)]
struct AppState {
    config: Config,
    expected_token_hash: [u8; 32],
    signing_key: Arc<SigningKey>,
    jobs: Arc<Mutex<HashMap<String, Job>>>,
    rollouts: Arc<Mutex<HashMap<String, RolloutCampaign>>>,
}

pub fn build_router(config: Config) -> Result<Router> {
    ensure!(
        !config.verify_api_token.trim().is_empty(),
        "api_token_empty"
    );
    ensure!(
        !config.signing_key_id.trim().is_empty(),
        "signing_key_id_empty"
    );
    ensure!(!config.oss_bucket.trim().is_empty(), "oss_bucket_empty");
    ensure!(!config.oss_endpoint.trim().is_empty(), "oss_endpoint_empty");
    ensure!(
        config.signed_url_ttl_seconds >= 60,
        "signed_url_ttl_too_small"
    );
    // Validate registry and signing key up front so broken config fails fast at boot.
    let _ = load_registry(&config.device_registry)?;
    let signing_key = SigningKey::from_pkcs8_pem(&fs::read_to_string(&config.signing_key_path)?)?;
    let jobs = load_jobs(&config.root)?;
    let rollouts = load_rollouts(&config.root)?;
    let expected_token_hash =
        Sha256::digest(format!("Bearer {}", config.verify_api_token).as_bytes()).into();
    let state = AppState {
        config,
        expected_token_hash,
        signing_key: Arc::new(signing_key),
        jobs: Arc::new(Mutex::new(jobs)),
        rollouts: Arc::new(Mutex::new(rollouts)),
    };
    let protected = Router::new()
        .route(
            "/v1/apps/android/releases",
            post(publish_android_app_release),
        )
        .route("/v1/devices/status", get(list_device_statuses))
        .route("/v1/devices/{device_id}/status", get(read_device_status))
        .route(
            "/v1/agent/devices/{device_id}/status",
            post(agent_report_device_status),
        )
        .route("/v1/devices/{device_id}/firmware/releases", get(releases))
        .route("/v1/devices/{device_id}/firmware/jobs", post(start_job))
        .route(
            "/v1/devices/{device_id}/firmware/jobs/{job_id}",
            get(read_job),
        )
        .route(
            "/v1/devices/{device_id}/firmware/jobs/{job_id}/cancel",
            post(cancel_job),
        )
        .route(
            "/v1/devices/{device_id}/firmware/next-job",
            get(agent_next_job),
        )
        .route(
            "/v1/devices/{device_id}/firmware/jobs/{job_id}/report",
            post(agent_report_job),
        )
        .route(
            "/v1/agent/devices/{device_id}/next-job",
            get(agent_next_job),
        )
        .route(
            "/v1/agent/devices/{device_id}/jobs/{job_id}/report",
            post(agent_report_job),
        )
        .route("/v1/rollouts", post(create_rollout).get(list_rollouts))
        .route("/v1/rollouts/{rollout_id}", get(get_rollout))
        .route("/v1/rollouts/{rollout_id}/promote", post(promote_rollout))
        .route("/v1/rollouts/{rollout_id}/pause", post(pause_rollout))
        .route("/v1/rollouts/{rollout_id}/resume", post(resume_rollout))
        .route("/v1/rollouts/{rollout_id}/abort", post(abort_rollout))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_bearer_auth,
        ));
    Ok(Router::new()
        .route("/health", get(health))
        .route("/v1/apps/android/latest", get(android_app_latest))
        .merge(protected)
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .layer(middleware::from_fn(security_headers))
        .with_state(state))
}

async fn security_headers(req: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    response
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut terminate) = signal(SignalKind::terminate()) {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

pub async fn run(config: Config, bind: SocketAddr) -> Result<()> {
    let root = config.root.clone();
    let app = build_router(config)?;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    eprintln!(
        "doso_ota_release_listening bind={bind} root={}",
        root.display()
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn health() -> Response {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "service": "doso-ota-release",
            "version": env!("CARGO_PKG_VERSION"),
            "ready": true
        })),
    )
        .into_response()
}

fn validate_channel(channel: &str) -> Result<&str> {
    let c = channel.trim();
    ensure!(
        !c.is_empty()
            && c.len() <= 32
            && c.chars().all(|ch| ch.is_ascii_lowercase()
                || ch.is_ascii_digit()
                || matches!(ch, '-' | '_')),
        "channel_invalid"
    );
    Ok(c)
}

fn android_app_release_path(root: &FsPath, channel: &str) -> PathBuf {
    root.join("apps")
        .join("android")
        .join(channel)
        .join("latest.json")
}

fn load_android_app_release(
    root: &FsPath,
    channel: &str,
) -> Result<Option<AndroidAppReleaseTemplate>> {
    let path = android_app_release_path(root, channel);
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(&path)?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}

fn persist_android_app_release(root: &FsPath, release: &AndroidAppReleaseTemplate) -> Result<()> {
    atomic_write_json(&android_app_release_path(root, &release.channel), release)
}

async fn android_app_latest(
    State(state): State<AppState>,
    Query(query): Query<AndroidAppLatestQuery>,
) -> Response {
    let channel = match validate_channel(&query.channel) {
        Ok(c) => c.to_string(),
        Err(error) => return error_response(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    let release = match load_android_app_release(&state.config.root, &channel) {
        Ok(Some(rel)) => rel,
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "app_release_not_found"),
        Err(error) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    let download_url = match oss_signed_url(&state.config, &release.object_key, now_ms() / 1000) {
        Ok(url) => url,
        Err(error) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    let update_available = release.version_code > query.version_code;
    let force_update =
        update_available && (release.mandatory || query.version_code < release.min_supported_code);
    (
        StatusCode::OK,
        Json(AndroidAppLatestResponse {
            channel: release.channel,
            version_name: release.version_name,
            version_code: release.version_code,
            min_supported_code: release.min_supported_code,
            mandatory: release.mandatory,
            update_available,
            force_update,
            changelog: release.changelog,
            apk_sha256: release.apk_sha256,
            apk_size_bytes: release.apk_size_bytes,
            download_url,
            published_at_ms: release.published_at_ms,
        }),
    )
        .into_response()
}

async fn publish_android_app_release(
    State(state): State<AppState>,
    Json(req): Json<AndroidAppPublishRequest>,
) -> Response {
    let channel = match validate_channel(&req.channel) {
        Ok(c) => c.to_string(),
        Err(error) => return error_response(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    if req.version_name.trim().is_empty()
        || req.version_code == 0
        || req.apk_size_bytes == 0
        || req.object_key.trim().is_empty()
        || req.apk_sha256.len() != 64
        || !req.apk_sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return error_response(StatusCode::BAD_REQUEST, "app_release_invalid");
    }
    let template = AndroidAppReleaseTemplate {
        channel,
        version_name: req.version_name.trim().to_string(),
        version_code: req.version_code,
        min_supported_code: req.min_supported_code,
        mandatory: req.mandatory,
        changelog: req.changelog,
        apk_sha256: req.apk_sha256.to_ascii_lowercase(),
        apk_size_bytes: req.apk_size_bytes,
        object_key: req.object_key.trim_start_matches('/').to_string(),
        published_at_ms: now_ms(),
    };
    if let Err(error) = persist_android_app_release(&state.config.root, &template) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
    }
    (StatusCode::OK, Json(template)).into_response()
}

fn validate_device_id(device_id: &str) -> Result<&str> {
    let id = device_id.trim();
    ensure!(
        !id.is_empty()
            && id.len() <= 128
            && !id.starts_with('.')
            && !id.contains("..")
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
        "device_id_invalid"
    );
    Ok(id)
}

fn device_status_path(root: &FsPath, device_id: &str) -> PathBuf {
    root.join("status").join(format!("{device_id}.json"))
}

fn persist_device_status(root: &FsPath, record: &DeviceStatusRecord) -> Result<()> {
    atomic_write_json(&device_status_path(root, &record.device_id), record)
}

fn load_device_status(root: &FsPath, device_id: &str) -> Result<Option<DeviceStatusRecord>> {
    let path = device_status_path(root, device_id);
    if !path.is_file() {
        return Ok(None);
    }
    let mut rec: DeviceStatusRecord = serde_json::from_slice(&fs::read(&path)?)?;
    rec.last_seen_age_ms = Some(now_ms().saturating_sub(rec.reported_at_ms).max(0));
    Ok(Some(rec))
}

async fn agent_report_device_status(
    State(state): State<AppState>,
    Path(device_id): Path<String>,
    Json(report): Json<DeviceStatusReportRequest>,
) -> Response {
    let clean_id = match validate_device_id(&device_id) {
        Ok(id) => id.to_string(),
        Err(error) => return error_response(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    match lookup_enabled_device(&state.config.device_registry, &clean_id) {
        Ok(Some(_)) => {}
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "device_not_found"),
        Err(error) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
    let record = DeviceStatusRecord {
        device_id: clean_id,
        reported_at_ms: now_ms(),
        last_seen_age_ms: Some(0),
        boot_id: report.boot_id,
        firmware_version: report.firmware_version,
        release_id: report.release_id,
        brainstem_ready: report.brainstem_ready,
        strategy_ready: report.strategy_ready,
        can_read_only: report.can_read_only,
        active_profile: report.active_profile,
        slots: report.slots,
        policies: report.policies,
        rt_loop: report.rt_loop,
    };
    if let Err(error) = persist_device_status(&state.config.root, &record) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
    }
    (StatusCode::OK, Json(record)).into_response()
}

async fn read_device_status(
    State(state): State<AppState>,
    Path(device_id): Path<String>,
) -> Response {
    let clean_id = match validate_device_id(&device_id) {
        Ok(id) => id.to_string(),
        Err(error) => return error_response(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    match load_device_status(&state.config.root, &clean_id) {
        Ok(Some(rec)) => (StatusCode::OK, Json(rec)).into_response(),
        Ok(None) => error_response(StatusCode::NOT_FOUND, "device_status_not_found"),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn list_device_statuses(State(state): State<AppState>) -> Response {
    let directory = state.config.root.join("status");
    let mut items = Vec::new();
    if directory.exists() {
        let entries = match fs::read_dir(&directory) {
            Ok(e) => e,
            Err(error) => {
                return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
            }
        };
        let now = now_ms();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            if let Ok(bytes) = fs::read(&path)
                && let Ok(mut rec) = serde_json::from_slice::<DeviceStatusRecord>(&bytes)
            {
                rec.last_seen_age_ms = Some(now.saturating_sub(rec.reported_at_ms).max(0));
                items.push(rec);
            }
        }
    }
    items.sort_by(|a, b| a.device_id.cmp(&b.device_id));
    (StatusCode::OK, Json(items)).into_response()
}

async fn releases(State(state): State<AppState>, Path(device_id): Path<String>) -> Response {
    let clean_id = match validate_device_id(&device_id) {
        Ok(id) => id.to_string(),
        Err(error) => return error_response(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    let device = match lookup_enabled_device(&state.config.device_registry, &clean_id) {
        Ok(Some(device)) => device,
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "device_not_found"),
        Err(error) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    match load_releases(&state.config.root, &device) {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn start_job(
    State(state): State<AppState>,
    Path(device_id): Path<String>,
    Json(request): Json<StartRequest>,
) -> Response {
    let clean_id = match validate_device_id(&device_id) {
        Ok(id) => id.to_string(),
        Err(error) => return error_response(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    let device = match lookup_enabled_device(&state.config.device_registry, &clean_id) {
        Ok(Some(device)) => device,
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "device_not_found"),
        Err(error) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    if let Err(error) = ensure_request(&request) {
        return error_response(StatusCode::BAD_REQUEST, &error.to_string());
    }
    let release_id = request.release_id.trim().to_string();
    let idempotency_key = request.idempotency_key.trim().to_string();
    let mut jobs = state.jobs.lock().await;
    if let Some(job) = jobs
        .values()
        .find(|job| job.device_id == clean_id && job.idempotency_key == idempotency_key)
    {
        if job.release_id != release_id {
            return error_response(StatusCode::CONFLICT, "idempotency_key_conflict");
        }
        return (StatusCode::ACCEPTED, Json(job)).into_response();
    }
    if jobs
        .values()
        .any(|job| job.device_id == clean_id && is_active_phase(&job.phase))
    {
        return error_response(StatusCode::CONFLICT, "job_already_active");
    }
    let template = match load_release_template(&state.config.root, &release_id) {
        Ok(template) => template,
        Err(error) => return error_response(StatusCode::NOT_FOUND, &error.to_string()),
    };
    if template.manifest.model != device.model
        || template.manifest.hardware_revision != device.hardware_revision
    {
        return error_response(StatusCode::CONFLICT, "release_incompatible");
    }
    let now = now_ms();
    let envelope = match build_signed_envelope(&state.config, &state.signing_key, template, now) {
        Ok(envelope) => envelope,
        Err(error) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    let job = Job {
        job_id: Uuid::new_v4().to_string(),
        device_id: clean_id,
        release_id,
        idempotency_key,
        phase: "scheduled".into(),
        manifest: envelope,
        created_at_ms: now,
        updated_at_ms: now,
        bytes_downloaded: 0,
        current_version: None,
        message: None,
        rollout_id: None,
    };
    if let Err(error) = persist_job(&state.config.root, &job) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
    }
    jobs.insert(job.job_id.clone(), job.clone());
    (StatusCode::ACCEPTED, Json(job)).into_response()
}

async fn read_job(
    State(state): State<AppState>,
    Path((device_id, job_id)): Path<(String, String)>,
) -> Response {
    let clean_id = match validate_device_id(&device_id) {
        Ok(id) => id.to_string(),
        Err(error) => return error_response(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    let jobs = state.jobs.lock().await;
    match jobs.get(&job_id).filter(|job| job.device_id == clean_id) {
        Some(job) => (StatusCode::OK, Json(job)).into_response(),
        None => error_response(StatusCode::NOT_FOUND, "job_not_found"),
    }
}

async fn cancel_job(
    State(state): State<AppState>,
    Path((device_id, job_id)): Path<(String, String)>,
) -> Response {
    let clean_id = match validate_device_id(&device_id) {
        Ok(id) => id.to_string(),
        Err(error) => return error_response(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    let mut jobs = state.jobs.lock().await;
    let Some(job) = jobs
        .get_mut(&job_id)
        .filter(|job| job.device_id == clean_id)
    else {
        return error_response(StatusCode::NOT_FOUND, "job_not_found");
    };
    if job.phase == "cancelled" {
        return (StatusCode::OK, Json(job)).into_response();
    }
    if !matches!(
        job.phase.as_str(),
        "scheduled" | "downloading" | "waiting_safe_window"
    ) {
        return error_response(StatusCode::CONFLICT, "job_not_cancellable");
    }
    let now = now_ms();
    job.phase = "cancelled".into();
    job.updated_at_ms = now;
    if let Err(error) = persist_job(&state.config.root, job) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
    }
    let cancelled_job = job.clone();
    if let Some(ref rid) = cancelled_job.rollout_id {
        let mut rollouts = state.rollouts.lock().await;
        if let Some(rollout) = rollouts.get_mut(rid) {
            if let Some(dev_state) = rollout.device_states.get_mut(&clean_id)
                && dev_state.job_id == cancelled_job.job_id
            {
                dev_state.phase = "cancelled".into();
                dev_state.updated_at_ms = now;
            }
            rollout.updated_at_ms = now;
            if let Err(error) = persist_rollout(&state.config.root, rollout) {
                return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
            }
        }
    }
    (StatusCode::OK, Json(cancelled_job)).into_response()
}

async fn agent_next_job(State(state): State<AppState>, Path(device_id): Path<String>) -> Response {
    let clean_id = match validate_device_id(&device_id) {
        Ok(id) => id.to_string(),
        Err(error) => return error_response(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    let registry = match load_registry(&state.config.device_registry) {
        Ok(reg) => reg,
        Err(error) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    let Some(device) = registry.get(&clean_id).filter(|d| d.enabled).cloned() else {
        return error_response(StatusCode::NOT_FOUND, "device_not_found");
    };

    let mut jobs = state.jobs.lock().await;
    if let Some(next_id) = jobs
        .values()
        .filter(|job| {
            job.device_id == clean_id && matches!(job.phase.as_str(), "scheduled" | "downloading")
        })
        .min_by_key(|job| job.created_at_ms)
        .map(|job| job.job_id.clone())
    {
        let job = jobs.get_mut(&next_id).expect("selected job exists");
        let now = now_ms();
        let template = match load_release_template(&state.config.root, &job.release_id) {
            Ok(template) => template,
            Err(error) => {
                return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
            }
        };
        match build_signed_envelope(&state.config, &state.signing_key, template, now) {
            Ok(envelope) => {
                job.manifest = envelope;
                job.updated_at_ms = now;
                if let Err(error) = persist_job(&state.config.root, job) {
                    return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
                }
            }
            Err(error) => {
                return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
            }
        }
        return (StatusCode::OK, Json(job.clone())).into_response();
    }

    // Do not auto-assign a rollout job if the device already has another active job in flight.
    if jobs
        .values()
        .any(|job| job.device_id == clean_id && is_active_phase(&job.phase))
    {
        return StatusCode::NO_CONTENT.into_response();
    }

    // Evaluate active canary rollouts in deterministic creation order.
    let now = now_ms();
    let mut rollouts = state.rollouts.lock().await;
    let mut rollout_ids: Vec<(i64, String)> = rollouts
        .values()
        .map(|r| (r.created_at_ms, r.rollout_id.clone()))
        .collect();
    rollout_ids.sort();

    for (_, rid) in rollout_ids {
        let Some(rollout) = rollouts.get_mut(&rid) else {
            continue;
        };
        if maybe_auto_promote(rollout, &registry, now)
            && let Err(error) = persist_rollout(&state.config.root, rollout)
        {
            eprintln!("persist_rollout_auto_promote_failed rollout_id={rid} err={error}");
        }
        let Some(stage_idx) = eligible_stage_for_device(rollout, &clean_id, &device) else {
            continue;
        };
        let template = match load_release_template(&state.config.root, &rollout.release_id) {
            Ok(t) => t,
            Err(_) => continue,
        };
        if template.manifest.model != device.model
            || template.manifest.hardware_revision != device.hardware_revision
        {
            continue;
        }
        let envelope = match build_signed_envelope(&state.config, &state.signing_key, template, now)
        {
            Ok(env) => env,
            Err(error) => {
                return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
            }
        };
        let job = Job {
            job_id: Uuid::new_v4().to_string(),
            device_id: clean_id.clone(),
            release_id: rollout.release_id.clone(),
            idempotency_key: format!("rollout:{rid}:{clean_id}:{stage_idx}"),
            phase: "scheduled".into(),
            manifest: envelope,
            created_at_ms: now,
            updated_at_ms: now,
            bytes_downloaded: 0,
            current_version: None,
            message: None,
            rollout_id: Some(rid.clone()),
        };
        if let Err(error) = persist_job(&state.config.root, &job) {
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
        }
        rollout.device_states.insert(
            clean_id.clone(),
            RolloutDeviceState {
                device_id: clean_id.clone(),
                stage_index: stage_idx,
                job_id: job.job_id.clone(),
                phase: job.phase.clone(),
                assigned_at_ms: now,
                updated_at_ms: now,
                message: None,
            },
        );
        rollout.updated_at_ms = now;
        if let Err(error) = persist_rollout(&state.config.root, rollout) {
            rollout.device_states.remove(&clean_id);
            let _ = fs::remove_file(
                state
                    .config
                    .root
                    .join("jobs")
                    .join(format!("{}.json", job.job_id)),
            );
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
        }
        jobs.insert(job.job_id.clone(), job.clone());
        return (StatusCode::OK, Json(job)).into_response();
    }

    StatusCode::NO_CONTENT.into_response()
}

async fn agent_report_job(
    State(state): State<AppState>,
    Path((device_id, job_id)): Path<(String, String)>,
    Json(report): Json<AgentReportRequest>,
) -> Response {
    let clean_id = match validate_device_id(&device_id) {
        Ok(id) => id.to_string(),
        Err(error) => return error_response(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    let registry = match load_registry(&state.config.device_registry) {
        Ok(reg) => reg,
        Err(error) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    if !registry.get(&clean_id).is_some_and(|d| d.enabled) {
        return error_response(StatusCode::NOT_FOUND, "device_not_found");
    }
    if !is_valid_agent_phase(&report.phase) {
        return error_response(StatusCode::BAD_REQUEST, "invalid_phase");
    }
    let mut jobs = state.jobs.lock().await;
    let Some(job) = jobs
        .get_mut(&job_id)
        .filter(|job| job.device_id == clean_id)
    else {
        return error_response(StatusCode::NOT_FOUND, "job_not_found");
    };
    if is_terminal_phase(&job.phase) && job.phase != report.phase {
        return error_response(StatusCode::CONFLICT, "job_already_terminal");
    }
    let now = now_ms();
    job.phase = report.phase;
    if let Some(bytes) = report.bytes_downloaded {
        job.bytes_downloaded = bytes;
    }
    if let Some(version) = report.current_version {
        job.current_version = Some(version);
    }
    if let Some(message) = report.message {
        job.message = Some(message);
    }
    job.updated_at_ms = now;
    if let Err(error) = persist_job(&state.config.root, job) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
    }
    let updated_job = job.clone();

    if let Some(ref rid) = updated_job.rollout_id {
        let mut rollouts = state.rollouts.lock().await;
        if let Some(rollout) = rollouts.get_mut(rid) {
            if let Some(dev_state) = rollout.device_states.get_mut(&clean_id) {
                dev_state.phase = updated_job.phase.clone();
                dev_state.updated_at_ms = now;
                dev_state.message = updated_job.message.clone();
            }
            rollout.updated_at_ms = now;
            if !evaluate_circuit_breaker(rollout, &clean_id, &updated_job.phase, now) {
                maybe_auto_promote(rollout, &registry, now);
            }
            if let Err(error) = persist_rollout(&state.config.root, rollout) {
                return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
            }
        }
    }

    (StatusCode::OK, Json(updated_job)).into_response()
}

async fn create_rollout(
    State(state): State<AppState>,
    Json(req): Json<CreateRolloutRequest>,
) -> Response {
    if let Err(error) = validate_rollout_request(&req) {
        return error_response(StatusCode::BAD_REQUEST, &error.to_string());
    }
    let release_id = req.release_id.trim().to_string();
    let template = match load_release_template(&state.config.root, &release_id) {
        Ok(t) => t,
        Err(error) => return error_response(StatusCode::NOT_FOUND, &error.to_string()),
    };
    if template.manifest.artifact_type == ArtifactType::ReleaseBundle {
        if template.manifest.bundle_items.is_empty() {
            return error_response(StatusCode::CONFLICT, "bundle_items_empty");
        }
    } else if template.object_key.trim().is_empty() {
        return error_response(StatusCode::CONFLICT, "release_object_key_missing");
    }
    let target_model = req
        .target_model
        .clone()
        .unwrap_or_else(|| template.manifest.model.clone());
    if target_model != template.manifest.model {
        return error_response(StatusCode::CONFLICT, "rollout_model_mismatch");
    }
    let target_hardware_revision = req
        .target_hardware_revision
        .clone()
        .or_else(|| Some(template.manifest.hardware_revision.clone()));
    if let Some(ref hw) = target_hardware_revision
        && hw != &template.manifest.hardware_revision
    {
        return error_response(StatusCode::CONFLICT, "rollout_hardware_mismatch");
    }

    let rollout_id = match req.rollout_id {
        Some(id) => id.trim().to_owned(),
        None => format!("ro-{}", Uuid::new_v4()),
    };
    let now = now_ms();
    let mut rollouts = state.rollouts.lock().await;
    if rollouts.contains_key(&rollout_id) {
        return error_response(StatusCode::CONFLICT, "rollout_already_exists");
    }
    let campaign = RolloutCampaign {
        rollout_id: rollout_id.clone(),
        release_id,
        target_model,
        target_hardware_revision,
        target_tags: req.target_tags,
        stages: req.stages,
        circuit_breaker: req.circuit_breaker,
        status: RolloutStatus::Active,
        current_stage_index: 0,
        created_at_ms: now,
        updated_at_ms: now,
        stage_started_at_ms: now,
        circuit_open_reason: None,
        device_states: BTreeMap::new(),
    };
    if let Err(error) = persist_rollout(&state.config.root, &campaign) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
    }
    let view = build_rollout_view(&campaign);
    rollouts.insert(rollout_id, campaign);
    (StatusCode::CREATED, Json(view)).into_response()
}

async fn list_rollouts(State(state): State<AppState>) -> Response {
    let registry = match load_registry(&state.config.device_registry) {
        Ok(reg) => reg,
        Err(error) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    let now = now_ms();
    let mut rollouts = state.rollouts.lock().await;
    let mut views = Vec::with_capacity(rollouts.len());
    for campaign in rollouts.values_mut() {
        if maybe_auto_promote(campaign, &registry, now)
            && let Err(error) = persist_rollout(&state.config.root, campaign)
        {
            eprintln!(
                "persist_rollout_auto_promote_failed rollout_id={} err={error}",
                campaign.rollout_id
            );
        }
        views.push(build_rollout_view(campaign));
    }
    views.sort_by_key(|v| (v.campaign.created_at_ms, v.campaign.rollout_id.clone()));
    (StatusCode::OK, Json(views)).into_response()
}

async fn get_rollout(State(state): State<AppState>, Path(rollout_id): Path<String>) -> Response {
    let clean_id = match validate_rollout_id(&rollout_id) {
        Ok(id) => id.to_string(),
        Err(error) => return error_response(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    let registry = match load_registry(&state.config.device_registry) {
        Ok(reg) => reg,
        Err(error) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    let now = now_ms();
    let mut rollouts = state.rollouts.lock().await;
    let Some(campaign) = rollouts.get_mut(&clean_id) else {
        return error_response(StatusCode::NOT_FOUND, "rollout_not_found");
    };
    if maybe_auto_promote(campaign, &registry, now)
        && let Err(error) = persist_rollout(&state.config.root, campaign)
    {
        eprintln!("persist_rollout_auto_promote_failed rollout_id={clean_id} err={error}");
    }
    (StatusCode::OK, Json(build_rollout_view(campaign))).into_response()
}

async fn promote_rollout(
    State(state): State<AppState>,
    Path(rollout_id): Path<String>,
) -> Response {
    let clean_id = match validate_rollout_id(&rollout_id) {
        Ok(id) => id.to_string(),
        Err(error) => return error_response(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    let mut rollouts = state.rollouts.lock().await;
    let Some(campaign) = rollouts.get_mut(&clean_id) else {
        return error_response(StatusCode::NOT_FOUND, "rollout_not_found");
    };
    if campaign.status != RolloutStatus::Active {
        return error_response(StatusCode::CONFLICT, "rollout_not_active");
    }
    let now = now_ms();
    if campaign.current_stage_index + 1 < campaign.stages.len() {
        campaign.current_stage_index += 1;
        campaign.stage_started_at_ms = now;
    } else {
        campaign.status = RolloutStatus::Completed;
    }
    campaign.updated_at_ms = now;
    if let Err(error) = persist_rollout(&state.config.root, campaign) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
    }
    (StatusCode::OK, Json(build_rollout_view(campaign))).into_response()
}

async fn pause_rollout(State(state): State<AppState>, Path(rollout_id): Path<String>) -> Response {
    let clean_id = match validate_rollout_id(&rollout_id) {
        Ok(id) => id.to_string(),
        Err(error) => return error_response(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    let mut rollouts = state.rollouts.lock().await;
    let Some(campaign) = rollouts.get_mut(&clean_id) else {
        return error_response(StatusCode::NOT_FOUND, "rollout_not_found");
    };
    if campaign.status != RolloutStatus::Active {
        return error_response(StatusCode::CONFLICT, "rollout_not_active");
    }
    campaign.status = RolloutStatus::PausedManual;
    campaign.updated_at_ms = now_ms();
    if let Err(error) = persist_rollout(&state.config.root, campaign) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
    }
    (StatusCode::OK, Json(build_rollout_view(campaign))).into_response()
}

async fn resume_rollout(State(state): State<AppState>, Path(rollout_id): Path<String>) -> Response {
    let clean_id = match validate_rollout_id(&rollout_id) {
        Ok(id) => id.to_string(),
        Err(error) => return error_response(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    let mut rollouts = state.rollouts.lock().await;
    let Some(campaign) = rollouts.get_mut(&clean_id) else {
        return error_response(StatusCode::NOT_FOUND, "rollout_not_found");
    };
    if !matches!(
        campaign.status,
        RolloutStatus::PausedManual | RolloutStatus::PausedCircuitOpen
    ) {
        return error_response(StatusCode::CONFLICT, "rollout_not_paused");
    }
    let now = now_ms();
    campaign.status = RolloutStatus::Active;
    campaign.circuit_open_reason = None;
    campaign.stage_started_at_ms = now;
    campaign.updated_at_ms = now;
    if let Err(error) = persist_rollout(&state.config.root, campaign) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
    }
    (StatusCode::OK, Json(build_rollout_view(campaign))).into_response()
}

async fn abort_rollout(State(state): State<AppState>, Path(rollout_id): Path<String>) -> Response {
    let clean_id = match validate_rollout_id(&rollout_id) {
        Ok(id) => id.to_string(),
        Err(error) => return error_response(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    let mut rollouts = state.rollouts.lock().await;
    let Some(campaign) = rollouts.get_mut(&clean_id) else {
        return error_response(StatusCode::NOT_FOUND, "rollout_not_found");
    };
    if campaign.status.is_terminal() {
        return error_response(StatusCode::CONFLICT, "rollout_already_terminal");
    }
    campaign.status = RolloutStatus::Aborted;
    campaign.updated_at_ms = now_ms();
    if let Err(error) = persist_rollout(&state.config.root, campaign) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
    }
    (StatusCode::OK, Json(build_rollout_view(campaign))).into_response()
}

fn is_active_phase(phase: &str) -> bool {
    matches!(
        phase,
        "scheduled"
            | "downloading"
            | "verifying"
            | "waiting_safe_window"
            | "installing"
            | "rebooting"
            | "health_checking"
            | "rollback_pending"
            | "rolling_back"
    )
}

fn is_terminal_phase(phase: &str) -> bool {
    matches!(
        phase,
        "succeeded" | "rolled_back" | "failed" | "cancelled" | "recovery_required"
    )
}

fn is_valid_agent_phase(phase: &str) -> bool {
    matches!(
        phase,
        "downloading"
            | "verifying"
            | "waiting_safe_window"
            | "installing"
            | "rebooting"
            | "health_checking"
            | "succeeded"
            | "rollback_pending"
            | "rolling_back"
            | "rolled_back"
            | "recovery_required"
            | "failed"
            | "cancelled"
    )
}

fn load_registry(path: &FsPath) -> Result<HashMap<String, DeviceRecord>> {
    let bytes =
        fs::read(path).with_context(|| format!("read device registry {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parse device registry {}", path.display()))
}

fn lookup_enabled_device(path: &FsPath, device_id: &str) -> Result<Option<DeviceRecord>> {
    let registry = load_registry(path)?;
    Ok(registry.get(device_id).filter(|d| d.enabled).cloned())
}

fn validate_release_id(release_id: &str) -> Result<&str> {
    let id = release_id.trim();
    ensure!(
        !id.is_empty()
            && id.len() <= 128
            && !id.starts_with('.')
            && !id.contains("..")
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
        "release_id_invalid"
    );
    Ok(id)
}

fn load_release_template(root: &FsPath, release_id: &str) -> Result<ReleaseTemplate> {
    let clean_id = validate_release_id(release_id)?;
    let template_path = root
        .join("releases")
        .join(clean_id)
        .join("manifest.template.json");
    let bytes = fs::read(&template_path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn build_signed_envelope(
    config: &Config,
    signing_key: &SigningKey,
    mut template: ReleaseTemplate,
    now_ms: i64,
) -> Result<ManifestEnvelope> {
    let default_ttl_ms = (config.signed_url_ttl_seconds.max(60) as i64).saturating_mul(1000);
    let template_ttl_ms = template
        .manifest
        .expires_at_ms
        .saturating_sub(template.manifest.issued_at_ms);
    let ttl_ms = if template_ttl_ms > 0 {
        template_ttl_ms
    } else {
        default_ttl_ms
    };
    let now_secs = now_ms / 1000;
    template.manifest.issued_at_ms = now_ms;
    template.manifest.expires_at_ms = now_ms.saturating_add(ttl_ms);
    template.manifest.key_id = config.signing_key_id.clone();

    if !template.object_key.trim().is_empty() {
        template.manifest.artifact_url = oss_signed_url(config, &template.object_key, now_secs)?;
    } else if template.manifest.artifact_type != ArtifactType::ReleaseBundle {
        anyhow::bail!("release_object_key_missing");
    }

    if let (Some(delta), Some(delta_key)) =
        (&mut template.manifest.delta, &template.delta_object_key)
        && !delta_key.trim().is_empty()
    {
        delta.patch_url = oss_signed_url(config, delta_key, now_secs)?;
    }

    for item in &mut template.manifest.bundle_items {
        if let Some(key) = template.bundle_object_keys.get(&item.component)
            && !key.trim().is_empty()
        {
            item.artifact_url = oss_signed_url(config, key, now_secs)?;
        }
        if let (Some(delta), Some(dkey)) = (
            &mut item.delta,
            template
                .bundle_object_keys
                .get(&format!("{}:delta", item.component)),
        ) && !dkey.trim().is_empty()
        {
            delta.patch_url = oss_signed_url(config, dkey, now_secs)?;
        }
    }

    sign_manifest(&template.manifest, signing_key)
}

fn ensure_request(request: &StartRequest) -> Result<()> {
    validate_release_id(&request.release_id)?;
    ensure!(
        !request.idempotency_key.trim().is_empty() && request.idempotency_key.len() <= 128,
        "idempotency_key_invalid"
    );
    Ok(())
}

async fn require_bearer_auth(
    State(state): State<AppState>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    let actual = req
        .headers()
        .get(header::AUTHORIZATION)
        .map(|value| value.as_bytes())
        .unwrap_or_default();
    let supplied: [u8; 32] = Sha256::digest(actual).into();
    if bool::from(state.expected_token_hash.ct_eq(&supplied)) {
        next.run(req).await
    } else {
        let mut response = error_response(StatusCode::UNAUTHORIZED, "unauthorized");
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            header::HeaderValue::from_static("Bearer"),
        );
        response
    }
}

fn load_releases(root: &FsPath, device: &DeviceRecord) -> Result<Vec<ReleaseSummary>> {
    let mut output = Vec::new();
    let releases_dir = root.join("releases");
    if !releases_dir.exists() {
        return Ok(output);
    }
    for entry in fs::read_dir(releases_dir)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path().join("manifest.template.json");
        if !path.is_file() {
            continue;
        }
        let template: ReleaseTemplate = serde_json::from_slice(&fs::read(path)?)?;
        if template.manifest.model == device.model
            && template.manifest.hardware_revision == device.hardware_revision
        {
            output.push(ReleaseSummary {
                release_id: template.manifest.release_id,
                version: template.manifest.version,
                sequence: template.manifest.sequence,
                channel: template.manifest.channel,
                model: template.manifest.model,
                hardware_revision: template.manifest.hardware_revision,
                artifact_size_bytes: template.manifest.artifact_size_bytes,
                artifact_sha256: template.manifest.artifact_sha256,
                artifact_type: template.manifest.artifact_type,
                policy_slot: template.manifest.policy_slot,
                component: template.manifest.component,
            });
        }
    }
    output.sort_by_key(|release| release.sequence);
    Ok(output)
}

fn sign_manifest(manifest: &Manifest, key: &SigningKey) -> Result<ManifestEnvelope> {
    let payload = serde_json::to_vec(manifest)?;
    Ok(ManifestEnvelope {
        payload: URL_SAFE_NO_PAD.encode(&payload),
        signature: URL_SAFE_NO_PAD.encode(key.sign(&payload).to_bytes()),
    })
}

/// Signs an Aliyun OSS GET URL using Unix timestamp in **seconds**.
pub fn oss_signed_url(config: &Config, object_key: &str, now_secs: i64) -> Result<String> {
    let bucket = config.oss_bucket.trim();
    let endpoint = config
        .oss_endpoint
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/');
    let key = object_key.trim().trim_start_matches('/');
    ensure!(!bucket.is_empty(), "oss_bucket_empty");
    ensure!(!endpoint.is_empty(), "oss_endpoint_empty");
    ensure!(
        !key.is_empty() && !key.contains("..") && !key.contains('?') && !key.contains('#'),
        "oss_object_key_invalid"
    );
    let expires = now_secs.saturating_add(config.signed_url_ttl_seconds as i64);
    let resource = format!("/{bucket}/{key}");
    let string_to_sign = format!("GET\n\n\n{expires}\n{resource}");
    let mut mac = HmacSha1::new_from_slice(config.oss_access_key_secret.as_bytes())
        .map_err(|_| anyhow::anyhow!("oss_secret_invalid"))?;
    mac.update(string_to_sign.as_bytes());
    let signature = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
    let query = form_urlencoded::Serializer::new(String::new())
        .append_pair("OSSAccessKeyId", &config.oss_access_key_id)
        .append_pair("Expires", &expires.to_string())
        .append_pair("Signature", &signature)
        .finish();
    Ok(format!("https://{bucket}.{endpoint}/{key}?{query}"))
}

fn load_jobs(root: &FsPath) -> Result<HashMap<String, Job>> {
    let mut jobs = HashMap::new();
    let directory = root.join("jobs");
    if !directory.exists() {
        return Ok(jobs);
    }
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let job: Job = serde_json::from_slice(&fs::read(path)?)?;
        jobs.insert(job.job_id.clone(), job);
    }
    Ok(jobs)
}

fn persist_job(root: &FsPath, job: &Job) -> Result<()> {
    atomic_write_json(&root.join("jobs").join(format!("{}.json", job.job_id)), job)
}

pub(crate) fn atomic_write_json<T: Serialize>(path: &FsPath, value: &T) -> Result<()> {
    let directory = path.parent().context("atomic_write_missing_parent")?;
    fs::create_dir_all(directory)?;
    let stem = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state");
    let temp = directory.join(format!("{stem}.{}.tmp", Uuid::new_v4()));
    fs::write(&temp, serde_json::to_vec_pretty(value)?)?;
    let file = fs::OpenOptions::new().read(true).write(true).open(&temp)?;
    file.sync_all()?;
    drop(file);
    if let Err(error) = fs::rename(&temp, path) {
        if error.kind() != std::io::ErrorKind::PermissionDenied || !path.exists() {
            let _ = fs::remove_file(&temp);
            return Err(error.into());
        }
        // Windows std::fs::rename cannot replace an existing file.
        fs::remove_file(path)?;
        fs::rename(&temp, path)?;
    }
    Ok(())
}

fn error_response(status: StatusCode, code: &str) -> Response {
    (status, Json(serde_json::json!({"error":{"code":code}}))).into_response()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use ed25519_dalek::pkcs8::EncodePrivateKey;
    use tower::ServiceExt;

    fn write_test_env(
        root: &FsPath,
        devices_json: serde_json::Value,
        template_manifest: serde_json::Value,
        object_key: &str,
        release_id: &str,
    ) -> (Config, SigningKey) {
        let key = SigningKey::from_bytes(&[11; 32]);
        let key_path = root.join("signing.key.pem");
        fs::write(
            &key_path,
            key.to_pkcs8_pem(ed25519_dalek::pkcs8::spki::der::pem::LineEnding::LF)
                .unwrap()
                .as_str(),
        )
        .unwrap();
        let registry_path = root.join("devices.json");
        fs::write(
            &registry_path,
            serde_json::to_vec_pretty(&devices_json).unwrap(),
        )
        .unwrap();

        let rel_dir = root.join("releases").join(release_id);
        fs::create_dir_all(&rel_dir).unwrap();
        fs::write(
            rel_dir.join("manifest.template.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "manifest": template_manifest,
                "objectKey": object_key
            }))
            .unwrap(),
        )
        .unwrap();

        let config = Config {
            root: root.to_path_buf(),
            device_registry: registry_path,
            verify_api_token: "test-bearer-token-1234567890".into(),
            signing_key_path: key_path,
            signing_key_id: "release-key-2026-01".into(),
            oss_bucket: "doso-ota-prod-shanghai".into(),
            oss_endpoint: "oss-cn-shanghai.aliyuncs.com".into(),
            oss_access_key_id: "ak-test".into(),
            oss_access_key_secret: "sk-secret-value".into(),
            signed_url_ttl_seconds: 1800,
        };
        (config, key)
    }

    fn base_template_manifest(
        release_id: &str,
        issued_ms: i64,
        expires_ms: i64,
    ) -> serde_json::Value {
        serde_json::json!({
            "manifestVersion": 1,
            "releaseId": release_id,
            "channel": "qa",
            "sequence": 7,
            "version": "1.0.1-rust",
            "issuedAtMs": issued_ms,
            "expiresAtMs": expires_ms,
            "model": "THUNDER 01",
            "hardwareRevision": "rdk-s100p",
            "minimumBootloaderVersion": "1.0.0",
            "minimumBrainstemVersion": "1.0.0",
            "minimumBatteryPercent": 20,
            "artifactUrl": "https://placeholder.invalid/old",
            "artifactSizeBytes": 128,
            "artifactSha256": Manifest::artifact_hash(b"dummy-bundle"),
            "keyId": "old-key",
            "rollbackPolicy": { "mode": "automatic", "healthTimeoutMs": 15000 }
        })
    }

    #[test]
    fn signs_oss_url_without_exposing_secret() {
        let config = Config {
            root: PathBuf::from("."),
            device_registry: PathBuf::from("devices.json"),
            verify_api_token: "x".repeat(32),
            signing_key_path: PathBuf::from("key.pem"),
            signing_key_id: "k".into(),
            oss_bucket: "doso-ota".into(),
            oss_endpoint: "oss-cn-shanghai.aliyuncs.com".into(),
            oss_access_key_id: "ak".into(),
            oss_access_key_secret: "secret".into(),
            signed_url_ttl_seconds: 600,
        };
        let url = oss_signed_url(&config, "firmware/qa/a.bundle", 1_700_000_000).unwrap();
        assert!(
            url.starts_with("https://doso-ota.oss-cn-shanghai.aliyuncs.com/firmware/qa/a.bundle?")
        );
        assert!(url.contains("Expires=1700000600"));
        assert!(!url.contains("secret"));
    }

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("doso-release-test-{}", Uuid::new_v4()));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self) -> &FsPath {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn start_job_uses_unix_seconds_for_oss_and_refreshes_stale_manifest_ttl() {
        let dir = TestDir::new();
        // Template authored in the distant past with a 10-minute TTL (600_000 ms).
        let stale_issued = 1_700_000_000_000i64;
        let stale_expires = stale_issued + 600_000;
        let (config, key) = write_test_env(
            dir.path(),
            serde_json::json!({
                "dev-1": { "model": "THUNDER 01", "hardwareRevision": "rdk-s100p", "enabled": true }
            }),
            base_template_manifest("rel-1", stale_issued, stale_expires),
            "firmware/qa/rel-1/brainstem.tar.gz",
            "rel-1",
        );
        let token = config.verify_api_token.clone();
        let app = build_router(config).unwrap();

        let before_ms = now_ms();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/devices/dev-1/firmware/jobs")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "releaseId": "rel-1",
                            "idempotencyKey": "idem-oss-ttl"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let after_ms = now_ms();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let job: Job = serde_json::from_slice(&body).unwrap();
        let manifest = job.manifest.decode(&key.verifying_key()).unwrap();

        // Bug 2: manifest issuedAtMs/expiresAtMs refreshed dynamically preserving 600_000ms TTL.
        assert!(manifest.issued_at_ms >= before_ms && manifest.issued_at_ms <= after_ms);
        assert_eq!(manifest.expires_at_ms - manifest.issued_at_ms, 600_000);
        assert_eq!(manifest.key_id, "release-key-2026-01");

        // Bug 1: OSS URL Expires must be ~10-digit Unix seconds (now_sec + 1800), never milliseconds!
        let parsed = url::Url::parse(&manifest.artifact_url).unwrap();
        let expires_str = parsed
            .query_pairs()
            .find(|(k, _)| k == "Expires")
            .map(|(_, v)| v.into_owned())
            .expect("Expires query parameter present");
        let expires_val: i64 = expires_str.parse().unwrap();
        let expected_min = before_ms / 1000 + 1800;
        let expected_max = after_ms / 1000 + 1800;
        assert!(
            expires_val >= expected_min && expires_val <= expected_max,
            "Expires={expires_val} expected in [{expected_min}, {expected_max}] (seconds, not ms)"
        );
        assert!(expires_val < 10_000_000_000);
    }

    #[tokio::test]
    async fn devices_json_hot_reloads_and_agent_next_job_report_lifecycle_works() {
        let dir = TestDir::new();
        let mut policy_manifest = base_template_manifest("pol-1", 0, 0);
        policy_manifest["artifactType"] = serde_json::json!("policy_package");
        policy_manifest["policySlot"] = serde_json::json!("flat");
        policy_manifest["shadowFrames"] = serde_json::json!(50);
        policy_manifest["activate"] = serde_json::json!(true);

        // Start with empty registry — Bug 3 checks hot reload without restarting router!
        let (config, key) = write_test_env(
            dir.path(),
            serde_json::json!({}),
            policy_manifest,
            "firmware/qa/pol-1/policy.tar.gz",
            "pol-1",
        );
        let registry_path = config.device_registry.clone();
        let token = config.verify_api_token.clone();
        let root = config.root.clone();
        let app = build_router(config).unwrap();

        // Unknown device returns 404 before hot-reload.
        let r0 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/devices/dev-hot/firmware/releases")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r0.status(), StatusCode::NOT_FOUND);

        // Dynamically add device to devices.json on disk (no restart!).
        fs::write(
            &registry_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "dev-hot": { "model": "THUNDER 01", "hardwareRevision": "rdk-s100p", "enabled": true }
            }))
            .unwrap(),
        )
        .unwrap();

        // Releases endpoint sees the new device & returns dual-channel policy metadata.
        let r1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/devices/dev-hot/firmware/releases")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r1.status(), StatusCode::OK);
        let rels: Vec<ReleaseSummary> = serde_json::from_slice(
            &axum::body::to_bytes(r1.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].artifact_type, ArtifactType::PolicyPackage);
        assert_eq!(rels[0].policy_slot.as_deref(), Some("flat"));

        // No queued job yet -> agent next-job returns 204 No Content.
        let r_empty = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/agent/devices/dev-hot/next-job")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_empty.status(), StatusCode::NO_CONTENT);

        // Create job via operator endpoint.
        let r_start = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/devices/dev-hot/firmware/jobs")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "releaseId": "pol-1",
                            "idempotencyKey": "idem-pol-1"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_start.status(), StatusCode::ACCEPTED);
        let created: Job = serde_json::from_slice(
            &axum::body::to_bytes(r_start.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();

        // Agent polls next-job -> gets freshly signed manifest with valid OSS seconds expiry.
        let r_next = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/agent/devices/dev-hot/next-job")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_next.status(), StatusCode::OK);
        let next_job: Job = serde_json::from_slice(
            &axum::body::to_bytes(r_next.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(next_job.job_id, created.job_id);
        let decoded = next_job.manifest.decode(&key.verifying_key()).unwrap();
        assert_eq!(decoded.artifact_type, ArtifactType::PolicyPackage);
        assert_eq!(decoded.policy_slot.as_deref(), Some("flat"));
        assert!(decoded.expires_at_ms > decoded.issued_at_ms);

        // Agent reports downloading, then succeeded.
        let r_dl = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/agent/devices/dev-hot/jobs/{}/report",
                        created.job_id
                    ))
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "phase": "downloading",
                            "bytes_downloaded": 64,
                            "current_version": "1.0.0-rust"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_dl.status(), StatusCode::OK);

        let r_ok = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/agent/devices/dev-hot/jobs/{}/report",
                        created.job_id
                    ))
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "phase": "succeeded",
                            "bytesDownloaded": 128,
                            "currentVersion": "1.0.1-rust",
                            "message": "policy activated"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_ok.status(), StatusCode::OK);
        let final_job: Job = serde_json::from_slice(
            &axum::body::to_bytes(r_ok.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(final_job.phase, "succeeded");
        assert_eq!(final_job.bytes_downloaded, 128);
        assert_eq!(final_job.current_version.as_deref(), Some("1.0.1-rust"));
        assert_eq!(final_job.message.as_deref(), Some("policy activated"));

        // Persisted job file on disk reflects final state.
        let disk_job: Job = serde_json::from_slice(
            &fs::read(root.join("jobs").join(format!("{}.json", created.job_id))).unwrap(),
        )
        .unwrap();
        assert_eq!(disk_job.phase, "succeeded");
        assert_eq!(disk_job.bytes_downloaded, 128);

        // Terminal job cannot be revived to another phase (409).
        let r_revive = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/agent/devices/dev-hot/jobs/{}/report",
                        created.job_id
                    ))
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"phase":"failed"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_revive.status(), StatusCode::CONFLICT);

        // After completion, next-job returns 204 No Content.
        let r_done = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/agent/devices/dev-hot/next-job")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_done.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn android_apk_ota_publish_and_latest_check_works() {
        let dir = TestDir::new();
        let (config, _key) = write_test_env(
            dir.path(),
            serde_json::json!({}),
            base_template_manifest("rel-1", 0, 0),
            "firmware/qa/rel-1/brainstem.tar.gz",
            "rel-1",
        );
        let token = config.verify_api_token.clone();
        let app = build_router(config).unwrap();

        // No APK published yet -> 404.
        let r_missing = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/apps/android/latest?channel=prod&version_code=100")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_missing.status(), StatusCode::NOT_FOUND);

        let apk_sha = Manifest::artifact_hash(b"fake-android-apk-bytes");
        let publish_payload = serde_json::to_vec(&serde_json::json!({
            "channel": "prod",
            "versionName": "1.2.0",
            "versionCode": 120,
            "minSupportedCode": 110,
            "mandatory": false,
            "changelog": "新增机器狗运动策略 OTA 管理",
            "apkSha256": apk_sha,
            "apkSizeBytes": 42000000,
            "objectKey": "apps/android/prod/doso-1.2.0.apk"
        }))
        .unwrap();

        // Publish without Bearer -> 401.
        let r_unauth = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/apps/android/releases")
                    .header("content-type", "application/json")
                    .body(Body::from(publish_payload.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_unauth.status(), StatusCode::UNAUTHORIZED);

        // Publish with Bearer -> 200.
        let r_pub = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/apps/android/releases")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(publish_payload))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_pub.status(), StatusCode::OK);

        // Client on version_code=100 (< minSupportedCode 110) -> updateAvailable=true, forceUpdate=true, signed OSS URL.
        let r_old = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/apps/android/latest?channel=prod&version_code=100")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_old.status(), StatusCode::OK);
        let old_resp: AndroidAppLatestResponse = serde_json::from_slice(
            &axum::body::to_bytes(r_old.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(old_resp.update_available);
        assert!(old_resp.force_update);
        assert_eq!(old_resp.version_name, "1.2.0");
        assert!(old_resp
            .download_url
            .starts_with("https://doso-ota-prod-shanghai.oss-cn-shanghai.aliyuncs.com/apps/android/prod/doso-1.2.0.apk?"));
        assert!(!old_resp.download_url.contains("sk-secret-value"));

        // Client already on version_code=120 -> updateAvailable=false, forceUpdate=false.
        let r_cur = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/apps/android/latest?channel=prod&version_code=120")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_cur.status(), StatusCode::OK);
        let cur_resp: AndroidAppLatestResponse = serde_json::from_slice(
            &axum::body::to_bytes(r_cur.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(!cur_resp.update_available);
        assert!(!cur_resp.force_update);
    }

    #[tokio::test]
    async fn device_status_and_policy_inventory_roundtrip_works() {
        let dir = TestDir::new();
        let (config, _key) = write_test_env(
            dir.path(),
            serde_json::json!({
                "SUNRISE-S100P-RUST-013": { "model": "THUNDER 01", "hardwareRevision": "rdk-s100p", "enabled": true }
            }),
            base_template_manifest("rel-1", 0, 0),
            "firmware/qa/rel-1/brainstem.tar.gz",
            "rel-1",
        );
        let token = config.verify_api_token.clone();
        let app = build_router(config).unwrap();

        let report_body = serde_json::to_vec(&serde_json::json!({
            "bootId": "boot-001",
            "firmwareVersion": "v0.0.1",
            "releaseId": "v0.0.1",
            "brainstemReady": true,
            "strategyReady": true,
            "canReadOnly": true,
            "activeProfile": "flat_w5",
            "slots": { "flat": "flat_w5", "stairs": "stairs_cts_v39" },
            "policies": [
                { "name": "flat_w5", "sha256": "abc123", "warmupUs": 45, "goldenSteps": 1, "goldenMaxErr": 0.0 }
            ],
            "rtLoop": { "maxTickUs": 50, "maxJitterUs": 75, "maxMutexWaitUs": 0, "ticks": 1000 }
        }))
        .unwrap();

        let r_post = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/agent/devices/SUNRISE-S100P-RUST-013/status")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(report_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_post.status(), StatusCode::OK);

        let r_get = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/devices/SUNRISE-S100P-RUST-013/status")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_get.status(), StatusCode::OK);
        let rec: DeviceStatusRecord = serde_json::from_slice(
            &axum::body::to_bytes(r_get.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(rec.firmware_version.as_deref(), Some("v0.0.1"));
        assert_eq!(rec.active_profile.as_deref(), Some("flat_w5"));
        assert_eq!(
            rec.slots.get("stairs").map(String::as_str),
            Some("stairs_cts_v39")
        );
        assert_eq!(rec.policies.len(), 1);
        assert!(rec.last_seen_age_ms.is_some());

        let r_list = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/devices/status")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_list.status(), StatusCode::OK);
        let list: Vec<DeviceStatusRecord> = serde_json::from_slice(
            &axum::body::to_bytes(r_list.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].device_id, "SUNRISE-S100P-RUST-013");
    }

    #[test]
    fn device_bucket_is_deterministic_and_within_1_to_100() {
        let b1 = device_bucket("ro-2026-001", "dog-013");
        let b2 = device_bucket("ro-2026-001", "dog-013");
        assert_eq!(b1, b2);
        assert!((1..=100).contains(&b1));

        let mut in_half = 0usize;
        for i in 0..200 {
            let b = device_bucket("ro-dist-check", &format!("dev-{i:04}"));
            assert!((1..=100).contains(&b));
            if b <= 50 {
                in_half += 1;
            }
        }
        // Reasonable spread around 50% across 200 devices.
        assert!((70..=130).contains(&in_half), "in_half={in_half}");
    }

    #[tokio::test]
    async fn canary_rollout_auto_assign_promote_and_circuit_breaker_on_rollback() {
        let dir = TestDir::new();
        // Mix of legacy record (omitting ring/tags -> defaults to prod/[]) and ringed/tagged devices.
        let devices = serde_json::json!({
            "canary-1": { "model": "THUNDER 01", "hardwareRevision": "rdk-s100p", "enabled": true, "ring": "canary", "tags": ["dog3", "rdk-s100p"] },
            "canary-2": { "model": "THUNDER 01", "hardwareRevision": "rdk-s100p", "enabled": true, "ring": "canary", "tags": ["dog3"] },
            "prod-1": { "model": "THUNDER 01", "hardwareRevision": "rdk-s100p", "enabled": true, "ring": "prod", "tags": ["dog3"] },
            "prod-2": { "model": "THUNDER 01", "hardwareRevision": "rdk-s100p", "enabled": true, "ring": "prod", "tags": ["dog3"] },
            "prod-legacy": { "model": "THUNDER 01", "hardwareRevision": "rdk-s100p", "enabled": true }
        });
        let (config, _key) = write_test_env(
            dir.path(),
            devices,
            base_template_manifest("rel-canary-1", 0, 0),
            "firmware/qa/rel-canary-1/brainstem.tar.gz",
            "rel-canary-1",
        );
        let token = config.verify_api_token.clone();
        let app = build_router(config.clone()).unwrap();

        let create_body = serde_json::to_vec(&serde_json::json!({
            "rolloutId": "ro-test-001",
            "releaseId": "rel-canary-1",
            "targetModel": "THUNDER 01",
            "targetHardwareRevision": "rdk-s100p",
            "targetTags": ["dog3"],
            "stages": [
                { "name": "canary", "ring": "canary", "percent": 100, "maxDevices": 1, "bakeSeconds": 0, "autoPromote": false },
                { "name": "prod-100", "ring": "prod", "percent": 100, "bakeSeconds": 0, "autoPromote": false }
            ],
            "circuitBreaker": { "maxFailureCount": 1, "maxFailureRate": 0.05 }
        }))
        .unwrap();

        // Unauthorized create -> 401.
        let r_unauth = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/rollouts")
                    .header("content-type", "application/json")
                    .body(Body::from(create_body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_unauth.status(), StatusCode::UNAUTHORIZED);

        // Create rollout -> 201 Created, Stage 0 active.
        let r_create = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/rollouts")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(create_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_create.status(), StatusCode::CREATED);
        let created: RolloutView = serde_json::from_slice(
            &axum::body::to_bytes(r_create.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(created.campaign.rollout_id, "ro-test-001");
        assert_eq!(created.campaign.status, RolloutStatus::Active);
        assert_eq!(created.campaign.current_stage_index, 0);

        // Prod device polling during canary Stage 0 gets 204 No Content.
        let r_prod_early = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/devices/prod-1/firmware/next-job")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_prod_early.status(), StatusCode::NO_CONTENT);

        // Canary-1 polls -> auto-scheduled job!
        let r_c1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/devices/canary-1/firmware/next-job")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_c1.status(), StatusCode::OK);
        let c1_job: Job = serde_json::from_slice(
            &axum::body::to_bytes(r_c1.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(c1_job.rollout_id.as_deref(), Some("ro-test-001"));

        // Canary-2 blocked by Stage 0 maxDevices=1 -> 204 No Content.
        let r_c2 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/devices/canary-2/firmware/next-job")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_c2.status(), StatusCode::NO_CONTENT);

        // Canary-1 reports waiting_safe_window, then succeeded.
        let r_wait = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/devices/canary-1/firmware/jobs/{}/report",
                        c1_job.job_id
                    ))
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"phase":"waiting_safe_window","message":"cms_state_not_grounded: walking"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_wait.status(), StatusCode::OK);

        let r_view_wait = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/rollouts/ro-test-001")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let view_wait: RolloutView = serde_json::from_slice(
            &axum::body::to_bytes(r_view_wait.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(view_wait.stage_counts[0].waiting_safe_window, 1);

        let r_c1_ok = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/devices/canary-1/firmware/jobs/{}/report",
                        c1_job.job_id
                    ))
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"phase":"succeeded"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_c1_ok.status(), StatusCode::OK);

        // Manually promote to Stage 1 (prod-100).
        let r_prom = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/rollouts/ro-test-001/promote")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_prom.status(), StatusCode::OK);
        let promoted: RolloutView = serde_json::from_slice(
            &axum::body::to_bytes(r_prom.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(promoted.campaign.current_stage_index, 1);

        // prod-legacy lacks tag "dog3" -> 204 No Content.
        let r_legacy = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/devices/prod-legacy/firmware/next-job")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_legacy.status(), StatusCode::NO_CONTENT);

        // prod-1 polls -> auto-assigned job!
        let r_p1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/devices/prod-1/firmware/next-job")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_p1.status(), StatusCode::OK);
        let p1_job: Job = serde_json::from_slice(
            &axum::body::to_bytes(r_p1.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();

        // prod-1 reports rolled_back -> Circuit Breaker trips immediately to PausedCircuitOpen!
        let r_rb = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/devices/prod-1/firmware/jobs/{}/report",
                        p1_job.job_id
                    ))
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"phase":"rolled_back","message":"health_timeout"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_rb.status(), StatusCode::OK);

        let r_tripped = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/rollouts/ro-test-001")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let tripped: RolloutView = serde_json::from_slice(
            &axum::body::to_bytes(r_tripped.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(tripped.campaign.status, RolloutStatus::PausedCircuitOpen);
        assert!(tripped.circuit_breaker_state.open);
        assert_eq!(tripped.circuit_breaker_state.failure_count, 1);

        // prod-2 polling AFTER circuit opens MUST receive 204 No Content!
        let r_p2_blocked = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/devices/prod-2/firmware/next-job")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_p2_blocked.status(), StatusCode::NO_CONTENT);

        // Resume -> Pause -> Abort lifecycle works & persists across router restart.
        let r_resume = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/rollouts/ro-test-001/resume")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_resume.status(), StatusCode::OK);

        let r_pause = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/rollouts/ro-test-001/pause")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_pause.status(), StatusCode::OK);

        let r_abort = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/rollouts/ro-test-001/abort")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_abort.status(), StatusCode::OK);

        // Reload fresh router from disk and verify list returns persisted aborted rollout.
        let reloaded = build_router(config).unwrap();
        let r_list = reloaded
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/rollouts")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_list.status(), StatusCode::OK);
        let views: Vec<RolloutView> = serde_json::from_slice(
            &axum::body::to_bytes(r_list.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].campaign.status, RolloutStatus::Aborted);
    }

    #[tokio::test]
    async fn rollout_auto_promotes_and_completes_when_bake_elapsed_with_zero_failures() {
        let dir = TestDir::new();
        let devices = serde_json::json!({
            "canary-a": { "model": "THUNDER 01", "hardwareRevision": "rdk-s100p", "enabled": true, "ring": "canary", "tags": [] },
            "prod-a": { "model": "THUNDER 01", "hardwareRevision": "rdk-s100p", "enabled": true, "ring": "prod", "tags": [] }
        });
        let (config, _key) = write_test_env(
            dir.path(),
            devices,
            base_template_manifest("rel-auto-1", 0, 0),
            "firmware/qa/rel-auto-1/brainstem.tar.gz",
            "rel-auto-1",
        );
        let token = config.verify_api_token.clone();
        let app = build_router(config).unwrap();

        let create_body = serde_json::to_vec(&serde_json::json!({
            "rolloutId": "ro-auto-001",
            "releaseId": "rel-auto-1",
            "stages": [
                { "name": "canary", "ring": "canary", "percent": 100, "bakeSeconds": 0, "autoPromote": true },
                { "name": "prod-all", "ring": "prod", "percent": 100, "bakeSeconds": 0, "autoPromote": true }
            ]
        }))
        .unwrap();
        let r_create = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/rollouts")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(create_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_create.status(), StatusCode::CREATED);

        // Canary-a polls and succeeds -> auto-promotes to Stage 1!
        let r_c = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/agent/devices/canary-a/next-job")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_c.status(), StatusCode::OK);
        let job_c: Job = serde_json::from_slice(
            &axum::body::to_bytes(r_c.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        let r_c_ok = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/agent/devices/canary-a/jobs/{}/report",
                        job_c.job_id
                    ))
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"phase":"succeeded"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_c_ok.status(), StatusCode::OK);

        let r_mid = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/rollouts/ro-auto-001")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let mid: RolloutView = serde_json::from_slice(
            &axum::body::to_bytes(r_mid.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(mid.campaign.current_stage_index, 1);
        assert_eq!(mid.campaign.status, RolloutStatus::Active);

        // Prod-a polls and succeeds -> auto-completes rollout!
        let r_p = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/agent/devices/prod-a/next-job")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_p.status(), StatusCode::OK);
        let job_p: Job = serde_json::from_slice(
            &axum::body::to_bytes(r_p.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        let r_p_ok = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/agent/devices/prod-a/jobs/{}/report",
                        job_p.job_id
                    ))
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"phase":"succeeded"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_p_ok.status(), StatusCode::OK);

        let r_end = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/rollouts/ro-auto-001")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let end: RolloutView = serde_json::from_slice(
            &axum::body::to_bytes(r_end.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(end.campaign.status, RolloutStatus::Completed);
    }

    #[tokio::test]
    async fn release_bundle_signs_bundle_items_without_bucket_root_url() {
        let dir = TestDir::new();
        let bundle_manifest = serde_json::json!({
            "manifestVersion": 1,
            "releaseId": "rel-bundle-1",
            "channel": "qa",
            "sequence": 10,
            "version": "2.0.0",
            "issuedAtMs": 0,
            "expiresAtMs": 0,
            "model": "THUNDER 01",
            "hardwareRevision": "rdk-s100p",
            "minimumBootloaderVersion": "0.0.1",
            "minimumBrainstemVersion": "0.0.1",
            "minimumBatteryPercent": 20,
            "artifactUrl": "",
            "artifactSizeBytes": 0,
            "artifactSha256": "",
            "keyId": "old-key",
            "rollbackPolicy": { "mode": "automatic", "healthTimeoutMs": 15000 },
            "artifactType": "release_bundle",
            "bundleItems": [
                {
                    "component": "brainstem",
                    "version": "1.2.0",
                    "artifactUrl": "https://placeholder.invalid/brainstem",
                    "artifactSizeBytes": 10,
                    "artifactSha256": Manifest::artifact_hash(b"0123456789")
                },
                {
                    "component": "cloud-link",
                    "version": "0.4.0",
                    "artifactUrl": "https://placeholder.invalid/cloud-link",
                    "artifactSizeBytes": 10,
                    "artifactSha256": Manifest::artifact_hash(b"abcdefghij"),
                    "requires": { "brainstem": "1.2.0" }
                }
            ]
        });
        let (config, key) = write_test_env(
            dir.path(),
            serde_json::json!({
                "dev-1": { "model": "THUNDER 01", "hardwareRevision": "rdk-s100p", "enabled": true }
            }),
            bundle_manifest,
            "",
            "rel-bundle-1",
        );
        fs::write(
            dir.path()
                .join("releases")
                .join("rel-bundle-1")
                .join("manifest.template.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "manifest": serde_json::from_slice::<serde_json::Value>(
                    &fs::read(
                        dir.path()
                            .join("releases")
                            .join("rel-bundle-1")
                            .join("manifest.template.json")
                    )
                    .unwrap()
                )
                .unwrap()["manifest"],
                "objectKey": "",
                "bundleObjectKeys": {
                    "brainstem": "firmware/qa/rel-bundle-1/brainstem.tar.gz",
                    "cloud-link": "components/qa/rel-bundle-1/cloud-link.tar.gz"
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let token = config.verify_api_token.clone();
        let app = build_router(config).unwrap();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/devices/dev-1/firmware/jobs")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"releaseId":"rel-bundle-1","idempotencyKey":"idem-bundle-1"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        assert_eq!(resp.headers().get("cache-control").unwrap(), "no-store");
        assert_eq!(
            resp.headers().get("x-content-type-options").unwrap(),
            "nosniff"
        );
        let job: Job = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        let decoded = job.manifest.decode(&key.verifying_key()).unwrap();
        assert_eq!(decoded.artifact_type, ArtifactType::ReleaseBundle);
        assert!(decoded.artifact_url.is_empty());
        assert!(
            decoded.bundle_items[0]
                .artifact_url
                .starts_with("https://doso-ota-prod-shanghai.oss-cn-shanghai.aliyuncs.com/firmware/qa/rel-bundle-1/brainstem.tar.gz?")
        );
        assert!(
            decoded.bundle_items[1]
                .artifact_url
                .starts_with("https://doso-ota-prod-shanghai.oss-cn-shanghai.aliyuncs.com/components/qa/rel-bundle-1/cloud-link.tar.gz?")
        );
        let profile = doso_ota::DeviceProfile {
            device_id: "dev-1".into(),
            model: "THUNDER 01".into(),
            hardware_revision: "rdk-s100p".into(),
            bootloader_version: "1.0.0".into(),
            brainstem_version: "1.0.0".into(),
            current_version: "1.0.0".into(),
            active_slot: doso_ota::Slot::A,
            battery_percent: 90,
            online: true,
            task_running: false,
            component_versions: BTreeMap::new(),
        };
        decoded.validate(now_ms(), &profile, 0).unwrap();
    }

    #[tokio::test]
    async fn empty_canary_stage_does_not_auto_promote_and_bake_waits_after_completion() {
        let dir = TestDir::new();
        let devices = serde_json::json!({
            "prod-only": { "model": "THUNDER 01", "hardwareRevision": "rdk-s100p", "enabled": true, "ring": "prod" }
        });
        let (config, _key) = write_test_env(
            dir.path(),
            devices,
            base_template_manifest("rel-empty-stage", 0, 0),
            "firmware/qa/rel-empty-stage/brainstem.tar.gz",
            "rel-empty-stage",
        );
        let token = config.verify_api_token.clone();
        let app = build_router(config).unwrap();

        let r_create = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/rollouts")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "rolloutId": "ro-no-empty-skip",
                            "releaseId": "rel-empty-stage",
                            "stages": [
                                { "name": "canary", "ring": "canary", "percent": 100, "bakeSeconds": 0, "autoPromote": true },
                                { "name": "prod", "ring": "prod", "percent": 100, "bakeSeconds": 0, "autoPromote": true }
                            ]
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_create.status(), StatusCode::CREATED);

        // Prod device must NOT receive job when canary stage has 0 eligible devices.
        let r_prod = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/agent/devices/prod-only/next-job")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_prod.status(), StatusCode::NO_CONTENT);

        let r_view = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/rollouts/ro-no-empty-skip")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let view: RolloutView = serde_json::from_slice(
            &axum::body::to_bytes(r_view.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(view.campaign.current_stage_index, 0);
        assert_eq!(view.campaign.status, RolloutStatus::Active);

        // Unit check: bakeSeconds > 0 waits after device completion timestamp.
        let mut campaign = view.campaign;
        campaign.stages[0].ring = "prod".into();
        campaign.stages[0].bake_seconds = 60;
        campaign.stage_started_at_ms = 1_000;
        campaign.device_states.insert(
            "prod-only".into(),
            RolloutDeviceState {
                device_id: "prod-only".into(),
                stage_index: 0,
                job_id: "j1".into(),
                phase: "succeeded".into(),
                assigned_at_ms: 50_000,
                updated_at_ms: 100_000,
                message: None,
            },
        );
        let reg = HashMap::from([(
            "prod-only".into(),
            DeviceRecord {
                model: "THUNDER 01".into(),
                hardware_revision: "rdk-s100p".into(),
                enabled: true,
                ring: "prod".into(),
                tags: vec![],
            },
        )]);
        assert!(!maybe_auto_promote(&mut campaign, &reg, 159_999));
        assert!(maybe_auto_promote(&mut campaign, &reg, 160_000));
        assert_eq!(campaign.current_stage_index, 1);
    }

    #[tokio::test]
    async fn cancel_rollout_job_syncs_rollout_state_and_guards_terminal_cancel() {
        let dir = TestDir::new();
        let devices = serde_json::json!({
            "canary-1": { "model": "THUNDER 01", "hardwareRevision": "rdk-s100p", "enabled": true, "ring": "canary" }
        });
        let (config, _key) = write_test_env(
            dir.path(),
            devices,
            base_template_manifest("rel-cancel-1", 0, 0),
            "firmware/qa/rel-cancel-1/brainstem.tar.gz",
            "rel-cancel-1",
        );
        let token = config.verify_api_token.clone();
        let app = build_router(config).unwrap();

        // Unauthorized request returns WWW-Authenticate: Bearer.
        let r_unauth = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/rollouts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_unauth.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            r_unauth.headers().get("www-authenticate").unwrap(),
            "Bearer"
        );

        // Idempotency key conflict returns 409.
        let r_start = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/devices/canary-1/firmware/jobs")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"releaseId":"rel-cancel-1","idempotencyKey":"idem-fixed"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_start.status(), StatusCode::ACCEPTED);
        let manual_job: Job = serde_json::from_slice(
            &axum::body::to_bytes(r_start.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();

        let r_conflict = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/devices/canary-1/firmware/jobs")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"releaseId":"rel-other","idempotencyKey":"idem-fixed"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_conflict.status(), StatusCode::CONFLICT);

        // Cancel manual job so canary-1 is free for rollout assignment.
        let r_cancel_manual = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/devices/canary-1/firmware/jobs/{}/cancel",
                        manual_job.job_id
                    ))
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_cancel_manual.status(), StatusCode::OK);

        // Create rollout and assign job to canary-1.
        let r_ro = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/rollouts")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "rolloutId": "ro-cancel-sync",
                            "releaseId": "rel-cancel-1",
                            "stages": [
                                { "name": "canary", "ring": "canary", "percent": 100, "bakeSeconds": 0, "autoPromote": false }
                            ]
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_ro.status(), StatusCode::CREATED);

        let r_next = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/agent/devices/canary-1/next-job")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_next.status(), StatusCode::OK);
        let ro_job: Job = serde_json::from_slice(
            &axum::body::to_bytes(r_next.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();

        // Transition to waiting_safe_window, then cancel -> rollout device state updates to cancelled!
        let r_wait = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/agent/devices/canary-1/jobs/{}/report",
                        ro_job.job_id
                    ))
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"phase":"waiting_safe_window"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_wait.status(), StatusCode::OK);

        let r_cancel = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/devices/canary-1/firmware/jobs/{}/cancel",
                        ro_job.job_id
                    ))
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_cancel.status(), StatusCode::OK);

        let r_view = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/rollouts/ro-cancel-sync")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let view: RolloutView = serde_json::from_slice(
            &axum::body::to_bytes(r_view.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(view.stage_counts[0].in_progress, 0);
        assert_eq!(view.stage_counts[0].waiting_safe_window, 0);
        assert_eq!(view.campaign.device_states["canary-1"].phase, "cancelled");
    }

    #[tokio::test]
    async fn unauthenticated_post_routes_reject_with_401_before_body_extraction() {
        let dir = TestDir::new();
        let (config, _key) = write_test_env(
            dir.path(),
            serde_json::json!({
                "dev-1": { "model": "THUNDER 01", "hardwareRevision": "rdk-s100p", "enabled": true }
            }),
            base_template_manifest("rel-auth-1", 0, 0),
            "firmware/qa/rel-auth-1/brainstem.tar.gz",
            "rel-auth-1",
        );
        let app = build_router(config).unwrap();

        let post_paths = [
            "/v1/rollouts",
            "/v1/devices/dev-1/firmware/jobs",
            "/v1/apps/android/releases",
            "/v1/agent/devices/dev-1/status",
            "/v1/agent/devices/dev-1/jobs/job-1/report",
            "/v1/devices/dev-1/firmware/jobs/job-1/report",
            "/v1/devices/dev-1/firmware/jobs/job-1/cancel",
            "/v1/rollouts/ro-1/promote",
            "/v1/rollouts/ro-1/pause",
            "/v1/rollouts/ro-1/resume",
            "/v1/rollouts/ro-1/abort",
        ];

        for path in post_paths {
            // Invalid empty JSON object must return 401 (never 422 schema leak).
            let r_json = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(r_json.status(), StatusCode::UNAUTHORIZED, "path={path}");
            assert_eq!(r_json.headers().get("www-authenticate").unwrap(), "Bearer");

            // Missing Content-Type must return 401 (never 415).
            let r_no_ct = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(r_no_ct.status(), StatusCode::UNAUTHORIZED, "path={path}");
        }

        // Public endpoints remain reachable without Authorization.
        let r_health = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_health.status(), StatusCode::OK);

        let r_android = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/apps/android/latest?channel=prod&version_code=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_android.status(), StatusCode::NOT_FOUND);
    }
}
