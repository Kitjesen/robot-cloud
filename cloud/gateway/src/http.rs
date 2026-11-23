use crate::{Error, Gateway};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use robot_cloud_common::{is_uuid, reporting::Permission};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};
use subtle::ConstantTimeEq;

impl Error {
    fn http(self) -> (StatusCode, &'static str) {
        match self {
            Self::Disabled => (StatusCode::FORBIDDEN, "disabled"),
            Self::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
            Self::Timeout => (StatusCode::GATEWAY_TIMEOUT, "timeout"),
            Self::UpstreamUnavailable => (StatusCode::SERVICE_UNAVAILABLE, "upstream_unavailable"),
            Self::Busy => (StatusCode::TOO_MANY_REQUESTS, "busy"),
            Self::InvalidDevice => (StatusCode::BAD_REQUEST, "invalid_device"),
            Self::InvalidPermission => (StatusCode::BAD_REQUEST, "invalid_permission"),
            Self::StalePermission => (StatusCode::CONFLICT, "stale_permission"),
        }
    }
}

#[derive(Clone)]
struct HttpState {
    gateway: Arc<Gateway>,
    authorization_hash: [u8; 32],
    control: Option<Arc<crate::control::Hub>>,
}

fn json_response(status: StatusCode, value: serde_json::Value) -> Response {
    let mut response = (status, Json(value)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    response
}

fn reject(status: StatusCode, code: &'static str) -> Response {
    json_response(status, json!({ "error": { "code": code } }))
}

async fn query(State(state): State<HttpState>, request: Request<Body>) -> Response {
    let authorization = request
        .headers()
        .get(header::AUTHORIZATION)
        .map(|value| value.as_bytes())
        .unwrap_or_default();
    let supplied: [u8; 32] = Sha256::digest(authorization).into();
    if !bool::from(state.authorization_hash.ct_eq(&supplied)) {
        let mut response = reject(StatusCode::UNAUTHORIZED, "unauthorized");
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, "Bearer".parse().unwrap());
        return response;
    }
    let path = request.uri().path().strip_prefix("/api/v1/robots/");
    if let Some((device_id, suffix)) = path.and_then(|path| path.split_once("/control/sessions"))
        && is_uuid(device_id)
        && (suffix.is_empty() || suffix.strip_prefix('/').is_some_and(is_uuid))
    {
        let device_id = device_id.to_owned();
        let session_id = suffix.strip_prefix('/').map(str::to_owned);
        return control_session(state, request, &device_id, session_id.as_deref()).await;
    }
    let route = path.and_then(|path| {
        [
            ("/status/latest", Method::GET),
            ("/reporting", Method::PUT),
            ("/video", Method::PUT),
            ("/status", Method::POST),
        ]
        .into_iter()
        .find_map(|(suffix, method)| {
            path.strip_suffix(suffix)
                .filter(|id| is_uuid(id))
                .map(|id| (id.to_owned(), method))
        })
    });
    let Some((device_id, method)) = route else {
        return reject(StatusCode::NOT_FOUND, "not_found");
    };
    if request.uri().query().is_some() {
        return reject(StatusCode::NOT_FOUND, "not_found");
    }
    if request.method() != method {
        let mut response = reject(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
        response
            .headers_mut()
            .insert(header::ALLOW, method.as_str().parse().unwrap());
        return response;
    }
    let request_path_is_video = request.uri().path().ends_with("/video");
    if method == Method::PUT {
        if request
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .is_some_and(|length| length > 2048)
        {
            return reject(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large");
        }
        let bytes =
            match tokio::time::timeout(Duration::from_secs(1), to_bytes(request.into_body(), 2048))
                .await
            {
                Ok(Ok(bytes)) => bytes,
                Ok(Err(_)) => return reject(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"),
                Err(_) => return reject(StatusCode::BAD_REQUEST, "invalid_permission"),
            };
        if request_path_is_video {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Video {
                enabled: bool,
            }
            let Ok(video) = serde_json::from_slice::<Video>(&bytes) else {
                return reject(StatusCode::BAD_REQUEST, "invalid_video_control");
            };
            return match state.gateway.set_video(&device_id, video.enabled) {
                Ok(accepted) => json_response(StatusCode::ACCEPTED, json!(accepted)),
                Err(error) => {
                    let (status, code) = error.http();
                    reject(status, code)
                }
            };
        }
        let Ok(permission) = serde_json::from_slice::<Permission>(&bytes) else {
            return reject(StatusCode::BAD_REQUEST, "invalid_permission");
        };
        return match state.gateway.set_reporting(&device_id, permission) {
            Ok(latest) => json_response(StatusCode::ACCEPTED, json!(latest)),
            Err(error) => {
                let (status, code) = error.http();
                reject(status, code)
            }
        };
    }
    if request.headers().contains_key(header::TRANSFER_ENCODING)
        || request
            .headers()
            .get(header::CONTENT_LENGTH)
            .is_some_and(|value| value.as_bytes() != b"0")
    {
        return reject(StatusCode::BAD_REQUEST, "body_not_allowed");
    }
    if !matches!(
        tokio::time::timeout(Duration::from_secs(1), to_bytes(request.into_body(), 0)).await,
        Ok(Ok(_))
    ) {
        return reject(StatusCode::BAD_REQUEST, "body_not_allowed");
    }
    if method == Method::GET {
        return match state.gateway.latest(&device_id) {
            Ok(latest) => json_response(StatusCode::OK, json!(latest)),
            Err(error) => {
                let (status, code) = error.http();
                reject(status, code)
            }
        };
    }
    match state.gateway.read_status(&device_id).await {
        Ok(snapshot) => json_response(StatusCode::OK, json!(snapshot)),
        Err(error) => {
            let (status, code) = error.http();
            reject(status, code)
        }
    }
}

pub fn app(gateway: Arc<Gateway>, api_key: &str) -> anyhow::Result<Router> {
    app_with_control(gateway, api_key, None)
}

pub fn app_with_control(
    gateway: Arc<Gateway>,
    api_key: &str,
    control: Option<Arc<crate::control::Hub>>,
) -> anyhow::Result<Router> {
    anyhow::ensure!(
        (32..=256).contains(&api_key.len())
            && api_key.bytes().all(|byte| (33..=126).contains(&byte)),
        "invalid_service_credential"
    );
    let authorization_hash = Sha256::digest(format!("Bearer {api_key}")).into();
    Ok(Router::new().fallback(query).with_state(HttpState {
        gateway,
        authorization_hash,
        control,
    }))
}

async fn control_session(
    state: HttpState,
    request: Request<Body>,
    device_id: &str,
    session_id: Option<&str>,
) -> Response {
    let Some(control) = state.control else {
        return reject(StatusCode::NOT_FOUND, "not_found");
    };
    if request.uri().query().is_some() {
        return reject(StatusCode::NOT_FOUND, "not_found");
    }
    let method = if session_id.is_some() {
        Method::DELETE
    } else {
        Method::POST
    };
    if request.method() != method {
        let mut response = reject(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
        response
            .headers_mut()
            .insert(header::ALLOW, method.as_str().parse().unwrap());
        return response;
    }
    let bytes =
        match tokio::time::timeout(Duration::from_secs(1), to_bytes(request.into_body(), 2048))
            .await
        {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(_)) => return reject(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"),
            Err(_) => return reject(StatusCode::BAD_REQUEST, "invalid_control_identity"),
        };
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Actor {
        operator_id: String,
        authorization_epoch: u64,
    }
    let Ok(actor) = serde_json::from_slice::<Actor>(&bytes) else {
        return reject(StatusCode::BAD_REQUEST, "invalid_control_identity");
    };
    let result = match session_id {
        Some(session_id) => control
            .revoke(
                device_id,
                session_id,
                &actor.operator_id,
                actor.authorization_epoch,
            )
            .map(|()| json_response(StatusCode::OK, json!({"closed": true}))),
        None => control
            .create_session(device_id, &actor.operator_id, actor.authorization_epoch)
            .map(|session| json_response(StatusCode::CREATED, json!(session))),
    };
    result.unwrap_or_else(|error| {
        let (status, code) = match error.code() {
            tonic::Code::InvalidArgument => (StatusCode::BAD_REQUEST, "invalid_control_identity"),
            tonic::Code::PermissionDenied => (StatusCode::FORBIDDEN, "control_forbidden"),
            tonic::Code::AlreadyExists => (StatusCode::CONFLICT, "control_session_active"),
            tonic::Code::NotFound => (StatusCode::NOT_FOUND, "control_session_missing"),
            tonic::Code::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "device_disconnected"),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, "control_unavailable"),
        };
        reject(status, code)
    })
}
