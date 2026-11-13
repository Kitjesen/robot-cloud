#!/usr/bin/env python3
"""Render a reviewed device registration patch without touching a server."""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import sys
import uuid
from pathlib import Path


def device_uuid(value: str) -> str:
    try:
        parsed = uuid.UUID(value)
    except ValueError as exc:
        raise ValueError("device id must be a UUID") from exc
    if parsed.version != 4 or parsed.variant != uuid.RFC_4122 or str(parsed) != value:
        raise ValueError("device id must be a canonical UUID v4")
    return value


def load_registry(path: Path, target: str) -> None:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ValueError(f"invalid device registry: {path}") from exc
    devices = value.get("devices") if isinstance(value, dict) else None
    if not isinstance(devices, list):
        raise ValueError("device registry must contain a devices array")
    seen: set[str] = set()
    for device in devices:
        if not isinstance(device, dict) or not isinstance(device.get("deviceId"), str):
            raise ValueError("device registry contains an invalid device")
        current = device_uuid(device["deviceId"])
        if current in seen:
            raise ValueError(f"duplicate device id in registry: {current}")
        seen.add(current)
    if target in seen:
        raise ValueError(f"device id already exists in registry: {target}")


def load_acl(path: Path, target: str) -> None:
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError as exc:
        raise ValueError(f"cannot read ACL: {path}") from exc
    if any(line == f"user {target}" for line in lines):
        raise ValueError(f"device id already exists in ACL: {target}")


def render_acl(device_id: str) -> str:
    base = f"doso/v1/devices/{device_id}"
    return "\n".join(
        [
            f"user {device_id}",
            f"topic read {base}/status/requests",
            f"topic write {base}/status/replies",
            f"topic write {base}/reporting/hello",
            f"topic read {base}/reporting/control",
            f"topic write {base}/reporting/acks",
            f"topic write {base}/status/reports",
            "",
        ]
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--device-id", required=True)
    parser.add_argument("--public-key", required=True, type=Path)
    parser.add_argument("--devices", required=True, type=Path)
    parser.add_argument("--acl", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    args = parser.parse_args()

    try:
        device_id = device_uuid(args.device_id)
        if not args.public_key.is_file():
            raise ValueError(f"public key does not exist: {args.public_key}")
        key = args.public_key.read_bytes()
        if b"-----BEGIN PUBLIC KEY-----" not in key or b"-----END PUBLIC KEY-----" not in key:
            raise ValueError("public key must be a PEM SubjectPublicKeyInfo")
        load_registry(args.devices, device_id)
        load_acl(args.acl, device_id)
        if args.output_dir.exists():
            if any(args.output_dir.iterdir()):
                raise ValueError(f"output directory is not empty: {args.output_dir}")
        else:
            args.output_dir.mkdir(parents=True)
        key_name = f"device-{device_id}-signing.pub"
        shutil.copyfile(args.public_key, args.output_dir / key_name)
        (args.output_dir / "device-entry.json").write_text(
            json.dumps(
                {"deviceId": device_id, "enabled": False, "publicKeyPath": key_name},
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )
        (args.output_dir / "acl-entry.conf").write_text(
            render_acl(device_id), encoding="utf-8"
        )
        report = {
            "deviceId": device_id,
            "enabled": False,
            "publicKey": key_name,
            "publicKeySha256": hashlib.sha256(key).hexdigest(),
            "sourceRegistry": str(args.devices),
            "sourceAcl": str(args.acl),
            "serverMutation": False,
        }
        (args.output_dir / "registration-report.json").write_text(
            json.dumps(report, indent=2) + "\n", encoding="utf-8"
        )
        print(json.dumps(report, separators=(",", ":")))
        return 0
    except (OSError, ValueError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
