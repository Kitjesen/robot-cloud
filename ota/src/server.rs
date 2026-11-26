use crate::{ArtifactType, CloudPollConfig, ManifestEnvelope, SystemHealthCheck, Updater, cloud};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    net::TcpListener,
    sync::{Mutex, watch},
};

#[derive(Clone)]
struct ApiState {
    updater: Arc<Mutex<Updater>>,
    token: Option<String>,
    health: Option<Arc<SystemHealthCheck>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StartRequest {
    manifest: ManifestEnvelope,
    idempotency_key: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusResponse {
    profile: crate::DeviceProfile,
    job: Option<crate::Job>,
}

struct ApiError {
    status: StatusCode,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        error_response(self.status, self.message)
    }
}

pub async fn serve(
    updater: Updater,
    bind: SocketAddr,
    token: Option<String>,
    health: Option<SystemHealthCheck>,
) -> anyhow::Result<()> {
    serve_with_cloud(updater, bind, token, health, None).await
}

pub async fn serve_with_cloud(
    updater: Updater,
    bind: SocketAddr,
    token: Option<String>,
    health: Option<SystemHealthCheck>,
    cloud_config: Option<CloudPollConfig>,
) -> anyhow::Result<()> {
    if !bind.ip().is_loopback() && token.is_none() {
        anyhow::bail!("ota_token_required_for_non_loopback_bind");
    }
    // Bind HTTP listener before startup self-upgrade reconciliation so port conflicts fail fast.
    let listener = TcpListener::bind(bind).await?;
    let updater = Arc::new(Mutex::new(updater));
    let health = health.map(Arc::new);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let bg_handle = if let Some(cfg) = cloud_config {
        let u = updater.clone();
        let h = health.clone();
        Some(tokio::spawn(async move {
            cloud::run_cloud_loop(u, h, cfg, shutdown_rx).await;
        }))
    } else {
        // Still recover any persisted interrupted local OTA job on startup.
        if let Err(error) =
            cloud::recover_on_startup(&updater, health.clone(), None, Duration::from_millis(500))
                .await
        {
            eprintln!("ota_startup_recovery_error: {error}");
        }
        None
    };

    let router = build_router(updater, token, health);
    let result = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await;
    let _ = shutdown_tx.send(true);
    if let Some(handle) = bg_handle {
        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
    }
    result?;
    Ok(())
}

pub fn build_router(
    updater: Arc<Mutex<Updater>>,
    token: Option<String>,
    health: Option<Arc<SystemHealthCheck>>,
) -> Router {
    let state = ApiState {
        updater,
        token,
        health,
    };
    Router::new()
        .route("/health", get(process_health))
        .route("/v1/ota/status", get(status))
        .route("/v1/ota/jobs", post(start))
        .route("/v1/ota/jobs/{job_id}/download", post(download))
        .route("/v1/ota/jobs/{job_id}/install", post(install))
        .route("/v1/ota/jobs/{job_id}/health", post(check_health))
        .route("/v1/ota/jobs/{job_id}/rollback", post(rollback))
        .route("/v1/ota/jobs/{job_id}/cancel", post(cancel))
        // Enforce Bearer auth before JSON/Path extractors so unauthenticated callers never leak schema 415/422 errors.
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth))
        .layer(DefaultBodyLimit::max(256 * 1024))
        .with_state(state)
}

async fn require_auth(State(state): State<ApiState>, request: Request, next: Next) -> Response {
    if let Err(error) = authorize(request.headers(), &state) {
        return error.into_response();
    }
    next.run(request).await
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = async {
                if let Some(ref mut s) = sigterm {
                    s.recv().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn process_health() -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "service": "doso-ota",
            "agentVersion": env!("CARGO_PKG_VERSION"),
            "ready": true
        })),
    )
        .into_response()
}

async fn status(State(state): State<ApiState>) -> Response {
    let updater = state.updater.lock().await;
    match updater.job().map(|job| StatusResponse {
        profile: updater.profile(),
        job,
    }) {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
    }
}

async fn start(State(state): State<ApiState>, Json(request): Json<StartRequest>) -> Response {
    match state
        .updater
        .lock()
        .await
        .schedule(&request.manifest, &request.idempotency_key)
    {
        Ok(job) => (StatusCode::ACCEPTED, Json(job)).into_response(),
        Err(error) => error_response(StatusCode::CONFLICT, error.to_string()),
    }
}

async fn download(State(state): State<ApiState>, Path(job_id): Path<String>) -> Response {
    let updater = state.updater.lock().await;
    if let Err(error) = verify_job(&updater, &job_id) {
        return error.into_response();
    }
    match updater.download().await {
        Ok(job) => (StatusCode::OK, Json(job)).into_response(),
        Err(error) => error_response(StatusCode::UNPROCESSABLE_ENTITY, error.to_string()),
    }
}

async fn install(State(state): State<ApiState>, Path(job_id): Path<String>) -> Response {
    let updater = state.updater.lock().await;
    if let Err(error) = verify_job(&updater, &job_id) {
        return error.into_response();
    }
    let artifact_type = updater
        .job()
        .ok()
        .flatten()
        .map(|j| j.manifest.artifact_type);
    let outcome = match artifact_type {
        Some(ArtifactType::PolicyPackage) => updater.install_policy().await,
        Some(ArtifactType::ReleaseBundle) => {
            updater
                .install_bundle(state.health.clone(), Duration::from_millis(500))
                .await
        }
        _ => updater.install_async().await,
    };
    match outcome {
        Ok(job) => (StatusCode::ACCEPTED, Json(job)).into_response(),
        Err(error) => error_response(StatusCode::UNPROCESSABLE_ENTITY, error.to_string()),
    }
}

