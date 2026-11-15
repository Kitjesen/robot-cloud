#!/usr/bin/env python3
"""Read-only preflight for the LingTu SDK Gateway release.

The check validates files and configuration only. It never starts or reloads a
service, creates a ProductControl session, or changes a device setting.
"""

from __future__ import annotations

import argparse
import json
import os
import stat
import subprocess
import sys
from pathlib import Path


def parse_env(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    if not path.is_file():
        return values
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        values[key.strip()] = value.strip().strip("\"'")
    return values


def readable_secret(path: Path) -> bool:
    try:
        mode = stat.S_IMODE(path.stat().st_mode)
        return path.is_file() and os.access(path, os.R_OK) and mode & 0o077 == 0
    except OSError:
        return False


def manifest_ok(path: Path) -> tuple[bool, dict[str, object], tuple[str, ...]]:
    try:
        manifest = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        return False, {"error": f"manifest:{exc.__class__.__name__}"}, ()
    if not isinstance(manifest, dict):
        return False, {"error": "manifest:not_object"}, ()
    architecture = manifest.get("architecture", manifest.get("arch"))
    signature = manifest.get("signature")
    signed = manifest.get("signed")
    required = manifest.get("requiredLibraries")
    libraries = tuple(value for value in required or () if isinstance(value, str) and value)
    ok = architecture in {"aarch64", "arm64"} and isinstance(required, list) and len(libraries) == len(required) and (
        isinstance(signature, str) and bool(signature.strip()) or signed is True
    )
    details = {
        "architecture": architecture,
        "signed": bool(signature) or signed is True,
        "requiredLibraries": libraries,
    }
    return ok, details, libraries


def service_state() -> dict[str, str]:
    try:
        result = subprocess.run(
            ["systemctl", "show", "lt-host.service", "-p", "LoadState", "-p", "ActiveState"],
            check=False,
            capture_output=True,
            text=True,
            timeout=3,
        )
    except (OSError, subprocess.TimeoutExpired):
        return {"LoadState": "unknown", "ActiveState": "unknown"}
    values = {}
    for line in result.stdout.splitlines():
        if "=" in line:
            key, value = line.split("=", 1)
            values[key] = value
    return values


def run(args: argparse.Namespace) -> tuple[int, dict[str, object]]:
    root = Path(args.current_root)
    env_path = Path(args.gateway_env)
    session_path = Path(args.session_file)
    env = parse_env(env_path)
    manifest_passed, manifest_details, required_libraries = manifest_ok(Path(args.manifest))

    libraries = {name: (root / "lib" / name).is_file() for name in required_libraries}
    api_key = env.get("LINGTU_GRPC_API_KEY", "")
    http_api_key = env.get("LINGTU_API_KEY", "")
    api_key_required = env.get("LINGTU_GRPC_REQUIRE_API_KEY", "").lower() == "true"
    tls_cert = Path(env["LINGTU_GRPC_TLS_CERT_FILE"]) if env.get("LINGTU_GRPC_TLS_CERT_FILE") else None
    tls_key = Path(env["LINGTU_GRPC_TLS_KEY_FILE"]) if env.get("LINGTU_GRPC_TLS_KEY_FILE") else None
    tls_pair = bool(tls_cert and tls_key and readable_secret(tls_cert) and readable_secret(tls_key))
    session_present = session_path.is_file() and session_path.stat().st_size > 0
    service = service_state()

    checks = {
        "manifest": manifest_passed,
        "current_root": root.is_dir(),
        "native_libraries": all(libraries.values()),
        "gateway_env": env_path.is_file(),
        "grpc_api_key": bool(api_key),
        "grpc_api_key_independent": bool(api_key) and api_key != http_api_key,
        "api_key_required": api_key_required,
        "tls_pair": tls_pair,
        "product_session": session_present,
        "lt_host_installed": service.get("LoadState") == "loaded",
    }
    report = {
        "readOnly": True,
        "checks": checks,
        "manifest": manifest_details,
        "libraries": libraries,
        "service": service,
        "sessionFile": str(session_path),
        "tlsConfigured": bool(tls_cert or tls_key),
    }
    return (0 if all(checks.values()) else 1), report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", required=True)
    parser.add_argument("--gateway-env", required=True)
    parser.add_argument("--session-file", required=True)
    parser.add_argument("--current-root", default="/opt/lingtu/current")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()
    code, report = run(args)
    if args.json:
        print(json.dumps(report, ensure_ascii=False, sort_keys=True))
    else:
        for key, value in report["checks"].items():
            print(f"{key}={'pass' if value else 'fail'}")
    return code


if __name__ == "__main__":
    sys.exit(main())
