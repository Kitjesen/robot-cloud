#!/usr/bin/env python3
"""On-demand Cloud Server ops (SWAS + external probes) & private Aliyun OSS manager.

Zero extra third-party dependencies: uses Python stdlib HTTPS + HMAC-SHA1 for OSS
and reuses robot_ops._connect_jump (Paramiko) for on-demand SWAS administration.
Never called during background overview polling — strictly on-demand to keep
channel and server overhead minimal.
"""

from __future__ import annotations

import base64
from concurrent.futures import ThreadPoolExecutor
from email.utils import formatdate
import hashlib
import hmac
import http.client
import json
import mimetypes
from pathlib import Path
import re
import shlex
import socket
import subprocess
import time
from typing import Any
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

import robot_ops


class CloudOpsError(ValueError):
    """Actionable cloud server or OSS error."""


# Strict service allowlists: destructive targets (sshd, supabase-db, ufw) are NEVER mutable.
CLOUD_SYSTEMD_SERVICES: tuple[dict[str, Any], ...] = (
    {"name": "doso-ota-release.service", "label": "OTA 发布服务", "actions": ("restart",)},
    {"name": "doso-frps.service", "label": "内网穿透服务", "actions": ("restart",)},
    {"name": "nginx.service", "label": "网关代理", "actions": ("reload",)},
    {"name": "doso-flow-gateway.service", "label": "状态网关", "actions": ("restart",)},
)

CLOUD_DOCKER_CONTAINERS: tuple[dict[str, Any], ...] = (
    {"name": "supabase-envoy", "label": "入口代理", "actions": ("restart",)},
    {"name": "supabase-edge-functions", "label": "云函数运行时", "actions": ("restart",)},
    {"name": "supabase-auth", "label": "认证服务", "actions": ()},
    {"name": "supabase-rest", "label": "数据接口", "actions": ()},
    {"name": "supabase-db", "label": "主数据库", "actions": ()},
)

_SYSTEMD_MAP = {item["name"]: item for item in CLOUD_SYSTEMD_SERVICES}
_DOCKER_MAP = {item["name"]: item for item in CLOUD_DOCKER_CONTAINERS}

DEFAULT_MQTT_HOST = "8.153.169.177"
DEFAULT_MQTT_PORT = 8883
DEFAULT_EDGE_URL = "https://api-test.dosorobotics.com/functions/v1/firmware-ota"
DEFAULT_GITHUB_REPO = "Kitjesen/brainstem"
GITHUB_REPO_RE = re.compile(r"^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$")
_GH_CLI_CACHE: tuple[float, str] | None = None


def _local_tag(tag: str) -> str:
    return tag.rsplit("}", 1)[-1] if "}" in tag else tag


def _require_oss_cfg(cfg: dict[str, str]) -> tuple[str, str, str, str]:
    bucket = (cfg.get("DOSO_OSS_BUCKET") or "").strip()
    endpoint = (cfg.get("DOSO_OSS_ENDPOINT") or "").strip()
    ak_id = (cfg.get("DOSO_OSS_ACCESS_KEY_ID") or "").strip()
    ak_secret = (cfg.get("DOSO_OSS_ACCESS_KEY_SECRET") or "").strip()
    if not (bucket and endpoint and ak_id and ak_secret):
        raise CloudOpsError("请先在「系统配置」完整填写对象存储凭证")
    return bucket, endpoint, ak_id, ak_secret


def validate_oss_key(raw_key: str) -> str:
    key = str(raw_key or "").strip().lstrip("/")
    if not key:
        raise CloudOpsError("对象路径不能为空")
    if len(key) > 512:
        raise CloudOpsError("对象路径过长")
    if "\\" in key or "\0" in key or "\r" in key or "\n" in key:
        raise CloudOpsError("对象路径包含非法字符")
    if "//" in key:
        raise CloudOpsError("对象路径不能包含连续斜杠")
    parts = key.split("/")
    if any(p in ("", ".", "..") for p in parts):
        raise CloudOpsError("对象路径不能包含相对目录段")
    return key


def _guess_content_type(object_key: str) -> str:
    lower = object_key.lower()
    if lower.endswith(".apk"):
        return "application/vnd.android.package-archive"
    if lower.endswith((".tar.gz", ".tgz", ".gz")):
        return "application/gzip"
    if lower.endswith(".json"):
        return "application/json"
    if lower.endswith((".txt", ".log", ".md", ".yaml", ".yml")):
        return "text/plain; charset=utf-8"
    mime, _ = mimetypes.guess_type(object_key)
    return mime or "application/octet-stream"


def _classify_oss_key(key: str) -> str:
    if key.startswith(("firmware/", "policies/")):
        return "firmware"
    if key.startswith("apps/"):
        return "apps"
    return "files"


