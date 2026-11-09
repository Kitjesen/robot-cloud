"""First activation on the reviewed MQTT ECS. Run as root after LE issuance."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


STAGE = Path("/root/doso-media-20261107/media")
DOMAIN = "media-test.dosorobotics.com"
VHOST = Path("/etc/nginx/conf.d/doso-media.conf")
LIVE = Path("/etc/letsencrypt/live") / DOMAIN
HOOK = Path("/etc/letsencrypt/renewal-hooks/deploy/doso-media")
FILES = (
    "bin/mediamtx", "bin/doso-cloud-media", "mediamtx.yml", "media.json",
    "tls/media.crt", "tls/media.key", "doso-mediamtx.service", "doso-cloud-media.service", "renew.py",
)


def run(*args):
    result = subprocess.run(args, check=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    return result.stdout.decode("utf-8").strip()


def private_write(path, value):
    with path.open("x", encoding="utf-8") as file:
        os.chmod(path, 0o600)
        file.write(value)


def main():
    if os.geteuid() != 0 or not STAGE.is_dir() or STAGE.is_symlink():
        raise RuntimeError("requires root and the reviewed private staging directory")
    os.umask(0o077)
    os.chmod(STAGE, 0o700)
    for path in (
        Path("/etc/doso-media"), Path("/opt/doso/bin/mediamtx"),
        Path("/opt/doso/bin/doso-cloud-media"), VHOST, HOOK,
        Path("/etc/systemd/system/doso-mediamtx.service"),
        Path("/etc/systemd/system/doso-cloud-media.service"),
    ):
        if path.exists() or path.is_symlink():
            raise RuntimeError(f"first activation refuses existing path {path}")
    for unit in ("doso-mediamtx.service", "doso-cloud-media.service"):
        if subprocess.run(["systemctl", "cat", unit], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0:
            raise RuntimeError(f"first activation refuses existing unit {unit}")
    run("nginx", "-t")
    run("systemctl", "is-active", "doso-robot-cloud-gateway.service")
    pid = run("systemctl", "show", "--property=MainPID", "--value", "doso-robot-cloud-gateway.service")
    if not pid.isdigit() or int(pid) <= 0:
        raise RuntimeError("Gateway has no active main process")
    if os.readlink(f"/proc/{pid}/exe") != "/opt/doso/bin/doso-cloud-gateway":
        raise RuntimeError("unexpected Gateway executable")
    values = [entry.partition(b"=")[2] for entry in Path(f"/proc/{pid}/environ").read_bytes().split(b"\0")
              if entry.startswith(b"DOSO_GATEWAY_API_KEY=")]
    if len(values) != 1 or not 32 <= len(values[0]) <= 256 or any(c < 33 or c > 126 for c in values[0]):
        raise RuntimeError("missing or invalid Gateway service credential")
    config_path = STAGE / "media.json"
    config = json.loads(config_path.read_text(encoding="utf-8"))
    config["sourceGatewayKey"] = values[0].decode("ascii")
    private_write(STAGE / "media.before-activation.json", config_path.read_text(encoding="utf-8"))
    config_path.write_text(json.dumps(config, indent=2) + "\n", encoding="utf-8")
    os.chmod(config_path, 0o600)
    (STAGE / "tls").mkdir(mode=0o700)
    for source, target in (("fullchain.pem", "media.crt"), ("privkey.pem", "media.key")):
        destination = STAGE / "tls" / target
        shutil.copyfile(LIVE / source, destination)
        os.chmod(destination, 0o600)
    private_write(STAGE / "SHA256SUMS", "".join(
        f"{hashlib.sha256((STAGE / name).read_bytes()).hexdigest()}  {name}\n" for name in FILES
    ))
    run("sh", str(STAGE / "install.sh"), str(STAGE))
    HOOK.parent.mkdir(parents=True, exist_ok=True)
    with HOOK.open("xb") as file:
        file.write((STAGE / "renew.py").read_bytes())
    os.chmod(HOOK, 0o755)
    proxy = (STAGE / "nginx-location.conf").read_text(encoding="utf-8")
    server = (
        f"server {{\n    listen 443 ssl;\n    server_name {DOMAIN};\n"
        f"    ssl_certificate {LIVE}/fullchain.pem;\n"
        f"    ssl_certificate_key {LIVE}/privkey.pem;\n"
        "    ssl_protocols TLSv1.2 TLSv1.3;\n"
        f"{proxy}\n    location / {{ return 404; }}\n}}\n"
    )
    added = False
    try:
        with VHOST.open("x", encoding="utf-8") as file:
            file.write(server)
        added = True
        os.chmod(VHOST, 0o644)
        run("nginx", "-t")
        run("systemctl", "reload", "nginx")
        run("systemctl", "start", "doso-cloud-media.service")
        deadline = time.monotonic() + 10
        while True:
            try:
                with urlopen("http://127.0.0.1:19890/api/v1/webrtc/status", timeout=2):
                    raise RuntimeError("anonymous media status unexpectedly allowed")
            except HTTPError as error:
                if error.code != 401:
                    raise RuntimeError(f"unexpected anonymous status {error.code}") from error
                break
            except URLError:
                if time.monotonic() >= deadline:
                    raise RuntimeError("media readiness timeout")
                time.sleep(0.2)
        request = Request("http://127.0.0.1:19890/api/v1/webrtc/status",
                          headers={"Authorization": "Bearer invalid-deployment-probe"})
        try:
            with urlopen(request, timeout=12):
                raise RuntimeError("invalid viewer credential unexpectedly allowed")
        except HTTPError as error:
            if error.code not in (401, 403):
                raise RuntimeError(f"unexpected invalid credential status {error.code}") from error
        run("systemctl", "is-active", "doso-cloud-media.service", "doso-mediamtx.service")
    except Exception:
        subprocess.run(["systemctl", "stop", "doso-cloud-media.service"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if added:
            VHOST.unlink()
            run("nginx", "-t")
            run("systemctl", "reload", "nginx")
        raise
    print("media_activation_passed services=active anonymous=401 invalid_viewer=rejected")
    print("video_source_control_pending existing_Gateway_requires_separate_upgrade")


if __name__ == "__main__":
    main()