async fn check_health(State(state): State<ApiState>, Path(job_id): Path<String>) -> Response {
    let Some(checker) = state.health else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "health_check_not_configured".into(),
        );
    };
    let updater = state.updater.lock().await;
    if let Err(error) = verify_job(&updater, &job_id) {
        return error.into_response();
    }
    match updater.check_health(checker.as_ref()) {
        Ok(job) => (StatusCode::OK, Json(job)).into_response(),
        Err(error) => error_response(StatusCode::UNPROCESSABLE_ENTITY, error.to_string()),
    }
}

async fn rollback(State(state): State<ApiState>, Path(job_id): Path<String>) -> Response {
    let Some(checker) = state.health else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "health_check_not_configured".into(),
        );
    };
    let updater = state.updater.lock().await;
    if let Err(error) = verify_job(&updater, &job_id) {
        return error.into_response();
    }
    match updater.rollback(checker.as_ref()) {
        Ok(job) => (StatusCode::OK, Json(job)).into_response(),
        Err(error) => error_response(StatusCode::UNPROCESSABLE_ENTITY, error.to_string()),
    }
}

async fn cancel(State(state): State<ApiState>, Path(job_id): Path<String>) -> Response {
    let updater = state.updater.lock().await;
    if let Err(error) = verify_job(&updater, &job_id) {
        return error.into_response();
    }
    match updater.cancel() {
        Ok(job) => (StatusCode::OK, Json(job)).into_response(),
        Err(error) => error_response(StatusCode::CONFLICT, error.to_string()),
    }
}

fn verify_job(updater: &Updater, expected: &str) -> Result<(), ApiError> {
    match updater.job() {
        Ok(Some(job)) if job.job_id == expected => Ok(()),
        Ok(Some(_)) => Err(ApiError {
            status: StatusCode::NOT_FOUND,
            message: "ota_job_id_mismatch".into(),
        }),
        Ok(None) => Err(ApiError {
            status: StatusCode::NOT_FOUND,
            message: "ota_job_missing".into(),
        }),
        Err(error) => Err(ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        }),
    }
}

fn eq_bytes_ct(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&a, &b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

fn authorize(headers: &HeaderMap, state: &ApiState) -> Result<(), ApiError> {
    let Some(expected) = &state.token else {
        return Ok(());
    };
    let expected_header = format!("Bearer {expected}");
    let actual = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok());
    if actual.is_some_and(|val| eq_bytes_ct(val.as_bytes(), expected_header.as_bytes())) {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::UNAUTHORIZED,
            message: "ota_authorization_required".into(),
        })
    }
}

fn error_response(status: StatusCode, message: String) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DEFAULT_KEEP_HISTORY, DEFAULT_MIN_VBUS_V, DEFAULT_OTA_UNIT, UpdaterConfig};
    use ed25519_dalek::{SigningKey, pkcs8::EncodePublicKey};

    #[tokio::test]
    async fn auth_middleware_rejects_unauthenticated_post_before_json_or_media_extractors() {
        let dir = tempfile::tempdir().unwrap();
        let key = SigningKey::from_bytes(&[55; 32]);
        let key_path = dir.path().join("pub.pem");
        std::fs::write(
            &key_path,
            key.verifying_key()
                .to_public_key_pem(Default::default())
                .unwrap(),
        )
        .unwrap();
        let cfg = UpdaterConfig {
            root: dir.path().join("state"),
            device_id: "d-auth".into(),
            model: "THUNDER 01".into(),
            hardware_revision: "rdk-s100p".into(),
            bootloader_version: "1.0.0".into(),
            brainstem_version: "0.0.3".into(),
            current_version: "0.0.3".into(),
            battery_percent: 100,
            online: true,
            task_running: false,
            boot_id_path: dir.path().join("boot_id"),
            verify_key_path: key_path,
            brainstem_unit: "doso-brainstem.service".into(),
            staging_dir: None,
            releases_dir: None,
            current_link: None,
            last_good_link: None,
            components_root: Some(dir.path().join("components")),
            keep_history: DEFAULT_KEEP_HISTORY,
            require_can_read_only: false,
            min_vbus_v: DEFAULT_MIN_VBUS_V,
            ota_unit: DEFAULT_OTA_UNIT.into(),
            policy_admin_url: None,
            policy_admin_token: None,
            allow_insecure_http_loopback: true,
        };
        let updater = Arc::new(Mutex::new(Updater::new(cfg).unwrap()));
        let app = build_router(updater, Some("ota-secret-tok".into()), None);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        // Empty JSON `{}` without Bearer must return 401 (not 422 missing field)!
        let r_json = client
            .post(format!("http://{addr}/v1/ota/jobs"))
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(r_json.status(), StatusCode::UNAUTHORIZED);

        // Missing Content-Type without Bearer must return 401 (not 415 Unsupported Media Type)!
        let r_raw = client
            .post(format!("http://{addr}/v1/ota/jobs"))
            .body("not-json")
            .send()
            .await
            .unwrap();
        assert_eq!(r_raw.status(), StatusCode::UNAUTHORIZED);

        // Authorized GET /v1/ota/status succeeds with 200.
        let r_ok = client
            .get(format!("http://{addr}/v1/ota/status"))
            .bearer_auth("ota-secret-tok")
            .send()
            .await
            .unwrap();
        assert_eq!(r_ok.status(), StatusCode::OK);

        srv.abort();
    }
}
