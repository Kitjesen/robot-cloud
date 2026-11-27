#!/usr/bin/env python3
"""Unit and integration tests for tools/ota-console/server.py."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import tempfile
import threading
import unittest
import urllib.error
import urllib.parse
import urllib.request
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from unittest import mock

CONSOLE_SERVER_PATH = Path(__file__).resolve().parent / "server.py"
SPEC = importlib.util.spec_from_file_location("doso_ota_console_server", CONSOLE_SERVER_PATH)
assert SPEC and SPEC.loader
console = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(console)


class MockCloudState:
    def __init__(self, expected_token: str = "secret-bearer-token-123456") -> None:
        self.expected_token = expected_token
        self.releases: dict[str, dict[str, Any]] = {}
        self.registered_devices: dict[str, dict[str, Any]] = {
            "dog-01": {
                "deviceId": "dog-01",
                "model": "THUNDER 01",
                "hardwareRevision": "rdk-s100p",
                "enabled": True,
                "ring": "canary",
                "tags": ["dog1"],
            }
        }
        self.devices: list[dict[str, Any]] = [
            {
                "deviceId": "dog-01",
                "reportedAtMs": 1764200000000,
                "lastSeenAgeMs": 5000,
                "firmwareVersion": "0.1.0",
                "releaseId": "brainstem-0.1.0",
                "brainstemReady": True,
                "strategyReady": True,
                "canReadOnly": False,
                "activeProfile": "walk",
                "slots": {"locomotion": "policy-0.1.0"},
                "policies": [],
            }
        ]
        self.jobs: dict[str, dict[str, Any]] = {}
        self.rollouts: dict[str, dict[str, Any]] = {}
        self.android: dict[str, dict[str, Any]] = {}


def start_mock_cloud(state: MockCloudState) -> tuple[ThreadingHTTPServer, str]:
    class CloudHandler(BaseHTTPRequestHandler):
        def log_message(self, format: str, *args: Any) -> None:
            return

        def _send(self, code: int, obj: Any) -> None:
            raw = json.dumps(obj).encode("utf-8")
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)

        def _check_auth(self) -> bool:
            if self.headers.get("Authorization") != f"Bearer {state.expected_token}":
                self._send(401, {"error": "unauthorized"})
                return False
            return True

        def _read_json(self) -> dict[str, Any]:
            n = int(self.headers.get("Content-Length", "0"))
            return json.loads(self.rfile.read(n).decode("utf-8")) if n else {}

        def do_GET(self) -> None:
            parsed = urllib.parse.urlparse(self.path)
            path = parsed.path
            if path == "/health":
                self._send(200, {"service": "doso-ota-release", "version": "0.1.0", "ready": True})
                return
            if path == "/v1/apps/android/latest":
                qs = urllib.parse.parse_qs(parsed.query)
                ch = qs.get("channel", ["prod"])[0]
                vc = int(qs.get("version_code", ["0"])[0])
                rel = state.android.get(ch)
                if not rel:
                    self._send(404, {"error": "app_release_not_found"})
                    return
                update_avail = rel["versionCode"] > vc
                self._send(
                    200,
                    {
                        **rel,
                        "updateAvailable": update_avail,
                        "forceUpdate": update_avail and rel.get("mandatory", False),
                        "downloadUrl": f"https://oss.example/{rel['objectKey']}?sig=ok",
                    },
                )
                return

            if not self._check_auth():
                return
            if path == "/v1/devices":
                self._send(200, list(state.registered_devices.values()))
                return
            if path == "/v1/devices/status":
                self._send(200, state.devices)
                return
            if path == "/v1/releases":
                self._send(200, list(state.releases.values()))
                return
            if path == "/v1/rollouts":
                self._send(200, list(state.rollouts.values()))
                return
            if path.startswith("/v1/devices/") and path.endswith("/firmware/releases"):
                self._send(200, list(state.releases.values()))
                return
            if "/firmware/jobs/" in path:
                job_id = path.rsplit("/", 1)[-1]
                job = state.jobs.get(job_id)
                if not job:
                    self._send(404, {"error": "job_not_found"})
                    return
                self._send(200, job)
                return
            self._send(404, {"error": "not_found"})

        def do_PUT(self) -> None:
            if not self._check_auth():
                return
            parsed = urllib.parse.urlparse(self.path)
            path = parsed.path
            body = self._read_json()
            if path.startswith("/v1/devices/"):
                dev_id = urllib.parse.unquote(path.split("/", 3)[3])
                rec = {"deviceId": dev_id, **body}
                state.registered_devices[dev_id] = rec
                self._send(200, rec)
                return
            self._send(404, {"error": "not_found"})

        def do_DELETE(self) -> None:
            if not self._check_auth():
                return
            parsed = urllib.parse.urlparse(self.path)
            path = parsed.path
            if path.startswith("/v1/devices/"):
                dev_id = urllib.parse.unquote(path.split("/", 3)[3])
                state.registered_devices.pop(dev_id, None)
                self._send(200, {"deleted": True, "deviceId": dev_id})
                return
            self._send(404, {"error": "not_found"})

        def do_POST(self) -> None:
            if not self._check_auth():
                return
            parsed = urllib.parse.urlparse(self.path)
            path = parsed.path
            body = self._read_json()

            if path == "/v1/releases/bundle":
                summary = {
                    "releaseId": body["releaseId"],
                    "version": body["version"],
                    "sequence": body["sequence"],
                    "channel": body["channel"],
                    "model": "THUNDER 01",
                    "hardwareRevision": "rdk-s100p",
                    "artifactSizeBytes": 0,
                    "artifactSha256": "",
                    "artifactType": "release_bundle",
                }
                state.releases[body["releaseId"]] = summary
                self._send(201, summary)
                return

            if path == "/v1/releases":
                m = body["manifest"]
                summary = {
                    "releaseId": m["releaseId"],
                    "version": m["version"],
                    "sequence": m["sequence"],
                    "channel": m["channel"],
                    "model": m["model"],
                    "hardwareRevision": m["hardwareRevision"],
                    "artifactSizeBytes": m["artifactSizeBytes"],
                    "artifactSha256": m["artifactSha256"],
                    "artifactType": m["artifactType"],
                }
                state.releases[m["releaseId"]] = summary
                self._send(201, summary)
                return

            if path == "/v1/apps/android/releases":
                rel = {**body, "publishedAtMs": 1764200100000}
                state.android[body["channel"]] = rel
                self._send(200, rel)
                return

            if path.startswith("/v1/devices/") and path.endswith("/firmware/jobs"):
                dev_id = path.split("/")[3]
                job_id = f"job-{len(state.jobs) + 1}"
                job = {
                    "jobId": job_id,
                    "deviceId": dev_id,
                    "releaseId": body["releaseId"],
                    "idempotencyKey": body["idempotencyKey"],
                    "phase": "scheduled",
                    "bytesDownloaded": 0,
                }
                state.jobs[job_id] = job
                self._send(202, job)
                return

            if "/firmware/jobs/" in path and path.endswith("/cancel"):
                job_id = path.split("/")[-2]
                job = state.jobs[job_id]
                job["phase"] = "cancelled"
                self._send(200, job)
                return

            if path == "/v1/rollouts":
                rid = body.get("rolloutId") or f"ro-{len(state.rollouts) + 1}"
                ro = {
                    "rolloutId": rid,
                    "releaseId": body["releaseId"],
                    "targetModel": body.get("targetModel", "THUNDER 01"),
                    "status": "active",
                    "currentStageIndex": 0,
                    "createdAtMs": 1764200200000,
                    "stageCounts": [
                        {
                            "stageIndex": idx,
                            "name": st["name"],
                            "ring": st["ring"],
                            "percent": st["percent"],
                            "total": 0,
                            "inProgress": 0,
                            "waitingSafeWindow": 0,
                            "succeeded": 0,
                            "failed": 0,
                            "rolledBack": 0,
                            "recoveryRequired": 0,
                        }
                        for idx, st in enumerate(body["stages"])
                    ],
                }
                state.rollouts[rid] = ro
                self._send(201, ro)
                return

            if path.startswith("/v1/rollouts/"):
                parts = path.split("/")
                rid, act = parts[3], parts[4]
                ro = state.rollouts[rid]
                if act == "promote":
                    ro["currentStageIndex"] += 1
                elif act == "pause":
                    ro["status"] = "paused_manual"
                elif act == "resume":
                    ro["status"] = "active"
                elif act == "abort":
                    ro["status"] = "aborted"
                self._send(200, ro)
                return

            self._send(404, {"error": "not_found"})

    srv = ThreadingHTTPServer(("127.0.0.1", 0), CloudHandler)
    t = threading.Thread(target=srv.serve_forever, daemon=True)
    t.start()
    return srv, f"http://127.0.0.1:{srv.server_address[1]}"


class OtaConsoleTests(unittest.TestCase):
    def setUp(self) -> None:
        os.environ["DOSO_OTA_CONSOLE_QUIET"] = "1"
        self.tmp = tempfile.TemporaryDirectory()
        self.tmp_path = Path(self.tmp.name)
        self.cfg_path = self.tmp_path / "console.env"

        self.cloud_state = MockCloudState()
        self.cloud_srv, self.cloud_url = start_mock_cloud(self.cloud_state)

        self.console_srv = console.create_server("127.0.0.1", 0, config_path=self.cfg_path)
        self.console_thread = threading.Thread(target=self.console_srv.serve_forever, daemon=True)
        self.console_thread.start()
        self.base_url = f"http://127.0.0.1:{self.console_srv.server_address[1]}"

    def tearDown(self) -> None:
        self.console_srv.shutdown()
        self.console_srv.server_close()
        self.cloud_srv.shutdown()
        self.cloud_srv.server_close()
        self.tmp.cleanup()

    def _req(
        self,
        method: str,
        path: str,
        body: Any = None,
        headers: dict[str, str] | None = None,
        raw_bytes: bytes | None = None,
    ) -> tuple[int, Any]:
        hdrs = {"X-DOSO-Console": "1"}
        if headers:
            hdrs.update(headers)
        data = raw_bytes
        if body is not None:
            hdrs["Content-Type"] = "application/json"
            data = json.dumps(body).encode("utf-8")
        req = urllib.request.Request(self.base_url + path, data=data, headers=hdrs, method=method)
        try:
            with urllib.request.urlopen(req, timeout=10) as resp:
                raw = resp.read()
                ctype = resp.headers.get("Content-Type", "")
                if "application/json" in ctype:
                    return resp.status, json.loads(raw.decode("utf-8"))
                return resp.status, raw
        except urllib.error.HTTPError as err:
            raw = err.read()
            return err.code, json.loads(raw.decode("utf-8"))

    def _configure_console(self) -> None:
        status, cfg = self._req(
            "POST",
            "/api/config",
            body={
                "cloudUrl": self.cloud_url,
                "apiToken": self.cloud_state.expected_token,
                "ossBucket": "test-bucket",
                "ossEndpoint": "oss-cn-shanghai.aliyuncs.com",
                "ossAccessKeyId": "LTAI5tTestAccessKeyId",
                "ossAccessKeySecret": "SuperSecretOssKey987654321",
            },
        )
        self.assertEqual(status, HTTPStatus.OK)
        self.assertTrue(cfg["tokenConfigured"])
        self.assertTrue(cfg["ossConfigured"])
        # Secrets must NEVER be echoed back in full
        cfg_dump = json.dumps(cfg)
        self.assertNotIn(self.cloud_state.expected_token, cfg_dump)
        self.assertNotIn("SuperSecretOssKey987654321", cfg_dump)

    def test_static_pages_and_security_guards(self) -> None:
        st, html = self._req("GET", "/")
        self.assertEqual(st, 200)
        self.assertIn(b"DOSO OTA", html)

        # Missing X-DOSO-Console header rejected on POST
        req = urllib.request.Request(
            self.base_url + "/api/config",
            data=b"{}",
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with self.assertRaises(urllib.error.HTTPError) as ctx:
            urllib.request.urlopen(req, timeout=5)
        self.assertEqual(ctx.exception.code, 403)

        # Cross-origin POST rejected
        st_origin, err = self._req(
            "POST",
            "/api/config",
            body={"cloudUrl": self.cloud_url},
            headers={"Origin": "https://evil.example"},
        )
        self.assertEqual(st_origin, 403)
        self.assertEqual(err["error"], "forbidden_origin")

    def test_config_preserves_existing_secrets(self) -> None:
        self._configure_console()
        st, cfg = self._req("POST", "/api/config", body={"defaultModel": "THUNDER 02"})
        self.assertEqual(st, 200)
        self.assertEqual(cfg["defaultModel"], "THUNDER 02")
        self.assertTrue(cfg["tokenConfigured"])
        self.assertTrue(cfg["ossConfigured"])

        st_h, health = self._req("GET", "/api/health")
        self.assertEqual(st_h, 200)
        self.assertTrue(health["cloudReachable"])
        self.assertTrue(health["authOk"])
        self.assertTrue(health["releasesApiReady"])

    def test_robot_publish_dispatch_and_rollout_lifecycle(self) -> None:
        self._configure_console()

        # Build a valid brainstem source directory
        src_dir = self.tmp_path / "pkg_brainstem"
        (src_dir / "bin").mkdir(parents=True)
        (src_dir / "bin" / "doso-brainstem").write_bytes(b"\x7fELF-mock-brainstem-binary")
        (src_dir / "data" / "gestures").mkdir(parents=True)
        (src_dir / "data" / "gestures" / "library.json").write_text('{"gestures":[]}', encoding="utf-8")

        st_ins, info = self._req("POST", "/api/inspect-path", body={"path": str(src_dir)})
        self.assertEqual(st_ins, 200)
        self.assertTrue(info["hasBrainstemBin"])
        self.assertEqual(info["guessedArtifactType"], "brainstem_release")

        uploaded_keys: list[str] = []

        def fake_oss_upload(**kwargs: Any) -> dict[str, Any]:
            uploaded_keys.append(kwargs["object_key"])
            sha, size = console.ops.sha256_file(kwargs["file_path"])
            return {
                "uploaded": True,
                "url": f"https://mock-oss/{kwargs['object_key']}",
                "objectKey": kwargs["object_key"],
                "sizeBytes": size,
                "sha256": sha,
            }

        with mock.patch.object(console.ops, "upload_to_oss", side_effect=fake_oss_upload):
            st_pub, res = self._req(
                "POST",
                "/api/releases/publish",
                body={
                    "artifactType": "brainstem_release",
                    "channel": "qa",
                    "version": "0.2.0",
                    "sequence": 10,
                    "releaseId": "brainstem-0.2.0-qa-s10",
                    "sourceMode": "local_dir",
                    "localPath": str(src_dir),
                    "dispatchMode": "device",
                    "deviceId": "dog-01",
                },
            )

        self.assertEqual(st_pub, 200)
        self.assertEqual(res["releaseId"], "brainstem-0.2.0-qa-s10")
        self.assertEqual(uploaded_keys, ["firmware/qa/brainstem-0.2.0-qa-s10/brainstem.tar.gz"])
        self.assertEqual(res["job"]["deviceId"], "dog-01")
        self.assertEqual(res["job"]["phase"], "scheduled")

        # Cancel the job
        job_id = res["job"]["jobId"]
        st_can, can = self._req("POST", f"/api/jobs/dog-01/{job_id}/cancel", body={})
        self.assertEqual(st_can, 200)
        self.assertEqual(can["phase"], "cancelled")

        # Create & promote/pause/resume/abort a rollout
        st_ro, ro = self._req(
            "POST",
            "/api/rollouts",
            body={"releaseId": "brainstem-0.2.0-qa-s10", "rolloutId": "ro-020", "preset": "standard"},
        )
        self.assertEqual(st_ro, 201)
        self.assertEqual(ro["status"], "active")
        self.assertEqual(len(ro["stageCounts"]), 3)

        st_p, ro_p = self._req("POST", "/api/rollouts/ro-020/promote", body={})
        self.assertEqual(st_p, 200)
        self.assertEqual(ro_p["currentStageIndex"], 1)

        st_pause, ro_pause = self._req("POST", "/api/rollouts/ro-020/pause", body={})
        self.assertEqual(st_pause, 200)
        self.assertEqual(ro_pause["status"], "paused_manual")

        st_res, ro_res = self._req("POST", "/api/rollouts/ro-020/resume", body={})
        self.assertEqual(st_res, 200)
        self.assertEqual(ro_res["status"], "active")

        st_ab, ro_ab = self._req("POST", "/api/rollouts/ro-020/abort", body={})
        self.assertEqual(st_ab, 200)
        self.assertEqual(ro_ab["status"], "aborted")

        # Overview aggregates everything cleanly
        st_ov, ov = self._req("GET", "/api/overview")
        self.assertEqual(st_ov, 200)
        self.assertEqual(len(ov["devices"]), 1)
        self.assertEqual(len(ov["releases"]), 1)
        self.assertEqual(len(ov["rollouts"]), 1)
        self.assertEqual(ov["trackedJobs"][0]["phase"], "cancelled")

    def test_upload_raw_brainstem_binary_dry_run_and_android_publish(self) -> None:
        self._configure_console()

        # Upload raw doso-brainstem binary -> auto-packages in dry-run
        bin_bytes = b"\x7fELF-direct-upload-brainstem"
        st_up, up = self._req(
            "POST",
            "/api/uploads",
            raw_bytes=bin_bytes,
            headers={"X-Upload-Filename": "doso-brainstem"},
        )
        self.assertEqual(st_up, 201)
        self.assertEqual(up["sha256"], hashlib.sha256(bin_bytes).hexdigest())

        st_dry, dry = self._req(
            "POST",
            "/api/releases/publish",
            body={
                "artifactType": "brainstem_release",
                "channel": "qa",
                "version": "0.2.1",
                "sequence": 11,
                "releaseId": "brainstem-0.2.1-qa-s11",
                "sourceMode": "upload",
                "uploadId": up["uploadId"],
                "dryRun": True,
            },
        )
        self.assertEqual(st_dry, 200)
        self.assertTrue(dry["dryRun"])
        self.assertEqual(dry["package"]["artifactType"], "brainstem_release")

        # Upload APK & publish end-to-end
        apk_bytes = b"PK\x03\x04mock-apk-payload-for-doso"
        st_apk_up, apk_up = self._req(
            "POST",
            "/api/uploads",
            raw_bytes=apk_bytes,
            headers={"X-Upload-Filename": "app-qa.apk"},
        )
        self.assertEqual(st_apk_up, 201)

        def fake_apk_oss(**kwargs: Any) -> dict[str, Any]:
            sha, size = console.ops.sha256_file(kwargs["file_path"])
            return {"uploaded": True, "objectKey": kwargs["object_key"], "sizeBytes": size, "sha256": sha}

        with mock.patch.object(console.ops, "upload_to_oss", side_effect=fake_apk_oss):
            st_pub, res = self._req(
                "POST",
                "/api/android/publish",
                body={
                    "sourceMode": "upload",
                    "uploadId": apk_up["uploadId"],
                    "channel": "qa",
                    "versionName": "0.1.0",
                    "versionCode": 57,
                    "changelog": "降载流畅版",
                },
            )
        self.assertEqual(st_pub, 200)
        self.assertTrue(res["verifiedLatest"]["updateAvailable"])
        self.assertEqual(res["verifiedLatest"]["versionCode"], 57)

        st_ov, ov = self._req("GET", "/api/overview")
        self.assertEqual(st_ov, 200)
        self.assertEqual(ov["android"]["qa"]["versionCode"], 57)

    def test_device_crud_bundle_and_robot_ops_guardrails(self) -> None:
        self._configure_console()

        # Device auto-UUID registration & deletion
        st_up, saved = self._req(
            "POST",
            "/api/devices/upsert",
            body={"model": "THUNDER 01", "hardwareRevision": "rdk-s100p", "ring": "prod", "tags": "batch1, dog9"},
        )
        self.assertEqual(st_up, 200)
        self.assertEqual(saved["ring"], "prod")
        self.assertEqual(saved["tags"], ["batch1", "dog9"])
        new_id = saved["deviceId"]
        self.assertGreaterEqual(len(new_id), 32)

        st_del, deleted = self._req("POST", f"/api/devices/{new_id}/delete", body={})
        self.assertEqual(st_del, 200)
        self.assertTrue(deleted["deleted"])

        # Release bundle composition
        st_b, bundle = self._req(
            "POST",
            "/api/releases/bundle",
            body={
                "releaseId": "bundle-1.0.0-stable",
                "version": "1.0.0",
                "sequence": 50,
                "channel": "stable",
                "sourceReleaseIds": ["rel-1"],
            },
        )
        self.assertEqual(st_b, 201)
        self.assertEqual(bundle["artifactType"], "release_bundle")

        # Robot ops safety guardrails
        st_bad_svc, err_svc = self._req(
            "POST",
            "/api/ops/service",
            body={"target": {"mode": "tunnel", "port": 22003}, "unit": "sshd.service", "action": "stop"},
        )
        self.assertEqual(st_bad_svc, 400)
        self.assertIn("白名单", err_svc["error"])

        st_bad_del, err_del = self._req(
            "POST",
            "/api/ops/files/delete",
            body={"target": {"mode": "tunnel", "port": 22003}, "path": "/etc/doso"},
        )
        self.assertEqual(st_bad_del, 400)
        self.assertIn("禁止删除", err_del["error"])

        # Mocked ops status & file listing
        with mock.patch.object(
            console.robot_ops,
            "list_dir",
            return_value={"path": "/opt/doso", "parent": "/opt", "entries": [{"name": "data", "path": "/opt/doso/data", "isDir": True, "sizeBytes": 0, "mode": "755", "modifiedAtMs": 0}]},
        ):
            st_ls, ls = self._req(
                "POST",
                "/api/ops/files/list",
                body={"target": {"mode": "tunnel", "port": 22003}, "path": "/opt/doso"},
            )
            self.assertEqual(st_ls, 200)
            self.assertEqual(ls["entries"][0]["name"], "data")


if __name__ == "__main__":
    unittest.main()
