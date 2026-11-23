use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use doso_ota::{ArtifactType, Manifest, ManifestEnvelope};
use ed25519_dalek::{Signer, SigningKey, pkcs8::DecodePrivateKey};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;
use url::form_urlencoded;
use uuid::Uuid;

type HmacSha1 = Hmac<Sha1>;

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
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReleaseTemplate {
    pub manifest: Manifest,
    pub object_key: String,
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
    signing_key: Arc<SigningKey>,
    jobs: Arc<Mutex<HashMap<String, Job>>>,
}

pub fn build_router(config: Config) -> Result<Router> {
    // Validate registry and signing key up front so broken config fails fast at boot.
    let _ = load_registry(&config.device_registry)?;
    let signing_key = SigningKey::from_pkcs8_pem(&fs::read_to_string(&config.signing_key_path)?)?;
    let jobs = load_jobs(&config.root)?;
    let state = AppState {
        config,
        signing_key: Arc::new(signing_key),
        jobs: Arc::new(Mutex::new(jobs)),
    };
    Ok(Router::new()
        .route("/health", get(health))
        .route("/v1/apps/android/latest", get(android_app_latest))
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
            "/v1/agent/devices/{device_id}/next-job",
            get(agent_next_job),
        )
        .route(
            "/v1/agent/devices/{device_id}/jobs/{job_id}/report",
            post(agent_report_job),
        )
        .with_state(state))
}

