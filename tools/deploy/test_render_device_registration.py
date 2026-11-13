import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from uuid import UUID


SCRIPT = Path(__file__).with_name("render-device-registration.py")
DEVICE_ID = "d82a831a-2e86-4b46-975b-3d42473583b7"


class RenderDeviceRegistrationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.devices = self.root / "devices.json"
        self.acl = self.root / "acl.conf"
        self.key = self.root / "device-signing.pub"
        self.devices.write_text('{"devices": []}\n', encoding="utf-8")
        self.acl.write_text("user doso-gateway\n", encoding="utf-8")
        self.key.write_text(
            "-----BEGIN PUBLIC KEY-----\nexample\n-----END PUBLIC KEY-----\n",
            encoding="utf-8",
        )

    def tearDown(self):
        self.temp.cleanup()

    def run_script(self, device_id=DEVICE_ID):
        output = self.root / "out"
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--device-id",
                device_id,
                "--public-key",
                str(self.key),
                "--devices",
                str(self.devices),
                "--acl",
                str(self.acl),
                "--output-dir",
                str(output),
            ],
            capture_output=True,
            text=True,
        ), output

    def test_renders_disabled_entry_and_exact_read_only_acl(self):
        result, output = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        entry = json.loads((output / "device-entry.json").read_text())
        self.assertEqual(entry["deviceId"], DEVICE_ID)
        self.assertFalse(entry["enabled"])
        acl = (output / "acl-entry.conf").read_text()
        self.assertIn(f"topic read doso/v1/devices/{DEVICE_ID}/status/requests", acl)
        self.assertNotIn("motion", acl)
        self.assertNotIn("video/control", acl)
        self.assertEqual(UUID(entry["deviceId"]).version, 4)

    def test_rejects_duplicate_registry_entry_without_writing(self):
        self.devices.write_text(
            json.dumps({"devices": [{"deviceId": DEVICE_ID, "enabled": False, "publicKeyPath": "old.pub"}]}),
            encoding="utf-8",
        )
        result, output = self.run_script()
        self.assertEqual(result.returncode, 2)
        self.assertFalse(output.exists())

    def test_rejects_non_v4_id(self):
        result, output = self.run_script("d82a831a-2e86-5b46-975b-3d42473583b7")
        self.assertEqual(result.returncode, 2)
        self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