def parse_oss_list_xml(xml_bytes: bytes) -> tuple[list[dict[str, Any]], bool, str]:
    """Parse Aliyun OSS ListObjectsV2 XML payload into object dicts."""
    try:
        root = ET.fromstring(xml_bytes)
    except ET.ParseError as exc:
        raise CloudOpsError(f"解析 OSS 列表响应失败: {exc}") from exc

    items: list[dict[str, Any]] = []
    is_truncated = False
    next_token = ""

    for child in root:
        tag = _local_tag(child.tag)
        if tag == "IsTruncated":
            is_truncated = (child.text or "").strip().lower() == "true"
        elif tag == "NextContinuationToken":
            next_token = (child.text or "").strip()
        elif tag == "Contents":
            entry: dict[str, Any] = {}
            for field in child:
                ftag = _local_tag(field.tag)
                val = (field.text or "").strip()
                if ftag == "Key":
                    entry["key"] = val
                elif ftag == "Size":
                    try:
                        entry["sizeBytes"] = int(val)
                    except ValueError:
                        entry["sizeBytes"] = 0
                elif ftag == "LastModified":
                    entry["lastModified"] = val
                elif ftag == "ETag":
                    entry["etag"] = val.strip('"')
                elif ftag == "StorageClass":
                    entry["storageClass"] = val
            if entry.get("key") and not str(entry["key"]).endswith("/"):
                items.append(entry)

    return items, is_truncated, next_token


def _oss_list_page(
    *,
    bucket: str,
    endpoint: str,
    ak_id: str,
    ak_secret: str,
    prefix: str = "",
    max_keys: int = 300,
    continuation_token: str = "",
    timeout_s: int = 20,
) -> tuple[list[dict[str, Any]], bool, str]:
    date_hdr = formatdate(usegmt=True)
    # Aliyun OSS ListObjectsV2 signs canonical resource as "/{bucket}/" (no query params).
    string_to_sign = f"GET\n\n\n{date_hdr}\n/{bucket}/"
    sig = base64.b64encode(
        hmac.new(ak_secret.encode("utf-8"), string_to_sign.encode("utf-8"), hashlib.sha1).digest()
    ).decode("ascii")
    host = f"{bucket}.{endpoint}"
    params: dict[str, str] = {
        "list-type": "2",
        "max-keys": str(max(1, min(1000, int(max_keys)))),
    }
    if prefix:
        params["prefix"] = prefix
    if continuation_token:
        params["continuation-token"] = continuation_token
    qs = urllib.parse.urlencode(params, quote_via=urllib.parse.quote)
    headers = {
        "Host": host,
        "Date": date_hdr,
        "Authorization": f"OSS {ak_id}:{sig}",
    }

    conn = http.client.HTTPSConnection(host, timeout=timeout_s)
    try:
        conn.request("GET", f"/?{qs}", headers=headers)
        resp = conn.getresponse()
        body = resp.read()
        if resp.status != 200:
            snippet = body.decode("utf-8", errors="replace")[:300]
            raise CloudOpsError(f"读取 OSS 对象列表失败 (HTTP {resp.status}): {snippet}")
        return parse_oss_list_xml(body)
    except CloudOpsError:
        raise
    except Exception as exc:
        raise CloudOpsError(f"连接 OSS 失败: {exc}") from exc
    finally:
        conn.close()


