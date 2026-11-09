#!/usr/bin/python3
"""Certbot deploy hook for the dedicated DOSO media certificate."""

import grp
import os
from pathlib import Path
import subprocess
import tempfile


LINEAGE = Path("/etc/letsencrypt/live/media-test.dosorobotics.com")
TLS = Path("/etc/doso-media/tls")


def run(*args):
    return subprocess.run(args, check=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT).stdout


def main():
    if os.environ.get("RENEWED_LINEAGE") != str(LINEAGE):
        return
    if os.geteuid() != 0:
        raise RuntimeError("media certificate deployment requires root")
    cert, key = LINEAGE / "fullchain.pem", LINEAGE / "privkey.pem"
    run("openssl", "x509", "-in", str(cert), "-checkend", "86400", "-noout")
    run("openssl", "x509", "-in", str(cert), "-checkhost", "media-test.dosorobotics.com", "-noout")
    if run("openssl", "x509", "-in", str(cert), "-pubkey", "-noout") != run("openssl", "pkey", "-in", str(key), "-pubout"):
        raise RuntimeError("renewed certificate and key do not match")
    run("nginx", "-t")
    group = grp.getgrnam("doso-media").gr_gid
    for source, name in ((cert, "media.crt"), (key, "media.key")):
        temporary = None
        try:
            with tempfile.NamedTemporaryFile(dir=TLS, prefix=".renew-", delete=False) as file:
                temporary = Path(file.name)
                os.fchown(file.fileno(), 0, group)
                os.fchmod(file.fileno(), 0o640)
                file.write(source.read_bytes())
                file.flush()
                os.fsync(file.fileno())
            os.replace(temporary, TLS / name)
        finally:
            if temporary is not None:
                temporary.unlink(missing_ok=True)
    run("systemctl", "restart", "doso-cloud-media.service")
    run("systemctl", "is-active", "doso-cloud-media.service", "doso-mediamtx.service")
    run("systemctl", "reload", "nginx")
    print("media_certificate_deployed")


if __name__ == "__main__":
    main()
