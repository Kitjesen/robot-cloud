use anyhow::{Result, ensure};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use reqwest::{Client, Url, redirect::Policy};
use robot_cloud_common::{is_uuid, now_ms};
use serde::Deserialize;
use serde_json::json;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::Mutex;
use uuid::Uuid;

mod source;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub port: u16,
    pub media_origin: String,
    pub media_api_origin: String,
    pub authorize_url: String,
    #[serde(default)]
    pub source_gateway_url: Option<String>,
    #[serde(default)]
    pub source_gateway_key: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Grant {
    viewer_id: String,
    device_id: String,
    expires_at: i64,
}

struct Session {
    grant: Grant,
    resource: Option<Url>,
    closing: bool,
}

pub struct Media {
    client: Client,
    origin: Url,
    api_origin: Url,
    authorize_url: Url,
    sessions: Mutex<HashMap<Uuid, Session>>,
    demand: Mutex<source::Demand>,
}

fn loopback(url: &Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    })
}

fn error(status: StatusCode, code: &'static str) -> Response {
    let mut response = (status, Json(json!({"error": {"code": code}}))).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

async fn read_body(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, Response> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| error(StatusCode::BAD_GATEWAY, "invalid_upstream"))?
    {
        if bytes.len() + chunk.len() > limit {
            return Err(error(StatusCode::BAD_GATEWAY, "invalid_upstream"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

impl Media {
    pub fn new(config: Config) -> Result<Self> {
        let demand = source::Demand::new(&config)?;
        let origin = Url::parse(&config.media_origin)?;
        let api_origin = Url::parse(&config.media_api_origin)?;
        let authorize_url = Url::parse(&config.authorize_url)?;
        for url in [&origin, &api_origin, &authorize_url] {
            ensure!(
                url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none(),
                "URL must not contain credentials, query or fragment"
            );
        }
        ensure!(
            origin.scheme() == "http" && loopback(&origin) && origin.path() == "/",
            "MediaMTX must be an HTTP loopback origin"
        );
        ensure!(
            api_origin.scheme() == "http" && loopback(&api_origin) && api_origin.path() == "/",
            "MediaMTX API must be an HTTP loopback origin"
        );
        ensure!(
            authorize_url.scheme() == "https"
                || authorize_url.scheme() == "http" && loopback(&authorize_url),
            "authorization backend requires HTTPS or an explicit loopback address"
        );
        Ok(Self {
            client: Client::builder()
                .no_proxy()
                .redirect(Policy::none())
                .timeout(Duration::from_secs(10))
                .build()?,
            origin,
            api_origin,
            authorize_url,
            sessions: Mutex::new(HashMap::new()),
            demand: Mutex::new(demand),
        })
    }

    pub fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route("/api/v1/webrtc/status", get(status))
            .route("/api/v1/webrtc/whep", post(offer))
            .route("/api/v1/webrtc/sessions/{id}", delete(close))
            .route("/api/v1/webrtc/sessions/{id}/renew", post(renew))
            .layer(DefaultBodyLimit::max(262144))
            .with_state(self.clone())
    }

    async fn authorize(&self, headers: &HeaderMap) -> Result<Grant, Response> {
        let bearer = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .filter(|value| value.starts_with("Bearer ") && value.len() > 7 && value.len() <= 2055)
            .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "unauthorized"))?;
        let response = self
            .client
            .post(self.authorize_url.clone())
            .header(header::AUTHORIZATION, bearer)
            .header(header::CONTENT_TYPE, "application/json")
            .body(r#"{"action":"video.view"}"#)
            .send()
            .await
            .map_err(|_| error(StatusCode::SERVICE_UNAVAILABLE, "authorization_unavailable"))?;
        if response.status() == StatusCode::UNAUTHORIZED
            || response.status() == StatusCode::FORBIDDEN
        {
            return Err(error(response.status(), "unauthorized"));
        }
        if response.status() != StatusCode::OK {
            return Err(error(
                StatusCode::SERVICE_UNAVAILABLE,
                "authorization_unavailable",
            ));
        }
        let body = read_body(response, 4096).await?;
        let grant: Grant = serde_json::from_slice(&body)
            .map_err(|_| error(StatusCode::BAD_GATEWAY, "invalid_authorization"))?;
        let now = now_ms();
        if !is_uuid(&grant.viewer_id)
            || !is_uuid(&grant.device_id)
            || grant.expires_at <= now
            || grant.expires_at > now + 300_000
        {
            return Err(error(StatusCode::FORBIDDEN, "invalid_authorization"));
        }
        Ok(grant)
    }

    async fn remove(&self, id: Uuid, expired_only: bool) -> bool {
        let mut sessions = self.sessions.lock().await;
        let Some(session) = sessions.get_mut(&id) else {
            return true;
        };
        if expired_only && session.grant.expires_at > now_ms() {
            return true;
        }
        session.closing = true;
        let device = session.grant.device_id.clone();
        let resource = session.resource.clone();
        if resource.is_none() {
            sessions.remove(&id);
            drop(sessions);
            let _ = self.sync_source(&device).await;
            return true;
        }
        drop(sessions);
        let response = self.client.delete(resource.unwrap()).send().await;
        if response.is_ok_and(|r| r.status().is_success() || r.status() == StatusCode::NOT_FOUND) {
            self.sessions.lock().await.remove(&id);
            let _ = self.sync_source(&device).await;
            true
        } else {
            false
        }
    }

    pub async fn cleanup(self: Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let now = now_ms();
            let expired: Vec<_> = self
                .sessions
                .lock()
                .await
                .iter()
                .filter(|(_, session)| session.grant.expires_at <= now)
                .map(|(id, _)| *id)
                .collect();
            for id in expired {
                self.remove(id, true).await;
            }
            self.sync_sources().await;
        }
    }

    pub async fn close_all(&self) {
        let ids: Vec<_> = self.sessions.lock().await.keys().copied().collect();
        for id in ids {
            self.remove(id, false).await;
        }
        self.demand.lock().await.warm.clear();
        self.sync_sources().await;
    }
}

async fn status(State(media): State<Arc<Media>>, headers: HeaderMap) -> Response {
    let grant = match media.authorize(&headers).await {
        Ok(grant) => grant,
        Err(response) => return response,
    };
    if media.warm_source(&grant).await.is_err() {
        return error(StatusCode::SERVICE_UNAVAILABLE, "source_unavailable");
    }
    let url = media
        .api_origin
        .join(&format!("v3/paths/get/doso_{}", grant.device_id))
        .unwrap();
    let response = match media
        .client
        .get(url)
        .timeout(Duration::from_secs(2))
        .send()
        .await
    {
        Ok(response) => response,
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "media_unavailable"),
    };
    let ready = if response.status() == StatusCode::NOT_FOUND {
        false
    } else if response.status() == StatusCode::OK {
        let bytes = match read_body(response, 65536).await {
            Ok(bytes) => bytes,
            Err(response) => return response,
        };
        match serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|value| value.get("ready").and_then(|ready| ready.as_bool()))
        {
            Some(ready) => ready,
            None => return error(StatusCode::BAD_GATEWAY, "invalid_source_status"),
        }
    } else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "media_unavailable");
    };
    let mut response = Json(
        json!({"available": true, "source_ready": ready, "rotation_degrees": 0, "ice_servers": []}),
    )
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    src: String,
}