pub async fn run(config: Config, bind: SocketAddr) -> Result<()> {
    let app = build_router(config)?;
    axum::serve(tokio::net::TcpListener::bind(bind).await?, app).await?;
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
    let path = android_app_release_path(root, &release.channel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension(format!("json.{}.tmp", Uuid::new_v4()));
    fs::write(&temp, serde_json::to_vec_pretty(release)?)?;
    let file = fs::OpenOptions::new().read(true).write(true).open(&temp)?;
    file.sync_all()?;
    drop(file);
    if let Err(error) = fs::rename(&temp, &path) {
        if error.kind() != std::io::ErrorKind::PermissionDenied || !path.exists() {
            let _ = fs::remove_file(&temp);
            return Err(error.into());
        }
        fs::remove_file(&path)?;
        fs::rename(&temp, &path)?;
    }
    Ok(())
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
    headers: HeaderMap,
    Json(req): Json<AndroidAppPublishRequest>,
) -> Response {
    if let Err(response) = authorize(&headers, &state.config) {
        return *response;
    }
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
    let directory = root.join("status");
    fs::create_dir_all(&directory)?;
    let path = device_status_path(root, &record.device_id);
    let temp = directory.join(format!("{}.{}.tmp", record.device_id, Uuid::new_v4()));
    fs::write(&temp, serde_json::to_vec_pretty(record)?)?;
    let file = fs::OpenOptions::new().read(true).write(true).open(&temp)?;
    file.sync_all()?;
    drop(file);
    if let Err(error) = fs::rename(&temp, &path) {
        if error.kind() != std::io::ErrorKind::PermissionDenied || !path.exists() {
            let _ = fs::remove_file(&temp);
            return Err(error.into());
        }
        fs::remove_file(&path)?;
        fs::rename(&temp, &path)?;
    }
    Ok(())
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
    headers: HeaderMap,
    Path(device_id): Path<String>,
    Json(report): Json<DeviceStatusReportRequest>,
) -> Response {
    if let Err(response) = authorize(&headers, &state.config) {
        return *response;
    }
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
    headers: HeaderMap,
    Path(device_id): Path<String>,
) -> Response {
    if let Err(response) = authorize(&headers, &state.config) {
        return *response;
    }
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

async fn list_device_statuses(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = authorize(&headers, &state.config) {
        return *response;
    }
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

async fn releases(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
) -> Response {
    if let Err(response) = authorize(&headers, &state.config) {
        return *response;
    }
    let device = match lookup_enabled_device(&state.config.device_registry, &device_id) {
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
    headers: HeaderMap,
    Path(device_id): Path<String>,
    Json(request): Json<StartRequest>,
) -> Response {
    if let Err(response) = authorize(&headers, &state.config) {
        return *response;
    }
    let device = match lookup_enabled_device(&state.config.device_registry, &device_id) {
        Ok(Some(device)) => device,
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "device_not_found"),
        Err(error) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    if let Err(error) = ensure_request(&request) {
        return error_response(StatusCode::BAD_REQUEST, &error.to_string());
    }
    let mut jobs = state.jobs.lock().await;
    if let Some(job) = jobs
        .values()
        .find(|job| job.device_id == device_id && job.idempotency_key == request.idempotency_key)
    {
        return (StatusCode::ACCEPTED, Json(job)).into_response();
    }
    if jobs
        .values()
        .any(|job| job.device_id == device_id && is_active_phase(&job.phase))
    {
        return error_response(StatusCode::CONFLICT, "job_already_active");
    }
    let template = match load_release_template(&state.config.root, &request.release_id) {
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
        device_id,
        release_id: request.release_id,
        idempotency_key: request.idempotency_key,
        phase: "scheduled".into(),
        manifest: envelope,
        created_at_ms: now,
        updated_at_ms: now,
        bytes_downloaded: 0,
        current_version: None,
        message: None,
    };
    if let Err(error) = persist_job(&state.config.root, &job) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
    }
    jobs.insert(job.job_id.clone(), job.clone());
    (StatusCode::ACCEPTED, Json(job)).into_response()
}

async fn read_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((device_id, job_id)): Path<(String, String)>,
) -> Response {
    if let Err(response) = authorize(&headers, &state.config) {
        return *response;
    }
    let jobs = state.jobs.lock().await;
    match jobs.get(&job_id).filter(|job| job.device_id == device_id) {
        Some(job) => (StatusCode::OK, Json(job)).into_response(),
        None => error_response(StatusCode::NOT_FOUND, "job_not_found"),
    }
}

async fn cancel_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((device_id, job_id)): Path<(String, String)>,
) -> Response {
    if let Err(response) = authorize(&headers, &state.config) {
        return *response;
    }
    let mut jobs = state.jobs.lock().await;
    let Some(job) = jobs
        .get_mut(&job_id)
        .filter(|job| job.device_id == device_id)
    else {
        return error_response(StatusCode::NOT_FOUND, "job_not_found");
    };
    if matches!(job.phase.as_str(), "scheduled" | "downloading") {
        job.phase = "cancelled".into();
        job.updated_at_ms = now_ms();
    }
    if let Err(error) = persist_job(&state.config.root, job) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
    }
    (StatusCode::OK, Json(job)).into_response()
}

async fn agent_next_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
) -> Response {
    if let Err(response) = authorize(&headers, &state.config) {
        return *response;
    }
    match lookup_enabled_device(&state.config.device_registry, &device_id) {
        Ok(Some(_)) => {}
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "device_not_found"),
        Err(error) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
    let mut jobs = state.jobs.lock().await;
    let Some(next_id) = jobs
        .values()
        .filter(|job| {
            job.device_id == device_id && matches!(job.phase.as_str(), "scheduled" | "downloading")
        })
        .min_by_key(|job| job.created_at_ms)
        .map(|job| job.job_id.clone())
    else {
        return StatusCode::NO_CONTENT.into_response();
    };
    let job = jobs.get_mut(&next_id).expect("selected job exists");
    let now = now_ms();
    if let Ok(template) = load_release_template(&state.config.root, &job.release_id) {
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
    }
    (StatusCode::OK, Json(job.clone())).into_response()
}

