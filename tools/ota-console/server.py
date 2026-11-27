#!/usr/bin/env python3
"""DOSO OTA Release Console — zero-dependency local web server & API bridge.

Reuses tools/deploy/doso-ota-ops.py as the single source of truth for packaging,
OSS HMAC-SHA1 uploads, manifest template rendering, and cloud release API calls.
"""

from __future__ import annotations

import argparse
import atexit
from concurrent.futures import ThreadPoolExecutor
import importlib.util
import json
import mimetypes
import os
import re
import socket
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
import webbrowser
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

if getattr(sys, "frozen", False) and hasattr(sys, "_MEIPASS"):
    _BUNDLE_ROOT = Path(getattr(sys, "_MEIPASS"))
    CONSOLE_DIR = _BUNDLE_ROOT
    WEB_DIR = _BUNDLE_ROOT / "web"
    REPO_ROOT = Path(sys.executable).resolve().parent
    OPS_SCRIPT_PATH = _BUNDLE_ROOT / "doso-ota-ops.py"
    ROBOT_OPS_PATH = _BUNDLE_ROOT / "robot_ops.py"
    PROVISION_OPS_PATH = _BUNDLE_ROOT / "provision_ops.py"
    FLEET_OPS_PATH = _BUNDLE_ROOT / "fleet_ops.py"
else:
    CONSOLE_DIR = Path(__file__).resolve().parent
    WEB_DIR = CONSOLE_DIR / "web"
    REPO_ROOT = CONSOLE_DIR.parent.parent
    OPS_SCRIPT_PATH = REPO_ROOT / "tools" / "deploy" / "doso-ota-ops.py"
    ROBOT_OPS_PATH = CONSOLE_DIR / "robot_ops.py"
    PROVISION_OPS_PATH = CONSOLE_DIR / "provision_ops.py"
    FLEET_OPS_PATH = CONSOLE_DIR / "fleet_ops.py"

MAX_JSON_BODY_BYTES = 2 * 1024 * 1024
MAX_UPLOAD_BYTES = 512 * 1024 * 1024
UPLOAD_CHUNK_BYTES = 65536
ANDROID_CHANNELS = ("prod", "qa", "beta")

CONFIG_KEYS = (
    "DOSO_OTA_CLOUD_URL",
    "DOSO_RELEASE_API_TOKEN",
    "DOSO_OSS_BUCKET",
    "DOSO_OSS_ENDPOINT",
    "DOSO_OSS_ACCESS_KEY_ID",
    "DOSO_OSS_ACCESS_KEY_SECRET",
    "DOSO_DEFAULT_MODEL",
    "DOSO_DEFAULT_HW_REV",
    "DOSO_SIGNING_KEY_ID",
    "DOSO_OPS_JUMP_HOST",
    "DOSO_OPS_JUMP_PORT",
    "DOSO_OPS_JUMP_USER",
    "DOSO_OPS_JUMP_KEY",
    "DOSO_OPS_ROBOT_USER",
    "DOSO_OPS_ROBOT_PASSWORD",
    "DOSO_OPS_ROBOT_KEY",
    "DOSO_ECS_INSTANCE_ID",
    "DOSO_ECS_REGION",
    "DOSO_ECS_ACCESS_KEY_ID",
    "DOSO_ECS_ACCESS_KEY_SECRET",
    "DOSO_DEFAULT_OWNER_EMAIL",
)
SECRET_CONFIG_KEYS = frozenset(
    {
        "DOSO_RELEASE_API_TOKEN",
        "DOSO_OSS_ACCESS_KEY_ID",
        "DOSO_OSS_ACCESS_KEY_SECRET",
        "DOSO_OPS_ROBOT_PASSWORD",
        "DOSO_ECS_ACCESS_KEY_ID",
        "DOSO_ECS_ACCESS_KEY_SECRET",
    }
)
DEFAULT_CONFIG: dict[str, str] = {
    "DOSO_OTA_CLOUD_URL": "https://api.dosorobotics.com/ota",
    "DOSO_RELEASE_API_TOKEN": "",
    "DOSO_OSS_BUCKET": "doso-ota-prod-shanghai",
    "DOSO_OSS_ENDPOINT": "oss-cn-shanghai.aliyuncs.com",
    "DOSO_OSS_ACCESS_KEY_ID": "",
    "DOSO_OSS_ACCESS_KEY_SECRET": "",
    "DOSO_DEFAULT_MODEL": "THUNDER 01",
    "DOSO_DEFAULT_HW_REV": "rdk-s100p",
    "DOSO_SIGNING_KEY_ID": "release-key-2026-01",
    "DOSO_OPS_JUMP_HOST": "47.103.89.137",
    "DOSO_OPS_JUMP_PORT": "22",
    "DOSO_OPS_JUMP_USER": "admin",
    "DOSO_OPS_JUMP_KEY": "",
    "DOSO_OPS_ROBOT_USER": "sunrise",
    "DOSO_OPS_ROBOT_PASSWORD": "",
    "DOSO_OPS_ROBOT_KEY": "",
    "DOSO_ECS_INSTANCE_ID": "i-uf6hb2phe34p0mt1xfca",
    "DOSO_ECS_REGION": "cn-shanghai",
    "DOSO_ECS_ACCESS_KEY_ID": "",
    "DOSO_ECS_ACCESS_KEY_SECRET": "",
    "DOSO_DEFAULT_OWNER_EMAIL": "995632531@qq.com",
}


def load_module_from_path(name: str, path: Path) -> Any:
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


ops = load_module_from_path("doso_ota_ops", OPS_SCRIPT_PATH)
robot_ops = load_module_from_path("doso_robot_ops", ROBOT_OPS_PATH)
sys.modules["robot_ops"] = robot_ops
provision_ops = load_module_from_path("doso_provision_ops", PROVISION_OPS_PATH)
fleet_ops = load_module_from_path("doso_fleet_ops", FLEET_OPS_PATH)


def default_config_path() -> Path:
    env_override = os.environ.get("DOSO_OTA_CONSOLE_CONFIG", "").strip()
    if env_override:
        return Path(env_override)
    return Path.home() / ".doso-ota" / "console.env"


def mask_secret(value: str) -> str:
    clean = value.strip()
    if not clean:
        return ""
    if len(clean) <= 8:
        return "••••••"
    return f"{clean[:4]}••••{clean[-4:]}"


def parse_env_file(path: Path) -> dict[str, str]:
    result: dict[str, str] = {}
    if not path.is_file():
        return result
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, val = line.split("=", 1)
        key = key.strip()
        val = val.strip()
        if len(val) >= 2 and val[0] == val[-1] and val[0] in ("'", '"'):
            val = val[1:-1]
        if key in CONFIG_KEYS:
            result[key] = val
    return result


def serialize_env_file(values: dict[str, str]) -> bytes:
    lines = [
        "# DOSO OTA Console local configuration (user-private, never commit to git)",
    ]
    for key in CONFIG_KEYS:
        raw = str(values.get(key, "")).replace("\r", "").replace("\n", "")
        if any(ch.isspace() or ch in "#'\"" for ch in raw):
            escaped = raw.replace("\\", "\\\\").replace('"', '\\"')
            lines.append(f'{key}="{escaped}"')
        else:
            lines.append(f"{key}={raw}")
    lines.append("")
    return "\n".join(lines).encode("utf-8")


