use crate::{Manifest, archive::validate_relative_entry_path};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path, time::Duration};

#[derive(Debug, Clone)]
pub struct PolicyAdminClient {
    pub admin_url: String,
    pub admin_token: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PushPolicyPayload {
    manifest_yaml: String,
    onnx_base64: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    golden_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    slot: Option<String>,
    shadow_frames: u32,
    activate: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PolicyPushResponse {
    pub ok: bool,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub stage: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub active_profile: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PolicyPushFailure {
    pub stage: Option<String>,
    pub message: String,
}

impl PolicyPushFailure {
    /// Returns true if `:5054` explicitly rejected the policy before mutating slot state
    /// (schema/golden/shadow validation gates in `hot_deploy.rs`).
    pub fn is_pre_mutation_gate(&self) -> bool {
        matches!(
            self.stage.as_deref(),
            Some("golden_or_schema_gate" | "shadow_eval" | "register")
        )
    }
}

impl std::fmt::Display for PolicyPushFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.stage {
            Some(stage) => write!(f, "policy_push_failed({stage}): {}", self.message),
            None => write!(f, "policy_push_failed: {}", self.message),
        }
    }
}

impl std::error::Error for PolicyPushFailure {}

impl PolicyAdminClient {
    pub fn new(admin_url: String, admin_token: Option<String>) -> Self {
        Self {
            admin_url: admin_url.trim_end_matches('/').to_owned(),
            admin_token: admin_token.filter(|t| !t.trim().is_empty()),
        }
    }

    pub fn from_env() -> Self {
        let url = std::env::var("DOSO_POLICY_ADMIN_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "http://127.0.0.1:5054".into());
        let token = std::env::var("DOSO_POLICY_ADMIN_TOKEN")
            .ok()
            .filter(|v| !v.trim().is_empty());
        Self::new(url, token)
    }

    /// Reads `(policy.yaml, model.onnx, optional golden.json)` from `pkg_dir` and pushes
    /// them to `doso-brainstem` `:5054/v1/policies/push` with zero process restart.
    pub async fn push_policy_dir(
        &self,
        pkg_dir: &Path,
        manifest: &Manifest,
    ) -> std::result::Result<PolicyPushResponse, PolicyPushFailure> {
        let (yaml_text, onnx_bytes, golden_json) =
            read_policy_package_files(pkg_dir).map_err(|e| PolicyPushFailure {
                stage: Some("register".into()),
                message: e.to_string(),
            })?;

        let payload = PushPolicyPayload {
            manifest_yaml: yaml_text,
            onnx_base64: B64.encode(onnx_bytes),
            golden_json,
            slot: manifest.policy_slot.clone(),
            shadow_frames: manifest.effective_shadow_frames(),
            activate: manifest.effective_activate(),
        };

        let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(30));
        if is_loopback_url(&self.admin_url) {
            builder = builder.no_proxy();
        }
        let client = builder.build().map_err(|e| PolicyPushFailure {
            stage: None,
            message: e.to_string(),
        })?;

        let url = format!("{}/v1/policies/push", self.admin_url);
        let mut req = client.post(&url).json(&payload);
        if let Some(token) = &self.admin_token {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await.map_err(|e| PolicyPushFailure {
            stage: None,
            message: format!("http_send_error: {e}"),
        })?;
        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        if let Ok(parsed) = serde_json::from_str::<PolicyPushResponse>(&body_text) {
            if status.is_success() && parsed.ok {
                return Ok(parsed);
            }
            return Err(PolicyPushFailure {
                stage: parsed.stage,
                message: parsed
                    .error
                    .unwrap_or_else(|| format!("http_{} ({})", status.as_u16(), body_text)),
            });
        }
        Err(PolicyPushFailure {
            stage: None,
            message: format!("http_{}: {}", status.as_u16(), body_text),
        })
    }

    /// Triggers zero-restart instant slot rollback via `POST :5054/v1/policies/rollback`.
    pub async fn rollback_slot(&self, slot: &str) -> Result<String> {
        let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(10));
        if is_loopback_url(&self.admin_url) {
            builder = builder.no_proxy();
        }
        let client = builder.build()?;
        let url = format!("{}/v1/policies/rollback", self.admin_url);
        let mut req = client.post(&url).json(&serde_json::json!({ "slot": slot }));
        if let Some(token) = &self.admin_token {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await.context("policy_rollback_send")?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("policy_rollback_http_{}: {}", status.as_u16(), text);
        }
        let value: serde_json::Value =
            serde_json::from_str(&text).context("policy_rollback_json")?;
        ensure!(
            value.get("ok").and_then(|v| v.as_bool()) == Some(true),
            "policy_rollback_rejected: {text}"
        );
        Ok(value
            .get("restored_policy")
            .and_then(|v| v.as_str())
            .unwrap_or("restored")
            .to_owned())
    }
}

fn read_policy_package_files(pkg_dir: &Path) -> Result<(String, Vec<u8>, Option<String>)> {
    let yaml_path = pkg_dir.join("policy.yaml");
    ensure!(yaml_path.is_file(), "policy_yaml_missing");
    let yaml_text = fs::read_to_string(&yaml_path).context("read policy.yaml")?;

    let model_rel_raw =
        parse_model_file_from_yaml(&yaml_text).unwrap_or_else(|| "model.onnx".into());
    let model_rel =
        validate_relative_entry_path(&model_rel_raw).context("policy_model_path_invalid")?;
    ensure!(!model_rel.as_os_str().is_empty(), "policy_model_path_empty");
    let onnx_path = pkg_dir.join(&model_rel);
    ensure!(
        onnx_path.is_file(),
        "policy_onnx_missing: {}",
        model_rel.display()
    );
    let onnx_bytes = fs::read(&onnx_path).context("read model.onnx")?;
    ensure!(!onnx_bytes.is_empty(), "policy_onnx_empty");

    let golden_path = pkg_dir.join("golden.json");
    let golden_json = if golden_path.is_file() {
        Some(fs::read_to_string(&golden_path).context("read golden.json")?)
    } else {
        None
    };
    Ok((yaml_text, onnx_bytes, golden_json))
}

fn is_loopback_url(raw: &str) -> bool {
    url::Url::parse(raw)
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| matches!(h, "127.0.0.1" | "localhost" | "::1"))
        })
        .unwrap_or(false)
}

fn parse_model_file_from_yaml(yaml_text: &str) -> Option<String> {
    for line in yaml_text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("file:") {
            let value = rest.trim().trim_matches('"').trim_matches('\'').trim();
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}
