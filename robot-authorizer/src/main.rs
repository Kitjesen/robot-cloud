use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use axum::{
    Router,
    extract::{Json, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

const DEFAULT_BIND: &str = "127.0.0.1:13148";
const DEFAULT_TOKEN_FILE: &str = "/etc/doso/robot-authorizer/token";
const DEFAULT_CHOICE_FILE: &str = "/var/lib/doso-cloud/choice";
const CLOUD_LINK_UNIT: &str = "doso-cloud-link.service";

#[derive(Clone)]
struct AppState {
    token_file: PathBuf,
    choice_file: PathBuf,
}

#[derive(Debug, Deserialize)]
struct ChoiceRequest {
    enabled: bool,
}

#[derive(Debug, Serialize)]
struct ChoiceResponse {
    enabled: bool,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: &'static str,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let bind = env::var("DOSO_AUTHORIZER_BIND").unwrap_or_else(|_| DEFAULT_BIND.into());
    let state = Arc::new(AppState {
        token_file: env::var_os("DOSO_AUTHORIZER_TOKEN_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_TOKEN_FILE)),
        choice_file: env::var_os("DOSO_CLOUD_CHOICE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CHOICE_FILE)),
    });
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    let app = router(state);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/cloud/choice", post(set_choice))
        .with_state(state)
}

async fn set_choice(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<ChoiceRequest>,
) -> Response {
    let authorized = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| token_matches(value, &state.token_file));
    if !authorized {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "unauthorized",
            }),
        )
            .into_response();
    }

    let choice_file = state.choice_file.clone();
    let enabled = request.enabled;
    let result = tokio::task::spawn_blocking(move || apply_choice(&choice_file, enabled)).await;
    match result {
        Ok(Ok(())) => Json(ChoiceResponse { enabled }).into_response(),
        Ok(Err(_)) | Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "choice_update_failed",
            }),
        )
            .into_response(),
    }
}

fn token_matches(header: &str, path: &Path) -> bool {
    let Some(supplied) = header.strip_prefix("Bearer ") else {
        return false;
    };
    let Ok(expected) = read_token(path) else {
        return false;
    };
    let expected: [u8; 32] = Sha256::digest(expected).into();
    let supplied: [u8; 32] = Sha256::digest(supplied.as_bytes()).into();
    expected.ct_eq(&supplied).into()
}

fn read_token(path: &Path) -> io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() {
        return Err(io::Error::other("token is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(io::Error::other("token file is writable by group or other"));
        }
    }
    let mut token = fs::read(path)?;
    while matches!(token.last(), Some(b'\n' | b'\r')) {
        token.pop();
    }
    if token.is_empty() {
        return Err(io::Error::other("token is empty"));
    }
    Ok(token)
}

fn apply_choice(path: &Path, enabled: bool) -> io::Result<()> {
    apply_choice_with(path, enabled, run_systemctl)
}

fn apply_choice_with<F>(path: &Path, enabled: bool, control: F) -> io::Result<()>
where
    F: Fn(&str) -> io::Result<()>,
{
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("choice has no parent"))?;
    fs::create_dir_all(parent)?;
    let previous = match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err(io::Error::other("choice is not a regular file"));
            }
            Some(fs::read(path)?)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    atomic_write(path, if enabled { b"true\n" } else { b"false\n" })?;
    if let Err(error) = control(if enabled { "start" } else { "stop" }) {
        let restored = match previous {
            Some(contents) => atomic_write(path, &contents),
            None => fs::remove_file(path),
        };
        return match restored {
            Ok(()) => Err(error),
            Err(restore_error) => Err(io::Error::other(format!(
                "systemctl failed and choice restore failed: {restore_error}"
            ))),
        };
    }
    Ok(())
}

fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("choice has no parent"))?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| io::Error::other("choice has no file name"))?;
    let temporary = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o640))?;
        }
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn run_systemctl(action: &str) -> io::Result<()> {
    let output = Command::new("systemctl")
        .args([action, CLOUD_LINK_UNIT])
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("systemctl {action} failed")))
    }
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tower::util::ServiceExt;

    #[test]
    fn writes_choice_atomically_with_expected_mode() {
        let dir = tempfile_dir();
        let path = dir.join("choice");
        apply_choice_without_service(&path, true).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "true\n");
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        apply_choice_without_service(&path, false).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "false\n");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn token_rejects_writable_files_and_accepts_exact_bearer() {
        let dir = tempfile_dir();
        let path = dir.join("token");
        fs::write(&path, "secret\n").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(token_matches("Bearer secret", &path));
        assert!(!token_matches("Bearer wrong", &path));
        #[cfg(unix)]
        {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o660)).unwrap();
            assert!(!token_matches("Bearer secret", &path));
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn route_rejects_missing_bearer_without_writing_choice() {
        let dir = tempfile_dir();
        let state = Arc::new(AppState {
            token_file: dir.join("token"),
            choice_file: dir.join("choice"),
        });
        fs::write(&state.token_file, "secret\n").unwrap();
        let response = router(state)
            .oneshot(
                Request::post("/v1/cloud/choice")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"enabled":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_systemctl_restores_previous_choice() {
        let dir = tempfile_dir();
        let path = dir.join("choice");
        fs::write(&path, "false\n").unwrap();
        let result = apply_choice_with(&path, true, |_| Err(io::Error::other("failed")));
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "false\n");
        fs::remove_dir_all(dir).unwrap();
    }

    fn apply_choice_without_service(path: &Path, enabled: bool) -> io::Result<()> {
        let parent = path.parent().unwrap();
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".test-{}.tmp", uuid::Uuid::new_v4()));
        fs::write(&temporary, if enabled { "true\n" } else { "false\n" })?;
        #[cfg(unix)]
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o640))?;
        fs::rename(temporary, path)
    }

    fn tempfile_dir() -> PathBuf {
        let path = env::temp_dir().join(format!("robot-authorizer-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