class ConfigStore:
    """Thread-safe local credential & defaults store outside the repository."""

    def __init__(self, path: Path) -> None:
        self.path = path
        self._lock = threading.Lock()
        self._file_values: dict[str, str] = parse_env_file(path)

    def _effective_unlocked(self) -> dict[str, str]:
        merged = dict(DEFAULT_CONFIG)
        for key in CONFIG_KEYS:
            env_val = os.environ.get(key, "").strip()
            if env_val:
                merged[key] = env_val
        for key, val in self._file_values.items():
            if val != "":
                merged[key] = val
        return merged

    def effective(self) -> dict[str, str]:
        with self._lock:
            return self._effective_unlocked()

    def public_view(self) -> dict[str, Any]:
        cfg = self.effective()
        token = cfg["DOSO_RELEASE_API_TOKEN"]
        ak_id = cfg["DOSO_OSS_ACCESS_KEY_ID"]
        ak_secret = cfg["DOSO_OSS_ACCESS_KEY_SECRET"]
        robot_pwd = cfg["DOSO_OPS_ROBOT_PASSWORD"]
        ecs_ak_id = cfg["DOSO_ECS_ACCESS_KEY_ID"]
        ecs_ak_sec = cfg["DOSO_ECS_ACCESS_KEY_SECRET"]
        jump_key_auto = (Path.home() / ".ssh" / "inovxio_deploy").is_file()
        return {
            "cloudUrl": cfg["DOSO_OTA_CLOUD_URL"],
            "ossBucket": cfg["DOSO_OSS_BUCKET"],
            "ossEndpoint": cfg["DOSO_OSS_ENDPOINT"],
            "defaultModel": cfg["DOSO_DEFAULT_MODEL"],
            "defaultHardwareRevision": cfg["DOSO_DEFAULT_HW_REV"],
            "signingKeyId": cfg["DOSO_SIGNING_KEY_ID"],
            "tokenConfigured": bool(token),
            "tokenPreview": mask_secret(token),
            "ossAkIdConfigured": bool(ak_id),
            "ossAkIdPreview": mask_secret(ak_id),
            "ossAkSecretConfigured": bool(ak_secret),
            "ossAkSecretPreview": mask_secret(ak_secret),
            "ossConfigured": bool(ak_id and ak_secret and cfg["DOSO_OSS_BUCKET"] and cfg["DOSO_OSS_ENDPOINT"]),
            "opsJumpHost": cfg["DOSO_OPS_JUMP_HOST"],
            "opsJumpPort": cfg["DOSO_OPS_JUMP_PORT"],
            "opsJumpUser": cfg["DOSO_OPS_JUMP_USER"],
            "opsJumpKey": cfg["DOSO_OPS_JUMP_KEY"],
            "opsJumpKeyReady": bool(cfg["DOSO_OPS_JUMP_KEY"] or jump_key_auto),
            "opsRobotUser": cfg["DOSO_OPS_ROBOT_USER"],
            "opsRobotKey": cfg["DOSO_OPS_ROBOT_KEY"],
            "opsRobotPasswordConfigured": bool(robot_pwd),
            "opsRobotPasswordPreview": mask_secret(robot_pwd),
            "ecsInstanceId": cfg["DOSO_ECS_INSTANCE_ID"],
            "ecsRegion": cfg["DOSO_ECS_REGION"],
            "ecsAkIdConfigured": bool(ecs_ak_id),
            "ecsAkIdPreview": mask_secret(ecs_ak_id),
            "ecsAkSecretConfigured": bool(ecs_ak_sec),
            "ecsAutoReady": bool(ecs_ak_id and ecs_ak_sec),
            "defaultOwnerEmail": cfg["DOSO_DEFAULT_OWNER_EMAIL"],
        }

    def update(self, payload: dict[str, Any]) -> dict[str, Any]:
        with self._lock:
            next_vals = dict(self._file_values)
            field_map = {
                "cloudUrl": "DOSO_OTA_CLOUD_URL",
                "ossBucket": "DOSO_OSS_BUCKET",
                "ossEndpoint": "DOSO_OSS_ENDPOINT",
                "defaultModel": "DOSO_DEFAULT_MODEL",
                "defaultHardwareRevision": "DOSO_DEFAULT_HW_REV",
                "signingKeyId": "DOSO_SIGNING_KEY_ID",
                "opsJumpHost": "DOSO_OPS_JUMP_HOST",
                "opsJumpPort": "DOSO_OPS_JUMP_PORT",
                "opsJumpUser": "DOSO_OPS_JUMP_USER",
                "opsJumpKey": "DOSO_OPS_JUMP_KEY",
                "opsRobotUser": "DOSO_OPS_ROBOT_USER",
                "opsRobotKey": "DOSO_OPS_ROBOT_KEY",
                "ecsInstanceId": "DOSO_ECS_INSTANCE_ID",
                "ecsRegion": "DOSO_ECS_REGION",
                "defaultOwnerEmail": "DOSO_DEFAULT_OWNER_EMAIL",
            }
            for json_key, env_key in field_map.items():
                if json_key in payload and payload[json_key] is not None:
                    val = str(payload[json_key]).strip()
                    if env_key == "DOSO_OTA_CLOUD_URL":
                        if not val.startswith(("http://", "https://")):
                            raise ops.OpsError("云端地址必须以 http:// 或 https:// 开头")
                        val = val.rstrip("/")
                    if val or env_key in ("DOSO_OPS_JUMP_KEY", "DOSO_OPS_ROBOT_KEY", "DOSO_DEFAULT_OWNER_EMAIL"):
                        next_vals[env_key] = val

            secret_map = {
                "apiToken": ("DOSO_RELEASE_API_TOKEN", "clearApiToken"),
                "ossAccessKeyId": ("DOSO_OSS_ACCESS_KEY_ID", "clearOssCredentials"),
                "ossAccessKeySecret": ("DOSO_OSS_ACCESS_KEY_SECRET", "clearOssCredentials"),
                "opsRobotPassword": ("DOSO_OPS_ROBOT_PASSWORD", "clearOpsRobotPassword"),
                "ecsAccessKeyId": ("DOSO_ECS_ACCESS_KEY_ID", "clearEcsCredentials"),
                "ecsAccessKeySecret": ("DOSO_ECS_ACCESS_KEY_SECRET", "clearEcsCredentials"),
            }
            for json_key, (env_key, clear_flag) in secret_map.items():
                if payload.get(clear_flag):
                    next_vals[env_key] = ""
                elif json_key in payload and payload[json_key] is not None:
                    val = str(payload[json_key]).strip()
                    if val:
                        next_vals[env_key] = val

            ops.atomic_write_bytes(self.path, serialize_env_file(next_vals))
            if os.name != "nt":
                try:
                    os.chmod(self.path, 0o600)
                except OSError:
                    pass
            self._file_values = next_vals
        return self.public_view()


class UploadStore:
    """Temporary local upload area for browser drag-and-drop artifacts."""

    def __init__(self) -> None:
        self._dir_ctx = tempfile.TemporaryDirectory(prefix="doso-ota-console-")
        self.root = Path(self._dir_ctx.name)
        self._lock = threading.Lock()
        self._items: dict[str, dict[str, Any]] = {}
        atexit.register(self.cleanup)

    def cleanup(self) -> None:
        try:
            self._dir_ctx.cleanup()
        except Exception:
            pass

    def sanitize_filename(self, raw_name: str) -> str:
        base = Path(raw_name.replace("\\", "/")).name
        cleaned = re.sub(r"[^A-Za-z0-9._-]+", "_", base).strip(".")
        return cleaned[:120] or "artifact.bin"

    def save_stream(self, stream: Any, content_length: int, filename: str) -> dict[str, Any]:
        if content_length <= 0:
            raise ops.OpsError("上传文件为空")
        if content_length > MAX_UPLOAD_BYTES:
            raise ops.OpsError(f"上传文件超过上限 ({MAX_UPLOAD_BYTES // (1024 * 1024)} MB)")
        safe_name = self.sanitize_filename(filename)
        upload_id = uuid.uuid4().hex
        dest = self.root / f"{upload_id}_{safe_name}"
        import hashlib

        digest = hashlib.sha256()
        remaining = content_length
        written = 0
        with dest.open("wb") as fh:
            while remaining > 0:
                chunk = stream.read(min(UPLOAD_CHUNK_BYTES, remaining))
                if not chunk:
                    break
                fh.write(chunk)
                digest.update(chunk)
                written += len(chunk)
                remaining -= len(chunk)
        if written != content_length:
            dest.unlink(missing_ok=True)
            raise ops.OpsError("上传中断，文件大小不完整")

        record = {
            "uploadId": upload_id,
            "filename": safe_name,
            "path": str(dest),
            "sizeBytes": written,
            "sha256": digest.hexdigest(),
            "createdAtMs": int(time.time() * 1000),
        }
        with self._lock:
            self._items[upload_id] = record
        return {
            "uploadId": upload_id,
            "filename": safe_name,
            "sizeBytes": written,
            "sha256": record["sha256"],
        }

    def resolve_record(self, upload_id: str) -> tuple[Path, str]:
        with self._lock:
            rec = self._items.get(upload_id)
        if not rec:
            raise ops.OpsError("上传文件已失效，请重新选择文件")
        path = Path(rec["path"])
        if not path.is_file():
            raise ops.OpsError("上传文件不存在，请重新选择文件")
        return path, str(rec.get("filename", path.name))

    def resolve(self, upload_id: str) -> Path:
        path, _ = self.resolve_record(upload_id)
        return path


class JobTracker:
    """In-memory tracker of jobs initiated from this console session."""

    def __init__(self, max_items: int = 30) -> None:
        self._lock = threading.Lock()
        self._max_items = max_items
        self._jobs: list[dict[str, Any]] = []

    def record(self, job: dict[str, Any]) -> None:
        job_id = str(job.get("jobId", ""))
        if not job_id:
            return
        with self._lock:
            self._jobs = [j for j in self._jobs if j.get("jobId") != job_id]
            self._jobs.insert(0, dict(job))
            self._jobs = self._jobs[: self._max_items]

    def list_items(self) -> list[dict[str, Any]]:
        with self._lock:
            return [dict(j) for j in self._jobs]


def raw_http_json(
    base_url: str,
    method: str,
    path: str,
    token: str | None = None,
    body: dict[str, Any] | None = None,
    timeout_s: int = 20,
) -> tuple[int, Any]:
    url = base_url.rstrip("/") + path
    headers = {"Accept": "application/json"}
    data: bytes | None = None
    if token:
        headers["Authorization"] = f"Bearer {token}"
    if body is not None:
        headers["Content-Type"] = "application/json"
        data = json.dumps(body).encode("utf-8")
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=timeout_s) as resp:
            raw = resp.read()
            parsed = json.loads(raw.decode("utf-8")) if raw else None
            return resp.status, parsed
    except urllib.error.HTTPError as err:
        raw = err.read()
        try:
            parsed = json.loads(raw.decode("utf-8")) if raw else {"error": f"http_{err.code}"}
        except ValueError:
            parsed = {"error": raw.decode("utf-8", errors="replace") or f"http_{err.code}"}
        return err.code, parsed
    except urllib.error.URLError as err:
        raise ops.OpsError(f"无法连接云端服务: {err.reason}") from err


