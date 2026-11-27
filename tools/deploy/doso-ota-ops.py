#!/usr/bin/env python3
"""Zero-dependency production OTA ops CLI for packaging, OSS upload, templates, devices, and rollouts."""

from __future__ import annotations

import argparse
import base64
import hashlib
import hmac
import http.client
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from email.utils import formatdate
from pathlib import Path
from typing import Any

COMPONENT_RE = re.compile(r"^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$")
DEVICE_ID_RE = re.compile(r"^[A-Za-z0-9._-]{1,128}$")
ROLLOUT_ID_RE = re.compile(r"^[A-Za-z0-9._-]{1,128}$")
SEMVER_PRE_RE = re.compile(r"^[A-Za-z0-9.-]+$")
HEX64_RE = re.compile(r"^[0-9a-fA-F]{64}$")

ALLOWED_CHANNELS = ("dev", "qa", "stable")
ALLOWED_RINGS = ("dev", "canary", "prod")
ALLOWED_PACKAGE_TYPES = (
    "brainstem_release",
    "policy_package",
    "component_release",
)
ALLOWED_ARTIFACT_TYPES = ALLOWED_PACKAGE_TYPES + ("release_bundle",)


class OpsError(ValueError):
    """Actionable operator/validation error."""


def normalize_version(value: str) -> str:
    trimmed = value.strip()
    return trimmed[1:] if trimmed.startswith("v") else trimmed


def validate_semver(value: str, label: str = "version") -> str:
    norm = normalize_version(value)
    if "-" in norm:
        core, pre = norm.split("-", 1)
        if not pre or not SEMVER_PRE_RE.fullmatch(pre):
            raise OpsError(f"{label} invalid prerelease: {value!r}")
    else:
        core = norm
    parts = core.split(".")
    if len(parts) != 3 or not all(p.isdigit() for p in parts):
        raise OpsError(f"{label} must be SemVer major.minor.patch[-pre]: {value!r}")
    return value.strip()


def validate_component_name(name: str) -> str:
    if not COMPONENT_RE.fullmatch(name):
        raise OpsError(f"component must match [a-z0-9-] (1..64 chars, no edge hyphen): {name!r}")
    return name


def validate_device_id(device_id: str) -> str:
    cleaned = device_id.strip()
    if ".." in cleaned or not DEVICE_ID_RE.fullmatch(cleaned):
        raise OpsError(f"device_id invalid: {device_id!r}")
    return cleaned


def validate_release_id(release_id: str) -> str:
    cleaned = release_id.strip()
    if (
        not cleaned
        or len(cleaned) > 128
        or ".." in cleaned
        or "/" in cleaned
        or "\\" in cleaned
        or not ROLLOUT_ID_RE.fullmatch(cleaned)
    ):
        raise OpsError(f"release_id invalid: {release_id!r}")
    return cleaned


def validate_rel_path(raw: str) -> Path:
    text = raw.strip()
    if not text or "\0" in text:
        raise OpsError(f"invalid relative path: {raw!r}")
    if text.startswith(("/", "\\")) or (len(text) >= 2 and text[0].isalpha() and text[1] == ":"):
        raise OpsError(f"absolute or drive path rejected: {raw!r}")
    parts = [p for p in text.replace("\\", "/").split("/") if p not in ("", ".")]
    if not parts or ".." in parts:
        raise OpsError(f"path traversal rejected: {raw!r}")
    return Path(*parts)


def parse_policy_model_file(yaml_text: str) -> str:
    for line in yaml_text.splitlines():
        trimmed = line.strip()
        if trimmed.startswith("file:"):
            val = trimmed[len("file:") :].strip().strip('"').strip("'").strip()
            if val:
                return val
    return "model.onnx"


def sha256_file(path: Path) -> tuple[str, int]:
    digest = hashlib.sha256()
    total = 0
    with path.open("rb") as fh:
        while chunk := fh.read(65536):
            digest.update(chunk)
            total += len(chunk)
    return digest.hexdigest(), total


def md5_b64_file(path: Path) -> tuple[str, int]:
    digest = hashlib.md5()
    total = 0
    with path.open("rb") as fh:
        while chunk := fh.read(65536):
            digest.update(chunk)
            total += len(chunk)
    return base64.b64encode(digest.digest()).decode("ascii"), total


def is_elf_file(path: Path) -> bool:
    try:
        with path.open("rb") as fh:
            return fh.read(4) == b"\x7fELF"
    except OSError:
        return False


def desired_tar_mode(rel_posix: str, src_path: Path) -> int:
    if rel_posix.startswith("bin/") or rel_posix.endswith(".sh") or ".so" in Path(rel_posix).name:
        return 0o755
    if is_elf_file(src_path):
        return 0o755
    return 0o644


