"""Load the deployment schema with the pinned local MediaMTX, using private loopback ports."""

import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
from urllib.request import urlopen


def port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def main():
    root = Path(__file__).resolve().parent
    mediamtx = Path(os.environ["MEDIAMTX_BIN"])
    openssl = Path(os.environ["OPENSSL_BIN"])
    flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
    with tempfile.TemporaryDirectory(prefix="doso-media-config-") as directory:
        work = Path(directory)
        cert = work / "cert.pem"
        key = work / "key.pem"
        openssl_config = work / "openssl.cnf"
        openssl_config.write_text("[req]\ndistinguished_name=dn\n[dn]\n", encoding="utf-8")
        subprocess.run(
            [str(openssl), "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
             "-subj", "/CN=localhost", "-keyout", str(key), "-out", str(cert)],
            check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            creationflags=flags,
            env=os.environ | {"OPENSSL_CONF": str(openssl_config)},
        )
        api = port()
        config = (root / "mediamtx.yml").read_text(encoding="utf-8")
        config = config.replace("/etc/doso-media/tls/media.crt", cert.as_posix())
        config = config.replace("/etc/doso-media/tls/media.key", key.as_posix())
        config = config.replace("127.0.0.1:19997", f"127.0.0.1:{api}")
        config = config.replace("127.0.0.1:19889", f"127.0.0.1:{port()}")
        config = config.replace("rtspsAddress: :8322", f"rtspsAddress: 127.0.0.1:{port()}")
        config = config.replace("webrtcLocalUDPAddress: :8189", f"webrtcLocalUDPAddress: 127.0.0.1:{port()}")
        config = config.replace("webrtcLocalTCPAddress: :8189", f"webrtcLocalTCPAddress: 127.0.0.1:{port()}")
        config = config.replace("['8.153.169.177']", "['127.0.0.1']")
        path = work / "mediamtx.yml"
        path.write_text(config, encoding="utf-8")
        with (work / "log.txt").open("w+", encoding="utf-8") as log:
            process = subprocess.Popen([str(mediamtx), str(path)], stdout=log, stderr=log, creationflags=flags)
            try:
                deadline = time.monotonic() + 8
                while True:
                    if process.poll() is not None:
                        log.seek(0)
                        raise AssertionError(log.read())
                    try:
                        with urlopen(f"http://127.0.0.1:{api}/v3/config/global/get", timeout=1) as response:
                            actual = json.load(response)
                        break
                    except OSError:
                        assert time.monotonic() < deadline, "MediaMTX startup timeout"
                        time.sleep(0.1)
                assert actual["rtspEncryption"] == "strict"
                assert actual["webrtcLocalTCPAddress"]
                assert not any(p["action"] == "publish" for u in actual["authInternalUsers"] for p in u["permissions"])
                with urlopen(f"http://127.0.0.1:{api}/v3/config/pathdefaults/get", timeout=1) as response:
                    defaults = json.load(response)
                assert defaults["record"] is False
                assert defaults["overridePublisher"] is False
                print("media_deploy_config_check_passed")
            finally:
                if process.poll() is None:
                    process.terminate()
                process.wait(timeout=5)


if __name__ == "__main__":
    main()
