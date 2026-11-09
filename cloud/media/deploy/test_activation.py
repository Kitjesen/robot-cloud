import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest.mock import patch


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(f"{name}.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ActivationTests(unittest.TestCase):
    def test_private_write_refuses_overwrite(self):
        activate = load("activate")
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "private.json"
            activate.private_write(path, json.dumps({"configured": True}))
            with self.assertRaises(FileExistsError):
                activate.private_write(path, "replacement")
            self.assertEqual(json.loads(path.read_text()), {"configured": True})

    def test_existing_vhost_stops_before_commands(self):
        activate = load("activate")
        with tempfile.TemporaryDirectory() as directory:
            stage = Path(directory)
            vhost = stage / "existing.conf"
            vhost.write_text("keep")
            with patch.object(activate, "STAGE", stage), patch.object(activate, "VHOST", vhost), patch.object(activate.os, "geteuid", return_value=0, create=True), patch.object(activate, "run") as command:
                with self.assertRaisesRegex(RuntimeError, "refuses existing"):
                    activate.main()
                command.assert_not_called()
            self.assertEqual(vhost.read_text(), "keep")

    def test_renewal_ignores_other_certificates(self):
        with patch.dict(sys.modules, {"grp": types.SimpleNamespace()}):
            renew = load("renew")
        with patch.dict(renew.os.environ, RENEWED_LINEAGE="unrelated"), patch.object(renew, "run") as command:
            renew.main()
            command.assert_not_called()

    def test_renewal_rejects_key_mismatch_before_install(self):
        with patch.dict(sys.modules, {"grp": types.SimpleNamespace()}):
            renew = load("renew")
        def command(*args):
            return b"cert public key" if "-pubkey" in args else b"different key"
        with patch.dict(renew.os.environ, RENEWED_LINEAGE=str(renew.LINEAGE)), patch.object(renew.os, "geteuid", return_value=0, create=True), patch.object(renew, "run", command), patch.object(renew.tempfile, "NamedTemporaryFile") as temporary:
            with self.assertRaisesRegex(RuntimeError, "do not match"):
                renew.main()
            temporary.assert_not_called()


if __name__ == "__main__":
    unittest.main()