def detect_local_artifacts() -> dict[str, list[dict[str, Any]]]:
    """Probe common local workspace build outputs that actually exist on disk."""
    apks: list[dict[str, Any]] = []
    robot_dirs: list[dict[str, Any]] = []

    workspace_parent = REPO_ROOT.parent
    apk_candidates = [
        ("qa", workspace_parent / "app-workspace" / "doso-app" / "android" / "app" / "build" / "outputs" / "apk" / "qa" / "app-qa.apk"),
        ("prod", workspace_parent / "app-workspace" / "doso-app" / "android" / "app" / "build" / "outputs" / "apk" / "release" / "app-release.apk"),
        ("qa", workspace_parent / "app-workspace" / "doso-app" / "android" / "app" / "build" / "outputs" / "apk" / "debug" / "app-debug.apk"),
    ]
    for channel_hint, candidate in apk_candidates:
        if candidate.is_file():
            st = candidate.stat()
            apks.append(
                {
                    "label": f"{candidate.name}",
                    "channelHint": channel_hint,
                    "path": str(candidate),
                    "sizeBytes": st.st_size,
                    "modifiedAtMs": int(st.st_mtime * 1000),
                }
            )

    dir_candidates = [
        ("brainstem_release", Path("D:/inovxio/brain/brainstem/target/aarch64-unknown-linux-gnu/release/doso-brainstem")),
    ]
    for kind_hint, candidate in dir_candidates:
        if candidate.is_file():
            st = candidate.stat()
            robot_dirs.append(
                {
                    "label": candidate.name,
                    "artifactType": kind_hint,
                    "path": str(candidate),
                    "sizeBytes": st.st_size,
                    "modifiedAtMs": int(st.st_mtime * 1000),
                }
            )

    return {"apks": apks, "robotFiles": robot_dirs}


def inspect_local_path(raw_path: str) -> dict[str, Any]:
    cleaned = raw_path.strip().strip('"').strip("'")
    if not cleaned:
        raise ops.OpsError("路径不能为空")
    p = Path(cleaned)
    if not p.exists():
        return {"exists": False, "path": cleaned}
    if p.is_dir():
        has_brainstem = (p / "bin" / "doso-brainstem").is_file()
        has_policy = (p / "policy.yaml").is_file()
        guessed = "brainstem_release" if has_brainstem else ("policy_package" if has_policy else "component_release")
        file_count = sum(1 for item in p.rglob("*") if item.is_file())
        return {
            "exists": True,
            "isDir": True,
            "isFile": False,
            "path": str(p.resolve()),
            "name": p.name,
            "fileCount": file_count,
            "hasBrainstemBin": has_brainstem,
            "hasPolicyYaml": has_policy,
            "guessedArtifactType": guessed,
        }
    st = p.stat()
    sha256, size_bytes = ops.sha256_file(p)
    return {
        "exists": True,
        "isDir": False,
        "isFile": True,
        "path": str(p.resolve()),
        "name": p.name,
        "sizeBytes": size_bytes,
        "sha256": sha256,
        "modifiedAtMs": int(st.st_mtime * 1000),
    }


def open_native_picker(kind: str) -> dict[str, Any]:
    """Open a local OS file/directory dialog when running on a desktop machine."""
    try:
        import tkinter as tk
        from tkinter import filedialog
    except Exception:
        return {"supported": False, "path": None}

    try:
        root = tk.Tk()
        root.withdraw()
        try:
            root.attributes("-topmost", True)
        except Exception:
            pass
        if kind == "dir":
            chosen = filedialog.askdirectory(title="选择打包目录")
        elif kind == "apk":
            chosen = filedialog.askopenfilename(
                title="选择 Android APK",
                filetypes=[("Android APK", "*.apk"), ("所有文件", "*.*")],
            )
        elif kind == "archive":
            chosen = filedialog.askopenfilename(
                title="选择固件包 (.tar.gz)",
                filetypes=[("压缩包", "*.tar.gz *.tgz"), ("所有文件", "*.*")],
            )
        else:
            chosen = filedialog.askopenfilename(title="选择文件")
        root.destroy()
        return {"supported": True, "path": chosen or None}
    except Exception:
        return {"supported": False, "path": None}


def build_rollout_stages(preset: str, payload: dict[str, Any]) -> list[dict[str, Any]]:
    custom = payload.get("stages")
    if isinstance(custom, list) and custom:
        return custom
    canary_max = int(payload.get("canaryMaxDevices", 1) or 1)
    canary_bake = int(payload.get("canaryBakeSeconds", 1800) or 0)
    prod_bake = int(payload.get("prodBakeSeconds", 3600) or 0)
    if preset == "fast":
        return [
            {
                "name": "canary",
                "ring": "canary",
                "percent": 100,
                "maxDevices": canary_max,
                "bakeSeconds": canary_bake,
                "autoPromote": False,
            },
            {
                "name": "prod-100",
                "ring": "prod",
                "percent": 100,
                "bakeSeconds": 0,
                "autoPromote": False,
            },
        ]
    return [
        {
            "name": "canary",
            "ring": "canary",
            "percent": 100,
            "maxDevices": canary_max,
            "bakeSeconds": canary_bake,
            "autoPromote": False,
        },
        {
            "name": "prod-10",
            "ring": "prod",
            "percent": 10,
            "bakeSeconds": prod_bake,
            "autoPromote": False,
        },
        {
            "name": "prod-100",
            "ring": "prod",
            "percent": 100,
            "bakeSeconds": 0,
            "autoPromote": False,
        },
    ]