async fn offer(
    State(media): State<Arc<Media>>,
    Query(source): Query<Source>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let grant = match media.authorize(&headers).await {
        Ok(grant) => grant,
        Err(response) => return response,
    };
    if source.src != "cam"
        || headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            != Some("application/sdp")
        || !body.starts_with(b"v=0")
    {
        return error(StatusCode::BAD_REQUEST, "invalid_offer");
    }
    let id = Uuid::new_v4();
    let device = grant.device_id.clone();
    let target = media
        .origin
        .join(&format!("doso_{}/whep", grant.device_id))
        .unwrap();
    {
        let mut sessions = media.sessions.lock().await;
        if sessions
            .values()
            .filter(|session| session.grant.viewer_id == grant.viewer_id)
            .count()
            >= 4
        {
            return error(StatusCode::TOO_MANY_REQUESTS, "viewer_limit");
        }
        let viewer = grant.viewer_id.clone();
        let device = grant.device_id.clone();
        sessions.insert(
            id,
            Session {
                grant,
                resource: None,
                closing: false,
            },
        );
        drop(sessions);
        media.demand.lock().await.warm.remove(&(viewer, device));
    }
    let result = async {
        media
            .sync_source(&device)
            .await
            .map_err(|_| error(StatusCode::SERVICE_UNAVAILABLE, "source_unavailable"))?;
        let response = media
            .client
            .post(target.clone())
            .header(header::CONTENT_TYPE, "application/sdp")
            .body(body)
            .send()
            .await
            .map_err(|_| error(StatusCode::SERVICE_UNAVAILABLE, "media_unavailable"))?;
        if response.status() != StatusCode::CREATED {
            return Err(error(StatusCode::SERVICE_UNAVAILABLE, "source_unavailable"));
        }
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|value| target.join(value).ok())
            .filter(|url| {
                url.origin() == target.origin()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
                    && url.path().starts_with(&format!("{}/", target.path()))
            })
            .ok_or_else(|| error(StatusCode::BAD_GATEWAY, "invalid_media_resource"))?;
        let mut sessions = media.sessions.lock().await;
        let session = sessions.get_mut(&id);
        if let Some(session) = session {
            session.resource = Some(location.clone());
        } else {
            drop(sessions);
            let _ = media.client.delete(location).send().await;
            return Err(error(StatusCode::FORBIDDEN, "expired"));
        }
        drop(sessions);
        let answer = read_body(response, 262144).await?;
        if !answer.starts_with(b"v=0") {
            return Err(error(StatusCode::BAD_GATEWAY, "invalid_answer"));
        }
        if media
            .sessions
            .lock()
            .await
            .get(&id)
            .is_none_or(|session| session.grant.expires_at <= now_ms())
        {
            return Err(error(StatusCode::FORBIDDEN, "expired"));
        }
        let mut reply = (StatusCode::CREATED, answer).into_response();
        reply
            .headers_mut()
            .insert(header::CONTENT_TYPE, "application/sdp".parse().unwrap());
        reply
            .headers_mut()
            .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
        reply.headers_mut().insert(
            header::LOCATION,
            format!("/api/v1/webrtc/sessions/{id}").parse().unwrap(),
        );
        Ok(reply)
    }
    .await;
    match result {
        Ok(response) => response,
        Err(response) => {
            if let Some(session) = media.sessions.lock().await.get_mut(&id) {
                session.grant.expires_at = 0;
            }
            media.remove(id, false).await;
            response
        }
    }
}

