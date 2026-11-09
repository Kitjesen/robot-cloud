use anyhow::{Result, ensure};
use reqwest::{
    Client, Method, StatusCode, Url,
    header::{HeaderMap, HeaderValue},
};
use serde_json::Value;
use std::{net::IpAddr, time::Duration};

pub const BODY_LIMIT: usize = 128 * 1024;

pub fn endpoint(value: &str) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| anyhow::anyhow!("invalid_telemetry_url"))?;
    let loopback = url.host_str().is_some_and(|host| {
        host.trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    });
    ensure!(
        url.host_str().is_some()
            && (url.scheme() == "https" || (url.scheme() == "http" && loopback))
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "invalid_telemetry_url"
    );
    Ok(url)
}

pub struct Http {
    client: Client,
}

impl Http {
    pub fn new(header: &'static str, key: &str) -> Result<Self> {
        ensure!(
            (32..=256).contains(&key.len()) && key.bytes().all(|b| (33..=126).contains(&b)),
            "invalid_telemetry_key"
        );
        let value = if header == "x-authorization" {
            format!("ApiKey {key}")
        } else {
            key.to_owned()
        };
        let mut value =
            HeaderValue::from_str(&value).map_err(|_| anyhow::anyhow!("invalid_telemetry_key"))?;
        value.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(header, value);
        Ok(Self {
            client: Client::builder()
                .default_headers(headers)
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(3))
                .build()?,
        })
    }

    pub async fn send(
        &self,
        method: Method,
        url: Url,
        body: Option<Value>,
    ) -> Result<(StatusCode, Vec<u8>)> {
        let mut request = self.client.request(method, url);
        if let Some(body) = body {
            let bytes = serde_json::to_vec(&body)?;
            ensure!(bytes.len() <= BODY_LIMIT, "telemetry_body_too_large");
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(bytes);
        }
        let mut response = request.send().await?;
        let status = response.status();
        ensure!(
            response
                .content_length()
                .is_none_or(|length| length <= BODY_LIMIT as u64),
            "telemetry_body_too_large"
        );
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                chunk.len() <= BODY_LIMIT - body.len(),
                "telemetry_body_too_large"
            );
            body.extend_from_slice(&chunk);
        }
        Ok((status, body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restricts_urls_and_credentials() {
        for url in [
            "https://example.com/api",
            "http://127.0.0.1:1234",
            "http://[::1]:1234",
        ] {
            assert!(endpoint(url).is_ok());
        }
        for url in [
            "http://example.com",
            "http://localhost",
            "https://user:pass@example.com",
            "https://example.com/?key=x",
            "https://example.com/#key",
            "file:///tmp/x",
        ] {
            assert!(endpoint(url).is_err());
        }
        for key in [
            "".to_owned(),
            "x".repeat(31),
            "x".repeat(257),
            " ".repeat(32),
            format!("{}\n", "x".repeat(32)),
        ] {
            assert!(Http::new("apikey", &key).is_err());
        }
    }

    #[tokio::test]
    async fn rejects_oversized_response_and_does_not_follow_redirects() {
        use axum::{
            Router,
            body::Body,
            response::{IntoResponse, Redirect},
            routing::get,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let app = Router::new()
            .route(
                "/large",
                get(|| async { Body::from(vec![b'x'; BODY_LIMIT + 1]).into_response() }),
            )
            .route(
                "/redirect",
                get(|| async { Redirect::temporary("/target") }),
            )
            .route("/target", get(|| async { "unexpected redirect target" }));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = Http::new("apikey", &"s".repeat(32)).unwrap();
        let oversized = client
            .send(Method::GET, url.join("large").unwrap(), None)
            .await;
        let redirect = client
            .send(Method::GET, url.join("redirect").unwrap(), None)
            .await;
        server.abort();
        assert!(oversized.is_err());
        assert_eq!(redirect.unwrap().0, StatusCode::TEMPORARY_REDIRECT);
    }
}
