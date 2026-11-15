#!/usr/bin/env python3
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


SCRIPT = Path(__file__).with_name("verify_gateway_grpc_preflight.py")
SPEC = importlib.util.spec_from_file_location("verify_gateway_grpc_preflight", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


class PreflightTest(unittest.TestCase):
    def make_inputs(self, complete: bool) -> list[str]:
        directory = Path(tempfile.mkdtemp())
        root = directory / "current"
        (root / "lib").mkdir(parents=True)
        required_libraries = (
            "liblingtu_nav_client.so",
            "liblingtu_inspection.so",
            "liblingtu_dds_probe.so",
        )
        for name in required_libraries:
            (root / "lib" / name).write_bytes(b"native")
        manifest = directory / "manifest.json"
        manifest.write_text(json.dumps({
            "architecture": "aarch64",
            "signature": "test",
            "requiredLibraries": required_libraries,
        }))
        gateway = directory / "gateway.env"
        gateway.write_text(
            "LINGTU_API_KEY=http-key\n"
            "LINGTU_GRPC_API_KEY=private\n"
            "LINGTU_GRPC_REQUIRE_API_KEY=true\n"
            f"LINGTU_GRPC_TLS_CERT_FILE={directory / 'server.crt'}\n"
            f"LINGTU_GRPC_TLS_KEY_FILE={directory / 'server.key'}\n"
        )
        (directory / "server.crt").write_text("cert")
        (directory / "server.key").write_text("key")
        (directory / "server.crt").chmod(0o600)
        (directory / "server.key").chmod(0o600)
        session = directory / "session.env"
        if complete:
            session.write_text("PRODUCT=field-cpp\n")
        return [
            "--manifest", str(manifest),
            "--gateway-env", str(gateway),
            "--session-file", str(session),
            "--current-root", str(root),
        ]

    def test_missing_session_fails_without_starting_service(self):
        parser = MODULE.argparse.ArgumentParser()
        parser.add_argument("--manifest", required=True)
        parser.add_argument("--gateway-env", required=True)
        parser.add_argument("--session-file", required=True)
        parser.add_argument("--current-root", required=True)
        args = parser.parse_args(self.make_inputs(False))
        with patch.object(MODULE, "service_state", return_value={"LoadState": "not-found", "ActiveState": "inactive"}):
            code, report = MODULE.run(args)
        self.assertEqual(code, 1)
        self.assertFalse(report["checks"]["product_session"])

    def test_complete_inputs_pass(self):
        parser = MODULE.argparse.ArgumentParser()
        parser.add_argument("--manifest", required=True)
        parser.add_argument("--gateway-env", required=True)
        parser.add_argument("--session-file", required=True)
        parser.add_argument("--current-root", required=True)
        args = parser.parse_args(self.make_inputs(True))
        with patch.object(MODULE, "service_state", return_value={"LoadState": "loaded", "ActiveState": "inactive"}):
            code, report = MODULE.run(args)
        self.assertEqual(code, 0)
        self.assertTrue(all(report["checks"].values()))


if __name__ == "__main__":
    unittest.main()