class ConsoleContext:
    def __init__(self, config_store: ConfigStore, allow_remote: bool = False) -> None:
        self.config_store = config_store
        self.uploads = UploadStore()
        self.jobs = JobTracker()
        self.allow_remote = allow_remote

    def _require_token(self, cfg: dict[str, str]) -> str:
        token = cfg.get("DOSO_RELEASE_API_TOKEN", "").strip()
        if not token:
            raise ops.OpsError("请先在「云端配置」填入发布接口令牌")
        return token

    def check_health(self) -> dict[str, Any]:
        cfg = self.config_store.effective()
        base_url = cfg["DOSO_OTA_CLOUD_URL"]
        token = cfg["DOSO_RELEASE_API_TOKEN"]
        res: dict[str, Any] = {
            "cloudUrl": base_url,
            "cloudReachable": False,
            "cloudService": None,
            "cloudVersion": None,
            "tokenConfigured": bool(token),
            "ossConfigured": bool(cfg["DOSO_OSS_ACCESS_KEY_ID"] and cfg["DOSO_OSS_ACCESS_KEY_SECRET"]),
            "authOk": False,
            "releasesApiReady": False,
            "error": None,
        }
        try:
            st, body = raw_http_json(base_url, "GET", "/health", timeout_s=10)
            if st == 200 and isinstance(body, dict):
                res["cloudReachable"] = True
                res["cloudService"] = body.get("service")
                res["cloudVersion"] = body.get("version")
            else:
                res["error"] = f"健康检查异常 (HTTP {st})"
                return res
        except ops.OpsError as exc:
            res["error"] = str(exc)
            return res

        if not token:
            res["error"] = "未配置访问令牌"
            return res

        def _probe(path: str) -> tuple[int | None, str | None]:
            try:
                st_p, _ = raw_http_json(base_url, "GET", path, token=token, timeout_s=10)
                return st_p, None
            except ops.OpsError as exc:
                return None, str(exc)

        with ThreadPoolExecutor(max_workers=2) as pool:
            f_dev = pool.submit(_probe, "/v1/devices/status")
            f_rel = pool.submit(_probe, "/v1/releases")
            st_dev, err_dev = f_dev.result()
            st_rel, _ = f_rel.result()

        if err_dev:
            res["error"] = err_dev
            return res
        if st_dev == 200:
            res["authOk"] = True
        elif st_dev == 401:
            res["error"] = "访问令牌无效"
            return res
        else:
            res["error"] = f"设备接口返回 HTTP {st_dev}"
        res["releasesApiReady"] = st_rel == 200
        return res

    def get_overview(self) -> dict[str, Any]:
        cfg = self.config_store.effective()
        base_url = cfg["DOSO_OTA_CLOUD_URL"]
        token = cfg["DOSO_RELEASE_API_TOKEN"]
        out: dict[str, Any] = {
            "config": self.config_store.public_view(),
            "cloudReachable": False,
            "authOk": False,
            "releasesApiReady": False,
            "devices": [],
            "registeredDevices": [],
            "fleet": [],
            "fleetSummary": {},
            "tunnelPorts": [],
            "releases": [],
            "rollouts": [],
            "android": {ch: None for ch in ANDROID_CHANNELS},
            "trackedJobs": [],
            "jobs": [],
            "warnings": [],
        }

        def _safe_get(path: str, use_token: bool = False, timeout_s: int = 12) -> tuple[int | None, Any, str | None]:
            try:
                st, body = raw_http_json(
                    base_url,
                    "GET",
                    path,
                    token=token if use_token else None,
                    timeout_s=timeout_s,
                )
                return st, body, None
            except ops.OpsError as exc:
                return None, None, str(exc)

        tracked_list = self.jobs.list_items()
        disable_swas = os.environ.get("DOSO_OTA_CONSOLE_DISABLE_SWAS", "").strip() == "1"
        with ThreadPoolExecutor(max_workers=12) as pool:
            f_health = pool.submit(_safe_get, "/health", False, 8)
            f_android = {
                ch: pool.submit(
                    _safe_get,
                    f"/v1/apps/android/latest?channel={urllib.parse.quote(ch)}&version_code=0",
                    False,
                    8,
                )
                for ch in ANDROID_CHANNELS
            }
            f_dev = pool.submit(_safe_get, "/v1/devices/status", True, 12) if token else None
            f_reg = pool.submit(_safe_get, "/v1/devices", True, 12) if token else None
            f_rel = pool.submit(_safe_get, "/v1/releases", True, 12) if token else None
            f_ro = pool.submit(_safe_get, "/v1/rollouts", True, 12) if token else None
            f_jobs_all = pool.submit(_safe_get, "/v1/jobs?limit=200", True, 12) if token else None
            f_swas = pool.submit(fleet_ops.fetch_swas_fleet_snapshot, cfg) if not disable_swas else None
            f_jobs = [
                (
                    tracked,
                    pool.submit(
                        _safe_get,
                        f"/v1/devices/{urllib.parse.quote(str(tracked['deviceId']))}/firmware/jobs/{urllib.parse.quote(str(tracked['jobId']))}",
                        True,
                        8,
                    ),
                )
                for tracked in tracked_list
                if token and tracked.get("deviceId") and tracked.get("jobId")
            ]

            st_h, _, err_h = f_health.result()
            out["cloudReachable"] = st_h == 200
            if err_h:
                out["warnings"].append(err_h)
                swas_snap = f_swas.result() if f_swas is not None else None
                unified = fleet_ops.build_unified_fleet(
                    registered_devices=[], ota_statuses=[], jobs=[], releases=[], swas_snapshot=swas_snap
                )
                out["fleet"] = unified["fleet"]
                out["fleetSummary"] = unified["summary"]
                out["tunnelPorts"] = unified["tunnelPorts"]
                return out

            for ch, fut in f_android.items():
                st_a, body_a, _ = fut.result()
                out["android"][ch] = body_a if st_a == 200 else None

            if not token:
                swas_snap = f_swas.result() if f_swas is not None else None
                unified = fleet_ops.build_unified_fleet(
                    registered_devices=[], ota_statuses=[], jobs=[], releases=[], swas_snapshot=swas_snap
                )
                out["fleet"] = unified["fleet"]
                out["fleetSummary"] = unified["summary"]
                out["tunnelPorts"] = unified["tunnelPorts"]
                return out

            st_d, body_d, err_d = f_dev.result()  # type: ignore[union-attr]
            if err_d:
                out["warnings"].append(err_d)
            elif st_d == 200 and isinstance(body_d, list):
                out["authOk"] = True
                out["devices"] = body_d
            elif st_d == 401:
                out["warnings"].append("访问令牌无效")
                return out
            else:
                out["warnings"].append(f"读取设备状态失败 (HTTP {st_d})")

            st_reg, body_reg, _ = f_reg.result()  # type: ignore[union-attr]
            if st_reg == 200 and isinstance(body_reg, list):
                out["registeredDevices"] = body_reg

            st_r, body_r, err_r = f_rel.result()  # type: ignore[union-attr]
            if err_r:
                out["warnings"].append(err_r)
            elif st_r == 200 and isinstance(body_r, list):
                out["releasesApiReady"] = True
                out["releases"] = body_r
            elif st_r == 404:
                out["releasesApiReady"] = False
                if out["devices"]:
                    first_dev = out["devices"][0].get("deviceId")
                    if first_dev:
                        st_dr, body_dr, _ = _safe_get(
                            f"/v1/devices/{urllib.parse.quote(str(first_dev))}/firmware/releases",
                            True,
                            10,
                        )
                        if st_dr == 200 and isinstance(body_dr, list):
                            out["releases"] = body_dr
            else:
                out["warnings"].append(f"读取版本库失败 (HTTP {st_r})")

            st_ro, body_ro, _ = f_ro.result()  # type: ignore[union-attr]
            if st_ro == 200 and isinstance(body_ro, list):
                out["rollouts"] = body_ro

            cloud_jobs: list[dict[str, Any]] = []
            if f_jobs_all is not None:
                st_ja, body_ja, _ = f_jobs_all.result()
                if st_ja == 200 and isinstance(body_ja, list):
                    cloud_jobs = body_ja

            refreshed_jobs: list[dict[str, Any]] = []
            for tracked, fut in f_jobs:
                st_j, body_j, _ = fut.result()
                if st_j == 200 and isinstance(body_j, dict):
                    self.jobs.record(body_j)
                    refreshed_jobs.append(body_j)
                else:
                    refreshed_jobs.append(tracked)

            # Merge persisted cloud jobs + session-tracked jobs by jobId
            merged_jobs_map: dict[str, dict[str, Any]] = {}
            for item in cloud_jobs:
                jid = str(item.get("jobId") or "")
                if jid:
                    merged_jobs_map[jid] = item
            for item in refreshed_jobs:
                jid = str(item.get("jobId") or "")
                if jid:
                    merged_jobs_map[jid] = {**merged_jobs_map.get(jid, {}), **item}
            merged_jobs = sorted(
                merged_jobs_map.values(),
                key=lambda x: max(int(x.get("updatedAtMs") or 0), int(x.get("createdAtMs") or 0)),
                reverse=True,
            )
            out["jobs"] = merged_jobs
            out["trackedJobs"] = [
                j for j in merged_jobs if str(j.get("phase") or "") in fleet_ops.ACTIVE_JOB_PHASES
            ][:20] or refreshed_jobs

            swas_snap = f_swas.result() if f_swas is not None else None
            unified = fleet_ops.build_unified_fleet(
                registered_devices=out["registeredDevices"],
                ota_statuses=out["devices"],
                jobs=merged_jobs,
                releases=out["releases"],
                swas_snapshot=swas_snap,
            )
            out["fleet"] = unified["fleet"]
            out["fleetSummary"] = unified["summary"]
            out["tunnelPorts"] = unified["tunnelPorts"]
        return out

    def publish_robot_release(self, payload: dict[str, Any]) -> dict[str, Any]:
        cfg = self.config_store.effective()
        artifact_type = str(payload.get("artifactType", "brainstem_release")).strip()
        if artifact_type not in ops.ALLOWED_PACKAGE_TYPES:
            raise ops.OpsError(f"不支持的版本类型: {artifact_type}")

        release_id = ops.validate_release_id(str(payload.get("releaseId", "")))
        version = ops.validate_semver(str(payload.get("version", "")))
        sequence = int(payload.get("sequence", 0))
        if sequence <= 0:
            raise ops.OpsError("版本序号必须大于 0")
        channel = str(payload.get("channel", "qa")).strip()
        if channel not in ops.ALLOWED_CHANNELS:
            raise ops.OpsError(f"不支持的发布渠道: {channel}")

        policy_slot = str(payload.get("policySlot", "") or "").strip() or None
        component = str(payload.get("component", "") or "").strip() or None
        source_mode = str(payload.get("sourceMode", "upload")).strip()
        dry_run = bool(payload.get("dryRun", False))
        skip_upload = bool(payload.get("skipUpload", False))
        overwrite = bool(payload.get("overwrite", False))
        dispatch_mode = str(payload.get("dispatchMode", "none")).strip()
        device_id = str(payload.get("deviceId", "") or "").strip()
        rollout_id = str(payload.get("rolloutId", "") or "").strip()

        if dispatch_mode == "device" and not device_id:
            raise ops.OpsError("请选择目标机器狗")
        if dispatch_mode == "rollout" and not rollout_id:
            rollout_id = f"ro-{release_id}"

        object_key = str(payload.get("objectKey", "") or "").strip().lstrip("/")
        if not object_key:
            object_key = ops.default_robot_object_key(
                artifact_type,
                channel,
                release_id,
                policy_slot=policy_slot,
                component=component,
            )

        steps: list[dict[str, Any]] = []
        tmp_ctx = tempfile.TemporaryDirectory(prefix="doso-ota-pkg-")
        try:
            package_meta: dict[str, Any] | None = None
            if source_mode == "local_dir":
                local_path = Path(str(payload.get("localPath", "")).strip().strip('"').strip("'"))
                if not local_path.is_dir():
                    raise ops.OpsError(f"打包目录不存在: {local_path}")
                tar_path = Path(tmp_ctx.name) / f"{release_id}.tar.gz"
                package_meta = ops.build_package(local_path, tar_path, artifact_type, component)
                artifact_path = tar_path
                steps.append(
                    {
                        "step": "package",
                        "status": "ok",
                        "detail": f"已打包 {package_meta['fileCount']} 个文件 ({package_meta['sizeBytes']} B)",
                    }
                )
            elif source_mode == "local_file":
                local_path = Path(str(payload.get("localPath", "")).strip().strip('"').strip("'"))
                if not local_path.is_file():
                    raise ops.OpsError(f"本地文件不存在: {local_path}")
                if local_path.name == "doso-brainstem" and artifact_type == "brainstem_release":
                    stage_dir = Path(tmp_ctx.name) / "stage"
                    bin_dir = stage_dir / "bin"
                    bin_dir.mkdir(parents=True, exist_ok=True)
                    import shutil

                    shutil.copy2(local_path, bin_dir / "doso-brainstem")
                    tar_path = Path(tmp_ctx.name) / f"{release_id}.tar.gz"
                    package_meta = ops.build_package(stage_dir, tar_path, artifact_type, component)
                    artifact_path = tar_path
                    steps.append(
                        {
                            "step": "package",
                            "status": "ok",
                            "detail": f"已自动封装固件包 ({package_meta['sizeBytes']} B)",
                        }
                    )
                else:
                    artifact_path = local_path
                    steps.append({"step": "package", "status": "skipped", "detail": "使用已有压缩包"})
            elif source_mode == "upload":
                upload_id = str(payload.get("uploadId", "")).strip()
                uploaded_path, orig_name = self.uploads.resolve_record(upload_id)
                if orig_name == "doso-brainstem" and artifact_type == "brainstem_release":
                    stage_dir = Path(tmp_ctx.name) / "stage"
                    bin_dir = stage_dir / "bin"
                    bin_dir.mkdir(parents=True, exist_ok=True)
                    import shutil

                    shutil.copy2(uploaded_path, bin_dir / "doso-brainstem")
                    tar_path = Path(tmp_ctx.name) / f"{release_id}.tar.gz"
                    package_meta = ops.build_package(stage_dir, tar_path, artifact_type, component)
                    artifact_path = tar_path
                    steps.append(
                        {
                            "step": "package",
                            "status": "ok",
                            "detail": f"已自动封装固件包 ({package_meta['sizeBytes']} B)",
                        }
                    )
                else:
                    artifact_path = uploaded_path
                    steps.append({"step": "package", "status": "skipped", "detail": "使用已上传压缩包"})
            else:
                raise ops.OpsError(f"不支持的文件来源: {source_mode}")

            tpl_args = argparse.Namespace(
                release_id=release_id,
                version=version,
                sequence=sequence,
                channel=channel,
                artifact_type=artifact_type,
                object_key=object_key,
                bundle_spec=None,
                delta_object_key=str(payload.get("deltaObjectKey", "") or "").strip() or None,
                artifact=artifact_path,
                size_bytes=None,
                sha256=None,
                model=str(payload.get("model") or cfg["DOSO_DEFAULT_MODEL"]).strip(),
                hardware_revision=str(payload.get("hardwareRevision") or cfg["DOSO_DEFAULT_HW_REV"]).strip(),
                min_bootloader=str(payload.get("minBootloader") or "1.0.0").strip(),
                min_brainstem=str(payload.get("minBrainstem") or "0.0.1").strip(),
                min_battery=int(payload.get("minBattery", 20)),
                health_timeout_ms=int(payload.get("healthTimeoutMs", 30000)),
                manifest_ttl_seconds=int(payload.get("manifestTtlSeconds", 1800)),
                key_id=str(payload.get("keyId") or cfg["DOSO_SIGNING_KEY_ID"]).strip(),
                policy_slot=policy_slot,
                shadow_frames=int(payload.get("shadowFrames", 50)),
                no_activate=not bool(payload.get("activate", True)),
                component=component,
                systemd_unit=str(payload.get("systemdUnit", "") or "").strip() or None,
                health_url=str(payload.get("healthUrl", "") or "").strip() or None,
                requires=payload.get("requires") or None,
            )
            template = ops.render_template(tpl_args)

            upload_result: dict[str, Any] | None = None
            if not skip_upload:
                upload_result = ops.upload_to_oss(
                    file_path=artifact_path,
                    object_key=object_key,
                    bucket=cfg["DOSO_OSS_BUCKET"],
                    endpoint=cfg["DOSO_OSS_ENDPOINT"],
                    content_type="application/gzip",
                    overwrite=overwrite,
                    dry_run=dry_run,
                    ak_id=cfg["DOSO_OSS_ACCESS_KEY_ID"],
                    ak_secret=cfg["DOSO_OSS_ACCESS_KEY_SECRET"],
                )
                steps.append(
                    {
                        "step": "upload",
                        "status": "dry_run" if dry_run else "ok",
                        "detail": object_key,
                    }
                )
            else:
                steps.append({"step": "upload", "status": "skipped", "detail": "跳过对象上传"})

            if dry_run:
                return {
                    "dryRun": True,
                    "releaseId": release_id,
                    "objectKey": object_key,
                    "steps": steps,
                    "package": package_meta,
                    "upload": upload_result,
                    "template": template,
                }

            token = self._require_token(cfg)
            base_url = cfg["DOSO_OTA_CLOUD_URL"]
            reg_status, registered = ops.api_call(base_url, "POST", "/v1/releases", token, body=template)
            steps.append(
                {
                    "step": "register",
                    "status": "ok",
                    "detail": f"云端登记成功 (HTTP {reg_status})",
                }
            )

            result: dict[str, Any] = {
                "dryRun": False,
                "releaseId": release_id,
                "objectKey": object_key,
                "steps": steps,
                "package": package_meta,
                "upload": upload_result,
                "registerStatus": reg_status,
                "release": registered,
            }

            if dispatch_mode == "device":
                dev_clean = ops.validate_device_id(device_id)
                idem = f"console-{release_id}-{dev_clean}-{int(time.time())}"
                _, job = ops.api_call(
                    base_url,
                    "POST",
                    f"/v1/devices/{urllib.parse.quote(dev_clean)}/firmware/jobs",
                    token,
                    body={"releaseId": release_id, "idempotencyKey": idem},
                )
                self.jobs.record(job)
                result["job"] = job
                steps.append({"step": "dispatch", "status": "ok", "detail": f"已下发至 {dev_clean}"})
            elif dispatch_mode == "rollout":
                ro_payload: dict[str, Any] = {
                    "rolloutId": rollout_id,
                    "releaseId": release_id,
                    "targetModel": tpl_args.model,
                    "targetHardwareRevision": tpl_args.hardware_revision,
                    "stages": build_rollout_stages(str(payload.get("rolloutPreset", "standard")), payload),
                    "circuitBreaker": {
                        "maxFailureCount": int(payload.get("maxFailureCount", 1)),
                        "maxFailureRate": float(payload.get("maxFailureRate", 0.05)),
                    },
                }
                _, rollout = ops.api_call(base_url, "POST", "/v1/rollouts", token, body=ro_payload)
                result["rollout"] = rollout
                steps.append({"step": "dispatch", "status": "ok", "detail": f"已创建灰度 {rollout_id}"})

            return result
        finally:
            tmp_ctx.cleanup()

    def publish_android_release(self, payload: dict[str, Any]) -> dict[str, Any]:
        cfg = self.config_store.effective()
        source_mode = str(payload.get("sourceMode", "upload")).strip()
        channel = str(payload.get("channel", "prod")).strip()
        if not channel:
            raise ops.OpsError("渠道不能为空")
        version_name = ops.validate_semver(str(payload.get("versionName", "")), "versionName")
        version_code = int(payload.get("versionCode", 0))
        if version_code <= 0:
            raise ops.OpsError("版本码必须大于 0")
        min_supported_code = int(payload.get("minSupportedCode", 0) or 0)
        mandatory = bool(payload.get("mandatory", False))
        changelog = str(payload.get("changelog", "") or "")
        overwrite = bool(payload.get("overwrite", False))
        skip_upload = bool(payload.get("skipUpload", False))
        skip_verify = bool(payload.get("skipVerify", False))
        dry_run = bool(payload.get("dryRun", False))

        if source_mode == "upload":
            apk_path = self.uploads.resolve(str(payload.get("uploadId", "")).strip())
        elif source_mode == "local_file":
            apk_path = Path(str(payload.get("localPath", "")).strip().strip('"').strip("'"))
            if not apk_path.is_file():
                raise ops.OpsError(f"APK 文件不存在: {apk_path}")
        else:
            raise ops.OpsError(f"不支持的文件来源: {source_mode}")

        apk_sha, apk_size = ops.sha256_file(apk_path)
        object_key = str(payload.get("objectKey", "") or "").strip().lstrip("/")
        if not object_key:
            object_key = ops.default_android_object_key(channel, version_name, version_code)

        steps: list[dict[str, Any]] = []
        upload_result: dict[str, Any] | None = None
        if not skip_upload:
            upload_result = ops.upload_to_oss(
                file_path=apk_path,
                object_key=object_key,
                bucket=cfg["DOSO_OSS_BUCKET"],
                endpoint=cfg["DOSO_OSS_ENDPOINT"],
                content_type="application/vnd.android.package-archive",
                overwrite=overwrite,
                dry_run=dry_run,
                ak_id=cfg["DOSO_OSS_ACCESS_KEY_ID"],
                ak_secret=cfg["DOSO_OSS_ACCESS_KEY_SECRET"],
            )
            steps.append(
                {
                    "step": "upload",
                    "status": "dry_run" if dry_run else "ok",
                    "detail": f"{object_key} ({apk_size} B)",
                }
            )
        else:
            steps.append({"step": "upload", "status": "skipped", "detail": "跳过对象上传"})

        publish_payload = {
            "channel": channel,
            "versionName": version_name,
            "versionCode": version_code,
            "minSupportedCode": min_supported_code,
            "mandatory": mandatory,
            "changelog": changelog,
            "apkSha256": apk_sha,
            "apkSizeBytes": apk_size,
            "objectKey": object_key,
        }

        if dry_run:
            return {
                "dryRun": True,
                "steps": steps,
                "upload": upload_result,
                "publishPayload": publish_payload,
            }

        token = self._require_token(cfg)
        base_url = cfg["DOSO_OTA_CLOUD_URL"]
        _, published = ops.api_call(base_url, "POST", "/v1/apps/android/releases", token, body=publish_payload)
        steps.append({"step": "publish", "status": "ok", "detail": f"已发布 {version_name} ({version_code})"})

        verified: dict[str, Any] | None = None
        if not skip_verify:
            probe_code = max(0, version_code - 1)
            _, latest = ops.api_call(
                base_url,
                "GET",
                f"/v1/apps/android/latest?channel={urllib.parse.quote(channel)}&version_code={probe_code}",
                token=None,
            )
            if (
                not isinstance(latest, dict)
                or latest.get("versionCode") != version_code
                or str(latest.get("apkSha256", "")).lower() != apk_sha.lower()
                or not latest.get("updateAvailable")
            ):
                raise ops.OpsError("云端公开更新接口校验不一致")
            verified = {
                "updateAvailable": latest.get("updateAvailable"),
                "versionCode": latest.get("versionCode"),
                "apkSha256": latest.get("apkSha256"),
            }
            steps.append({"step": "verify", "status": "ok", "detail": "更新检测接口校验通过"})

        return {
            "dryRun": False,
            "steps": steps,
            "upload": upload_result,
            "release": published,
            "verifiedLatest": verified,
        }


