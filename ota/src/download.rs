use anyhow::{Context, Result, ensure};
use reqwest::header::{CONTENT_LENGTH, RANGE};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

pub async fn download_resumable(url: &str, destination: &Path, size: u64) -> Result<u64> {
    let partial = destination.with_extension("part");
    let journal = destination.with_extension("journal");
    let mut offset = fs::metadata(&partial).map(|m| m.len()).unwrap_or(0);
    ensure!(offset <= size, "partial_download_too_large");
    if let Ok(value) = fs::read_to_string(&journal) {
        ensure!(
            value.trim().parse::<u64>().ok() == Some(offset),
            "download_journal_mismatch"
        );
    }
    let mut builder = reqwest::Client::builder();
    if url::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| matches!(h, "127.0.0.1" | "localhost" | "::1"))
        })
        .unwrap_or(false)
    {
        builder = builder.no_proxy();
    }
    let client = builder.build()?;
    if offset < size {
        let mut request = client.get(url);
        if offset > 0 {
            request = request.header(RANGE, format!("bytes={offset}-"));
        }
        let response = request.send().await?.error_for_status()?;
        if offset > 0 && response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            offset = 0;
            let _ = fs::remove_file(&partial);
        }
        let expected = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
            ensure!(expected == Some(size - offset), "partial_length_mismatch");
        }
        let mut file = if offset == 0 {
            File::create(&partial)?
        } else {
            OpenOptions::new().append(true).open(&partial)?
        };
        let mut received = offset;
        let mut response = response;
        while let Some(chunk) = response.chunk().await? {
            received += chunk.len() as u64;
            ensure!(received <= size, "download_exceeds_manifest_size");
            file.write_all(&chunk)?;
            file.flush()?;
            fs::write(&journal, received.to_string())?;
        }
        file.sync_all()?;
        offset = received;
    }
    ensure!(
        offset == size && fs::metadata(&partial)?.len() == size,
        "download_incomplete"
    );
    verify_hash(&partial, size, None)?;
    fs::rename(&partial, destination)?;
    let _ = fs::remove_file(journal);
    Ok(size)
}

pub fn verify_hash(path: &Path, expected_size: u64, expected_hash: Option<&str>) -> Result<()> {
    let mut file = File::open(path)?;
    ensure!(
        file.metadata()?.len() == expected_size,
        "artifact_size_mismatch"
    );
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    if let Some(expected) = expected_hash {
        ensure!(
            format!("{:x}", hash.finalize()) == expected,
            "artifact_hash_mismatch"
        );
    }
    Ok(())
}

pub fn copy_verified(source: &Path, destination: &Path, size: u64, hash: &str) -> Result<()> {
    verify_hash(source, size, Some(hash))?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = destination.with_extension("installing");
    fs::copy(source, &temporary)
        .with_context(|| format!("copy artifact to {}", temporary.display()))?;
    let file = OpenOptions::new().read(true).write(true).open(&temporary)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, destination)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
        response::Response,
        routing::get,
    };
    use std::io::Write;

    #[test]
    fn verifies_exact_size_and_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a");
        let mut f = File::create(&path).unwrap();
        f.write_all(b"abc").unwrap();
        assert!(
            verify_hash(
                &path,
                3,
                Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
            )
            .is_ok()
        );
        assert!(verify_hash(&path, 2, None).is_err());
    }

    #[tokio::test]
    async fn resumes_a_partial_download() {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/artifact",
            get(|request: Request<Body>| async move {
                let start = request
                    .headers()
                    .get("range")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.strip_prefix("bytes="))
                    .and_then(|value| value.strip_suffix("-"))
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                let bytes = b"abcdef";
                let body = Body::from(bytes[start..].to_vec());
                if start == 0 {
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(body)
                        .unwrap()
                } else {
                    Response::builder()
                        .status(StatusCode::PARTIAL_CONTENT)
                        .header("content-length", bytes.len() - start)
                        .body(body)
                        .unwrap()
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("artifact");
        let partial = destination.with_extension("part");
        let journal = destination.with_extension("journal");
        fs::write(&partial, b"abc").unwrap();
        fs::write(&journal, b"3").unwrap();
        download_resumable(&format!("http://{address}/artifact"), &destination, 6)
            .await
            .unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"abcdef");
        assert!(!partial.exists());
        assert!(!journal.exists());
        server.abort();
    }
}
