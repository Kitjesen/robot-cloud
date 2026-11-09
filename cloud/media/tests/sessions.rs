use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path, State},
    http::{HeaderMap, Request, StatusCode, header},
    response::IntoResponse,
    routing::post,
};
use doso_cloud_media::{Config, Media};
use robot_cloud_common::now_ms;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tower::ServiceExt;

const VIEWER: &str = "91de0e56-2b3f-4ae1-9575-e91541d49d33";
const DEVICE: &str = "d5619c98-0882-472a-b59c-d0f280f2253d";

struct Backend {
    deletes: AtomicUsize,
    offers: AtomicUsize,
    escape: bool,
    ready: AtomicBool,
    starts: AtomicUsize,
    stops: AtomicUsize,
}

async fn fixture(escape: bool) -> (Arc<Media>, Arc<Backend>, tokio::task::JoinHandle<()>) {
    fixture_source(escape, false).await
}

async fn fixture_source(
    escape: bool,
    source: bool,
) -> (Arc<Media>, Arc<Backend>, tokio::task::JoinHandle<()>) {
    let backend = Arc::new(Backend {
        deletes: AtomicUsize::new(0),
        offers: AtomicUsize::new(0),
        escape,
        ready: AtomicBool::new(true),
        starts: AtomicUsize::new(0),
        stops: AtomicUsize::new(0),
    });
    let app = Router::new()
        .route("/api/v1/robots/{device}/video", axum::routing::put(|State(state): State<Arc<Backend>>, Path(device): Path<String>, headers: HeaderMap, Json(body): Json<serde_json::Value>| async move {
            assert_eq!(device, DEVICE);
            assert_eq!(headers[header::AUTHORIZATION], format!("Bearer {}", "s".repeat(32)));
            let enabled = body["enabled"].as_bool().unwrap();
            if enabled { state.starts.fetch_add(1, Ordering::SeqCst); }
            else { state.stops.fetch_add(1, Ordering::SeqCst); }
            state.ready.store(enabled, Ordering::SeqCst);
            StatusCode::ACCEPTED
        }))
        .route("/v3/paths/get/{path}", axum::routing::get(|State(state): State<Arc<Backend>>| async move { Json(json!({"ready": state.ready.load(Ordering::SeqCst)})) }))
        .route("/authorize", post(|headers: HeaderMap| async move {
            let token = headers.get(header::AUTHORIZATION).and_then(|h| h.to_str().ok()).unwrap_or_default();
            if token == "Bearer denied" { return StatusCode::FORBIDDEN.into_response(); }
            let viewer = if token == "Bearer other" { "6c0365bb-adfc-44ca-a8b7-89c4417f0bdd" } else { VIEWER };
            Json(json!({"viewerId": viewer, "deviceId": DEVICE, "expiresAt": now_ms() + if token == "Bearer expired" { -1 } else if token == "Bearer brief" { 1100 } else { 60000 }})).into_response()
        }))
        .route("/{path}/whep", post(|State(state): State<Arc<Backend>>, Path(path): Path<String>| async move {
            assert_eq!(path, format!("doso_{DEVICE}"));
            state.offers.fetch_add(1, Ordering::SeqCst);
            (StatusCode::CREATED, [(header::LOCATION, if state.escape { "http://192.0.2.1/escape".to_string() } else { format!("/{path}/whep/resource") })], "v=0\r\nSIMULATED SDP\r\n")
        }))
        .route("/{path}/whep/resource", axum::routing::delete(|State(state): State<Arc<Backend>>| async move {
            state.deletes.fetch_add(1, Ordering::SeqCst);
            StatusCode::NO_CONTENT
        }))
        .with_state(backend.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let media = Arc::new(
        Media::new(Config {
            port: 0,
            media_origin: origin.clone(),
            media_api_origin: origin.clone(),
            authorize_url: format!("{origin}/authorize"),
            source_gateway_url: source.then(|| origin.clone()),
            source_gateway_key: source.then(|| "s".repeat(32)),
        })
        .unwrap(),
    );
    (media, backend, server)
}

#[tokio::test]
async fn authorized_viewers_start_one_source_and_last_close_stops_it() {
    let (media, backend, server) = fixture_source(false, true).await;
    backend.ready.store(false, Ordering::SeqCst);
    let denied = media
        .router()
        .oneshot(request("GET", "/api/v1/webrtc/status", Some("denied")))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert_eq!(backend.starts.load(Ordering::SeqCst), 0);
    let status = media
        .router()
        .oneshot(request("GET", "/api/v1/webrtc/status", Some("allowed")))
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);
    assert!(backend.ready.load(Ordering::SeqCst));
    let mut locations = Vec::new();
    for _ in 0..2 {
        let offer = media
            .router()
            .oneshot(request(
                "POST",
                "/api/v1/webrtc/whep?src=cam",
                Some("allowed"),
            ))
            .await
            .unwrap();
        assert_eq!(offer.status(), StatusCode::CREATED);
        locations.push(
            offer.headers()[header::LOCATION]
                .to_str()
                .unwrap()
                .to_owned(),
        );
    }
    assert_eq!(backend.starts.load(Ordering::SeqCst), 1);
    media
        .router()
        .oneshot(request("DELETE", &locations[0], Some("allowed")))
        .await
        .unwrap();
    assert_eq!(backend.stops.load(Ordering::SeqCst), 0);
    media
        .router()
        .oneshot(request("DELETE", &locations[1], Some("allowed")))
        .await
        .unwrap();
    assert_eq!(backend.stops.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn abandoned_warmup_stops_when_view_grant_expires() {
    let (media, backend, server) = fixture_source(false, true).await;
    assert_eq!(
        media
            .router()
            .oneshot(request("GET", "/api/v1/webrtc/status", Some("brief")))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let cleanup = tokio::spawn(media.cleanup());
    tokio::time::timeout(std::time::Duration::from_secs(4), async {
        while backend.stops.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    cleanup.abort();
    server.abort();
}

fn request(method: &str, path: &str, token: Option<&str>) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/sdp");
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    request
        .body(Body::from("v=0\r\nSIMULATED OFFER\r\n"))
        .unwrap()
}

#[tokio::test]
async fn requires_trusted_current_authorization() {
    let (media, backend, server) = fixture(false).await;
    for (token, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some("denied"), StatusCode::FORBIDDEN),
        (Some("expired"), StatusCode::FORBIDDEN),
    ] {
        let response = media
            .router()
            .oneshot(request("POST", "/api/v1/webrtc/whep?src=cam", token))
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    assert_eq!(backend.offers.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn limits_and_releases_viewer_sessions() {
    let (media, backend, server) = fixture(false).await;
    let mut locations = Vec::new();
    for _ in 0..4 {
        let response = media
            .router()
            .oneshot(request(
                "POST",
                "/api/v1/webrtc/whep?src=cam",
                Some("allowed"),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        locations.push(
            response.headers()[header::LOCATION]
                .to_str()
                .unwrap()
                .to_string(),
        );
    }
    assert_eq!(
        media
            .router()
            .oneshot(request(
                "POST",
                "/api/v1/webrtc/whep?src=cam",
                Some("allowed")
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        media
            .router()
            .oneshot(request("DELETE", &locations[0], Some("other")))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(backend.deletes.load(Ordering::SeqCst), 0);
    assert_eq!(
        media
            .router()
            .oneshot(request("DELETE", &locations[0], Some("allowed")))
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(backend.deletes.load(Ordering::SeqCst), 1);
    media.close_all().await;
    assert_eq!(backend.deletes.load(Ordering::SeqCst), 4);
    server.abort();
}

#[tokio::test]
async fn expires_even_without_client_delete() {
    let (media, backend, server) = fixture(false).await;
    let response = media
        .router()
        .oneshot(request(
            "POST",
            "/api/v1/webrtc/whep?src=cam",
            Some("brief"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let cleanup = tokio::spawn(media.cleanup());
    tokio::time::timeout(std::time::Duration::from_secs(4), async {
        while backend.deletes.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    cleanup.abort();
    server.abort();
}

#[tokio::test]
async fn renews_four_streams_without_reconnecting() {
    let (media, backend, server) = fixture(false).await;
    let mut locations = Vec::new();
    for _ in 0..4 {
        let response = media
            .router()
            .oneshot(request(
                "POST",
                "/api/v1/webrtc/whep?src=cam",
                Some("brief"),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        locations.push(
            response.headers()[header::LOCATION]
                .to_str()
                .unwrap()
                .to_owned(),
        );
    }
    assert_eq!(
        media
            .router()
            .oneshot(request(
                "POST",
                &format!("{}/renew", locations[0]),
                Some("other")
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    for location in &locations {
        assert_eq!(
            media
                .router()
                .oneshot(request(
                    "POST",
                    &format!("{location}/renew"),
                    Some("allowed")
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
    }
    let cleanup = tokio::spawn(media.clone().cleanup());
    tokio::time::sleep(std::time::Duration::from_millis(2200)).await;
    assert_eq!(backend.offers.load(Ordering::SeqCst), 4);
    assert_eq!(backend.deletes.load(Ordering::SeqCst), 0);
    media.close_all().await;
    assert_eq!(backend.deletes.load(Ordering::SeqCst), 4);
    cleanup.abort();
    server.abort();
}

#[tokio::test]
async fn rejects_media_resource_escape() {
    let (media, backend, server) = fixture(true).await;
    let response = media
        .router()
        .oneshot(request(
            "POST",
            "/api/v1/webrtc/whep?src=cam",
            Some("allowed"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(backend.deletes.load(Ordering::SeqCst), 0);
    server.abort();
}

#[test]
fn forbids_public_unencrypted_upstreams() {
    assert!(
        Media::new(Config {
            port: 0,
            media_origin: "http://192.0.2.1".into(),
            media_api_origin: "http://127.0.0.1:9997".into(),
            authorize_url: "https://example.com/authorize".into(),
            source_gateway_url: None,
            source_gateway_key: None,
        })
        .is_err()
    );
    assert!(
        Media::new(Config {
            port: 0,
            media_origin: "http://127.0.0.1:8889".into(),
            media_api_origin: "http://127.0.0.1:9997".into(),
            authorize_url: "http://example.com/authorize".into(),
            source_gateway_url: None,
            source_gateway_key: None,
        })
        .is_err()
    );
}

#[tokio::test]
async fn reports_actual_source_readiness() {
    let (media, backend, server) = fixture(false).await;
    for ready in [false, true] {
        backend.ready.store(ready, Ordering::SeqCst);
        let response = media
            .router()
            .oneshot(request("GET", "/api/v1/webrtc/status", Some("allowed")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
        let status: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(status["source_ready"], ready);
    }
    server.abort();
    server.await.ok();
    assert_eq!(
        media
            .router()
            .oneshot(request("GET", "/api/v1/webrtc/status", Some("allowed")))
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}