def make_handler(ctx: ConsoleContext) -> type[BaseHTTPRequestHandler]:
    class OtaConsoleHandler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, format: str, *args: Any) -> None:
            if os.environ.get("DOSO_OTA_CONSOLE_QUIET") == "1":
                return
            sys.stderr.write(f"[ota-console] {self.address_string()} - {format % args}\n")

        def _send_security_headers(self) -> None:
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.send_header("X-Frame-Options", "DENY")
            self.send_header("Referrer-Policy", "no-referrer")

        def _send_json(self, status: int, data: Any) -> None:
            raw = json.dumps(data, ensure_ascii=False).encode("utf-8")
            try:
                self.send_response(status)
                self._send_security_headers()
                self.send_header("Content-Type", "application/json; charset=utf-8")
                self.send_header("Content-Length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)
            except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                pass

        def _send_bytes(self, status: int, content_type: str, data: bytes) -> None:
            try:
                self.send_response(status)
                self._send_security_headers()
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
            except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                pass

        def _check_local_guard(self, is_mutation: bool) -> bool:
            if ctx.allow_remote:
                return True
            host_hdr = (self.headers.get("Host") or "").strip().lower()
            host_only = host_hdr.split(":")[0] if not host_hdr.startswith("[") else host_hdr.split("]")[0] + "]"
            if host_only not in ("127.0.0.1", "localhost", "[::1]"):
                self._send_json(HTTPStatus.FORBIDDEN, {"error": "forbidden_host"})
                return False
            if is_mutation:
                origin = (self.headers.get("Origin") or "").strip()
                if origin:
                    parsed = urllib.parse.urlparse(origin)
                    if parsed.hostname not in ("127.0.0.1", "localhost", "::1"):
                        self._send_json(HTTPStatus.FORBIDDEN, {"error": "forbidden_origin"})
                        return False
                if self.headers.get("X-DOSO-Console") != "1":
                    self._send_json(HTTPStatus.FORBIDDEN, {"error": "missing_console_header"})
                    return False
            return True

        def _read_json_body(self) -> dict[str, Any]:
            length_raw = self.headers.get("Content-Length", "0").strip()
            try:
                length = int(length_raw)
            except ValueError as exc:
                raise ops.OpsError("无效的请求长度") from exc
            if length < 0 or length > MAX_JSON_BODY_BYTES:
                raise ops.OpsError("请求体超出限制")
            if length == 0:
                return {}
            raw = self.rfile.read(length)
            try:
                parsed = json.loads(raw.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError) as exc:
                raise ops.OpsError("请求体不是合法 JSON") from exc
            if not isinstance(parsed, dict):
                raise ops.OpsError("请求体必须是 JSON 对象")
            return parsed

        def _serve_static(self, req_path: str) -> None:
            rel = "index.html" if req_path in ("", "/") else req_path.lstrip("/")
            if ".." in rel.split("/") or "\\" in rel:
                self._send_json(HTTPStatus.FORBIDDEN, {"error": "invalid_path"})
                return
            target = (WEB_DIR / rel).resolve()
            if WEB_DIR.resolve() not in target.parents and target != WEB_DIR.resolve():
                self._send_json(HTTPStatus.FORBIDDEN, {"error": "invalid_path"})
                return
            if not target.is_file():
                self._send_json(HTTPStatus.NOT_FOUND, {"error": "not_found"})
                return
            mime, _ = mimetypes.guess_type(str(target))
            content_type = mime or "application/octet-stream"
            if content_type.startswith(("text/", "application/javascript", "application/json")):
                content_type += "; charset=utf-8"
            self._send_bytes(HTTPStatus.OK, content_type, target.read_bytes())

        def do_GET(self) -> None:
            if not self._check_local_guard(is_mutation=False):
                return
            parsed = urllib.parse.urlparse(self.path)
            path = parsed.path
            try:
                if path == "/api/config":
                    self._send_json(HTTPStatus.OK, ctx.config_store.public_view())
                    return
                if path == "/api/health":
                    self._send_json(HTTPStatus.OK, ctx.check_health())
                    return
                if path == "/api/overview":
                    self._send_json(HTTPStatus.OK, ctx.get_overview())
                    return
                if path == "/api/local-artifacts":
                    self._send_json(HTTPStatus.OK, detect_local_artifacts())
                    return
                if path == "/api/ops/tunnels":
                    cfg = ctx.config_store.effective()
                    self._send_json(HTTPStatus.OK, robot_ops.discover_tunnels(cfg))
                    return
                if path == "/api/accounts":
                    cfg = ctx.config_store.effective()
                    self._send_json(HTTPStatus.OK, fleet_ops.list_accounts(cfg))
                    return

                m_dev_rel = re.fullmatch(r"/api/devices/([^/]+)/releases", path)
                if m_dev_rel:
                    cfg = ctx.config_store.effective()
                    token = ctx._require_token(cfg)
                    dev_id = ops.validate_device_id(urllib.parse.unquote(m_dev_rel.group(1)))
                    _, data = ops.api_call(
                        cfg["DOSO_OTA_CLOUD_URL"],
                        "GET",
                        f"/v1/devices/{urllib.parse.quote(dev_id)}/firmware/releases",
                        token,
                    )
                    self._send_json(HTTPStatus.OK, data)
                    return

                m_job = re.fullmatch(r"/api/jobs/([^/]+)/([^/]+)", path)
                if m_job:
                    cfg = ctx.config_store.effective()
                    token = ctx._require_token(cfg)
                    dev_id = ops.validate_device_id(urllib.parse.unquote(m_job.group(1)))
                    job_id = urllib.parse.unquote(m_job.group(2))
                    _, data = ops.api_call(
                        cfg["DOSO_OTA_CLOUD_URL"],
                        "GET",
                        f"/v1/devices/{urllib.parse.quote(dev_id)}/firmware/jobs/{urllib.parse.quote(job_id)}",
                        token,
                    )
                    if isinstance(data, dict):
                        ctx.jobs.record(data)
                    self._send_json(HTTPStatus.OK, data)
                    return

                if path.startswith("/api/"):
                    self._send_json(HTTPStatus.NOT_FOUND, {"error": "unknown_api"})
                    return

                self._serve_static(path)
            except (ops.OpsError, robot_ops.RobotOpsError, provision_ops.ProvisionError, fleet_ops.FleetOpsError) as exc:
                self._send_json(HTTPStatus.BAD_REQUEST, {"error": str(exc)})
            except Exception as exc:
                self._send_json(HTTPStatus.INTERNAL_SERVER_ERROR, {"error": str(exc)})

        def do_POST(self) -> None:
            if not self._check_local_guard(is_mutation=True):
                return
            parsed = urllib.parse.urlparse(self.path)
            path = parsed.path
            try:
                if path == "/api/uploads":
                    raw_len = self.headers.get("Content-Length", "0").strip()
                    length = int(raw_len)
                    raw_name = urllib.parse.unquote(self.headers.get("X-Upload-Filename", "artifact.bin"))
                    rec = ctx.uploads.save_stream(self.rfile, length, raw_name)
                    self._send_json(HTTPStatus.CREATED, rec)
                    return

                body = self._read_json_body()

                if path == "/api/config":
                    updated = ctx.config_store.update(body)
                    self._send_json(HTTPStatus.OK, updated)
                    return

                if path == "/api/inspect-path":
                    info = inspect_local_path(str(body.get("path", "")))
                    self._send_json(HTTPStatus.OK, info)
                    return

                if path == "/api/pick-path":
                    picked = open_native_picker(str(body.get("kind", "file")))
                    self._send_json(HTTPStatus.OK, picked)
                    return

                if path == "/api/releases/publish":
                    res = ctx.publish_robot_release(body)
                    self._send_json(HTTPStatus.OK, res)
                    return

                if path == "/api/releases/bundle":
                    cfg = ctx.config_store.effective()
                    token = ctx._require_token(cfg)
                    release_id = ops.validate_release_id(str(body.get("releaseId", "")))
                    version = ops.validate_semver(str(body.get("version", "")))
                    sequence = int(body.get("sequence", 0))
                    if sequence <= 0:
                        raise ops.OpsError("版本序号必须大于 0")
                    channel = str(body.get("channel", "stable")).strip()
                    src_ids = [str(s).strip() for s in (body.get("sourceReleaseIds") or []) if str(s).strip()]
                    if not src_ids:
                        raise ops.OpsError("请至少勾选 1 个组件版本")
                    bundle_payload = {
                        "releaseId": release_id,
                        "version": version,
                        "sequence": sequence,
                        "channel": channel,
                        "sourceReleaseIds": src_ids,
                        "minimumBootloaderVersion": str(body.get("minBootloader") or "1.0.0").strip(),
                        "minimumBrainstemVersion": str(body.get("minBrainstem") or "0.0.1").strip(),
                        "minimumBatteryPercent": int(body.get("minBattery", 20)),
                        "healthTimeoutMs": int(body.get("healthTimeoutMs", 30000)),
                    }
                    st_b, rel = ops.api_call(
                        cfg["DOSO_OTA_CLOUD_URL"],
                        "POST",
                        "/v1/releases/bundle",
                        token,
                        body=bundle_payload,
                    )
                    self._send_json(HTTPStatus.CREATED if st_b == 201 else HTTPStatus.OK, rel)
                    return

                if path == "/api/devices/upsert":
                    cfg = ctx.config_store.effective()
                    token = ctx._require_token(cfg)
                    raw_id = str(body.get("deviceId") or "").strip()
                    dev_id = ops.validate_device_id(raw_id) if raw_id else str(uuid.uuid4())
                    tags_raw = body.get("tags", [])
                    if isinstance(tags_raw, str):
                        tags = [t.strip() for t in re.split(r"[,，\s]+", tags_raw) if t.strip()]
                    elif isinstance(tags_raw, list):
                        tags = [str(t).strip() for t in tags_raw if str(t).strip()]
                    else:
                        tags = []
                    dev_payload = {
                        "model": str(body.get("model") or cfg["DOSO_DEFAULT_MODEL"]).strip(),
                        "hardwareRevision": str(body.get("hardwareRevision") or cfg["DOSO_DEFAULT_HW_REV"]).strip(),
                        "enabled": bool(body.get("enabled", True)),
                        "ring": str(body.get("ring") or "prod").strip(),
                        "tags": tags,
                    }
                    _, saved = ops.api_call(
                        cfg["DOSO_OTA_CLOUD_URL"],
                        "PUT",
                        f"/v1/devices/{urllib.parse.quote(dev_id)}",
                        token,
                        body=dev_payload,
                    )
                    self._send_json(HTTPStatus.OK, saved)
                    return

                m_dev_del = re.fullmatch(r"/api/devices/([^/]+)/delete", path)
                if m_dev_del:
                    cfg = ctx.config_store.effective()
                    token = ctx._require_token(cfg)
                    dev_id = ops.validate_device_id(urllib.parse.unquote(m_dev_del.group(1)))
                    _, deleted = ops.api_call(
                        cfg["DOSO_OTA_CLOUD_URL"],
                        "DELETE",
                        f"/v1/devices/{urllib.parse.quote(dev_id)}",
                        token,
                    )
                    self._send_json(HTTPStatus.OK, deleted)
                    return

                if path == "/api/provision/probe":
                    cfg = ctx.config_store.effective()
                    res = provision_ops.probe_robot(cfg, body)
                    self._send_json(HTTPStatus.OK, res)
                    return

                if path == "/api/provision/run":
                    cfg = ctx.config_store.effective()
                    res = provision_ops.provision_robot(cfg, ops, body, repo_root=REPO_ROOT)
                    self._send_json(HTTPStatus.OK, res)
                    return

                if path == "/api/provision/finish-mqtt":
                    cfg = ctx.config_store.effective()
                    res = provision_ops.finish_mqtt_on_robot(cfg, body)
                    self._send_json(HTTPStatus.OK, res)
                    return

                if path == "/api/fleet/patrol":
                    cfg = ctx.config_store.effective()
                    ov = ctx.get_overview()
                    res = fleet_ops.patrol_fleet(
                        cfg,
                        fleet_entries=ov.get("fleet") or [],
                        port_credentials=body.get("portCredentials") or {},
                    )
                    self._send_json(HTTPStatus.OK, res)
                    return

                if path == "/api/fleet/batch-ring":
                    cfg = ctx.config_store.effective()
                    token = ctx._require_token(cfg)
                    ring = str(body.get("ring") or "").strip()
                    if ring not in ("dev", "canary", "prod"):
                        raise fleet_ops.FleetOpsError("分环必须是 dev / canary / prod")
                    dev_ids = [str(d).strip() for d in (body.get("deviceIds") or []) if str(d).strip()]
                    if not dev_ids:
                        raise fleet_ops.FleetOpsError("请至少选择 1 台机器狗")
                    _, current_regs = ops.api_call(cfg["DOSO_OTA_CLOUD_URL"], "GET", "/v1/devices", token)
                    reg_map = {str(r["deviceId"]): r for r in (current_regs or []) if isinstance(r, dict) and r.get("deviceId")}
                    results = []
                    for raw_id in dev_ids:
                        did = ops.validate_device_id(raw_id)
                        existing = reg_map.get(did) or {}
                        payload_put = {
                            "model": str(existing.get("model") or cfg["DOSO_DEFAULT_MODEL"]),
                            "hardwareRevision": str(existing.get("hardwareRevision") or cfg["DOSO_DEFAULT_HW_REV"]),
                            "enabled": existing.get("enabled", True) is not False,
                            "ring": ring,
                            "tags": list(existing.get("tags") or []),
                        }
                        try:
                            _, saved = ops.api_call(
                                cfg["DOSO_OTA_CLOUD_URL"],
                                "PUT",
                                f"/v1/devices/{urllib.parse.quote(did)}",
                                token,
                                body=payload_put,
                            )
                            results.append({"deviceId": did, "ok": True, "ring": saved.get("ring", ring)})
                        except Exception as exc:
                            results.append({"deviceId": did, "ok": False, "error": str(exc)})
                    self._send_json(HTTPStatus.OK, {"ring": ring, "results": results})
                    return

                if path == "/api/fleet/batch-push":
                    cfg = ctx.config_store.effective()
                    token = ctx._require_token(cfg)
                    release_id = ops.validate_release_id(str(body.get("releaseId") or ""))
                    dev_ids = [str(d).strip() for d in (body.get("deviceIds") or []) if str(d).strip()]
                    if not dev_ids:
                        raise fleet_ops.FleetOpsError("请至少选择 1 台机器狗")
                    ts_key = int(time.time())
                    results = []
                    for raw_id in dev_ids:
                        did = ops.validate_device_id(raw_id)
                        idem = f"batch-{release_id}-{did[:8]}-{ts_key}"
                        try:
                            _, job = ops.api_call(
                                cfg["DOSO_OTA_CLOUD_URL"],
                                "POST",
                                f"/v1/devices/{urllib.parse.quote(did)}/firmware/jobs",
                                token,
                                body={"releaseId": release_id, "idempotencyKey": idem},
                            )
                            if isinstance(job, dict):
                                ctx.jobs.record(job)
                            results.append({"deviceId": did, "ok": True, "jobId": (job or {}).get("jobId")})
                        except Exception as exc:
                            results.append({"deviceId": did, "ok": False, "error": str(exc)})
                    self._send_json(HTTPStatus.OK, {"releaseId": release_id, "results": results})
                    return

                if path == "/api/ops/error-logs":
                    cfg = ctx.config_store.effective()
                    res = fleet_ops.fetch_robot_error_logs(
                        cfg,
                        body.get("target") or {},
                        int(body.get("hours") or 2),
                        int(body.get("lines") or 80),
                    )
                    self._send_json(HTTPStatus.OK, res)
                    return

                if path == "/api/accounts/role":
                    cfg = ctx.config_store.effective()
                    res = fleet_ops.update_account_role(
                        cfg, str(body.get("userId") or ""), str(body.get("appRole") or "")
                    )
                    self._send_json(HTTPStatus.OK, res)
                    return

                if path == "/api/devices/owner":
                    cfg = ctx.config_store.effective()
                    res = fleet_ops.update_device_owner(
                        cfg, str(body.get("deviceId") or ""), str(body.get("ownerEmail") or "")
                    )
                    self._send_json(HTTPStatus.OK, res)
                    return

                if path == "/api/ops/status":
                    cfg = ctx.config_store.effective()
                    self._send_json(HTTPStatus.OK, robot_ops.get_robot_status(cfg, body.get("target") or {}))
                    return

                if path == "/api/ops/service":
                    cfg = ctx.config_store.effective()
                    res = robot_ops.control_service(
                        cfg,
                        body.get("target") or {},
                        str(body.get("unit") or ""),
                        str(body.get("action") or ""),
                    )
                    self._send_json(HTTPStatus.OK, res)
                    return

                if path == "/api/ops/logs":
                    cfg = ctx.config_store.effective()
                    res = robot_ops.get_service_logs(
                        cfg,
                        body.get("target") or {},
                        str(body.get("unit") or ""),
                        int(body.get("lines") or 100),
                        str(body.get("keyword") or ""),
                    )
                    self._send_json(HTTPStatus.OK, res)
                    return

                if path == "/api/ops/files/list":
                    cfg = ctx.config_store.effective()
                    res = robot_ops.list_dir(cfg, body.get("target") or {}, str(body.get("path") or "/opt/doso"))
                    self._send_json(HTTPStatus.OK, res)
                    return

                if path == "/api/ops/files/read":
                    cfg = ctx.config_store.effective()
                    res = robot_ops.read_text_file(cfg, body.get("target") or {}, str(body.get("path") or ""))
                    self._send_json(HTTPStatus.OK, res)
                    return

                if path == "/api/ops/files/write":
                    cfg = ctx.config_store.effective()
                    res = robot_ops.write_text_file(
                        cfg,
                        body.get("target") or {},
                        str(body.get("path") or ""),
                        str(body.get("content") or ""),
                    )
                    self._send_json(HTTPStatus.OK, res)
                    return

                if path == "/api/ops/files/upload":
                    cfg = ctx.config_store.effective()
                    local_path, orig_name = ctx.uploads.resolve_record(str(body.get("uploadId") or "").strip())
                    res = robot_ops.upload_file(
                        cfg,
                        body.get("target") or {},
                        str(body.get("remoteDir") or "/tmp"),
                        local_path,
                        str(body.get("filename") or orig_name),
                    )
                    self._send_json(HTTPStatus.OK, res)
                    return

                if path == "/api/ops/files/download":
                    cfg = ctx.config_store.effective()
                    fname, data = robot_ops.download_file_bytes(
                        cfg, body.get("target") or {}, str(body.get("path") or "")
                    )
                    try:
                        self.send_response(HTTPStatus.OK)
                        self._send_security_headers()
                        self.send_header("Content-Type", "application/octet-stream")
                        self.send_header(
                            "Content-Disposition",
                            f"attachment; filename*=UTF-8''{urllib.parse.quote(fname)}",
                        )
                        self.send_header("Content-Length", str(len(data)))
                        self.end_headers()
                        self.wfile.write(data)
                    except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                        pass
                    return

                if path == "/api/ops/files/mkdir":
                    cfg = ctx.config_store.effective()
                    res = robot_ops.make_dir(cfg, body.get("target") or {}, str(body.get("path") or ""))
                    self._send_json(HTTPStatus.OK, res)
                    return

                if path == "/api/ops/files/delete":
                    cfg = ctx.config_store.effective()
                    res = robot_ops.delete_path(cfg, body.get("target") or {}, str(body.get("path") or ""))
                    self._send_json(HTTPStatus.OK, res)
                    return

                if path == "/api/android/publish":
                    res = ctx.publish_android_release(body)
                    self._send_json(HTTPStatus.OK, res)
                    return

                if path == "/api/jobs/start":
                    cfg = ctx.config_store.effective()
                    token = ctx._require_token(cfg)
                    dev_id = ops.validate_device_id(str(body.get("deviceId", "")))
                    rel_id = ops.validate_release_id(str(body.get("releaseId", "")))
                    idem = str(body.get("idempotencyKey", "") or "").strip() or f"console-{rel_id}-{dev_id}-{int(time.time())}"
                    _, job = ops.api_call(
                        cfg["DOSO_OTA_CLOUD_URL"],
                        "POST",
                        f"/v1/devices/{urllib.parse.quote(dev_id)}/firmware/jobs",
                        token,
                        body={"releaseId": rel_id, "idempotencyKey": idem},
                    )
                    if isinstance(job, dict):
                        ctx.jobs.record(job)
                    self._send_json(HTTPStatus.ACCEPTED, job)
                    return

                m_cancel = re.fullmatch(r"/api/jobs/([^/]+)/([^/]+)/cancel", path)
                if m_cancel:
                    cfg = ctx.config_store.effective()
                    token = ctx._require_token(cfg)
                    dev_id = ops.validate_device_id(urllib.parse.unquote(m_cancel.group(1)))
                    job_id = urllib.parse.unquote(m_cancel.group(2))
                    _, job = ops.api_call(
                        cfg["DOSO_OTA_CLOUD_URL"],
                        "POST",
                        f"/v1/devices/{urllib.parse.quote(dev_id)}/firmware/jobs/{urllib.parse.quote(job_id)}/cancel",
                        token,
                    )
                    if isinstance(job, dict):
                        ctx.jobs.record(job)
                    self._send_json(HTTPStatus.OK, job)
                    return

                if path == "/api/rollouts":
                    cfg = ctx.config_store.effective()
                    token = ctx._require_token(cfg)
                    release_id = ops.validate_release_id(str(body.get("releaseId", "")))
                    preset = str(body.get("preset", "standard"))
                    ro_payload: dict[str, Any] = {
                        "releaseId": release_id,
                        "stages": build_rollout_stages(preset, body),
                        "circuitBreaker": {
                            "maxFailureCount": int(body.get("maxFailureCount", 1)),
                            "maxFailureRate": float(body.get("maxFailureRate", 0.05)),
                        },
                    }
                    if body.get("rolloutId"):
                        ro_payload["rolloutId"] = str(body["rolloutId"]).strip()
                    if body.get("targetModel"):
                        ro_payload["targetModel"] = str(body["targetModel"]).strip()
                    if body.get("targetHardwareRevision"):
                        ro_payload["targetHardwareRevision"] = str(body["targetHardwareRevision"]).strip()
                    if isinstance(body.get("targetTags"), list) and body["targetTags"]:
                        ro_payload["targetTags"] = [str(t).strip() for t in body["targetTags"] if str(t).strip()]
                    _, rollout = ops.api_call(
                        cfg["DOSO_OTA_CLOUD_URL"],
                        "POST",
                        "/v1/rollouts",
                        token,
                        body=ro_payload,
                    )
                    self._send_json(HTTPStatus.CREATED, rollout)
                    return

                m_ro_act = re.fullmatch(r"/api/rollouts/([^/]+)/(promote|pause|resume|abort)", path)
                if m_ro_act:
                    cfg = ctx.config_store.effective()
                    token = ctx._require_token(cfg)
                    rid = urllib.parse.unquote(m_ro_act.group(1))
                    action = m_ro_act.group(2)
                    _, rollout = ops.api_call(
                        cfg["DOSO_OTA_CLOUD_URL"],
                        "POST",
                        f"/v1/rollouts/{urllib.parse.quote(rid)}/{action}",
                        token,
                    )
                    self._send_json(HTTPStatus.OK, rollout)
                    return

                self._send_json(HTTPStatus.NOT_FOUND, {"error": "unknown_api"})
            except (ops.OpsError, robot_ops.RobotOpsError, provision_ops.ProvisionError, fleet_ops.FleetOpsError) as exc:
                self._send_json(HTTPStatus.BAD_REQUEST, {"error": str(exc)})
            except ValueError as exc:
                self._send_json(HTTPStatus.BAD_REQUEST, {"error": str(exc)})
            except Exception as exc:
                self._send_json(HTTPStatus.INTERNAL_SERVER_ERROR, {"error": str(exc)})

    return OtaConsoleHandler


def is_port_listening(host: str, port: int) -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.settimeout(0.4)
        return sock.connect_ex((host, port)) == 0


def create_server(
    host: str = "127.0.0.1",
    port: int = 8190,
    config_path: Path | None = None,
    allow_remote: bool = False,
) -> ThreadingHTTPServer:
    store = ConfigStore(config_path or default_config_path())
    ctx = ConsoleContext(store, allow_remote=allow_remote)
    handler_cls = make_handler(ctx)
    server = ThreadingHTTPServer((host, port), handler_cls)
    server.daemon_threads = True
    return server


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="DOSO OTA 管理台本地服务")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8190)
    parser.add_argument("--config", type=Path, help="自定义本地配置文件路径")
    parser.add_argument(
        "--open",
        dest="open_browser",
        action="store_true",
        default=bool(getattr(sys, "frozen", False)),
        help="启动后自动打开浏览器",
    )
    parser.add_argument("--no-open", dest="open_browser", action="store_false", help="启动时不自动打开浏览器")
    parser.add_argument("--allow-remote", action="store_true", help="允许非回环 Host 访问（仅反向代理场景）")
    args = parser.parse_args(argv)

    url = f"http://{args.host}:{args.port}/"
    if args.host == "127.0.0.1" and is_port_listening(args.host, args.port):
        print(f"DOSO OTA 管理台已在运行: {url}")
        if args.open_browser:
            webbrowser.open(url)
        return 0

    server = create_server(
        host=args.host,
        port=args.port,
        config_path=args.config,
        allow_remote=args.allow_remote,
    )
    actual_port = server.server_address[1]
    actual_url = f"http://{args.host}:{actual_port}/"
    print(f"DOSO OTA 管理台已启动: {actual_url}", flush=True)
    if getattr(sys, "frozen", False):
        print("关闭此窗口即可停止本地服务。", flush=True)
    if args.open_browser:
        threading.Timer(0.35, lambda: webbrowser.open(actual_url)).start()
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\n已停止 DOSO OTA 管理台")
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