async fn renew(
    State(media): State<Arc<Media>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let grant = match media.authorize(&headers).await {
        Ok(grant) => grant,
        Err(response) => return response,
    };
    let Ok(id) = Uuid::parse_str(&id) else {
        return error(StatusCode::BAD_REQUEST, "invalid_session");
    };
    let mut sessions = media.sessions.lock().await;
    let Some(session) = sessions.get_mut(&id) else {
        return error(StatusCode::NOT_FOUND, "session_not_found");
    };
    if session.closing || session.resource.is_none() || session.grant.expires_at <= now_ms() {
        return error(StatusCode::FORBIDDEN, "expired");
    }
    if session.grant.viewer_id != grant.viewer_id || session.grant.device_id != grant.device_id {
        return error(StatusCode::FORBIDDEN, "unauthorized");
    }
    session.grant = grant;
    StatusCode::NO_CONTENT.into_response()
}

async fn close(
    State(media): State<Arc<Media>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let grant = match media.authorize(&headers).await {
        Ok(grant) => grant,
        Err(response) => return response,
    };
    let id = match Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid_session"),
    };
    {
        let sessions = media.sessions.lock().await;
        match sessions.get(&id) {
            None => return StatusCode::NO_CONTENT.into_response(),
            Some(session)
                if session.grant.viewer_id == grant.viewer_id
                    && session.grant.device_id == grant.device_id => {}
            _ => return error(StatusCode::FORBIDDEN, "unauthorized"),
        }
    }
    if let Some(session) = media.sessions.lock().await.get_mut(&id) {
        session.grant.expires_at = 0;
    }
    if media.remove(id, false).await {
        StatusCode::NO_CONTENT.into_response()
    } else {
        error(StatusCode::SERVICE_UNAVAILABLE, "media_unavailable")
    }
}