def oss_list_objects(
    cfg: dict[str, str],
    *,
    prefix: str = "",
    max_objects: int = 500,
    protected_keys: dict[str, str] | None = None,
    referenced_keys: dict[str, str] | None = None,
) -> dict[str, Any]:
    bucket, endpoint, ak_id, ak_secret = _require_oss_cfg(cfg)
    clean_prefix = str(prefix or "").strip().lstrip("/")
    prot = protected_keys or {}
    refs = referenced_keys or {}

    collected: list[dict[str, Any]] = []
    token = ""
    truncated = False
    max_pages = 5
    for _ in range(max_pages):
        remaining = max(1, max_objects - len(collected))
        page_items, is_trunc, next_tok = _oss_list_page(
            bucket=bucket,
            endpoint=endpoint,
            ak_id=ak_id,
            ak_secret=ak_secret,
            prefix=clean_prefix,
            max_keys=min(300, remaining),
            continuation_token=token,
        )
        collected.extend(page_items)
        truncated = is_trunc
        if not is_trunc or not next_tok or len(collected) >= max_objects:
            break
        token = next_tok

    by_category: dict[str, dict[str, int]] = {
        "firmware": {"count": 0, "sizeBytes": 0},
        "apps": {"count": 0, "sizeBytes": 0},
        "files": {"count": 0, "sizeBytes": 0},
    }
    total_size = 0
    enriched: list[dict[str, Any]] = []

    for item in collected:
        key = str(item["key"])
        size = int(item.get("sizeBytes") or 0)
        cat = _classify_oss_key(key)
        total_size += size
        by_category[cat]["count"] += 1
        by_category[cat]["sizeBytes"] += size

        protect_reason = prot.get(key) or ""
        if not protect_reason and key.startswith("firmware/stable/"):
            protect_reason = "稳定版固件"
        ref_label = refs.get(key) or ""
        if not ref_label and refs:
            for pattern_key, label in refs.items():
                if pattern_key.startswith("rid:") and f"/{pattern_key[4:]}/" in f"/{key}":
                    ref_label = label
                    break
        if not protect_reason and prot:
            for pattern_key, reason in prot.items():
                if pattern_key.startswith("rid:") and f"/{pattern_key[4:]}/" in f"/{key}":
                    protect_reason = reason
                    break
        enriched.append(
            {
                "key": key,
                "name": key.rsplit("/", 1)[-1],
                "category": cat,
                "sizeBytes": size,
                "lastModified": item.get("lastModified") or "",
                "etag": item.get("etag") or "",
                "storageClass": item.get("storageClass") or "Standard",
                "protected": bool(protect_reason),
                "protectReason": protect_reason,
                "releaseTag": ref_label,
            }
        )

    enriched.sort(key=lambda x: str(x.get("lastModified") or ""), reverse=True)
    return {
        "bucket": bucket,
        "endpoint": endpoint,
        "prefix": clean_prefix,
        "totalCount": len(enriched),
        "totalSizeBytes": total_size,
        "byCategory": by_category,
        "truncated": truncated,
        "objects": enriched,
    }


def oss_presign_get_url(
    cfg: dict[str, str],
    object_key: str,
    ttl_seconds: int = 1800,
) -> dict[str, Any]:
    bucket, endpoint, ak_id, ak_secret = _require_oss_cfg(cfg)
    clean_key = validate_oss_key(object_key)
    ttl = max(60, min(86400, int(ttl_seconds or 1800)))
    expires = int(time.time()) + ttl
    canonical_resource = f"/{bucket}/{clean_key}"
    string_to_sign = f"GET\n\n\n{expires}\n{canonical_resource}"
    sig = base64.b64encode(
        hmac.new(ak_secret.encode("utf-8"), string_to_sign.encode("utf-8"), hashlib.sha1).digest()
    ).decode("ascii")
    quoted_key = urllib.parse.quote(clean_key, safe="/-_.~")
    url = (
        f"https://{bucket}.{endpoint}/{quoted_key}"
        f"?OSSAccessKeyId={urllib.parse.quote(ak_id, safe='')}"
        f"&Expires={expires}"
        f"&Signature={urllib.parse.quote(sig, safe='')}"
    )
    return {
        "objectKey": clean_key,
        "url": url,
        "ttlSeconds": ttl,
        "expiresAtMs": expires * 1000,
    }


def oss_upload_file(
    cfg: dict[str, str],
    ops_mod: Any,
    local_path: Path,
    object_key: str,
    *,
    overwrite: bool = False,
    content_type: str = "",
) -> dict[str, Any]:
    bucket, endpoint, ak_id, ak_secret = _require_oss_cfg(cfg)
    clean_key = validate_oss_key(object_key)
    if not local_path.is_file():
        raise CloudOpsError(f"本地文件不存在: {local_path}")
    ct = (content_type or "").strip() or _guess_content_type(clean_key)
    try:
        res = ops_mod.upload_to_oss(
            file_path=local_path,
            object_key=clean_key,
            bucket=bucket,
            endpoint=endpoint,
            content_type=ct,
            overwrite=overwrite,
            dry_run=False,
            ak_id=ak_id,
            ak_secret=ak_secret,
        )
    except Exception as exc:
        raise CloudOpsError(str(exc)) from exc

    presigned = oss_presign_get_url(cfg, clean_key, ttl_seconds=1800)
    return {
        **res,
        "contentType": ct,
        "signedUrl": presigned["url"],
        "expiresAtMs": presigned["expiresAtMs"],
    }