def atomic_write_bytes(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(prefix=f".{path.name}.", suffix=".tmp", dir=str(path.parent))
    tmp_path = Path(tmp_name)
    try:
        with os.fdopen(fd, "wb") as fh:
            fh.write(data)
            fh.flush()
            os.fsync(fh.fileno())
        os.replace(tmp_path, path)
    finally:
        if tmp_path.exists():
            tmp_path.unlink(missing_ok=True)


def build_package(
    source_dir: Path,
    output_archive: Path,
    artifact_type: str,
    component: str | None = None,
) -> dict[str, Any]:
    if artifact_type not in ALLOWED_PACKAGE_TYPES:
        raise OpsError(f"unsupported artifact_type for package: {artifact_type!r}")
    if not source_dir.is_dir():
        raise OpsError(f"source directory does not exist: {source_dir}")
    if artifact_type == "component_release":
        if not component:
            raise OpsError("--component is required for component_release")
        component = validate_component_name(component)

    root = source_dir.resolve()
    files: list[tuple[str, Path]] = []
    dirs: list[str] = []

    for cur_root, dirnames, filenames in os.walk(root, followlinks=False):
        dirnames.sort()
        filenames.sort()
        cur = Path(cur_root)
        for name in dirnames:
            p = cur / name
            if p.is_symlink():
                raise OpsError(f"directory symlink rejected in package source: {p}")
            rel = p.relative_to(root).as_posix()
            dirs.append(rel)
        for name in filenames:
            p = cur / name
            if p.is_symlink():
                target = os.readlink(p)
                if os.path.isabs(target):
                    raise OpsError(f"absolute symlink rejected: {p} -> {target}")
                resolved = (p.parent / target).resolve()
                if root not in resolved.parents and resolved != root:
                    raise OpsError(f"escaping symlink rejected: {p} -> {target}")
                raise OpsError(f"symlink entries are disallowed in generated packages: {p}")
            rel = p.relative_to(root).as_posix()
            if rel == "SHA256SUMS":
                continue
            files.append((rel, p))

    if not files:
        raise OpsError(f"source directory has no payload files: {source_dir}")

    rel_set = {rel for rel, _ in files}
    if artifact_type == "brainstem_release" or (
        artifact_type == "component_release" and component == "brainstem"
    ):
        if "bin/doso-brainstem" not in rel_set:
            raise OpsError("brainstem package must contain bin/doso-brainstem")
    elif artifact_type == "policy_package":
        if "policy.yaml" not in rel_set:
            raise OpsError("policy package must contain policy.yaml")
        model_rel = parse_policy_model_file((root / "policy.yaml").read_text(encoding="utf-8"))
        norm_model = validate_rel_path(model_rel).as_posix()
        if norm_model not in rel_set:
            raise OpsError(f"policy.yaml references missing model file: {norm_model}")
        if (root / norm_model).stat().st_size == 0:
            raise OpsError(f"policy model file is empty: {norm_model}")

    sums_lines: list[str] = []
    for rel, path in sorted(files, key=lambda item: item[0]):
        digest, _ = sha256_file(path)
        sums_lines.append(f"{digest}  {rel}\n")
    sums_bytes = "".join(sums_lines).encode("utf-8")

    output_archive.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(
        prefix=f".{output_archive.name}.", suffix=".tmp", dir=str(output_archive.parent)
    )
    os.close(fd)
    tmp_archive = Path(tmp_name)
    try:
        with tarfile.open(tmp_archive, "w:gz", format=tarfile.USTAR_FORMAT) as tf:
            for d_rel in sorted(dirs):
                info = tarfile.TarInfo(name=d_rel)
                info.type = tarfile.DIRTYPE
                info.mode = 0o755
                info.uid = 0
                info.gid = 0
                info.uname = "root"
                info.gname = "root"
                tf.addfile(info)

            for rel, src_path in sorted(files, key=lambda item: item[0]):
                info = tarfile.TarInfo(name=rel)
                info.size = src_path.stat().st_size
                info.mode = desired_tar_mode(rel, src_path)
                info.uid = 0
                info.gid = 0
                info.uname = "root"
                info.gname = "root"
                info.mtime = int(src_path.stat().st_mtime)
                with src_path.open("rb") as fh:
                    tf.addfile(info, fh)

            sums_info = tarfile.TarInfo(name="SHA256SUMS")
            sums_info.size = len(sums_bytes)
            sums_info.mode = 0o644
            sums_info.uid = 0
            sums_info.gid = 0
            sums_info.uname = "root"
            sums_info.gname = "root"
            with tempfile.TemporaryFile() as buf:
                buf.write(sums_bytes)
                buf.seek(0)
                tf.addfile(sums_info, buf)

        os.replace(tmp_archive, output_archive)
    finally:
        if tmp_archive.exists():
            tmp_archive.unlink(missing_ok=True)

    archive_sha256, archive_size = sha256_file(output_archive)
    return {
        "output": str(output_archive),
        "artifactType": artifact_type,
        "component": component,
        "sizeBytes": archive_size,
        "sha256": archive_sha256,
        "fileCount": len(files) + 1,
    }


def sign_oss_header(
    method: str,
    bucket: str,
    object_key: str,
    ak_id: str,
    ak_secret: str,
    date_hdr: str,
    content_md5: str = "",
    content_type: str = "",
) -> str:
    clean_key = object_key.lstrip("/")
    canonical_resource = f"/{bucket}/{clean_key}"
    string_to_sign = f"{method}\n{content_md5}\n{content_type}\n{date_hdr}\n{canonical_resource}"
    sig = base64.b64encode(
        hmac.new(ak_secret.encode("utf-8"), string_to_sign.encode("utf-8"), hashlib.sha1).digest()
    ).decode("ascii")
    return f"OSS {ak_id}:{sig}"


def oss_request(
    method: str,
    bucket: str,
    endpoint: str,
    object_key: str,
    ak_id: str,
    ak_secret: str,
    body_path: Path | None = None,
    content_type: str = "",
    timeout_s: int = 120,
) -> tuple[int, dict[str, str], bytes]:
    clean_key = object_key.lstrip("/")
    date_hdr = formatdate(usegmt=True)
    content_md5 = ""
    size = 0
    if body_path is not None:
        content_md5, size = md5_b64_file(body_path)
    auth = sign_oss_header(
        method=method,
        bucket=bucket,
        object_key=clean_key,
        ak_id=ak_id,
        ak_secret=ak_secret,
        date_hdr=date_hdr,
        content_md5=content_md5,
        content_type=content_type,
    )
    host = f"{bucket}.{endpoint}"
    headers: dict[str, str] = {
        "Host": host,
        "Date": date_hdr,
        "Authorization": auth,
    }
    if content_type:
        headers["Content-Type"] = content_type
    if content_md5:
        headers["Content-MD5"] = content_md5
    if body_path is not None:
        headers["Content-Length"] = str(size)

    conn = http.client.HTTPSConnection(host, timeout=timeout_s)
    try:
        url_path = "/" + urllib.parse.quote(clean_key, safe="/-_.~")
        if body_path is not None:
            conn.putrequest(method, url_path)
            for k, v in headers.items():
                conn.putheader(k, v)
            conn.endheaders()
            with body_path.open("rb") as fh:
                while chunk := fh.read(65536):
                    conn.send(chunk)
        else:
            conn.request(method, url_path, headers=headers)
        resp = conn.getresponse()
        resp_body = resp.read()
        resp_headers = {k.lower(): v for k, v in resp.getheaders()}
        return resp.status, resp_headers, resp_body
    finally:
        conn.close()


def render_template(args: argparse.Namespace) -> dict[str, Any]:
    release_id = validate_release_id(args.release_id)
    if args.channel not in ALLOWED_CHANNELS:
        raise OpsError(f"channel must be one of {ALLOWED_CHANNELS}: {args.channel!r}")
    if args.artifact_type not in ALLOWED_ARTIFACT_TYPES:
        raise OpsError(f"artifactType must be one of {ALLOWED_ARTIFACT_TYPES}: {args.artifact_type!r}")
    if args.sequence <= 0:
        raise OpsError("sequence must be > 0")
    version = validate_semver(args.version)
    min_bootloader = validate_semver(args.min_bootloader, "minimumBootloaderVersion")
    min_brainstem = validate_semver(args.min_brainstem, "minimumBrainstemVersion")
    if not (0 <= args.min_battery <= 100):
        raise OpsError("minimumBatteryPercent must be 0..100")
    if args.health_timeout_ms <= 0:
        raise OpsError("healthTimeoutMs must be > 0")

    object_key = (args.object_key or "").lstrip("/")
    bundle_items: list[dict[str, Any]] = []
    bundle_object_keys: dict[str, str] = {}

    if args.artifact_type == "release_bundle":
        if not args.bundle_spec:
            raise OpsError("--bundle-spec is required for release_bundle")
        raw_items = json.loads(args.bundle_spec.read_text(encoding="utf-8"))
        if not isinstance(raw_items, list) or not raw_items:
            raise OpsError("--bundle-spec must be a non-empty JSON array")
        seen_components: set[str] = set()
        for entry in raw_items:
            if not isinstance(entry, dict):
                raise OpsError("bundle item must be a JSON object")
            comp = validate_component_name(str(entry.get("component", "")))
            if comp == "ota":
                raise OpsError("component 'ota' cannot be included in release_bundle")
            if comp in seen_components:
                raise OpsError(f"duplicate component in bundle: {comp}")
            seen_components.add(comp)
            item_ver = validate_semver(str(entry.get("version", "")), f"bundle[{comp}].version")
            item_obj_key = str(entry.get("objectKey", "")).lstrip("/")
            if not item_obj_key:
                raise OpsError(f"bundle[{comp}].objectKey is required")
            if "artifact" in entry and entry["artifact"]:
                item_path = Path(entry["artifact"])
                if not item_path.is_file():
                    raise OpsError(f"bundle[{comp}] artifact not found: {item_path}")
                item_sha, item_size = sha256_file(item_path)
            else:
                item_size = int(entry.get("artifactSizeBytes", 0))
                item_sha = str(entry.get("artifactSha256", "")).lower()
                if item_size <= 0 or not HEX64_RE.fullmatch(item_sha):
                    raise OpsError(f"bundle[{comp}] requires artifact or valid artifactSizeBytes/artifactSha256")
            bundle_obj: dict[str, Any] = {
                "component": comp,
                "version": item_ver,
                "artifactUrl": "https://placeholder.invalid/signed-at-dispatch",
                "artifactSizeBytes": item_size,
                "artifactSha256": item_sha,
            }
            for opt_key in ("systemdUnit", "healthUrl", "requires"):
                if entry.get(opt_key):
                    bundle_obj[opt_key] = entry[opt_key]
            bundle_items.append(bundle_obj)
            bundle_object_keys[comp] = item_obj_key
        size_bytes = 0
        sha256 = ""
        artifact_url = ""
    else:
        if not object_key:
            raise OpsError("objectKey must be non-empty")
        if args.artifact is not None:
            if not args.artifact.is_file():
                raise OpsError(f"artifact file not found: {args.artifact}")
            sha256, size_bytes = sha256_file(args.artifact)
        else:
            if not args.size_bytes or args.size_bytes <= 0:
                raise OpsError("provide --artifact or positive --size-bytes")
            if not args.sha256 or not HEX64_RE.fullmatch(args.sha256):
                raise OpsError("provide --artifact or valid 64-hex --sha256")
            size_bytes = args.size_bytes
            sha256 = args.sha256.lower()
        artifact_url = "https://placeholder.invalid/signed-at-dispatch"

    requires: dict[str, str] = {}
    for item in args.requires or []:
        if "=" not in item:
            raise OpsError(f"--requires entry must be component=semver: {item!r}")
        dep, dep_ver = item.split("=", 1)
        requires[validate_component_name(dep.strip())] = validate_semver(dep_ver.strip(), f"requires[{dep}]")

    manifest: dict[str, Any] = {
        "manifestVersion": 1,
        "releaseId": release_id,
        "channel": args.channel,
        "sequence": args.sequence,
        "version": version,
        "issuedAtMs": 0,
        "expiresAtMs": args.manifest_ttl_seconds * 1000 if args.manifest_ttl_seconds > 0 else 0,
        "model": args.model,
        "hardwareRevision": args.hardware_revision,
        "minimumBootloaderVersion": min_bootloader,
        "minimumBrainstemVersion": min_brainstem,
        "minimumBatteryPercent": args.min_battery,
        "artifactUrl": artifact_url,
        "artifactSizeBytes": size_bytes,
        "artifactSha256": sha256,
        "keyId": args.key_id,
        "rollbackPolicy": {
            "mode": "automatic",
            "healthTimeoutMs": args.health_timeout_ms,
        },
        "artifactType": args.artifact_type,
    }

    if args.artifact_type == "policy_package":
        if not args.policy_slot:
            raise OpsError("--policy-slot is required for policy_package")
        manifest["policySlot"] = args.policy_slot
        manifest["shadowFrames"] = args.shadow_frames
        manifest["activate"] = not args.no_activate
    elif args.artifact_type == "component_release":
        if not args.component:
            raise OpsError("--component is required for component_release")
        manifest["component"] = validate_component_name(args.component)
    elif args.artifact_type == "release_bundle":
        manifest["bundleItems"] = bundle_items

    if args.systemd_unit:
        manifest["systemdUnit"] = args.systemd_unit
    if args.health_url:
        manifest["healthUrl"] = args.health_url
    if requires:
        manifest["requires"] = requires

    out: dict[str, Any] = {
        "manifest": manifest,
        "objectKey": object_key,
    }
    if bundle_object_keys:
        out["bundleObjectKeys"] = bundle_object_keys
    if args.delta_object_key:
        out["deltaObjectKey"] = args.delta_object_key.lstrip("/")
    return out


def provision_device(
    devices_file: Path,
    device_id: str,
    model: str,
    hardware_revision: str,
    ring: str,
    tags: list[str],
    enabled: bool,
    dry_run: bool,
) -> dict[str, Any]:
    clean_id = validate_device_id(device_id)
    if ring not in ALLOWED_RINGS:
        raise OpsError(f"ring must be one of {ALLOWED_RINGS}: {ring!r}")
    if not model.strip() or not hardware_revision.strip():
        raise OpsError("model and hardwareRevision must be non-empty")

    if devices_file.exists():
        raw = json.loads(devices_file.read_text(encoding="utf-8"))
        if not isinstance(raw, dict):
            raise OpsError(f"{devices_file} must be a JSON object mapping deviceId -> DeviceRecord")
        if "devices" in raw and isinstance(raw["devices"], list):
            raise OpsError(
                f"{devices_file} looks like Gateway's array registry; OTA release registry uses {{deviceId: record}}"
            )
    else:
        raw = {}

    dedup_tags = list(dict.fromkeys(t.strip() for t in tags if t.strip()))
    record = {
        "model": model.strip(),
        "hardwareRevision": hardware_revision.strip(),
        "enabled": enabled,
        "ring": ring,
        "tags": dedup_tags,
    }
    raw[clean_id] = record
    if not dry_run:
        atomic_write_bytes(devices_file, (json.dumps(raw, indent=2, ensure_ascii=False) + "\n").encode("utf-8"))
    return {"deviceId": clean_id, "record": record, "dryRun": dry_run, "devicesFile": str(devices_file)}


def render_robot_env(args: argparse.Namespace) -> str:
    clean_id = validate_device_id(args.device_id)
    ver = validate_semver(args.brainstem_version, "brainstemVersion")
    lines = [
        f"DOSO_DEVICE_ID={clean_id}",
        f"DOSO_MODEL={args.model}",
        f"DOSO_HARDWARE_REVISION={args.hardware_revision}",
        f"DOSO_BOOTLOADER_VERSION={validate_semver(args.bootloader_version, 'bootloaderVersion')}",
        f"DOSO_BRAINSTEM_VERSION={ver}",
        f"DOSO_BRAINSTEM_RELEASE={args.brainstem_release}",
        "DOSO_BATTERY_PERCENT=100",
        "",
        f"DOSO_OTA_VERIFY_KEY={args.verify_key}",
        "DOSO_DEVICE_ONLINE=true",
        "DOSO_TASK_RUNNING=false",
        "DOSO_OTA_ROOT=/var/lib/doso-ota",
        "DOSO_OTA_STAGING_DIR=/ota/staging",
        "DOSO_OTA_RELEASES_DIR=/opt/doso/releases",
        "DOSO_OTA_CURRENT_LINK=/opt/doso/current",
        "DOSO_OTA_LAST_GOOD_LINK=/opt/doso/last-known-good",
        "DOSO_OTA_COMPONENTS_ROOT=/opt/doso/components",
        "DOSO_OTA_KEEP_HISTORY=2",
        "DOSO_OTA_UNIT=doso-ota.service",
        "DOSO_OTA_BIND=127.0.0.1:13147",
        "DOSO_OTA_TOKEN=",
        "DOSO_BRAINSTEM_UNIT=doso-brainstem.service",
        "",
        "DOSO_OTA_BOOT_ID=/run/doso/brainstem.boot_id",
        "DOSO_OTA_BRAINSTEM_HEALTH_URL=http://127.0.0.1:5054/health",
        "DOSO_OTA_REQUIRE_CAN_READ_ONLY=false",
        "DOSO_OTA_MIN_VBUS_V=44.0",
        "",
        "DOSO_POLICY_ADMIN_URL=http://127.0.0.1:5054",
        "# DOSO_POLICY_ADMIN_TOKEN=",
        "",
        f"DOSO_OTA_CLOUD_URL={args.cloud_url.rstrip('/')}",
        "# Populate on-device only; never commit.",
        "DOSO_OTA_CLOUD_TOKEN=",
        f"DOSO_OTA_POLL_INTERVAL_SECS={args.poll_interval_secs}",
        "",
    ]
    return "\n".join(lines)


def b64url_decode_nopad(data: str) -> bytes:
    pad = "=" * ((4 - len(data) % 4) % 4)
    return base64.urlsafe_b64decode(data + pad)


def verify_manifest_envelope(envelope_path: Path, pub_key_path: Path) -> dict[str, Any]:
    envelope = json.loads(envelope_path.read_text(encoding="utf-8"))
    if not isinstance(envelope, dict):
        raise OpsError("envelope JSON must be an object")
    if "manifest" in envelope and isinstance(envelope["manifest"], dict):
        envelope = envelope["manifest"]
    payload_b64 = envelope.get("payload")
    sig_b64 = envelope.get("signature")
    if not isinstance(payload_b64, str) or not isinstance(sig_b64, str):
        raise OpsError("envelope must contain string payload and signature")
    payload = b64url_decode_nopad(payload_b64)
    sig = b64url_decode_nopad(sig_b64)
    if len(sig) != 64:
        raise OpsError(f"signature must be 64 bytes, got {len(sig)}")

    verified = False
    try:
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
        from cryptography.hazmat.primitives.serialization import load_pem_public_key

        pub = load_pem_public_key(pub_key_path.read_bytes())
        if not isinstance(pub, Ed25519PublicKey):
            raise OpsError(f"public key is not Ed25519: {pub_key_path}")
        pub.verify(sig, payload)
        verified = True
    except ImportError:
        openssl_bin = shutil.which("openssl")
        if not openssl_bin:
            raise OpsError("neither Python cryptography nor openssl CLI is available for Ed25519 verify")
        with tempfile.TemporaryDirectory() as tmp:
            tdir = Path(tmp)
            p_file = tdir / "payload.bin"
            s_file = tdir / "sig.bin"
            p_file.write_bytes(payload)
            s_file.write_bytes(sig)
            res = subprocess.run(
                [
                    openssl_bin,
                    "pkeyutl",
                    "-verify",
                    "-pubin",
                    "-inkey",
                    str(pub_key_path),
                    "-rawin",
                    "-in",
                    str(p_file),
                    "-sigfile",
                    str(s_file),
                ],
                capture_output=True,
                text=True,
            )
            if res.returncode != 0:
                raise OpsError(f"manifest signature verification failed: {res.stderr.strip()}")
            verified = True
    except Exception as exc:
        raise OpsError(f"manifest signature verification failed: {exc}") from exc

    if not verified:
        raise OpsError("manifest signature verification failed")
    return json.loads(payload.decode("utf-8"))


def api_call(
    base_url: str,
    method: str,
    path: str,
    token: str | None,
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
            return resp.status, json.loads(raw.decode("utf-8")) if raw else None
    except urllib.error.HTTPError as err:
        raw = err.read()
        parsed: Any
        try:
            parsed = json.loads(raw.decode("utf-8")) if raw else {"status": err.code}
        except ValueError:
            parsed = {"status": err.code, "body": raw.decode("utf-8", errors="replace")}
        raise OpsError(f"{method} {path} failed HTTP {err.code}: {json.dumps(parsed, ensure_ascii=False)}") from err


def resolve_token(args: argparse.Namespace, required: bool = True) -> str | None:
    env_name = getattr(args, "token_env", "DOSO_RELEASE_API_TOKEN")
    token = os.environ.get(env_name, "").strip()
    if required and not token:
        raise OpsError(f"set environment variable {env_name} with the Bearer API token")
    return token or None


def cmd_package(args: argparse.Namespace) -> int:
    result = build_package(args.source_dir, args.output, args.artifact_type, args.component)
    print(json.dumps(result, indent=2, ensure_ascii=False))
    return 0


TERMINAL_JOB_PHASES = frozenset({"succeeded", "failed", "rolled_back", "recovery_required", "cancelled"})


def default_robot_object_key(
    artifact_type: str,
    channel: str,
    release_id: str,
    policy_slot: str | None = None,
    component: str | None = None,
) -> str:
    clean_id = validate_release_id(release_id)
    if artifact_type == "policy_package":
        slot = (policy_slot or "policy").strip()
        return f"policies/{channel}/{clean_id}/{slot}.tar.gz"
    if artifact_type == "component_release":
        comp = validate_component_name(component or "")
        return f"components/{comp}/{channel}/{clean_id}/{comp}.tar.gz"
    return f"firmware/{channel}/{clean_id}/brainstem.tar.gz"


def default_android_object_key(channel: str, version_name: str, version_code: int) -> str:
    clean_ver = validate_semver(version_name, "versionName")
    clean_channel = channel.strip()
    if not clean_channel:
        raise OpsError("channel must be non-empty")
    return f"apps/android/{clean_channel}/{clean_ver}/doso-{clean_ver}-vc{version_code}.apk"


def upload_to_oss(
    file_path: Path,
    object_key: str,
    bucket: str,
    endpoint: str,
    content_type: str,
    overwrite: bool = False,
    dry_run: bool = False,
    ak_id: str | None = None,
    ak_secret: str | None = None,
) -> dict[str, Any]:
    if not file_path.is_file():
        raise OpsError(f"file not found: {file_path}")
    clean_key = object_key.lstrip("/")
    if not clean_key:
        raise OpsError("object_key must be non-empty")
    sha256, size = sha256_file(file_path)
    md5_b64, _ = md5_b64_file(file_path)

    resolved_ak_id = (ak_id if ak_id is not None else os.environ.get("DOSO_OSS_ACCESS_KEY_ID", "")).strip()
    resolved_ak_secret = (
        ak_secret if ak_secret is not None else os.environ.get("DOSO_OSS_ACCESS_KEY_SECRET", "")
    ).strip()

    if dry_run:
        date_hdr = formatdate(usegmt=True)
        auth = sign_oss_header(
            method="PUT",
            bucket=bucket,
            object_key=clean_key,
            ak_id=resolved_ak_id or "DRY_RUN_AK",
            ak_secret=resolved_ak_secret or "DRY_RUN_SK",
            date_hdr=date_hdr,
            content_md5=md5_b64,
            content_type=content_type,
        )
        return {
            "dryRun": True,
            "url": f"https://{bucket}.{endpoint}/{clean_key}",
            "objectKey": clean_key,
            "sizeBytes": size,
            "sha256": sha256,
            "contentMd5": md5_b64,
            "authScheme": auth.split(" ", 1)[0],
        }

    if not resolved_ak_id or not resolved_ak_secret:
        raise OpsError("set DOSO_OSS_ACCESS_KEY_ID and DOSO_OSS_ACCESS_KEY_SECRET in environment")

    if not overwrite:
        head_status, head_headers, _ = oss_request(
            "HEAD", bucket, endpoint, clean_key, resolved_ak_id, resolved_ak_secret
        )
        if head_status == 200:
            raise OpsError(
                f"immutable OSS object already exists ({clean_key}, size={head_headers.get('content-length')}); pass --overwrite only if replacing an unpublished object"
            )
        if head_status != 404:
            raise OpsError(f"OSS HEAD {clean_key} returned unexpected HTTP {head_status}")

    status, headers, body = oss_request(
        "PUT",
        bucket,
        endpoint,
        clean_key,
        resolved_ak_id,
        resolved_ak_secret,
        body_path=file_path,
        content_type=content_type,
    )
    if status not in (200, 201):
        raise OpsError(f"OSS PUT failed HTTP {status}: {body.decode('utf-8', errors='replace')}")
    return {
        "uploaded": True,
        "url": f"https://{bucket}.{endpoint}/{clean_key}",
        "objectKey": clean_key,
        "sizeBytes": size,
        "sha256": sha256,
        "etag": headers.get("etag", "").strip('"'),
    }


def watch_device_job(
    base_url: str,
    token: str | None,
    device_id: str,
    job_id: str,
    poll_interval_s: float = 3.0,
    timeout_s: float = 300.0,
) -> dict[str, Any]:
    dev = urllib.parse.quote(validate_device_id(device_id))
    jid = urllib.parse.quote(job_id)
    deadline = time.monotonic() + max(1.0, timeout_s)
    last_sig: tuple[Any, ...] | None = None
    while True:
        _, job = api_call(base_url, "GET", f"/v1/devices/{dev}/firmware/jobs/{jid}", token)
        phase = str(job.get("phase", ""))
        downloaded = job.get("bytesDownloaded", 0)
        message = job.get("message")
        sig = (phase, downloaded, message)
        if sig != last_sig:
            extra = f" bytes={downloaded}" if downloaded else ""
            if message:
                extra += f" msg={message}"
            print(f"[ota-watch] device={device_id} job={job_id} phase={phase}{extra}", file=sys.stderr)
            last_sig = sig
        if phase in TERMINAL_JOB_PHASES:
            return job
        if time.monotonic() >= deadline:
            raise OpsError(f"timed out waiting for job {job_id} (last phase={phase})")
        time.sleep(max(0.05, poll_interval_s))


def cmd_oss_upload(args: argparse.Namespace) -> int:
    result = upload_to_oss(
        file_path=args.file,
        object_key=args.object_key,
        bucket=args.bucket,
        endpoint=args.endpoint,
        content_type=args.content_type,
        overwrite=args.overwrite,
        dry_run=args.dry_run,
    )
    print(json.dumps(result, indent=2, ensure_ascii=False))
    return 0


def cmd_render_template(args: argparse.Namespace) -> int:
    tpl = render_template(args)
    encoded = (json.dumps(tpl, indent=2, ensure_ascii=False) + "\n").encode("utf-8")
    if args.output:
        atomic_write_bytes(args.output, encoded)
    else:
        sys.stdout.buffer.write(encoded)
    return 0


def cmd_provision_device(args: argparse.Namespace) -> int:
    res = provision_device(
        devices_file=args.devices_file,
        device_id=args.device_id,
        model=args.model,
        hardware_revision=args.hardware_revision,
        ring=args.ring,
        tags=args.tag or [],
        enabled=not args.disabled,
        dry_run=args.dry_run,
    )
    print(json.dumps(res, indent=2, ensure_ascii=False))
    return 0


def cmd_render_robot_env(args: argparse.Namespace) -> int:
    content = render_robot_env(args).encode("utf-8")
    if args.output:
        atomic_write_bytes(args.output, content)
    else:
        sys.stdout.buffer.write(content)
    return 0


def cmd_verify_manifest(args: argparse.Namespace) -> int:
    decoded = verify_manifest_envelope(args.envelope, args.public_key)
    print(json.dumps(decoded, indent=2, ensure_ascii=False))
    return 0


def cmd_rollout(args: argparse.Namespace) -> int:
    token = resolve_token(args)
    action = args.rollout_cmd
    if action == "create":
        if args.spec:
            payload = json.loads(args.spec.read_text(encoding="utf-8"))
        else:
            if not args.release_id:
                raise OpsError("provide --spec or --release-id")
            if args.stages_json:
                stages = json.loads(args.stages_json)
            else:
                stages = [
                    {
                        "name": "canary",
                        "ring": "canary",
                        "percent": 100,
                        "maxDevices": args.canary_max_devices,
                        "bakeSeconds": args.canary_bake_seconds,
                        "autoPromote": False,
                    },
                    {
                        "name": "prod-10",
                        "ring": "prod",
                        "percent": 10,
                        "bakeSeconds": args.prod_bake_seconds,
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
            payload = {
                "releaseId": validate_release_id(args.release_id),
                "stages": stages,
                "circuitBreaker": {
                    "maxFailureCount": args.max_failure_count,
                    "maxFailureRate": args.max_failure_rate,
                },
            }
            if args.rollout_id:
                payload["rolloutId"] = args.rollout_id
            if args.model:
                payload["targetModel"] = args.model
            if args.hardware_revision:
                payload["targetHardwareRevision"] = args.hardware_revision
            if args.tag:
                payload["targetTags"] = args.tag
        _, data = api_call(args.base_url, "POST", "/v1/rollouts", token, body=payload)
    elif action == "list":
        _, data = api_call(args.base_url, "GET", "/v1/rollouts", token)
    elif action == "status":
        _, data = api_call(args.base_url, "GET", f"/v1/rollouts/{urllib.parse.quote(args.rollout_id)}", token)
    elif action in ("promote", "pause", "resume", "abort"):
        _, data = api_call(
            args.base_url,
            "POST",
            f"/v1/rollouts/{urllib.parse.quote(args.rollout_id)}/{action}",
            token,
        )
    else:
        raise OpsError(f"unknown rollout action: {action}")
    print(json.dumps(data, indent=2, ensure_ascii=False))
    return 0


def cmd_job(args: argparse.Namespace) -> int:
    token = resolve_token(args)
    dev = urllib.parse.quote(validate_device_id(args.device_id))
    action = args.job_cmd
    if action == "start":
        payload = {
            "releaseId": validate_release_id(args.release_id),
            "idempotencyKey": args.idempotency_key,
        }
        _, data = api_call(args.base_url, "POST", f"/v1/devices/{dev}/firmware/jobs", token, body=payload)
    elif action == "status":
        jid = urllib.parse.quote(args.job_id)
        _, data = api_call(args.base_url, "GET", f"/v1/devices/{dev}/firmware/jobs/{jid}", token)
    elif action == "cancel":
        jid = urllib.parse.quote(args.job_id)
        _, data = api_call(args.base_url, "POST", f"/v1/devices/{dev}/firmware/jobs/{jid}/cancel", token)
    else:
        raise OpsError(f"unknown job action: {action}")
    print(json.dumps(data, indent=2, ensure_ascii=False))
    return 0


def cmd_releases(args: argparse.Namespace) -> int:
    token = resolve_token(args)
    if args.device_id:
        dev = urllib.parse.quote(validate_device_id(args.device_id))
        _, data = api_call(args.base_url, "GET", f"/v1/devices/{dev}/firmware/releases", token)
    else:
        _, data = api_call(args.base_url, "GET", "/v1/releases", token)
    print(json.dumps(data, indent=2, ensure_ascii=False))
    return 0


def cmd_register_release(args: argparse.Namespace) -> int:
    token = resolve_token(args)
    if not args.template.is_file():
        raise OpsError(f"template file not found: {args.template}")
    payload = json.loads(args.template.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise OpsError("template JSON must be an object")
    status, data = api_call(args.base_url, "POST", "/v1/releases", token, body=payload)
    print(json.dumps({"httpStatus": status, "release": data}, indent=2, ensure_ascii=False))
    return 0


def cmd_publish(args: argparse.Namespace) -> int:
    if bool(args.source_dir) == bool(args.artifact):
        raise OpsError("provide exactly one of --source-dir or --artifact")
    if args.device_id and args.rollout_id:
        raise OpsError("provide at most one of --device-id or --rollout-id")
    if args.watch and not args.device_id:
        raise OpsError("--watch requires --device-id")

    release_id = validate_release_id(args.release_id)
    object_key = (
        args.object_key.lstrip("/")
        if args.object_key
        else default_robot_object_key(
            args.artifact_type,
            args.channel,
            release_id,
            policy_slot=args.policy_slot,
            component=args.component,
        )
    )

    tmp_ctx = tempfile.TemporaryDirectory() if (args.source_dir and not args.output) else None
    try:
        package_meta: dict[str, Any] | None = None
        if args.source_dir:
            tar_path = args.output if args.output else Path(tmp_ctx.name) / f"{release_id}.tar.gz"  # type: ignore[union-attr]
            package_meta = build_package(args.source_dir, tar_path, args.artifact_type, args.component)
            artifact_path = tar_path
        else:
            artifact_path = args.artifact
            if not artifact_path.is_file():
                raise OpsError(f"artifact file not found: {artifact_path}")

        tpl_args = argparse.Namespace(
            release_id=release_id,
            version=args.version,
            sequence=args.sequence,
            channel=args.channel,
            artifact_type=args.artifact_type,
            object_key=object_key,
            bundle_spec=None,
            delta_object_key=args.delta_object_key,
            artifact=artifact_path,
            size_bytes=None,
            sha256=None,
            model=args.model,
            hardware_revision=args.hardware_revision,
            min_bootloader=args.min_bootloader,
            min_brainstem=args.min_brainstem,
            min_battery=args.min_battery,
            health_timeout_ms=args.health_timeout_ms,
            manifest_ttl_seconds=args.manifest_ttl_seconds,
            key_id=args.key_id,
            policy_slot=args.policy_slot,
            shadow_frames=args.shadow_frames,
            no_activate=args.no_activate,
            component=args.component,
            systemd_unit=args.systemd_unit,
            health_url=args.health_url,
            requires=args.requires,
        )
        template = render_template(tpl_args)
        if args.save_template:
            atomic_write_bytes(
                args.save_template,
                (json.dumps(template, indent=2, ensure_ascii=False) + "\n").encode("utf-8"),
            )

        upload_result: dict[str, Any] | None = None
        if not args.skip_upload:
            upload_result = upload_to_oss(
                file_path=artifact_path,
                object_key=object_key,
                bucket=args.bucket,
                endpoint=args.endpoint,
                content_type="application/gzip",
                overwrite=args.overwrite,
                dry_run=args.dry_run,
            )

        if args.dry_run:
            summary: dict[str, Any] = {
                "dryRun": True,
                "releaseId": release_id,
                "objectKey": object_key,
                "package": package_meta,
                "upload": upload_result,
                "template": template,
            }
            if args.device_id:
                summary["plannedJob"] = {
                    "deviceId": validate_device_id(args.device_id),
                    "releaseId": release_id,
                    "idempotencyKey": args.idempotency_key or f"pub-{release_id}-{args.device_id}",
                    "watch": bool(args.watch),
                }
            if args.rollout_id:
                summary["plannedRollout"] = {
                    "rolloutId": args.rollout_id,
                    "releaseId": release_id,
                }
            print(json.dumps(summary, indent=2, ensure_ascii=False))
            return 0

        token = resolve_token(args)
        reg_status, registered = api_call(args.base_url, "POST", "/v1/releases", token, body=template)

        result: dict[str, Any] = {
            "releaseId": release_id,
            "objectKey": object_key,
            "package": package_meta,
            "upload": upload_result,
            "registerStatus": reg_status,
            "release": registered,
        }

        if args.device_id:
            dev = urllib.parse.quote(validate_device_id(args.device_id))
            idem = args.idempotency_key or f"pub-{release_id}-{args.device_id}"
            _, job = api_call(
                args.base_url,
                "POST",
                f"/v1/devices/{dev}/firmware/jobs",
                token,
                body={"releaseId": release_id, "idempotencyKey": idem},
            )
            result["job"] = job
            if args.watch:
                final_job = watch_device_job(
                    args.base_url,
                    token,
                    args.device_id,
                    str(job["jobId"]),
                    poll_interval_s=args.poll_interval,
                    timeout_s=args.watch_timeout,
                )
                result["job"] = final_job
                print(json.dumps(result, indent=2, ensure_ascii=False))
                return 0 if final_job.get("phase") == "succeeded" else 1
        elif args.rollout_id:
            rollout_payload: dict[str, Any] = {
                "rolloutId": args.rollout_id,
                "releaseId": release_id,
                "targetModel": args.model,
                "targetHardwareRevision": args.hardware_revision,
                "stages": [
                    {
                        "name": "canary",
                        "ring": "canary",
                        "percent": 100,
                        "maxDevices": args.canary_max_devices,
                        "bakeSeconds": args.canary_bake_seconds,
                        "autoPromote": False,
                    },
                    {
                        "name": "prod-10",
                        "ring": "prod",
                        "percent": 10,
                        "bakeSeconds": args.prod_bake_seconds,
                        "autoPromote": False,
                    },
                    {
                        "name": "prod-100",
                        "ring": "prod",
                        "percent": 100,
                        "bakeSeconds": 0,
                        "autoPromote": False,
                    },
                ],
                "circuitBreaker": {
                    "maxFailureCount": args.max_failure_count,
                    "maxFailureRate": args.max_failure_rate,
                },
            }
            if args.tag:
                rollout_payload["targetTags"] = args.tag
            _, rollout = api_call(args.base_url, "POST", "/v1/rollouts", token, body=rollout_payload)
            result["rollout"] = rollout

        print(json.dumps(result, indent=2, ensure_ascii=False))
        return 0
    finally:
        if tmp_ctx is not None:
            tmp_ctx.cleanup()


def cmd_publish_android(args: argparse.Namespace) -> int:
    if not args.apk.is_file():
        raise OpsError(f"APK file not found: {args.apk}")
    if args.version_code <= 0:
        raise OpsError("--version-code must be positive")
    apk_sha, apk_size = sha256_file(args.apk)
    object_key = (
        args.object_key.lstrip("/")
        if args.object_key
        else default_android_object_key(args.channel, args.version_name, args.version_code)
    )

    upload_result: dict[str, Any] | None = None
    if not args.skip_upload:
        upload_result = upload_to_oss(
            file_path=args.apk,
            object_key=object_key,
            bucket=args.bucket,
            endpoint=args.endpoint,
            content_type="application/vnd.android.package-archive",
            overwrite=args.overwrite,
            dry_run=args.dry_run,
        )

    payload = {
        "channel": args.channel,
        "versionName": args.version_name.strip(),
        "versionCode": args.version_code,
        "minSupportedCode": args.min_supported_code,
        "mandatory": args.mandatory,
        "changelog": args.changelog,
        "apkSha256": apk_sha,
        "apkSizeBytes": apk_size,
        "objectKey": object_key,
    }
    if args.dry_run:
        print(
            json.dumps(
                {"dryRun": True, "upload": upload_result, "publishPayload": payload},
                indent=2,
                ensure_ascii=False,
            )
        )
        return 0

    token = resolve_token(args)
    _, published = api_call(args.base_url, "POST", "/v1/apps/android/releases", token, body=payload)

    verified: dict[str, Any] | None = None
    if not args.skip_verify:
        probe_code = max(0, args.version_code - 1)
        ch = urllib.parse.quote(args.channel)
        _, latest = api_call(
            args.base_url,
            "GET",
            f"/v1/apps/android/latest?channel={ch}&version_code={probe_code}",
            token=None,
        )
        if (
            not isinstance(latest, dict)
            or latest.get("versionCode") != args.version_code
            or str(latest.get("apkSha256", "")).lower() != apk_sha.lower()
            or not latest.get("updateAvailable")
        ):
            raise OpsError(f"public /v1/apps/android/latest verification mismatch: {latest}")
        verified = {
            "updateAvailable": latest.get("updateAvailable"),
            "versionCode": latest.get("versionCode"),
            "apkSha256": latest.get("apkSha256"),
        }

    print(
        json.dumps(
            {"upload": upload_result, "release": published, "verifiedLatest": verified},
            indent=2,
            ensure_ascii=False,
        )
    )
    return 0


def cmd_fleet_status(args: argparse.Namespace) -> int:
    token = resolve_token(args)
    if args.device_id:
        dev = urllib.parse.quote(validate_device_id(args.device_id))
        _, data = api_call(args.base_url, "GET", f"/v1/devices/{dev}/status", token)
    else:
        _, data = api_call(args.base_url, "GET", "/v1/devices/status", token)
    print(json.dumps(data, indent=2, ensure_ascii=False))
    return 0


def cmd_android_publish(args: argparse.Namespace) -> int:
    token = resolve_token(args)
    if args.apk is not None:
        apk_sha, apk_size = sha256_file(args.apk)
    else:
        if not args.apk_size_bytes or not args.apk_sha256:
            raise OpsError("provide --apk or both --apk-size-bytes and --apk-sha256")
        apk_size = args.apk_size_bytes
        apk_sha = args.apk_sha256.lower()
    payload = {
        "channel": args.channel,
        "versionName": args.version_name,
        "versionCode": args.version_code,
        "minSupportedCode": args.min_supported_code,
        "mandatory": args.mandatory,
        "changelog": args.changelog,
        "apkSha256": apk_sha,
        "apkSizeBytes": apk_size,
        "objectKey": args.object_key.lstrip("/"),
    }
    _, data = api_call(args.base_url, "POST", "/v1/apps/android/releases", token, body=payload)
    print(json.dumps(data, indent=2, ensure_ascii=False))
    return 0


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument(
        "--base-url",
        default=os.environ.get("DOSO_OTA_CLOUD_URL", "https://api.dosorobotics.com/ota"),
        help="doso-ota-release base URL (default: $DOSO_OTA_CLOUD_URL or https://api.dosorobotics.com/ota)",
    )
    p.add_argument(
        "--token-env",
        default="DOSO_RELEASE_API_TOKEN",
        help="Environment variable containing Bearer API token (default: DOSO_RELEASE_API_TOKEN)",
    )
    sub = p.add_subparsers(dest="command", required=True)

    sp_pkg = sub.add_parser("package", help="Build and verify an immutable OTA .tar.gz with SHA256SUMS")
    sp_pkg.add_argument("--type", dest="artifact_type", required=True, choices=ALLOWED_PACKAGE_TYPES)
    sp_pkg.add_argument("--source-dir", required=True, type=Path)
    sp_pkg.add_argument("--output", required=True, type=Path)
    sp_pkg.add_argument("--component", help="Required when --type component_release")
    sp_pkg.set_defaults(func=cmd_package)

    sp_oss = sub.add_parser("oss-upload", help="Upload an artifact to private Aliyun OSS via HMAC-SHA1")
    sp_oss.add_argument("--file", required=True, type=Path)
    sp_oss.add_argument("--object-key", required=True)
    sp_oss.add_argument("--bucket", default=os.environ.get("DOSO_OSS_BUCKET", "doso-ota-prod-shanghai"))
    sp_oss.add_argument("--endpoint", default=os.environ.get("DOSO_OSS_ENDPOINT", "oss-cn-shanghai.aliyuncs.com"))
    sp_oss.add_argument("--content-type", default="application/gzip")
    sp_oss.add_argument("--overwrite", action="store_true")
    sp_oss.add_argument("--dry-run", action="store_true")
    sp_oss.set_defaults(func=cmd_oss_upload)

    sp_tpl = sub.add_parser("render-template", help="Generate manifest.template.json for doso-ota-release")
    sp_tpl.add_argument("--release-id", required=True)
    sp_tpl.add_argument("--version", required=True)
    sp_tpl.add_argument("--sequence", required=True, type=int)
    sp_tpl.add_argument("--channel", required=True, choices=ALLOWED_CHANNELS)
    sp_tpl.add_argument("--type", dest="artifact_type", required=True, choices=ALLOWED_ARTIFACT_TYPES)
    sp_tpl.add_argument("--object-key", default="")
    sp_tpl.add_argument("--bundle-spec", type=Path, help="JSON array of bundle items for release_bundle")
    sp_tpl.add_argument("--delta-object-key", help="Optional OSS objectKey for top-level delta patch")
    sp_tpl.add_argument("--artifact", type=Path, help="Compute size/SHA-256 directly from this file")
    sp_tpl.add_argument("--size-bytes", type=int)
    sp_tpl.add_argument("--sha256")
    sp_tpl.add_argument("--model", default="THUNDER 01")
    sp_tpl.add_argument("--hardware-revision", default="rdk-s100p")
    sp_tpl.add_argument("--min-bootloader", default="1.0.0")
    sp_tpl.add_argument("--min-brainstem", default="0.0.1")
    sp_tpl.add_argument("--min-battery", type=int, default=20)
    sp_tpl.add_argument("--health-timeout-ms", type=int, default=30000)
    sp_tpl.add_argument("--manifest-ttl-seconds", type=int, default=1800)
    sp_tpl.add_argument("--key-id", default="release-key-2026-01")
    sp_tpl.add_argument("--policy-slot")
    sp_tpl.add_argument("--shadow-frames", type=int, default=50)
    sp_tpl.add_argument("--no-activate", action="store_true")
    sp_tpl.add_argument("--component")
    sp_tpl.add_argument("--systemd-unit")
    sp_tpl.add_argument("--health-url")
    sp_tpl.add_argument("--requires", action="append", help="component=semver dependency (repeatable)")
    sp_tpl.add_argument("--output", type=Path)
    sp_tpl.set_defaults(func=cmd_render_template)

    sp_prov = sub.add_parser("provision-device", help="Add or update a device in OTA release devices.json")
    sp_prov.add_argument("--devices-file", required=True, type=Path)
    sp_prov.add_argument("--device-id", required=True)
    sp_prov.add_argument("--model", default="THUNDER 01")
    sp_prov.add_argument("--hardware-revision", default="rdk-s100p")
    sp_prov.add_argument("--ring", default="prod", choices=ALLOWED_RINGS)
    sp_prov.add_argument("--tag", action="append")
    sp_prov.add_argument("--disabled", action="store_true")
    sp_prov.add_argument("--dry-run", action="store_true")
    sp_prov.set_defaults(func=cmd_provision_device)

    sp_env = sub.add_parser("render-robot-env", help="Render /etc/doso/ota.env without secrets")
    sp_env.add_argument("--device-id", required=True)
    sp_env.add_argument("--model", default="THUNDER 01")
    sp_env.add_argument("--hardware-revision", default="rdk-s100p")
    sp_env.add_argument("--bootloader-version", default="1.0.0")
    sp_env.add_argument("--brainstem-version", required=True)
    sp_env.add_argument("--brainstem-release", required=True)
    sp_env.add_argument("--verify-key", default="/etc/doso/robot-cloud/ota/release-signing.pub.pem")
    sp_env.add_argument("--cloud-url", default="https://api.dosorobotics.com/ota")
    sp_env.add_argument("--poll-interval-secs", type=int, default=15)
    sp_env.add_argument("--output", type=Path)
    sp_env.set_defaults(func=cmd_render_robot_env)

    sp_ver = sub.add_parser("verify-manifest", help="Verify an Ed25519 ManifestEnvelope JSON offline")
    sp_ver.add_argument("--envelope", required=True, type=Path)
    sp_ver.add_argument("--public-key", required=True, type=Path)
    sp_ver.set_defaults(func=cmd_verify_manifest)

    sp_ro = sub.add_parser("rollout", help="Manage canary->prod rollouts and circuit breaker")
    ro_sub = sp_ro.add_subparsers(dest="rollout_cmd", required=True)
    ro_c = ro_sub.add_parser("create")
    ro_c.add_argument("--spec", type=Path, help="Full CreateRolloutRequest JSON file")
    ro_c.add_argument("--rollout-id")
    ro_c.add_argument("--release-id")
    ro_c.add_argument("--model")
    ro_c.add_argument("--hardware-revision")
    ro_c.add_argument("--tag", action="append")
    ro_c.add_argument("--stages-json")
    ro_c.add_argument("--canary-max-devices", type=int, default=1)
    ro_c.add_argument("--canary-bake-seconds", type=int, default=1800)
    ro_c.add_argument("--prod-bake-seconds", type=int, default=3600)
    ro_c.add_argument("--max-failure-count", type=int, default=1)
    ro_c.add_argument("--max-failure-rate", type=float, default=0.05)
    ro_sub.add_parser("list")
    for act in ("status", "promote", "pause", "resume", "abort"):
        act_p = ro_sub.add_parser(act)
        act_p.add_argument("rollout_id")
    sp_ro.set_defaults(func=cmd_rollout)

    sp_job = sub.add_parser("job", help="Trigger or inspect a single-device OTA job")
    job_sub = sp_job.add_subparsers(dest="job_cmd", required=True)
    j_start = job_sub.add_parser("start")
    j_start.add_argument("--device-id", required=True)
    j_start.add_argument("--release-id", required=True)
    j_start.add_argument("--idempotency-key", required=True)
    for act in ("status", "cancel"):
        jp = job_sub.add_parser(act)
        jp.add_argument("--device-id", required=True)
        jp.add_argument("--job-id", required=True)
    sp_job.set_defaults(func=cmd_job)

    sp_rel = sub.add_parser("releases", help="List cloud releases (all, or compatible for one device)")
    sp_rel.add_argument("--device-id", help="Optional device ID to filter compatible releases")
    sp_rel.set_defaults(func=cmd_releases)

    sp_reg = sub.add_parser("register-release", help="Register a manifest.template.json via POST /v1/releases")
    sp_reg.add_argument("--template", required=True, type=Path)
    sp_reg.set_defaults(func=cmd_register_release)

    sp_pub = sub.add_parser(
        "publish",
        help="One-click robot OTA: package -> OSS upload -> HTTPS register -> optional device job / rollout",
    )
    sp_pub.add_argument("--type", dest="artifact_type", required=True, choices=ALLOWED_PACKAGE_TYPES)
    sp_pub.add_argument("--source-dir", type=Path, help="Source directory to package into .tar.gz")
    sp_pub.add_argument("--artifact", type=Path, help="Pre-built .tar.gz artifact (mutually exclusive with --source-dir)")
    sp_pub.add_argument("--output", type=Path, help="Optional local path to save the built .tar.gz")
    sp_pub.add_argument("--save-template", type=Path, help="Optional local path to save generated manifest.template.json")
    sp_pub.add_argument("--release-id", required=True)
    sp_pub.add_argument("--version", required=True)
    sp_pub.add_argument("--sequence", required=True, type=int)
    sp_pub.add_argument("--channel", default="qa", choices=ALLOWED_CHANNELS)
    sp_pub.add_argument("--object-key", default="", help="Optional OSS object key (auto-derived if omitted)")
    sp_pub.add_argument("--delta-object-key")
    sp_pub.add_argument("--bucket", default=os.environ.get("DOSO_OSS_BUCKET", "doso-ota-prod-shanghai"))
    sp_pub.add_argument("--endpoint", default=os.environ.get("DOSO_OSS_ENDPOINT", "oss-cn-shanghai.aliyuncs.com"))
    sp_pub.add_argument("--overwrite", action="store_true")
    sp_pub.add_argument("--skip-upload", action="store_true")
    sp_pub.add_argument("--dry-run", action="store_true")
    sp_pub.add_argument("--model", default="THUNDER 01")
    sp_pub.add_argument("--hardware-revision", default="rdk-s100p")
    sp_pub.add_argument("--min-bootloader", default="1.0.0")
    sp_pub.add_argument("--min-brainstem", default="0.0.1")
    sp_pub.add_argument("--min-battery", type=int, default=20)
    sp_pub.add_argument("--health-timeout-ms", type=int, default=30000)
    sp_pub.add_argument("--manifest-ttl-seconds", type=int, default=1800)
    sp_pub.add_argument("--key-id", default="release-key-2026-01")
    sp_pub.add_argument("--policy-slot")
    sp_pub.add_argument("--shadow-frames", type=int, default=50)
    sp_pub.add_argument("--no-activate", action="store_true")
    sp_pub.add_argument("--component")
    sp_pub.add_argument("--systemd-unit")
    sp_pub.add_argument("--health-url")
    sp_pub.add_argument("--requires", action="append")
    sp_pub.add_argument("--device-id", help="Immediately dispatch a single-device OTA job after registration")
    sp_pub.add_argument("--idempotency-key", help="Optional idempotency key for --device-id job")
    sp_pub.add_argument("--watch", action="store_true", help="Poll job status until terminal phase (requires --device-id)")
    sp_pub.add_argument("--poll-interval", type=float, default=3.0)
    sp_pub.add_argument("--watch-timeout", type=float, default=300.0)
    sp_pub.add_argument("--rollout-id", help="Immediately create a canary->prod rollout after registration")
    sp_pub.add_argument("--tag", action="append")
    sp_pub.add_argument("--canary-max-devices", type=int, default=1)
    sp_pub.add_argument("--canary-bake-seconds", type=int, default=1800)
    sp_pub.add_argument("--prod-bake-seconds", type=int, default=3600)
    sp_pub.add_argument("--max-failure-count", type=int, default=1)
    sp_pub.add_argument("--max-failure-rate", type=float, default=0.05)
    sp_pub.set_defaults(func=cmd_publish)

    sp_pub_apk = sub.add_parser(
        "publish-android",
        help="One-click Android App OTA: OSS upload APK -> publish metadata -> verify public /latest",
    )
    sp_pub_apk.add_argument("--apk", required=True, type=Path)
    sp_pub_apk.add_argument("--channel", default="prod")
    sp_pub_apk.add_argument("--version-name", required=True)
    sp_pub_apk.add_argument("--version-code", required=True, type=int)
    sp_pub_apk.add_argument("--min-supported-code", type=int, default=0)
    sp_pub_apk.add_argument("--mandatory", action="store_true")
    sp_pub_apk.add_argument("--changelog", default="")
    sp_pub_apk.add_argument("--object-key", default="", help="Optional OSS object key (auto-derived if omitted)")
    sp_pub_apk.add_argument("--bucket", default=os.environ.get("DOSO_OSS_BUCKET", "doso-ota-prod-shanghai"))
    sp_pub_apk.add_argument("--endpoint", default=os.environ.get("DOSO_OSS_ENDPOINT", "oss-cn-shanghai.aliyuncs.com"))
    sp_pub_apk.add_argument("--overwrite", action="store_true")
    sp_pub_apk.add_argument("--skip-upload", action="store_true")
    sp_pub_apk.add_argument("--skip-verify", action="store_true")
    sp_pub_apk.add_argument("--dry-run", action="store_true")
    sp_pub_apk.set_defaults(func=cmd_publish_android)

    sp_st = sub.add_parser("fleet-status", help="Read reported device inventory & RT metrics")
    sp_st.add_argument("--device-id")
    sp_st.set_defaults(func=cmd_fleet_status)

    sp_apk = sub.add_parser("android-publish", help="Publish Android APK release metadata")
    sp_apk.add_argument("--channel", default="prod")
    sp_apk.add_argument("--version-name", required=True)
    sp_apk.add_argument("--version-code", required=True, type=int)
    sp_apk.add_argument("--min-supported-code", type=int, default=0)
    sp_apk.add_argument("--mandatory", action="store_true")
    sp_apk.add_argument("--changelog", default="")
    sp_apk.add_argument("--object-key", required=True)
    sp_apk.add_argument("--apk", type=Path)
    sp_apk.add_argument("--apk-size-bytes", type=int)
    sp_apk.add_argument("--apk-sha256")
    sp_apk.set_defaults(func=cmd_android_publish)

    return p


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        return args.func(args)
    except (OpsError, OSError, json.JSONDecodeError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
