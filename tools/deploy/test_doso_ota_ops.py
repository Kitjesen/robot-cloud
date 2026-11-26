import base64
import hashlib
import importlib.util
import json
import os
import tarfile
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

MODULE_PATH = Path(__file__).with_name("doso-ota-ops.py")
SPEC = importlib.util.spec_from_file_location("doso_ota_ops", MODULE_PATH)
assert SPEC and SPEC.loader
ops = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ops)


class MockReleaseHandler(BaseHTTPRequestHandler):
    requests_log: list[tuple[str, str, str | None, dict | None]] = []

    def log_message(self, format: str, *args) -> None:  # noqa: A002
        return

    def _read_json(self) -> dict | None:
        length = int(self.headers.get("Content-Length", "0"))
        if length == 0:
            return None
        return json.loads(self.rfile.read(length).decode("utf-8"))

    def _send_json(self, status: int, body) -> None:
        payload = json.dumps(body).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self) -> None:  # noqa: N802
        auth = self.headers.get("Authorization")
        self.requests_log.append(("GET", self.path, auth, None))
        if self.path == "/ota/v1/rollouts/ro-1":
            self._send_json(200, {"rolloutId": "ro-1", "status": "active"})
        elif self.path == "/ota/v1/devices/status":
            self._send_json(200, [{"deviceId": "dev-1", "firmwareVersion": "v0.0.3"}])
        else:
            self._send_json(404, {"error": {"code": "not_found"}})

    def do_POST(self) -> None:  # noqa: N802
        auth = self.headers.get("Authorization")
        body = self._read_json()
        self.requests_log.append(("POST", self.path, auth, body))
        if self.path == "/ota/v1/rollouts":
            self._send_json(201, {"rolloutId": "ro-1", "status": "active", "releaseId": body["releaseId"]})
        elif self.path in (
            "/ota/v1/rollouts/ro-1/promote",
            "/ota/v1/rollouts/ro-1/pause",
            "/ota/v1/rollouts/ro-1/abort",
        ):
            self._send_json(200, {"rolloutId": "ro-1", "action": self.path.rsplit("/", 1)[-1]})
        elif self.path == "/ota/v1/devices/dev-1/firmware/jobs":
            self._send_json(202, {"jobId": "job-1", "releaseId": body["releaseId"]})
        else:
            self._send_json(404, {"error": {"code": "not_found"}})


class DosoOtaOpsTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)

    def tearDown(self) -> None:
        self.temp.cleanup()

    def test_package_brainstem_and_policy_generate_valid_sha256sums_and_modes(self) -> None:
        bs_src = self.root / "bs_src"
        (bs_src / "bin").mkdir(parents=True)
        (bs_src / "lib").mkdir(parents=True)
        (bs_src / "bin" / "doso-brainstem").write_bytes(b"\x7fELFbrainstem")
        (bs_src / "lib" / "libdoso_sdk_ffi.so").write_bytes(b"\x7fELFso")
        bs_tar = self.root / "brainstem.tar.gz"

        meta = ops.build_package(bs_src, bs_tar, "brainstem_release")
        self.assertEqual(meta["fileCount"], 3)
        self.assertEqual(meta["sizeBytes"], bs_tar.stat().st_size)

        with tarfile.open(bs_tar, "r:gz") as tf:
            names = tf.getnames()
            self.assertIn("bin/doso-brainstem", names)
            self.assertIn("SHA256SUMS", names)
            self.assertEqual(tf.getmember("bin/doso-brainstem").mode & 0o777, 0o755)
            sums = tf.extractfile("SHA256SUMS").read().decode("utf-8")
            self.assertIn("bin/doso-brainstem", sums)
            self.assertNotIn("\r\n", sums)

        pol_src = self.root / "pol_src"
        pol_src.mkdir()
        (pol_src / "policy.yaml").write_text('schema_version: 2\nmodel:\n  file: "flat.onnx"\n', encoding="utf-8")
        (pol_src / "flat.onnx").write_bytes(b"onnx-bytes")
        pol_tar = self.root / "policy.tar.gz"
        pol_meta = ops.build_package(pol_src, pol_tar, "policy_package")
        self.assertEqual(pol_meta["artifactType"], "policy_package")

    def test_package_rejects_missing_entrypoint_and_invalid_component(self) -> None:
        empty_bin = self.root / "bad_bs"
        (empty_bin / "bin").mkdir(parents=True)
        (empty_bin / "bin" / "other").write_bytes(b"x")
        with self.assertRaises(ops.OpsError):
            ops.build_package(empty_bin, self.root / "bad.tar.gz", "brainstem_release")
        with self.assertRaises(ops.OpsError):
            ops.build_package(empty_bin, self.root / "bad2.tar.gz", "component_release", component="BadComp")

    def test_render_template_and_provision_device_roundtrip(self) -> None:
        artifact = self.root / "artifact.tar.gz"
        artifact.write_bytes(b"tarball-payload")
        parser = ops.build_parser()
        args = parser.parse_args(
            [
                "render-template",
                "--release-id",
                "bs-v0.0.4",
                "--version",
                "0.0.4",
                "--sequence",
                "4",
                "--channel",
                "qa",
                "--type",
                "brainstem_release",
                "--object-key",
                "firmware/qa/bs-v0.0.4/brainstem.tar.gz",
                "--artifact",
                str(artifact),
            ]
        )
        tpl = ops.render_template(args)
        self.assertEqual(tpl["objectKey"], "firmware/qa/bs-v0.0.4/brainstem.tar.gz")
        self.assertEqual(tpl["manifest"]["artifactSizeBytes"], len(b"tarball-payload"))
        self.assertEqual(
            tpl["manifest"]["artifactSha256"],
            hashlib.sha256(b"tarball-payload").hexdigest(),
        )

        devices_file = self.root / "devices.json"
        ops.provision_device(
            devices_file=devices_file,
            device_id="13131313-1313-4131-8131-131313131313",
            model="THUNDER 01",
            hardware_revision="rdk-s100p",
            ring="canary",
            tags=["dog3", "rdk-s100p", "dog3"],
            enabled=True,
            dry_run=False,
        )
        saved = json.loads(devices_file.read_text(encoding="utf-8"))
        self.assertEqual(saved["13131313-1313-4131-8131-131313131313"]["ring"], "canary")
        self.assertEqual(saved["13131313-1313-4131-8131-131313131313"]["tags"], ["dog3", "rdk-s100p"])

        # ReleaseBundle template with bundleObjectKeys (empty top-level objectKey).
        cl_art = self.root / "cl.tar.gz"
        cl_art.write_bytes(b"cl-payload")
        bundle_spec = self.root / "bundle.json"
        bundle_spec.write_text(
            json.dumps(
                [
                    {
                        "component": "brainstem",
                        "version": "0.0.4",
                        "objectKey": "firmware/qa/b1/brainstem.tar.gz",
                        "artifact": str(artifact),
                    },
                    {
                        "component": "cloud-link",
                        "version": "0.2.0",
                        "objectKey": "firmware/qa/b1/cloud-link.tar.gz",
                        "artifact": str(cl_art),
                        "requires": {"brainstem": "0.0.4"},
                    },
                ]
            ),
            encoding="utf-8",
        )
        b_args = parser.parse_args(
            [
                "render-template",
                "--release-id",
                "bundle-0.0.4",
                "--version",
                "0.0.4",
                "--sequence",
                "5",
                "--channel",
                "qa",
                "--type",
                "release_bundle",
                "--bundle-spec",
                str(bundle_spec),
            ]
        )
        b_tpl = ops.render_template(b_args)
        self.assertEqual(b_tpl["objectKey"], "")
        self.assertEqual(
            b_tpl["bundleObjectKeys"],
            {
                "brainstem": "firmware/qa/b1/brainstem.tar.gz",
                "cloud-link": "firmware/qa/b1/cloud-link.tar.gz",
            },
        )
        self.assertEqual(len(b_tpl["manifest"]["bundleItems"]), 2)

    def test_verify_manifest_ed25519_and_rejects_tamper(self) -> None:
        priv = Ed25519PrivateKey.generate()
        pub_pem = priv.public_key().public_bytes(Encoding.PEM, PublicFormat.SubjectPublicKeyInfo)
        pub_path = self.root / "pub.pem"
        pub_path.write_bytes(pub_pem)

        payload = json.dumps({"releaseId": "r1", "version": "0.0.4"}).encode("utf-8")
        sig = priv.sign(payload)
        env_path = self.root / "env.json"
        env_path.write_text(
            json.dumps(
                {
                    "payload": base64.urlsafe_b64encode(payload).rstrip(b"=").decode("ascii"),
                    "signature": base64.urlsafe_b64encode(sig).rstrip(b"=").decode("ascii"),
                }
            ),
            encoding="utf-8",
        )
        decoded = ops.verify_manifest_envelope(env_path, pub_path)
        self.assertEqual(decoded["releaseId"], "r1")

        bad_env = self.root / "bad_env.json"
        bad_env.write_text(
            json.dumps(
                {
                    "payload": base64.urlsafe_b64encode(b'{"releaseId":"tampered"}').rstrip(b"=").decode("ascii"),
                    "signature": base64.urlsafe_b64encode(sig).rstrip(b"=").decode("ascii"),
                }
            ),
            encoding="utf-8",
        )
        with self.assertRaises(ops.OpsError):
            ops.verify_manifest_envelope(bad_env, pub_path)

    def test_oss_upload_dry_run_hides_secret_and_rollout_http_commands(self) -> None:
        artifact = self.root / "up.tar.gz"
        artifact.write_bytes(b"oss-bytes")
        old_ak = os.environ.get("DOSO_OSS_ACCESS_KEY_ID")
        old_sk = os.environ.get("DOSO_OSS_ACCESS_KEY_SECRET")
        old_tok = os.environ.get("DOSO_RELEASE_API_TOKEN")
        try:
            os.environ["DOSO_OSS_ACCESS_KEY_ID"] = "ak-test"
            os.environ["DOSO_OSS_ACCESS_KEY_SECRET"] = "top-secret-sk"
            os.environ["DOSO_RELEASE_API_TOKEN"] = "bearer-secret-token"
            rc = ops.main(
                [
                    "oss-upload",
                    "--file",
                    str(artifact),
                    "--object-key",
                    "firmware/qa/r1/up.tar.gz",
                    "--dry-run",
                ]
            )
            self.assertEqual(rc, 0)

            MockReleaseHandler.requests_log = []
            server = ThreadingHTTPServer(("127.0.0.1", 0), MockReleaseHandler)
            t = threading.Thread(target=server.serve_forever, daemon=True)
            t.start()
            try:
                base = f"http://127.0.0.1:{server.server_address[1]}/ota"
                self.assertEqual(
                    ops.main(["--base-url", base, "rollout", "create", "--release-id", "rel-1"]),
                    0,
                )
                self.assertEqual(ops.main(["--base-url", base, "rollout", "status", "ro-1"]), 0)
                self.assertEqual(ops.main(["--base-url", base, "rollout", "promote", "ro-1"]), 0)
                self.assertEqual(ops.main(["--base-url", base, "rollout", "abort", "ro-1"]), 0)
                self.assertEqual(ops.main(["--base-url", base, "fleet-status"]), 0)
                self.assertEqual(
                    ops.main(
                        [
                            "--base-url",
                            base,
                            "job",
                            "start",
                            "--device-id",
                            "dev-1",
                            "--release-id",
                            "rel-1",
                            "--idempotency-key",
                            "idem-1",
                        ]
                    ),
                    0,
                )
            finally:
                server.shutdown()
                server.server_close()

            paths = [(m, p, a) for m, p, a, _ in MockReleaseHandler.requests_log]
            self.assertIn(("POST", "/ota/v1/rollouts", "Bearer bearer-secret-token"), paths)
            self.assertIn(("GET", "/ota/v1/rollouts/ro-1", "Bearer bearer-secret-token"), paths)
            self.assertIn(("POST", "/ota/v1/rollouts/ro-1/promote", "Bearer bearer-secret-token"), paths)
            self.assertIn(("POST", "/ota/v1/rollouts/ro-1/abort", "Bearer bearer-secret-token"), paths)
            self.assertIn(("GET", "/ota/v1/devices/status", "Bearer bearer-secret-token"), paths)
            self.assertIn(
                ("POST", "/ota/v1/devices/dev-1/firmware/jobs", "Bearer bearer-secret-token"),
                paths,
            )
        finally:
            for k, old in (
                ("DOSO_OSS_ACCESS_KEY_ID", old_ak),
                ("DOSO_OSS_ACCESS_KEY_SECRET", old_sk),
                ("DOSO_RELEASE_API_TOKEN", old_tok),
            ):
                if old is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = old

    def test_default_ota_cloud_url_uses_formal_domain(self) -> None:
        old_url = os.environ.pop("DOSO_OTA_CLOUD_URL", None)
        try:
            parser = ops.build_parser()
            fleet_args = parser.parse_args(["fleet-status"])
            self.assertEqual(fleet_args.base_url, "https://api.dosorobotics.com/ota")
            env_args = parser.parse_args(
                [
                    "render-robot-env",
                    "--device-id",
                    "dev-1",
                    "--brainstem-version",
                    "0.0.3",
                    "--brainstem-release",
                    "v0.0.3",
                ]
            )
            self.assertEqual(env_args.cloud_url, "https://api.dosorobotics.com/ota")
        finally:
            if old_url is not None:
                os.environ["DOSO_OTA_CLOUD_URL"] = old_url


if __name__ == "__main__":
    unittest.main()