def oss_delete_object(
    cfg: dict[str, str],
    ops_mod: Any,
    object_key: str,
    *,
    protected_keys: dict[str, str] | None = None,
) -> dict[str, Any]:
    bucket, endpoint, ak_id, ak_secret = _require_oss_cfg(cfg)
    clean_key = validate_oss_key(object_key)
    prot = protected_keys or {}
    reason = prot.get(clean_key) or ("稳定版固件" if clean_key.startswith("firmware/stable/") else "")
    if not reason and prot:
        for pattern_key, r_text in prot.items():
            if pattern_key.startswith("rid:") and f"/{pattern_key[4:]}/" in f"/{clean_key}":
                reason = r_text
                break
    if reason:
        raise CloudOpsError(f"文件受保护（{reason}），禁止删除")

    status, _, resp_body = ops_mod.oss_request(
        method="DELETE",
        bucket=bucket,
        endpoint=endpoint,
        object_key=clean_key,
        ak_id=ak_id,
        ak_secret=ak_secret,
        timeout_s=30,
    )
    if status not in (200, 204):
        snippet = resp_body.decode("utf-8", errors="replace")[:300]
        raise CloudOpsError(f"删除对象失败 (HTTP {status}): {snippet}")
    return {"deleted": True, "objectKey": clean_key}


