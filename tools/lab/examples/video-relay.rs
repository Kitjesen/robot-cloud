use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use std::{net::Ipv4Addr, sync::Arc, time::Duration};
use tokio::{net::TcpListener, sync::RwLock};

#[derive(Default)]
struct Frame {
    bytes: Bytes,
    sequence: u64,
}
type Shared = Arc<RwLock<Frame>>;

async fn put_frame(State(frame): State<Shared>, bytes: Bytes) -> StatusCode {
    if !bytes.starts_with(&[0xff, 0xd8]) || !bytes.ends_with(&[0xff, 0xd9]) {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE;
    }
    let mut frame = frame.write().await;
    frame.bytes = bytes;
    frame.sequence += 1;
    StatusCode::NO_CONTENT
}

async fn get_frame(State(frame): State<Shared>) -> Response {
    let frame = frame.read().await;
    if frame.bytes.is_empty() {
        return StatusCode::NO_CONTENT.into_response();
    }
    (
        [
            (header::CONTENT_TYPE, "image/jpeg".to_owned()),
            (header::CACHE_CONTROL, "no-store".to_owned()),
            (
                header::HeaderName::from_static("x-frame-sequence"),
                frame.sequence.to_string(),
            ),
        ],
        frame.bytes.clone(),
    )
        .into_response()
}

const PAGE: &str = r#"<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>DOSO 模拟视频</title><style>body{background:#101922;color:#eef4f7;font:16px system-ui;text-align:center;margin:2rem}img{width:min(100%,800px);border-radius:10px;background:#222}p{color:#c8dae2}</style><h1>模拟视频 · 云端中转测试</h1><p>这是电脑生成的模拟画面，不是机器狗摄像头。</p><img id="frame" alt="模拟视频画面"><p id="status">正在等待画面…</p><script>let previous,active=false;async function update(){if(active)return;active=true;try{let r=await fetch('/frame',{cache:'no-store'});if(r.status===200){let n=r.headers.get('X-Frame-Sequence'),b=await r.blob(),u=URL.createObjectURL(b);document.getElementById('frame').src=u;if(previous)URL.revokeObjectURL(previous);previous=u;document.getElementById('status').textContent='模拟帧序号：'+n}else document.getElementById('status').textContent='正在等待画面…'}catch(e){document.getElementById('status').textContent='连接已中断'}finally{active=false}}update();setInterval(update,250)</script></html>"#;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let port: u16 = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "19089".into())
        .parse()?;
    let app = Router::new()
        .route("/", get(|| async { Html(PAGE) }))
        .route("/frame", get(get_frame).post(put_frame))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .with_state(Shared::default());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
    println!("SIMULATED video relay listening on 127.0.0.1:{port} for 10 minutes");
    axum::serve(listener, app)
        .with_graceful_shutdown(async { tokio::time::sleep(Duration::from_secs(600)).await })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_invalid_frame_and_keeps_latest() {
        let frame = Shared::default();
        assert_eq!(
            put_frame(State(frame.clone()), Bytes::from_static(b"bad")).await,
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        assert_eq!(
            get_frame(State(frame.clone())).await.status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            put_frame(
                State(frame.clone()),
                Bytes::from_static(b"\xff\xd8frame\xff\xd9")
            )
            .await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(frame.read().await.sequence, 1);
        assert_eq!(get_frame(State(frame)).await.status(), StatusCode::OK);
    }
}
