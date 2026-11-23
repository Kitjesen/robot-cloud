use crate::{ArtifactType, CloudPollConfig, ManifestEnvelope, SystemHealthCheck, Updater, cloud};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
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

    let state = ApiState {
        updater,
        token,
        health,
    };
    let router = Router::new()
        .route("/health", get(process_health))
        .route("/v1/ota/status", get(status))
        .route("/v1/ota/jobs", post(start))
        .route("/v1/ota/jobs/{job_id}/download", post(download))
        .route("/v1/ota/jobs/{job_id}/install", post(install))
        .route("/v1/ota/jobs/{job_id}/health", post(check_health))
        .route("/v1/ota/jobs/{job_id}/rollback", post(rollback))
        .route("/v1/ota/jobs/{job_id}/cancel", post(cancel))
        .with_state(state);
    let result = axum::serve(TcpListener::bind(bind).await?, router).await;
    let _ = shutdown_tx.send(true);
    if let Some(handle) = bg_handle {
        let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;
    }
    result?;
    Ok(())
}

async fn process_health(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    if let Err(error) = authorize(&headers, &state) {
        return error.into_response();
    }
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

async fn status(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    if let Err(error) = authorize(&headers, &state) {
        return error.into_response();
    }
    let updater = state.updater.lock().await;
    match updater.job().map(|job| StatusResponse {
        profile: updater.profile(),
        job,
    }) {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
    }
}

async fn start(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<StartRequest>,
) -> Response {
    if let Err(error) = authorize(&headers, &state) {
        return error.into_response();
    }
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

async fn download(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> Response {
    if let Err(error) = authorize(&headers, &state) {
        return error.into_response();
    }
    let updater = state.updater.lock().await;
    if let Err(error) = verify_job(&updater, &job_id) {
        return error.into_response();
    }
    match updater.download().await {
        Ok(job) => (StatusCode::OK, Json(job)).into_response(),
        Err(error) => error_response(StatusCode::UNPROCESSABLE_ENTITY, error.to_string()),
    }
}

async fn install(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> Response {
    if let Err(error) = authorize(&headers, &state) {
        return error.into_response();
    }
    let updater = state.updater.lock().await;
    if let Err(error) = verify_job(&updater, &job_id) {
        return error.into_response();
    }
    let is_policy = updater
        .job()
        .ok()
        .flatten()
        .is_some_and(|j| j.manifest.artifact_type == ArtifactType::PolicyPackage);
    let outcome = if is_policy {
        updater.install_policy().await
    } else {
        updater.install()
    };
    match outcome {
        Ok(job) => (StatusCode::ACCEPTED, Json(job)).into_response(),
        Err(error) => error_response(StatusCode::UNPROCESSABLE_ENTITY, error.to_string()),
    }
}

async fn check_health(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> Response {
    if let Err(error) = authorize(&headers, &state) {
        return error.into_response();
    }
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

async fn rollback(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> Response {
    if let Err(error) = authorize(&headers, &state) {
        return error.into_response();
    }
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

async fn cancel(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> Response {
    if let Err(error) = authorize(&headers, &state) {
        return error.into_response();
    }
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

fn authorize(headers: &HeaderMap, state: &ApiState) -> Result<(), ApiError> {
    let Some(expected) = &state.token else {
        return Ok(());
    };
    let actual = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok());
    if actual == Some(&format!("Bearer {expected}")) {
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