def _probe_http(url: str, *, expected_statuses: tuple[int, ...], timeout_s: float = 8.0) -> dict[str, Any]:
    t0 = time.monotonic()
    req = urllib.request.Request(url, method="GET", headers={"Accept": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout_s) as resp:
            code = resp.status
            ms = int((time.monotonic() - t0) * 1000)
            ok = code in expected_statuses
            return {"ok": ok, "status": code, "latencyMs": ms, "detail": f"HTTP {code}"}
    except urllib.error.HTTPError as err:
        ms = int((time.monotonic() - t0) * 1000)
        ok = err.code in expected_statuses
        return {"ok": ok, "status": err.code, "latencyMs": ms, "detail": f"HTTP {err.code}"}
    except Exception as exc:
        ms = int((time.monotonic() - t0) * 1000)
        return {"ok": False, "status": None, "latencyMs": ms, "detail": str(exc)[:80]}


def _probe_tcp(host: str, port: int, timeout_s: float = 6.0) -> dict[str, Any]:
    t0 = time.monotonic()
    try:
        with socket.create_connection((host, port), timeout=timeout_s):
            ms = int((time.monotonic() - t0) * 1000)
            return {"ok": True, "latencyMs": ms, "detail": f"{host}:{port}"}
    except Exception as exc:
        ms = int((time.monotonic() - t0) * 1000)
        return {"ok": False, "latencyMs": ms, "detail": str(exc)[:80]}


def _run_external_probes(cfg: dict[str, str]) -> list[dict[str, Any]]:
    ota_health_url = (cfg.get("DOSO_OTA_CLOUD_URL") or "https://api.dosorobotics.com/ota").rstrip("/") + "/health"
    with ThreadPoolExecutor(max_workers=3) as pool:
        f_ota = pool.submit(_probe_http, ota_health_url, expected_statuses=(200,))
        f_edge = pool.submit(_probe_http, DEFAULT_EDGE_URL, expected_statuses=(200, 401, 405))
        f_mqtt = pool.submit(_probe_tcp, DEFAULT_MQTT_HOST, DEFAULT_MQTT_PORT)
        r_ota = f_ota.result()
        r_edge = f_edge.result()
        r_mqtt = f_mqtt.result()
    return [
        {"id": "ota", "label": "OTA 网关", **r_ota},
        {"id": "edge", "label": "云函数网关", **r_edge},
        {"id": "mqtt", "label": "MQTT 网关", **r_mqtt},
    ]


def _parse_swas_telemetry(raw_out: str, host_address: str) -> dict[str, Any]:
    sections: dict[str, str] = {}
    current = ""
    buf: list[str] = []
    for line in raw_out.splitlines():
        if line.startswith("===") and line.endswith("==="):
            if current:
                sections[current] = "\n".join(buf).strip()
            current = line.strip("=")
            buf = []
        else:
            buf.append(line)
    if current:
        sections[current] = "\n".join(buf).strip()

    host: dict[str, Any] = {
        "address": host_address,
        "hostname": "",
        "uptime": "",
        "cpuCores": 0,
        "load1": 0.0,
        "load5": 0.0,
        "load15": 0.0,
        "memTotalBytes": 0,
        "memUsedBytes": 0,
        "memAvailBytes": 0,
        "memPercent": 0,
        "diskTotalBytes": 0,
        "diskUsedBytes": 0,
        "diskAvailBytes": 0,
        "diskPercent": 0,
        "otaDataBytes": 0,
    }
    for line in sections.get("HOST", "").splitlines():
        if "=" not in line:
            continue
        k, v = line.split("=", 1)
        k = k.strip()
        v = v.strip()
        if k == "HOSTNAME":
            host["hostname"] = v
        elif k == "UPTIME":
            host["uptime"] = v
        elif k == "CORES":
            host["cpuCores"] = int(v) if v.isdigit() else 0
        elif k == "LOAD":
            parts = v.split()
            if len(parts) >= 3:
                try:
                    host["load1"] = float(parts[0])
                    host["load5"] = float(parts[1])
                    host["load15"] = float(parts[2])
                except ValueError:
                    pass
        elif k == "MEM":
            # free -b: total used free shared buff/cache available
            parts = v.split()
            if len(parts) >= 6:
                try:
                    total = int(parts[0])
                    avail = int(parts[5])
                    used = max(0, total - avail)
                    host["memTotalBytes"] = total
                    host["memUsedBytes"] = used
                    host["memAvailBytes"] = avail
                    host["memPercent"] = round(used * 100 / total) if total > 0 else 0
                except ValueError:
                    pass
        elif k == "DISK":
            # df -B1 /: total used avail pct
            parts = v.split()
            if len(parts) >= 4:
                try:
                    total = int(parts[0])
                    used = int(parts[1])
                    avail = int(parts[2])
                    host["diskTotalBytes"] = total
                    host["diskUsedBytes"] = used
                    host["diskAvailBytes"] = avail
                    host["diskPercent"] = int(parts[3].rstrip("%")) if parts[3].rstrip("%").isdigit() else (
                        round(used * 100 / total) if total > 0 else 0
                    )
                except ValueError:
                    pass
        elif k == "OTA_DATA":
            host["otaDataBytes"] = int(v) if v.isdigit() else 0

    systemd_map: dict[str, dict[str, str]] = {}
    for line in sections.get("SYSTEMD", "").splitlines():
        parts = line.split("\t")
        if len(parts) >= 4:
            systemd_map[parts[0].strip()] = {
                "loadState": parts[1].strip(),
                "activeState": parts[2].strip(),
                "subState": parts[3].strip(),
            }

    systemd_list: list[dict[str, Any]] = []
    for spec in CLOUD_SYSTEMD_SERVICES:
        st = systemd_map.get(spec["name"], {})
        active = st.get("activeState") or "unknown"
        sub = st.get("subState") or ""
        load = st.get("loadState") or "unknown"
        systemd_list.append(
            {
                "name": spec["name"],
                "label": spec["label"],
                "loadState": load,
                "activeState": active,
                "subState": sub,
                "healthy": active == "active",
                "allowedActions": list(spec["actions"]),
            }
        )

    docker_map: dict[str, dict[str, str]] = {}
    for line in sections.get("DOCKER", "").splitlines():
        parts = line.split("\t")
        if len(parts) >= 3:
            docker_map[parts[0].strip()] = {
                "state": parts[1].strip(),
                "status": parts[2].strip(),
            }

    docker_list: list[dict[str, Any]] = []
    for spec in CLOUD_DOCKER_CONTAINERS:
        st = docker_map.get(spec["name"], {})
        state = st.get("state") or "missing"
        status_text = st.get("status") or "未运行"
        healthy = state == "running" and "unhealthy" not in status_text.lower()
        docker_list.append(
            {
                "name": spec["name"],
                "label": spec["label"],
                "state": state,
                "status": status_text,
                "healthy": healthy,
                "allowedActions": list(spec["actions"]),
            }
        )

    return {"host": host, "systemd": systemd_list, "docker": docker_list}


def get_cloud_server_status(cfg: dict[str, str]) -> dict[str, Any]:
    """On-demand health check of SWAS host, cloud systemd/Docker services, and public endpoints."""
    host_addr = (cfg.get("DOSO_OPS_JUMP_HOST") or "47.103.89.137").strip()
    unit_names = " ".join(shlex.quote(s["name"]) for s in CLOUD_SYSTEMD_SERVICES)
    remote_script = f"""
printf '===HOST===\\n'
printf 'HOSTNAME=%s\\n' "$(hostname 2>/dev/null || echo unknown)"
printf 'UPTIME=%s\\n' "$(uptime -p 2>/dev/null | sed 's/^up //' || echo unknown)"
printf 'CORES=%s\\n' "$(nproc 2>/dev/null || echo 0)"
printf 'LOAD=%s\\n' "$(awk '{{print $1,$2,$3}}' /proc/loadavg 2>/dev/null || echo '0 0 0')"
printf 'MEM=%s\\n' "$(free -b 2>/dev/null | awk '/^Mem:/{{print $2,$3,$4,$5,$6,$7}}')"
printf 'DISK=%s\\n' "$(df -B1 / 2>/dev/null | awk 'NR==2{{print $2,$3,$4,$5}}')"
printf 'OTA_DATA=%s\\n' "$(sudo -n du -sb /var/lib/doso-ota-release 2>/dev/null | awk '{{print $1}}' || echo 0)"
printf '===SYSTEMD===\\n'
for u in {unit_names}; do
  ld=$(systemctl show -p LoadState --value "$u" 2>/dev/null || echo unknown)
  ac=$(systemctl show -p ActiveState --value "$u" 2>/dev/null || echo unknown)
  sb=$(systemctl show -p SubState --value "$u" 2>/dev/null || echo unknown)
  printf '%s\\t%s\\t%s\\t%s\\n' "$u" "$ld" "$ac" "$sb"
done
printf '===DOCKER===\\n'
sudo -n docker ps -a --format '{{{{.Names}}}}\\t{{{{.State}}}}\\t{{{{.Status}}}}' 2>/dev/null || true
""".strip()

    def _fetch_swas() -> tuple[dict[str, Any] | None, str | None]:
        try:
            jump = robot_ops._connect_jump(cfg, timeout_s=8)
        except Exception as exc:
            return None, str(exc)
        try:
            code, out, err = robot_ops._exec_cmd(jump, "bash -s", timeout_s=12, stdin_text=remote_script + "\n")
            if code != 0 and "===HOST===" not in out:
                return None, (err or out or "远端采集失败").strip()[:200]
            return _parse_swas_telemetry(out, host_addr), None
        except Exception as exc:
            return None, str(exc)
        finally:
            jump.close()

    with ThreadPoolExecutor(max_workers=2) as pool:
        f_swas = pool.submit(_fetch_swas)
        f_probes = pool.submit(_run_external_probes, cfg)
        swas_data, swas_err = f_swas.result()
        probes = f_probes.result()

    if swas_data is None:
        return {
            "checkedAtMs": int(time.time() * 1000),
            "sshOk": False,
            "sshError": swas_err,
            "host": {"address": host_addr},
            "systemd": [],
            "docker": [],
            "probes": probes,
        }

    return {
        "checkedAtMs": int(time.time() * 1000),
        "sshOk": True,
        "sshError": None,
        "host": swas_data["host"],
        "systemd": swas_data["systemd"],
        "docker": swas_data["docker"],
        "probes": probes,
    }


def validate_cloud_service_action(kind: str, name: str, action: str) -> tuple[str, str, str]:
    k = str(kind or "").strip().lower()
    n = str(name or "").strip()
    a = str(action or "").strip().lower()
    if k == "systemd":
        spec = _SYSTEMD_MAP.get(n)
    elif k == "docker":
        spec = _DOCKER_MAP.get(n)
    else:
        raise CloudOpsError(f"不支持的服务类型: {kind}")
    if not spec:
        raise CloudOpsError(f"不在允许范围的服务: {name}")
    if a not in spec["actions"]:
        raise CloudOpsError(f"出于安全保护，禁止对 {n} 执行 {a}")
    return k, n, a


def validate_cloud_log_target(kind: str, name: str) -> tuple[str, str]:
    k = str(kind or "").strip().lower()
    n = str(name or "").strip()
    if k == "systemd" and n in _SYSTEMD_MAP:
        return k, n
    if k == "docker" and n in _DOCKER_MAP:
        return k, n
    raise CloudOpsError(f"不允许查看该服务日志: {name}")


def control_cloud_service(cfg: dict[str, str], kind: str, name: str, action: str) -> dict[str, Any]:
    k, n, a = validate_cloud_service_action(kind, name, action)
    jump = robot_ops._connect_jump(cfg, timeout_s=8)
    try:
        if k == "systemd":
            if n == "nginx.service" and a == "reload":
                cmd = "sudo -n nginx -t && sudo -n systemctl reload nginx && systemctl is-active nginx.service"
            else:
                cmd = f"sudo -n systemctl restart {shlex.quote(n)} && sleep 0.6 && systemctl is-active {shlex.quote(n)}"
        else:
            cmd = f"sudo -n docker restart {shlex.quote(n)} && sudo -n docker inspect -f '{{{{.State.Status}}}}' {shlex.quote(n)}"
        code, out, err = robot_ops._exec_cmd(jump, cmd, timeout_s=25)
        if code != 0:
            detail = (err or out or f"exit {code}").strip()[:300]
            raise CloudOpsError(f"操作失败: {detail}")
        lines = [ln.strip() for ln in out.splitlines() if ln.strip()]
        state = lines[-1] if lines else "active"
        return {
            "ok": True,
            "kind": k,
            "name": n,
            "action": a,
            "state": state,
            "output": out.strip()[-500:],
        }
    finally:
        jump.close()


def get_cloud_service_logs(
    cfg: dict[str, str],
    kind: str,
    name: str,
    lines: int = 120,
    keyword: str = "",
) -> dict[str, Any]:
    k, n = validate_cloud_log_target(kind, name)
    n_lines = max(20, min(500, int(lines or 120)))
    kw = str(keyword or "").strip()
    jump = robot_ops._connect_jump(cfg, timeout_s=8)
    try:
        if k == "systemd":
            cmd = f"sudo -n journalctl -u {shlex.quote(n)} -n {n_lines} --no-pager"
        else:
            cmd = f"sudo -n docker logs --tail {n_lines} {shlex.quote(n)} 2>&1"
        code, out, err = robot_ops._exec_cmd(jump, cmd, timeout_s=15)
        text = out if out else err
        if code != 0 and not text.strip():
            raise CloudOpsError(f"读取日志失败 (exit {code})")
        log_lines = text.splitlines()
        if kw:
            kw_low = kw.lower()
            log_lines = [ln for ln in log_lines if kw_low in ln.lower()]
        return {
            "kind": k,
            "name": n,
            "lines": len(log_lines),
            "text": "\n".join(log_lines[-n_lines:]),
        }
    finally:
        jump.close()


# ---------- GitHub Release Import Helpers (Plan B) ----------


class _NoAuthRedirectHandler(urllib.request.HTTPRedirectHandler):
    """Strip GitHub Bearer token when following cross-origin redirects to S3 asset URLs."""

    def redirect_request(self, req: urllib.request.Request, fp: Any, code: int, msg: str, headers: Any, newurl: str) -> Any:
        redirected = super().redirect_request(req, fp, code, msg, headers, newurl)
        if redirected is not None:
            orig_host = (urllib.parse.urlparse(req.full_url).hostname or "").lower()
            next_host = (urllib.parse.urlparse(newurl).hostname or "").lower()
            if orig_host != next_host:
                for hdr in ("Authorization", "authorization"):
                    redirected.headers.pop(hdr, None)
                    redirected.unredirected_hdrs.pop(hdr, None)
        return redirected


def validate_github_repo(raw_repo: str) -> str:
    cleaned = str(raw_repo or "").strip().strip("/")
    if not cleaned:
        cleaned = DEFAULT_GITHUB_REPO
    if not GITHUB_REPO_RE.fullmatch(cleaned):
        raise CloudOpsError(f"GitHub 仓库格式无效（应为 owner/repo）: {cleaned}")
    return cleaned


def resolve_github_token(cfg: dict[str, str]) -> tuple[str, str]:
    """Return (source, token): 'config' | 'env' | 'gh_cli' | 'none'."""
    global _GH_CLI_CACHE
    tok = (cfg.get("DOSO_GITHUB_TOKEN") or "").strip()
    if tok:
        return "config", tok
    env_tok = (cfg.get("GITHUB_TOKEN") or "").strip()
    if env_tok:
        return "env", env_tok
    now = time.monotonic()
    if _GH_CLI_CACHE is not None and (now - _GH_CLI_CACHE[0]) < 120.0:
        return ("gh_cli", _GH_CLI_CACHE[1]) if _GH_CLI_CACHE[1] else ("none", "")
    try:
        proc = subprocess.run(
            ["gh", "auth", "token"],
            capture_output=True,
            text=True,
            timeout=3,
        )
        cli_tok = proc.stdout.strip() if proc.returncode == 0 else ""
    except Exception:
        cli_tok = ""
    _GH_CLI_CACHE = (now, cli_tok)
    return ("gh_cli", cli_tok) if cli_tok else ("none", "")


def _infer_release_channel(tag: str, prerelease: bool) -> str:
    low = (tag or "").lower()
    if "-dev" in low:
        return "dev"
    if prerelease or any(tok in low for tok in ("-qa", "-rc", "-beta", "-alpha", "-pre")):
        return "qa"
    return "stable"


def _asset_sort_key(name: str) -> tuple[int, str]:
    low = name.lower()
    if low.startswith("doso-brainstem-") and low.endswith(".tar.gz"):
        return (0, low)
    if low.endswith(".tar.gz") or low.endswith(".tgz"):
        return (1, low)
    if low.endswith(".apk"):
        return (2, low)
    return (3, low)


def list_github_releases(cfg: dict[str, str], repo: str = "", limit: int = 15) -> dict[str, Any]:
    repo_clean = validate_github_repo(repo or cfg.get("DOSO_GITHUB_REPO") or DEFAULT_GITHUB_REPO)
    auth_source, token = resolve_github_token(cfg)
    per_page = max(1, min(30, int(limit or 15)))
    url = f"https://api.github.com/repos/{repo_clean}/releases?per_page={per_page}"
    headers = {
        "Accept": "application/vnd.github+json",
        "User-Agent": "doso-ota-console/1.0",
        "X-GitHub-Api-Version": "2022-11-28",
    }
    if token:
        headers["Authorization"] = f"Bearer {token}"

    req = urllib.request.Request(url, headers=headers, method="GET")
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            raw = resp.read()
            payload = json.loads(raw.decode("utf-8"))
    except urllib.error.HTTPError as err:
        if err.code == 404:
            raise CloudOpsError(f"未找到仓库 {repo_clean} 或无访问权限，请在「系统配置」填入 GitHub 令牌") from err
        if err.code in (401, 403):
            raise CloudOpsError("GitHub 鉴权失败或频率受限，请在「系统配置」检查 GitHub 令牌") from err
        raise CloudOpsError(f"读取 GitHub Release 失败 (HTTP {err.code})") from err
    except Exception as exc:
        raise CloudOpsError(f"连接 GitHub 失败: {exc}") from exc

    releases: list[dict[str, Any]] = []
    for item in payload if isinstance(payload, list) else []:
        if not isinstance(item, dict) or item.get("draft"):
            continue
        tag = str(item.get("tag_name") or "").strip()
        if not tag:
            continue
        prerelease = bool(item.get("prerelease", False))
        assets: list[dict[str, Any]] = []
        for a in item.get("assets") or []:
            if not isinstance(a, dict):
                continue
            aname = str(a.get("name") or "").strip()
            if not aname.lower().endswith((".tar.gz", ".tgz", ".apk")):
                continue
            raw_digest = str(a.get("digest") or "").strip()
            sha = raw_digest.split(":", 1)[1].lower() if raw_digest.lower().startswith("sha256:") else ""
            assets.append(
                {
                    "id": int(a.get("id") or 0),
                    "name": aname,
                    "sizeBytes": int(a.get("size") or 0),
                    "sha256": sha,
                    "contentType": str(a.get("content_type") or ""),
                    "updatedAt": str(a.get("updated_at") or ""),
                }
            )
        assets.sort(key=lambda x: _asset_sort_key(x["name"]))
        if not assets:
            continue
        releases.append(
            {
                "id": int(item.get("id") or 0),
                "tagName": tag,
                "version": tag.lstrip("vV"),
                "name": str(item.get("name") or tag),
                "prerelease": prerelease,
                "suggestedChannel": _infer_release_channel(tag, prerelease),
                "publishedAt": str(item.get("published_at") or item.get("created_at") or ""),
                "htmlUrl": str(item.get("html_url") or ""),
                "assets": assets,
            }
        )

    return {
        "repo": repo_clean,
        "authSource": auth_source,
        "releases": releases,
    }


def download_github_release_asset(
    cfg: dict[str, str],
    repo: str,
    asset_id: int,
    dest_path: Path,
    *,
    expected_sha256: str = "",
    timeout_s: int = 120,
) -> dict[str, Any]:
    repo_clean = validate_github_repo(repo or cfg.get("DOSO_GITHUB_REPO") or DEFAULT_GITHUB_REPO)
    aid = int(asset_id or 0)
    if aid <= 0:
        raise CloudOpsError("无效的 GitHub 资产 ID")
    _, token = resolve_github_token(cfg)
    url = f"https://api.github.com/repos/{repo_clean}/releases/assets/{aid}"
    headers = {
        "Accept": "application/octet-stream",
        "User-Agent": "doso-ota-console/1.0",
        "X-GitHub-Api-Version": "2022-11-28",
    }
    if token:
        headers["Authorization"] = f"Bearer {token}"

    opener = urllib.request.build_opener(_NoAuthRedirectHandler())
    req = urllib.request.Request(url, headers=headers, method="GET")
    digest = hashlib.sha256()
    total = 0
    dest_path.parent.mkdir(parents=True, exist_ok=True)
    try:
        with opener.open(req, timeout=timeout_s) as resp, dest_path.open("wb") as fh:
            while chunk := resp.read(65536):
                fh.write(chunk)
                digest.update(chunk)
                total += len(chunk)
    except urllib.error.HTTPError as err:
        dest_path.unlink(missing_ok=True)
        raise CloudOpsError(f"下载 GitHub 固件包失败 (HTTP {err.code})") from err
    except Exception as exc:
        dest_path.unlink(missing_ok=True)
        raise CloudOpsError(f"下载 GitHub 固件包失败: {exc}") from exc

    if total <= 0:
        dest_path.unlink(missing_ok=True)
        raise CloudOpsError("下载的 GitHub 固件包为空")

    actual_sha = digest.hexdigest()
    exp = (expected_sha256 or "").strip().lower()
    if exp and actual_sha != exp:
        dest_path.unlink(missing_ok=True)
        raise CloudOpsError(f"GitHub 资产校验不一致: 预期 {exp[:12]}，实际 {actual_sha[:12]}")

    return {"path": str(dest_path), "sizeBytes": total, "sha256": actual_sha}