async fn agent_report_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((device_id, job_id)): Path<(String, String)>,
    Json(report): Json<AgentReportRequest>,
) -> Response {
    if let Err(response) = authorize(&headers, &state.config) {
        return *response;
    }
    match lookup_enabled_device(&state.config.device_registry, &device_id) {
        Ok(Some(_)) => {}
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "device_not_found"),
        Err(error) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
    if !is_valid_agent_phase(&report.phase) {
        return error_response(StatusCode::BAD_REQUEST, "invalid_phase");
    }
    let mut jobs = state.jobs.lock().await;
    let Some(job) = jobs
        .get_mut(&job_id)
        .filter(|job| job.device_id == device_id)
    else {
        return error_response(StatusCode::NOT_FOUND, "job_not_found");
    };
    if is_terminal_phase(&job.phase) && job.phase != report.phase {
        return error_response(StatusCode::CONFLICT, "job_already_terminal");
    }
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
    job.updated_at_ms = now_ms();
    if let Err(error) = persist_job(&state.config.root, job) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
    }
    (StatusCode::OK, Json(job.clone())).into_response()
}

fn is_active_phase(phase: &str) -> bool {
    matches!(
        phase,
        "scheduled"
            | "downloading"
            | "verifying"
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

fn load_release_template(root: &FsPath, release_id: &str) -> Result<ReleaseTemplate> {
    ensure!(
        !release_id.is_empty()
            && !release_id.contains('/')
            && !release_id.contains('\\')
            && !release_id.contains(".."),
        "release_id_invalid"
    );
    let template_path = root
        .join("releases")
        .join(release_id)
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
    template.manifest.issued_at_ms = now_ms;
    template.manifest.expires_at_ms = now_ms.saturating_add(ttl_ms);
    template.manifest.key_id = config.signing_key_id.clone();
    template.manifest.artifact_url = oss_signed_url(config, &template.object_key, now_ms / 1000)?;
    sign_manifest(&template.manifest, signing_key)
}

fn ensure_request(request: &StartRequest) -> Result<()> {
    ensure!(!request.release_id.trim().is_empty(), "release_id_required");
    ensure!(
        !request.idempotency_key.trim().is_empty() && request.idempotency_key.len() <= 128,
        "idempotency_key_invalid"
    );
    Ok(())
}

fn authorize(headers: &HeaderMap, config: &Config) -> Result<(), Box<Response>> {
    let expected = format!("Bearer {}", config.verify_api_token);
    let actual = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok());
    if actual == Some(expected.as_str()) {
        Ok(())
    } else {
        Err(Box::new(error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        )))
    }
}

fn load_releases(root: &FsPath, device: &DeviceRecord) -> Result<Vec<ReleaseSummary>> {
    let mut output = Vec::new();
    let releases_dir = root.join("releases");
    if !releases_dir.exists() {
        return Ok(output);
    }
    for entry in fs::read_dir(releases_dir)? {
        let path = entry?.path().join("manifest.template.json");
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
    let expires = now_secs.saturating_add(config.signed_url_ttl_seconds as i64);
    let resource = format!(
        "/{}/{}",
        config.oss_bucket,
        object_key.trim_start_matches('/')
    );
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
    Ok(format!(
        "https://{}.{}/{}?{}",
        config.oss_bucket,
        config.oss_endpoint,
        object_key.trim_start_matches('/'),
        query
    ))
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
    let directory = root.join("jobs");
    fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{}.json", job.job_id));
    let temp = directory.join(format!("{}.{}.tmp", job.job_id, Uuid::new_v4()));
    fs::write(&temp, serde_json::to_vec_pretty(job)?)?;
    let file = fs::OpenOptions::new().read(true).write(true).open(&temp)?;
    file.sync_all()?;
    drop(file);
    if let Err(error) = fs::rename(&temp, &path) {
        if error.kind() != std::io::ErrorKind::PermissionDenied || !path.exists() {
            let _ = fs::remove_file(&temp);
            return Err(error.into());
        }
        // Windows std::fs::rename cannot replace an existing file.
        fs::remove_file(&path)?;
        fs::rename(&temp, &path)?;
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
}
