#!/usr/bin/env python3
"""One-click robot provisioning engine for DOSO OTA Console.

Orchestrates:
1. Robot probe (architecture, hardware revision, brainstem health, existing UUID safety check).
2. Business inventory registration on SWAS Supabase (public.devices + private.device_remote_state).
3. OTA device registration via cloud PUT /v1/devices/{id}, /etc/doso/ota.env & pubkey sync, and doso-ota activation.
4. 443-WSS outbound ops tunnel installation (doso-ops-tunnel.service on dedicated port 22000 + N).
5. On-robot private key + CSR + Ed25519 signing key generation (private keys never leave the robot).
6. MQTT client certificate issuance & Gateway/ACL registration on ECS 8.153.169.177 — either fully
   automatic via Aliyun ECS Cloud Assistant OpenAPI (when ECS RAM AK/SK is configured) or via a
   one-paste Cloud Assistant script + one-click certificate install & activation.
"""

from __future__ import annotations

import base64
import datetime
import hashlib
import hmac
import json
import os
import re
import shlex
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from pathlib import Path
from typing import Any

import robot_ops


UUID_V4_RE = re.compile(
    r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
)
EMAIL_RE = re.compile(r"^[^@\s]+@[^@\s]+\.[^@\s]+$")
SERIAL_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{1,63}$")
TUNNEL_NAME_RE = re.compile(r"^[a-z0-9][a-z0-9_-]{1,31}$")

ECS_BUNDLE_BEGIN = "===DOSO_ECS_BUNDLE_BEGIN==="
ECS_BUNDLE_END = "===DOSO_ECS_BUNDLE_END==="


class ProvisionError(ValueError):
    """Actionable provisioning error."""


def is_valid_uuid_v4(value: str) -> bool:
    return bool(UUID_V4_RE.fullmatch((value or "").strip()))


def _sql_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def infer_hardware_revision(dt_model: str, fallback: str = "rdk-s100p") -> str:
    low = (dt_model or "").lower()
    if "xavier nx" in low or "jetson" in low:
        return "jetson-xavier-nx"
    if "s100" in low:
        return "rdk-s100p"
    if "x5" in low:
        return "rdk-x5"
    return fallback


def build_direct_target(req: dict[str, Any], cfg: dict[str, str]) -> dict[str, Any]:
    host = str(req.get("host") or "").strip()
    port = int(req.get("port") or 22)
    username = str(req.get("username") or cfg.get("DOSO_OPS_ROBOT_USER") or "sunrise").strip()
    password = str(req.get("password") or cfg.get("DOSO_OPS_ROBOT_PASSWORD") or "").strip()
    if not host:
        raise ProvisionError("请填写机器狗 IP 地址")
    return {
        "mode": "direct",
        "host": host,
        "port": port,
        "username": username,
        "password": password,
    }


def _find_local_frpc(repo_root: Path | None) -> Path | None:
    candidates: list[Path] = []
    if repo_root is not None:
        candidates.append(repo_root / "tools" / "ops-tunnel" / "frpc")
    here = Path(__file__).resolve().parent
    candidates.append(here.parent / "ops-tunnel" / "frpc")
    candidates.append(here / "frpc")
    for p in candidates:
        if p.is_file() and p.stat().st_size > 100_000:
            return p
    return None


def _suggest_next_slot(cfg: dict[str, str]) -> dict[str, Any]:
    """Query SWAS Supabase + frps to suggest next unused robot number and tunnel port."""
    used_nums: set[int] = set()
    used_ports: set[int] = set()
    existing_devices: list[dict[str, Any]] = []
    try:
        jump = robot_ops._connect_jump(cfg, timeout_s=8)
        try:
            cmd = (
                "sudo -n docker exec supabase-db psql -U postgres -d postgres -At -F '|' "
                "-c \"SELECT id, COALESCE(serial_number,''), COALESCE(display_name,''), model_code FROM public.devices ORDER BY created_at;\" 2>/dev/null || true; "
                "echo '===PORTS==='; "
                "ss -tln | awk '{print $4}'"
            )
            _, out, _ = robot_ops._exec_cmd(jump, cmd, timeout_s=10)
            in_ports = False
            for line in out.splitlines():
                s = line.strip()
                if s == "===PORTS===":
                    in_ports = True
                    continue
                if not s:
                    continue
                if not in_ports and "|" in s:
                    parts = s.split("|")
                    if len(parts) >= 4:
                        existing_devices.append(
                            {
                                "deviceId": parts[0],
                                "serialNumber": parts[1],
                                "displayName": parts[2],
                                "modelCode": parts[3],
                            }
                        )
                        m = re.search(r"(\d+)$", parts[1])
                        if m:
                            used_nums.add(int(m.group(1)))
                elif in_ports:
                    mp = re.search(r"(?:127\.0\.0\.1|\[::1\]):(\d+)$", s)
                    if mp:
                        port = int(mp.group(1))
                        if 22001 <= port <= 22999:
                            used_ports.add(port)
                            used_nums.add(port - 22000)
        finally:
            jump.close()
    except Exception:
        pass

    next_num = 1
    while next_num in used_nums or (22000 + next_num) in used_ports:
        next_num += 1
    return {
        "suggestedNumber": next_num,
        "suggestedSerial": f"THUNDER-DEV-{next_num:03d}",
        "suggestedDisplayName": f"THUNDER 研发机 {next_num:03d}",
        "suggestedTunnelPort": 22000 + next_num,
        "suggestedUuid": str(uuid.uuid4()),
        "existingInventory": existing_devices,
    }


def probe_robot(cfg: dict[str, str], req: dict[str, Any]) -> dict[str, Any]:
    target = build_direct_target(req, cfg)
    script = (
        "printf '===META===\\n'; "
        "printf 'HOST=%s\\n' \"$(hostname)\"; "
        "printf 'ARCH=%s\\n' \"$(uname -m)\"; "
        "printf 'DT=%s\\n' \"$(cat /proc/device-tree/model 2>/dev/null | tr -d '\\0' || echo unknown)\"; "
        "printf 'HAS_OTA=%s\\n' \"$(test -x /opt/doso/ota/bin/doso-ota && echo 1 || echo 0)\"; "
        "printf 'HAS_CLOUD_LINK=%s\\n' \"$(test -x /opt/doso/bin/doso-cloud-link && echo 1 || echo 0)\"; "
        "printf 'HAS_FRPC=%s\\n' \"$(test -x /opt/doso/bin/frpc && echo 1 || echo 0)\"; "
        "printf 'HAS_CRT=%s\\n' \"$(test -s /etc/doso/robot-cloud/agent/device.crt && echo 1 || echo 0)\"; "
        "printf '===SVCS===\\n'; "
        "for u in doso-brainstem.service doso-ota.service doso-cloud-link.service doso-ops-tunnel.service; do "
        "  printf '%s=%s\\n' \"$u\" \"$(systemctl is-active \"$u\" 2>/dev/null || echo inactive)\"; "
        "done; "
        "printf '===IDS===\\n'; "
        "printf 'OTA_ID=%s\\n' \"$(grep -E '^DOSO_DEVICE_ID=' /etc/doso/ota.env 2>/dev/null | head -n1 | cut -d= -f2- | tr -d '\\r\\n' || true)\"; "
        "printf 'AGENT_ID=%s\\n' \"$(grep -E '^DOSO_DEVICE_ID=' /etc/doso/robot-cloud/agent/agent.env 2>/dev/null | head -n1 | cut -d= -f2- | tr -d '\\r\\n' || true)\"; "
        "printf 'OTA_MODEL=%s\\n' \"$(grep -E '^DOSO_MODEL=' /etc/doso/ota.env 2>/dev/null | head -n1 | cut -d= -f2- | tr -d '\\r\\n' || true)\"; "
        "printf 'OTA_HW=%s\\n' \"$(grep -E '^DOSO_HARDWARE_REVISION=' /etc/doso/ota.env 2>/dev/null | head -n1 | cut -d= -f2- | tr -d '\\r\\n' || true)\"; "
        "printf '===HEALTH===\\n'; "
        "curl -s --max-time 3 http://127.0.0.1:5054/health || echo '{}'"
    )
    with robot_ops.open_robot_ssh(cfg, target, timeout_s=10) as client:
        code, out, err = robot_ops._exec_sudo(client, cfg, script, timeout_s=15, target=target)
        if code != 0 and "===META===" not in out:
            raise ProvisionError(f"探测机器狗失败（检查账号密码与 sudo 权限）: {err.strip() or out.strip()}")

    sections: dict[str, list[str]] = {}
    cur = ""
    for line in out.splitlines():
        m = re.fullmatch(r"===([A-Z]+)===", line.strip())
        if m:
            cur = m.group(1)
            sections[cur] = []
        elif cur:
            sections[cur].append(line)

    meta: dict[str, str] = {}
    for line in sections.get("META", []) + sections.get("IDS", []):
        if "=" in line:
            k, v = line.split("=", 1)
            meta[k.strip()] = v.strip()
    svcs: dict[str, str] = {}
    for line in sections.get("SVCS", []):
        if "=" in line:
            k, v = line.split("=", 1)
            svcs[k.strip()] = v.strip()
    try:
        health = json.loads("\n".join(sections.get("HEALTH", [])).strip() or "{}")
    except json.JSONDecodeError:
        health = {}

    dt_model = meta.get("DT", "unknown")
    hw_rev = meta.get("OTA_HW") or infer_hardware_revision(dt_model, cfg.get("DOSO_DEFAULT_HW_REV", "rdk-s100p"))
    ota_id = meta.get("OTA_ID", "")
    agent_id = meta.get("AGENT_ID", "")
    existing_valid_uuid = ota_id if is_valid_uuid_v4(ota_id) else (agent_id if is_valid_uuid_v4(agent_id) else "")
    slot = _suggest_next_slot(cfg)

    return {
        "host": target["host"],
        "hostname": meta.get("HOST", ""),
        "arch": meta.get("ARCH", ""),
        "deviceTreeModel": dt_model,
        "hardwareRevision": hw_rev,
        "model": meta.get("OTA_MODEL") or cfg.get("DOSO_DEFAULT_MODEL") or "THUNDER 01",
        "brainstemVersion": str(health.get("version") or health.get("release_id") or ""),
        "motorOutputEnabled": bool(health.get("motor_output_enabled", False)),
        "hasOtaBin": meta.get("HAS_OTA") == "1",
        "hasCloudLinkBin": meta.get("HAS_CLOUD_LINK") == "1",
        "hasFrpcBin": meta.get("HAS_FRPC") == "1",
        "hasDeviceCert": meta.get("HAS_CRT") == "1",
        "services": svcs,
        "existingOtaDeviceId": ota_id,
        "existingAgentDeviceId": agent_id,
        "existingValidUuid": existing_valid_uuid,
        "alreadyProvisioned": bool(existing_valid_uuid),
        "ecsAutoConfigured": bool(
            (cfg.get("DOSO_ECS_ACCESS_KEY_ID") or "").strip()
            and (cfg.get("DOSO_ECS_ACCESS_KEY_SECRET") or "").strip()
        ),
        **slot,
    }


def build_ecs_issue_script(device_id: str, csr_pem: str, signing_pub_pem: str) -> str:
    """Build a self-contained idempotent root script for ECS 8.153.169.177 Cloud Assistant."""
    if not is_valid_uuid_v4(device_id):
        raise ProvisionError("device_id 必须是合法的小写 UUID v4")
    csr_b64 = base64.b64encode(csr_pem.encode("utf-8")).decode("ascii")
    pub_b64 = base64.b64encode(signing_pub_pem.encode("utf-8")).decode("ascii")
    return f"""#!/usr/bin/env bash
set -euo pipefail

DEVICE_ID="{device_id}"
CSR_B64="{csr_b64}"
PUB_B64="{pub_b64}"
GATEWAY_DIR="/etc/doso/robot-cloud/gateway"
ACL_FILE="/etc/doso-mqtt/acl.conf"
MOSQ_CONF="/etc/doso-mqtt/mosquitto.conf"
REGISTRY_FILE="$GATEWAY_DIR/devices.json"
KEY_NAME="device-${{DEVICE_ID}}-signing.pub"

TMP_DIR="$(mktemp -d /tmp/doso-ecs-issue.XXXXXX)"
trap 'rm -rf "$TMP_DIR"' EXIT
printf '%s' "$CSR_B64" | base64 -d > "$TMP_DIR/device.csr"
printf '%s' "$PUB_B64" | base64 -d > "$TMP_DIR/$KEY_NAME"

# 1. Verify CSR CN matches DEVICE_ID
CSR_SUBJ="$(openssl req -in "$TMP_DIR/device.csr" -noout -subject)"
echo "$CSR_SUBJ" | grep -Fq "CN = $DEVICE_ID" || echo "$CSR_SUBJ" | grep -Fq "CN=$DEVICE_ID" || {{
  echo "ERROR: CSR subject mismatch: $CSR_SUBJ" >&2
  exit 1
}}

# 2. Locate active MQTT CA cert and key
CA_CERT="$(awk '$1=="cafile"{{print $2; exit}}' "$MOSQ_CONF" 2>/dev/null || true)"
if [[ -z "$CA_CERT" || ! -f "$CA_CERT" ]]; then
  for c in /etc/doso-mqtt/ca.crt /etc/doso-mqtt/certs/ca.crt /root/doso-mqtt-ca/ca.crt; do
    if [[ -f "$c" ]]; then CA_CERT="$c"; break; fi
  done
fi
CA_KEY=""
for k in "${{CA_CERT%.*}}.key" /etc/doso-mqtt/ca.key /etc/doso-mqtt/certs/ca.key /root/doso-mqtt-ca/ca.key /etc/doso-mqtt/private/ca.key /root/ca.key; do
  if [[ -f "$k" ]]; then CA_KEY="$k"; break; fi
done
if [[ -z "$CA_KEY" ]]; then
  CA_KEY="$(find /etc/doso-mqtt /root /etc/doso -maxdepth 3 -name '*ca*.key' 2>/dev/null | head -n1 || true)"
fi
test -f "$CA_CERT" && test -f "$CA_KEY" || {{
  echo "ERROR: MQTT CA cert/key not found (CA_CERT=$CA_CERT CA_KEY=$CA_KEY)" >&2
  exit 1
}}

# 3. Sign clientAuth certificate (365 days)
cat > "$TMP_DIR/ext.cnf" <<'EXT'
basicConstraints=CA:FALSE
keyUsage=digitalSignature,keyEncipherment
extendedKeyUsage=clientAuth
EXT
openssl x509 -req -in "$TMP_DIR/device.csr" -CA "$CA_CERT" -CAkey "$CA_KEY" -CAcreateserial \
  -out "$TMP_DIR/device.crt" -days 365 -sha256 -extfile "$TMP_DIR/ext.cnf" 2>/dev/null
openssl verify -CAfile "$CA_CERT" "$TMP_DIR/device.crt" >/dev/null

# 4. Back up and atomically register in Gateway devices.json + pubkey + Mosquitto ACL
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
BACKUP="/root/doso-device-register-$STAMP"
mkdir -m 700 "$BACKUP"
cp -a "$REGISTRY_FILE" "$BACKUP/devices.json"
cp -a "$ACL_FILE" "$BACKUP/acl.conf"

install -o root -g doso-cloud -m 0640 "$TMP_DIR/$KEY_NAME" "$GATEWAY_DIR/$KEY_NAME"

python3 - "$REGISTRY_FILE" "$DEVICE_ID" "$KEY_NAME" <<'PY'
import json, os, sys, tempfile
from pathlib import Path
reg_path, dev_id, key_name = Path(sys.argv[1]), sys.argv[2], sys.argv[3]
data = json.loads(reg_path.read_text(encoding="utf-8"))
devices = data.setdefault("devices", [])
found = False
for item in devices:
    if isinstance(item, dict) and item.get("deviceId") == dev_id:
        item["enabled"] = True
        item["publicKeyPath"] = key_name
        found = True
if not found:
    devices.append({{"deviceId": dev_id, "enabled": True, "publicKeyPath": key_name}})
fd, tmp = tempfile.mkstemp(prefix=".devices.json.", dir=str(reg_path.parent))
try:
    with os.fdopen(fd, "w", encoding="utf-8") as fh:
        json.dump(data, fh, indent=2)
        fh.write("\\n")
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp, reg_path)
finally:
    if os.path.exists(tmp):
        os.unlink(tmp)
PY
chown root:doso-cloud "$REGISTRY_FILE"
chmod 0640 "$REGISTRY_FILE"

if ! grep -Fqx "user $DEVICE_ID" "$ACL_FILE"; then
  cat >> "$ACL_FILE" <<ACL

user $DEVICE_ID
topic read doso/v1/devices/$DEVICE_ID/status/requests
topic write doso/v1/devices/$DEVICE_ID/status/replies
topic write doso/v1/devices/$DEVICE_ID/reporting/hello
topic read doso/v1/devices/$DEVICE_ID/reporting/control
topic write doso/v1/devices/$DEVICE_ID/reporting/acks
topic write doso/v1/devices/$DEVICE_ID/status/reports
ACL
fi
chown mosquitto:mosquitto "$ACL_FILE"
chmod 0640 "$ACL_FILE"
mosquitto --test-config -c "$MOSQ_CONF"
systemctl reload mosquitto

# 5. Locate gateway signing public key and emit copy-paste JSON bundle
GW_PUB=""
for gp in "$GATEWAY_DIR/gateway-signing.pub" /etc/doso/robot-cloud/gateway/signing.pub; do
  if [[ -f "$gp" ]]; then GW_PUB="$gp"; break; fi
done
if [[ -z "$GW_PUB" && -f "$GATEWAY_DIR/gateway-signing.key" ]]; then
  openssl pkey -in "$GATEWAY_DIR/gateway-signing.key" -pubout -out "$TMP_DIR/gw.pub" 2>/dev/null || true
  if [[ -s "$TMP_DIR/gw.pub" ]]; then GW_PUB="$TMP_DIR/gw.pub"; fi
fi

python3 - "$DEVICE_ID" "$TMP_DIR/device.crt" "$CA_CERT" "$GW_PUB" <<'PY'
import json, sys
from pathlib import Path
dev_id, crt_p, ca_p, gw_p = sys.argv[1:]
bundle = {{
    "deviceId": dev_id,
    "deviceCrt": Path(crt_p).read_text(encoding="utf-8"),
    "caCrt": Path(ca_p).read_text(encoding="utf-8"),
    "gatewaySigningPub": Path(gw_p).read_text(encoding="utf-8") if gw_p and Path(gw_p).is_file() else "",
}}
print("{ECS_BUNDLE_BEGIN}")
print(json.dumps(bundle, ensure_ascii=False))
print("{ECS_BUNDLE_END}")
PY
"""


def _aliyun_ecs_rpc(cfg: dict[str, str], action: str, extra: dict[str, str]) -> dict[str, Any]:
    ak = (cfg.get("DOSO_ECS_ACCESS_KEY_ID") or "").strip()
    sk = (cfg.get("DOSO_ECS_ACCESS_KEY_SECRET") or "").strip()
    region = (cfg.get("DOSO_ECS_REGION") or "cn-shanghai").strip()
    if not ak or not sk:
        raise ProvisionError("未配置阿里云 ECS 运维 AccessKey")

    params: dict[str, str] = {
        "Format": "JSON",
        "Version": "2014-05-26",
        "AccessKeyId": ak,
        "SignatureMethod": "HMAC-SHA1",
        "Timestamp": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "SignatureVersion": "1.0",
        "SignatureNonce": str(uuid.uuid4()),
        "Action": action,
        "RegionId": region,
    }
    params.update(extra)
    canonical_query = "&".join(
        f"{urllib.parse.quote(k, safe='~')}={urllib.parse.quote(str(v), safe='~')}"
        for k, v in sorted(params.items())
    )
    string_to_sign = "POST&%2F&" + urllib.parse.quote(canonical_query, safe="~")
    sig = base64.b64encode(
        hmac.new((sk + "&").encode("utf-8"), string_to_sign.encode("utf-8"), hashlib.sha1).digest()
    ).decode("ascii")
    body = (canonical_query + "&Signature=" + urllib.parse.quote(sig, safe="~")).encode("utf-8")
    url = f"https://ecs.{region}.aliyuncs.com/"
    req = urllib.request.Request(
        url,
        data=body,
        method="POST",
        headers={"Content-Type": "application/x-www-form-urlencoded"},
    )
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as exc:
        detail = exc.read().decode("utf-8", errors="replace")[:400]
        raise ProvisionError(f"阿里云 ECS 接口失败 ({exc.code}): {detail}") from exc
    except Exception as exc:
        raise ProvisionError(f"调用阿里云 ECS 接口异常: {exc}") from exc


def parse_ecs_bundle_output(raw_text: str) -> dict[str, str]:
    text = (raw_text or "").strip()
    if not text:
        raise ProvisionError("请粘贴签发回执内容")

    if ECS_BUNDLE_BEGIN in text and ECS_BUNDLE_END in text:
        body = text.split(ECS_BUNDLE_BEGIN, 1)[1].split(ECS_BUNDLE_END, 1)[0].strip()
        try:
            obj = json.loads(body)
        except json.JSONDecodeError as exc:
            raise ProvisionError(f"签发回执 JSON 解析失败: {exc}") from exc
    elif text.startswith("{"):
        try:
            obj = json.loads(text)
        except json.JSONDecodeError as exc:
            raise ProvisionError(f"JSON 解析失败: {exc}") from exc
    elif "-----BEGIN CERTIFICATE-----" in text:
        obj = {"deviceCrt": text}
    else:
        raise ProvisionError("未识别的证书回执格式，请粘贴完整 ECS 输出或 PEM 证书")

    device_crt = str(obj.get("deviceCrt") or "").strip()
    if "-----BEGIN CERTIFICATE-----" not in device_crt:
        raise ProvisionError("回执中缺少有效的 deviceCrt PEM 证书")
    return {
        "deviceId": str(obj.get("deviceId") or "").strip(),
        "deviceCrt": device_crt + "\n",
        "caCrt": (str(obj.get("caCrt") or "").strip() + "\n") if str(obj.get("caCrt") or "").strip() else "",
        "gatewaySigningPub": (
            (str(obj.get("gatewaySigningPub") or "").strip() + "\n")
            if str(obj.get("gatewaySigningPub") or "").strip()
            else ""
        ),
    }


def run_ecs_issue_via_cloud_assistant(cfg: dict[str, str], script: str) -> dict[str, str]:
    instance_id = (cfg.get("DOSO_ECS_INSTANCE_ID") or "i-uf6hb2phe34p0mt1xfca").strip()
    run_res = _aliyun_ecs_rpc(
        cfg,
        "RunCommand",
        {
            "Type": "RunShellScript",
            "InstanceId.1": instance_id,
            "CommandContent": base64.b64encode(script.encode("utf-8")).decode("ascii"),
            "ContentEncoding": "Base64",
            "Timeout": "60",
            "Name": f"doso-issue-{int(time.time())}",
        },
    )
    invoke_id = str(run_res.get("InvokeId") or "").strip()
    if not invoke_id:
        raise ProvisionError(f"ECS 云助手未返回 InvokeId: {run_res}")

    for _ in range(30):
        time.sleep(2)
        res = _aliyun_ecs_rpc(
            cfg,
            "DescribeInvocationResults",
            {
                "InvokeId": invoke_id,
                "InstanceId": instance_id,
                "ContentEncoding": "PlainText",
            },
        )
        items = (
            res.get("Invocation", {})
            .get("InvocationResults", {})
            .get("InvocationResult", [])
        )
        if not items:
            continue
        item = items[0]
        status = str(item.get("InvocationStatus") or "")
        if status in ("Pending", "Running", "Stopping"):
            continue
        output = str(item.get("Output") or "")
        exit_code = item.get("ExitCode")
        if status != "Finished" or (exit_code not in (0, "0", None)):
            raise ProvisionError(f"ECS 云助手签发失败 ({status}, exit={exit_code}): {output[-400:]}")
        parsed = parse_ecs_bundle_output(output)
        parsed["invokeId"] = invoke_id
        return parsed

    raise ProvisionError(f"ECS 云助手执行超时 (InvokeId={invoke_id})")


def finish_mqtt_on_robot(
    cfg: dict[str, str],
    req: dict[str, Any],
) -> dict[str, Any]:
    """Install signed MQTT certs on robot, verify key/cert match, and enable doso-cloud-link."""
    target = build_direct_target(req, cfg)
    raw_bundle = str(req.get("bundleText") or "").strip()
    if raw_bundle:
        bundle = parse_ecs_bundle_output(raw_bundle)
    else:
        bundle = {
            "deviceId": str(req.get("deviceId") or "").strip(),
            "deviceCrt": str(req.get("deviceCrt") or "").strip(),
            "caCrt": str(req.get("caCrt") or "").strip(),
            "gatewaySigningPub": str(req.get("gatewaySigningPub") or "").strip(),
        }
        if "-----BEGIN CERTIFICATE-----" not in bundle["deviceCrt"]:
            raise ProvisionError("缺少有效的设备证书 deviceCrt")
        bundle["deviceCrt"] += "\n"
        if bundle["caCrt"]:
            bundle["caCrt"] += "\n"
        if bundle["gatewaySigningPub"]:
            bundle["gatewaySigningPub"] += "\n"

    enable_service = bool(req.get("enableService", True))
    device_id = str(req.get("deviceId") or bundle.get("deviceId") or "").strip()
    if device_id and not is_valid_uuid_v4(device_id):
        raise ProvisionError("deviceId 格式无效")

    steps: list[dict[str, Any]] = []
    with robot_ops.open_robot_ssh(cfg, target, timeout_s=12) as client:
        sftp = client.open_sftp()
        try:
            tmp_crt = f"/tmp/.doso_device_{uuid.uuid4().hex[:8]}.crt"
            with sftp.open(tmp_crt, "wb") as fh:
                fh.write(bundle["deviceCrt"].encode("utf-8"))
            tmp_ca = ""
            if bundle["caCrt"]:
                tmp_ca = f"/tmp/.doso_ca_{uuid.uuid4().hex[:8]}.crt"
                with sftp.open(tmp_ca, "wb") as fh:
                    fh.write(bundle["caCrt"].encode("utf-8"))
            tmp_gw = ""
            if bundle["gatewaySigningPub"]:
                tmp_gw = f"/tmp/.doso_gw_{uuid.uuid4().hex[:8]}.pub"
                with sftp.open(tmp_gw, "wb") as fh:
                    fh.write(bundle["gatewaySigningPub"].encode("utf-8"))
        finally:
            sftp.close()

        inner_lines = [
            "set -euo pipefail",
            f"trap 'rm -f {shlex.quote(tmp_crt)} {shlex.quote(tmp_ca) if tmp_ca else ''} {shlex.quote(tmp_gw) if tmp_gw else ''}' EXIT",
            "AGENT_DIR=/etc/doso/robot-cloud/agent",
            "test -f \"$AGENT_DIR/device.key\" || { echo 'MISSING_DEVICE_KEY' >&2; exit 1; }",
            # Verify certificate public key matches on-robot device.key
            f"CRT_PUB=$(openssl x509 -in {shlex.quote(tmp_crt)} -pubkey -noout)",
            "KEY_PUB=$(openssl pkey -in \"$AGENT_DIR/device.key\" -pubout)",
            "test \"$CRT_PUB\" = \"$KEY_PUB\" || { echo 'CERT_KEY_MISMATCH' >&2; exit 1; }",
        ]
        if device_id:
            inner_lines.append(
                f"SUBJ=$(openssl x509 -in {shlex.quote(tmp_crt)} -noout -subject); "
                f"echo \"$SUBJ\" | grep -Fq {shlex.quote(device_id)} || {{ echo \"CERT_CN_MISMATCH:$SUBJ\" >&2; exit 1; }}"
            )
        inner_lines.extend(
            [
                "install -d -o root -g root -m 0700 /root/doso-link-private",
                f"install -o root -g root -m 0600 {shlex.quote(tmp_crt)} /root/doso-link-private/device.crt",
                f"install -o root -g doso-cloud -m 0640 {shlex.quote(tmp_crt)} \"$AGENT_DIR/device.crt\"",
            ]
        )
        if tmp_ca:
            inner_lines.append(
                f"install -o root -g doso-cloud -m 0640 {shlex.quote(tmp_ca)} \"$AGENT_DIR/ca.crt\""
            )
        if tmp_gw:
            inner_lines.append(
                f"install -o root -g doso-cloud -m 0640 {shlex.quote(tmp_gw)} \"$AGENT_DIR/gateway-signing.pub\""
            )
        inner_lines.append("test -s \"$AGENT_DIR/ca.crt\" || { echo 'MISSING_CA_CRT' >&2; exit 1; }")
        inner_lines.append("test -s \"$AGENT_DIR/gateway-signing.pub\" || { echo 'MISSING_GW_PUB' >&2; exit 1; }")

        if enable_service:
            inner_lines.extend(
                [
                    "install -d -o doso-cloud -g doso-cloud -m 0700 /var/lib/doso-cloud",
                    "printf 'true\\n' > /var/lib/doso-cloud/choice",
                    "chown doso-cloud:doso-cloud /var/lib/doso-cloud/choice",
                    "chmod 0600 /var/lib/doso-cloud/choice",
                    "sed -i 's/^DOSO_CLOUD_ON=.*/DOSO_CLOUD_ON=true/' \"$AGENT_DIR/agent.env\"",
                    "systemctl daemon-reload",
                    "systemctl enable --now doso-cloud-link.service",
                    "systemctl restart doso-cloud-link.service",
                    "sleep 3",
                    "echo STATE=$(systemctl is-active doso-cloud-link.service || true)",
                ]
            )
        else:
            inner_lines.append("echo STATE=installed_disabled")

        code, out, err = robot_ops._exec_sudo(client, cfg, "\n".join(inner_lines), timeout_s=25, target=target)
        if code != 0:
            raise ProvisionError(f"写入证书失败: {(err or out).strip()}")

    state_val = "unknown"
    for line in out.splitlines():
        if line.startswith("STATE="):
            state_val = line.split("=", 1)[1].strip()
    steps.append({"step": "安装设备证书并校验公私钥匹配", "status": "ok", "detail": f"doso-cloud-link: {state_val}"})
    return {
        "ok": True,
        "deviceId": device_id,
        "serviceState": state_val,
        "steps": steps,
    }


def _upsert_swas_inventory(
    cfg: dict[str, str],
    *,
    device_id: str,
    serial_number: str,
    display_name: str,
    model: str,
    owner_email: str,
) -> str:
    jump = robot_ops._connect_jump(cfg, timeout_s=10)
    try:
        owner_clause = "NULL"
        if owner_email:
            if not EMAIL_RE.fullmatch(owner_email):
                raise ProvisionError(f"归属账号邮箱格式无效: {owner_email}")
            owner_clause = f"(SELECT id FROM auth.users WHERE lower(email) = lower({_sql_literal(owner_email)}) LIMIT 1)"

        sql = (
            "BEGIN; "
            f"INSERT INTO public.devices (id, owner_id, model_code, display_name, category, environment, serial_number, is_demo, is_simulated) "
            f"VALUES ({_sql_literal(device_id)}::uuid, {owner_clause}, {_sql_literal(model)}, {_sql_literal(display_name)}, "
            f"'四足', '室内', {_sql_literal(serial_number)}, false, false) "
            f"ON CONFLICT (id) DO UPDATE SET display_name = EXCLUDED.display_name, model_code = EXCLUDED.model_code; "
            f"INSERT INTO private.device_remote_state (device_id, owner_id, authorization_epoch, revision, enabled) "
            f"SELECT id, owner_id, robot_authorization_epoch, 1, true FROM public.devices WHERE id = {_sql_literal(device_id)}::uuid "
            f"ON CONFLICT (device_id) DO UPDATE SET enabled = true, owner_id = EXCLUDED.owner_id; "
            "COMMIT;"
        )
        cmd = f"sudo -n docker exec -i supabase-db psql -v ON_ERROR_STOP=1 -U postgres -d postgres"
        code, out, err = robot_ops._exec_cmd(jump, cmd, timeout_s=15, stdin_text=sql + "\n")
        if code != 0:
            raise ProvisionError(f"写入云端业务设备库存失败: {(err or out).strip()}")

        verify_sql = f"SELECT id, serial_number, COALESCE(owner_id::text, '') FROM public.devices WHERE id = {_sql_literal(device_id)}::uuid;"
        _, vout, _ = robot_ops._exec_cmd(
            jump,
            "sudo -n docker exec -i supabase-db psql -At -F '|' -U postgres -d postgres",
            timeout_s=10,
            stdin_text=verify_sql + "\n",
        )
        return vout.strip().splitlines()[-1] if vout.strip() else device_id
    finally:
        jump.close()


def _fetch_swas_ota_and_tunnel_assets(cfg: dict[str, str], need_frpc_bytes: bool) -> dict[str, Any]:
    """Read live OTA pubkey, OTA token (if missing locally), and frps tunnel token from SWAS."""
    jump = robot_ops._connect_jump(cfg, timeout_s=10)
    try:
        cmd = (
            "printf '===PUB===\\n'; sudo -n cat /etc/doso/ota-release/release-signing.pub.pem; "
            "printf '\\n===OTA_TOKEN===\\n'; sudo -n grep -E '^DOSO_RELEASE_API_TOKEN=' /etc/doso/ota-release/release.env | head -n1 | cut -d= -f2- | tr -d '\\r\\n'; "
            "printf '\\n===FRP_TOKEN===\\n'; sudo -n awk -F= '/token/{gsub(/[\"\\x27[:space:]]/,\"\",$2); print $2; exit}' /etc/doso/ops-tunnel/frps.toml"
        )
        code, out, err = robot_ops._exec_cmd(jump, cmd, timeout_s=12)
        if code != 0:
            raise ProvisionError(f"读取云端签发材料失败: {err.strip()}")

        sections: dict[str, list[str]] = {}
        cur = ""
        for line in out.splitlines():
            m = re.fullmatch(r"===([A-Z_]+)===", line.strip())
            if m:
                cur = m.group(1)
                sections[cur] = []
            elif cur:
                sections[cur].append(line)

        result: dict[str, Any] = {
            "otaPubPem": "\n".join(sections.get("PUB", [])).strip() + "\n",
            "otaToken": "".join(sections.get("OTA_TOKEN", [])).strip(),
            "frpToken": "".join(sections.get("FRP_TOKEN", [])).strip(),
            "frpcBytes": None,
        }
        if need_frpc_bytes:
            # Optional fallback if standalone .exe runs without repo tools/ops-tunnel/frpc
            sftp = jump.open_sftp()
            try:
                for remote_p in ("/opt/doso/ota-staging/frpc-aarch64", "/opt/doso/bin/frpc-aarch64"):
                    try:
                        with sftp.open(remote_p, "rb") as fh:
                            result["frpcBytes"] = fh.read()
                        break
                    except OSError:
                        continue
            finally:
                sftp.close()
        return result
    finally:
        jump.close()


def _install_ota_on_robot(
    client: Any,
    cfg: dict[str, str],
    target: dict[str, Any],
    *,
    device_id: str,
    model: str,
    hardware_revision: str,
    brainstem_version: str,
    cloud_url: str,
    cloud_token: str,
    ota_pub_pem: str,
) -> str:
    env_content = "\n".join(
        [
            f"# /etc/doso/ota.env — Provisioned {datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')}",
            f"DOSO_DEVICE_ID={device_id}",
            f"DOSO_MODEL={model}",
            f"DOSO_HARDWARE_REVISION={hardware_revision}",
            "DOSO_BOOTLOADER_VERSION=1.0.0",
            f"DOSO_BRAINSTEM_VERSION={brainstem_version or '0.0.1'}",
            f"DOSO_BRAINSTEM_RELEASE={brainstem_version or 'v0.0.1'}",
            "DOSO_BATTERY_PERCENT=100",
            "DOSO_OTA_VERIFY_KEY=/etc/doso/ota-release-signing.pub.pem",
            "DOSO_DEVICE_ONLINE=true",
            "DOSO_TASK_RUNNING=false",
            "DOSO_OTA_ROOT=/var/lib/doso-ota",
            "DOSO_OTA_STAGING_DIR=/ota/staging",
            "DOSO_OTA_RELEASES_DIR=/opt/doso/releases",
            "DOSO_OTA_CURRENT_LINK=/opt/doso/current",
            "DOSO_OTA_LAST_GOOD_LINK=/opt/doso/last-known-good",
            "DOSO_OTA_COMPONENTS_ROOT=/opt/doso/components",
            "DOSO_OTA_BIND=127.0.0.1:13147",
            "DOSO_BRAINSTEM_UNIT=doso-brainstem.service",
            "DOSO_OTA_BOOT_ID=/run/doso/brainstem.boot_id",
            "DOSO_OTA_BRAINSTEM_HEALTH_URL=http://127.0.0.1:5054/health",
            "DOSO_POLICY_ADMIN_URL=http://127.0.0.1:5054",
            f"DOSO_OTA_CLOUD_URL={cloud_url}",
            f"DOSO_OTA_CLOUD_TOKEN={cloud_token}",
            "DOSO_OTA_POLL_INTERVAL_SECS=30",
            "",
        ]
    )

    tag = uuid.uuid4().hex[:8]
    tmp_env = f"/tmp/.doso_ota_env_{tag}"
    tmp_pub = f"/tmp/.doso_ota_pub_{tag}"
    sftp = client.open_sftp()
    try:
        with sftp.open(tmp_env, "wb") as fh:
            fh.write(env_content.encode("utf-8"))
        sftp.chmod(tmp_env, 0o600)
        with sftp.open(tmp_pub, "wb") as fh:
            fh.write(ota_pub_pem.encode("utf-8"))
    finally:
        sftp.close()

    inner = "\n".join(
        [
            "set -euo pipefail",
            f"trap 'rm -f {shlex.quote(tmp_env)} {shlex.quote(tmp_pub)}' EXIT",
            "install -d -o root -g root -m 0755 /etc/doso /var/lib/doso-ota /ota/staging /run/doso",
            "if test -f /etc/doso/ota.env; then cp -a /etc/doso/ota.env /etc/doso/ota.env.bak.$(date -u +%Y%m%dT%H%M%SZ); fi",
            f"install -o root -g root -m 0644 {shlex.quote(tmp_pub)} /etc/doso/ota-release-signing.pub.pem",
            f"install -o root -g root -m 0600 {shlex.quote(tmp_env)} /etc/doso/ota.env",
            "systemctl daemon-reload",
            "systemctl enable --now doso-ota.service",
            "systemctl restart doso-ota.service",
            "sleep 3",
            "echo STATE=$(systemctl is-active doso-ota.service || true)",
            "echo HEALTH=$(curl -sS --max-time 4 http://127.0.0.1:13147/health || echo '{}')",
        ]
    )
    code, out, err = robot_ops._exec_sudo(client, cfg, inner, timeout_s=25, target=target)
    if code != 0:
        raise ProvisionError(f"启用机端 doso-ota 失败: {(err or out).strip()}")
    state = "unknown"
    for line in out.splitlines():
        if line.startswith("STATE="):
            state = line.split("=", 1)[1].strip()
    if state != "active":
        raise ProvisionError(f"doso-ota 启动后状态异常: {state}")
    return state


def _install_tunnel_on_robot(
    client: Any,
    cfg: dict[str, str],
    target: dict[str, Any],
    *,
    tunnel_name: str,
    remote_port: int,
    frp_token: str,
    frpc_path: Path | None,
    frpc_bytes: bytes | None,
    server_addr: str = "api-test.dosorobotics.com",
) -> str:
    if not TUNNEL_NAME_RE.fullmatch(tunnel_name):
        raise ProvisionError(f"隧道名称不合法: {tunnel_name}")
    if not (22001 <= remote_port <= 22999):
        raise ProvisionError("隧道端口必须在 22001–22999 之间")
    if len(frp_token) < 24:
        raise ProvisionError("云端隧道令牌缺失或过短")

    # Check if robot already has /opt/doso/bin/frpc
    _, chk_out, _ = robot_ops._exec_cmd(client, "test -x /opt/doso/bin/frpc && echo YES || echo NO", timeout_s=8)
    need_upload = "YES" not in chk_out

    tag = uuid.uuid4().hex[:8]
    tmp_frpc = f"/tmp/.doso_frpc_{tag}"
    tmp_conf = f"/tmp/.doso_frpc_toml_{tag}"
    tmp_unit = f"/tmp/.doso_tunnel_unit_{tag}"

    conf_text = f"""serverAddr = "{server_addr}"
serverPort = 443
loginFailExit = false

[auth]
method = "token"
token = "{frp_token}"

[transport]
protocol = "wss"
heartbeatInterval = 20
heartbeatTimeout = 90
dialServerTimeout = 15
dialServerKeepalive = 30

[log]
to = "console"
level = "info"

[[proxies]]
name = "{tunnel_name}-ssh"
type = "tcp"
localIP = "127.0.0.1"
localPort = 22
remotePort = {remote_port}
"""
    unit_text = """[Unit]
Description=DOSO Outbound Ops Reverse Tunnel (frpc over 443 WSS)
Wants=network-online.target
After=network-online.target
StartLimitIntervalSec=0

[Service]
Type=simple
User=doso-tunnel
Group=doso-tunnel
ExecStart=/opt/doso/bin/frpc -c /etc/doso/ops-tunnel/frpc.toml
Restart=always
RestartSec=5
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true

[Install]
WantedBy=multi-user.target
"""

    sftp = client.open_sftp()
    try:
        if need_upload:
            if frpc_path is not None and frpc_path.is_file():
                sftp.put(str(frpc_path), tmp_frpc)
            elif frpc_bytes:
                with sftp.open(tmp_frpc, "wb") as fh:
                    fh.write(frpc_bytes)
            else:
                raise ProvisionError("未找到 ARM64 frpc 程序，无法安装运维隧道")
        with sftp.open(tmp_conf, "wb") as fh:
            fh.write(conf_text.encode("utf-8"))
        sftp.chmod(tmp_conf, 0o600)
        with sftp.open(tmp_unit, "wb") as fh:
            fh.write(unit_text.encode("utf-8"))
    finally:
        sftp.close()

    inner_lines = [
        "set -euo pipefail",
        f"trap 'rm -f {shlex.quote(tmp_frpc)} {shlex.quote(tmp_conf)} {shlex.quote(tmp_unit)}' EXIT",
        "if ! id doso-tunnel >/dev/null 2>&1; then useradd --system --no-create-home --shell /usr/sbin/nologin doso-tunnel; fi",
        "install -d -o root -g root -m 0755 /opt/doso/bin",
    ]
    if need_upload:
        inner_lines.append(f"install -o root -g root -m 0755 {shlex.quote(tmp_frpc)} /opt/doso/bin/frpc")
    inner_lines.extend(
        [
            "install -d -o root -g doso-tunnel -m 0750 /etc/doso/ops-tunnel",
            f"install -o root -g doso-tunnel -m 0640 {shlex.quote(tmp_conf)} /etc/doso/ops-tunnel/frpc.toml",
            "/opt/doso/bin/frpc verify -c /etc/doso/ops-tunnel/frpc.toml",
            f"install -o root -g root -m 0644 {shlex.quote(tmp_unit)} /etc/systemd/system/doso-ops-tunnel.service",
            "systemctl daemon-reload",
            "systemctl enable --now doso-ops-tunnel.service",
            "systemctl restart doso-ops-tunnel.service",
            "sleep 3",
            "echo STATE=$(systemctl is-active doso-ops-tunnel.service || true)",
        ]
    )
    code, out, err = robot_ops._exec_sudo(client, cfg, "\n".join(inner_lines), timeout_s=30, target=target)
    if code != 0:
        raise ProvisionError(f"安装运维隧道失败: {(err or out).strip()}")
    state = "unknown"
    for line in out.splitlines():
        if line.startswith("STATE="):
            state = line.split("=", 1)[1].strip()
    if state != "active":
        raise ProvisionError(f"运维隧道启动异常: {state}")
    return state


def _generate_keys_on_robot(
    client: Any,
    cfg: dict[str, str],
    target: dict[str, Any],
    *,
    device_id: str,
) -> tuple[str, str]:
    """Generate P-256 TLS key+CSR and Ed25519 signing keypair on the robot; return (csr_pem, signing_pub_pem)."""
    inner = f"""set -euo pipefail
umask 077
DEVICE_ID={shlex.quote(device_id)}
PRIV_DIR="/root/doso-link-private"
AGENT_DIR="/etc/doso/robot-cloud/agent"
install -d -o root -g root -m 0700 "$PRIV_DIR"
install -d -o root -g root -m 0755 "$AGENT_DIR"

# Reuse if already generated for the exact same UUID, otherwise generate fresh
REGEN=1
if test -f "$PRIV_DIR/device.csr" && test -f "$PRIV_DIR/device.key" && test -f "$PRIV_DIR/device-signing.key" && test -f "$PRIV_DIR/device-signing.pub"; then
  if openssl req -in "$PRIV_DIR/device.csr" -noout -subject 2>/dev/null | grep -Fq "$DEVICE_ID"; then
    REGEN=0
  fi
fi

if test "$REGEN" = "1"; then
  if test -f "$PRIV_DIR/device.key"; then
    STAMP=$(date -u +%Y%m%dT%H%M%SZ)
    mkdir -m 700 "$PRIV_DIR/bak-$STAMP"
    mv "$PRIV_DIR"/device* "$PRIV_DIR/bak-$STAMP/" 2>/dev/null || true
  fi
  openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out "$PRIV_DIR/device.key"
  openssl req -new -sha256 -key "$PRIV_DIR/device.key" -subj "/CN=$DEVICE_ID" -out "$PRIV_DIR/device.csr"
  openssl req -in "$PRIV_DIR/device.csr" -verify -noout >/dev/null
  openssl genpkey -algorithm ED25519 -out "$PRIV_DIR/device-signing.key"
  openssl pkey -in "$PRIV_DIR/device-signing.key" -pubout -out "$PRIV_DIR/device-signing.pub"
fi

if ! id doso-cloud >/dev/null 2>&1; then
  useradd --system --no-create-home --shell /usr/sbin/nologin doso-cloud
fi
install -o root -g doso-cloud -m 0640 "$PRIV_DIR/device.key" "$AGENT_DIR/device.key"
install -o root -g doso-cloud -m 0640 "$PRIV_DIR/device-signing.key" "$AGENT_DIR/device-signing.key"

if test -f "$AGENT_DIR/agent.env"; then
  cp -a "$AGENT_DIR/agent.env" "$AGENT_DIR/agent.env.bak.$(date -u +%Y%m%dT%H%M%SZ)"
  sed -i "s/^DOSO_DEVICE_ID=.*/DOSO_DEVICE_ID=$DEVICE_ID/" "$AGENT_DIR/agent.env"
else
  cat > "$AGENT_DIR/agent.env" <<EOF
DOSO_CLOUD_ON=false
DOSO_CLOUD_CHOICE=/var/lib/doso-cloud/choice
DOSO_DEVICE_ID=$DEVICE_ID
DOSO_MQTT_URL=mqtts://8.153.169.177:8883
DOSO_MQTT_CA=/etc/doso/robot-cloud/agent/ca.crt
DOSO_MQTT_CERT=/etc/doso/robot-cloud/agent/device.crt
DOSO_MQTT_KEY=/etc/doso/robot-cloud/agent/device.key
DOSO_DEVICE_SIGNING_KEY=/etc/doso/robot-cloud/agent/device-signing.key
DOSO_GATEWAY_PUBLIC_KEY=/etc/doso/robot-cloud/agent/gateway-signing.pub
EOF
fi
chown root:doso-cloud "$AGENT_DIR/agent.env"
chmod 0640 "$AGENT_DIR/agent.env"

printf '===CSR===\\n'
cat "$PRIV_DIR/device.csr"
printf '\\n===PUB===\\n'
cat "$PRIV_DIR/device-signing.pub"
"""
    code, out, err = robot_ops._exec_sudo(client, cfg, inner, timeout_s=25, target=target)
    if code != 0 or "===CSR===" not in out or "===PUB===" not in out:
        raise ProvisionError(f"机端生成密钥与 CSR 失败: {(err or out).strip()}")

    csr_part = out.split("===CSR===", 1)[1].split("===PUB===", 1)[0].strip() + "\n"
    pub_part = out.split("===PUB===", 1)[1].strip() + "\n"
    if "-----BEGIN CERTIFICATE REQUEST-----" not in csr_part or "-----BEGIN PUBLIC KEY-----" not in pub_part:
        raise ProvisionError("机端返回的 CSR 或公钥格式无效")
    return csr_part, pub_part


def provision_robot(
    cfg: dict[str, str],
    ops_mod: Any,
    req: dict[str, Any],
    *,
    repo_root: Path | None = None,
) -> dict[str, Any]:
    """Run one-click provisioning across Robot + SWAS (+ ECS Cloud Assistant if configured)."""
    target = build_direct_target(req, cfg)
    probe = probe_robot(cfg, req)
    if probe.get("arch") != "aarch64":
        raise ProvisionError(f"仅支持 ARM64 机器狗，当前架构: {probe.get('arch')}")

    force = bool(req.get("force", False))
    requested_id = str(req.get("deviceId") or "").strip()
    if requested_id:
        if not is_valid_uuid_v4(requested_id):
            raise ProvisionError("设备编号必须是小写 UUID v4")
        device_id = requested_id
    elif probe.get("existingValidUuid"):
        device_id = str(probe["existingValidUuid"])
    else:
        device_id = str(uuid.uuid4())

    if probe.get("existingValidUuid") and probe["existingValidUuid"] != device_id and not force:
        raise ProvisionError(
            f"机器狗已签发有效身份 {probe['existingValidUuid']}，为防覆盖串机已拒绝改写"
        )

    robot_num = int(req.get("robotNumber") or probe.get("suggestedNumber") or 4)
    if not (1 <= robot_num <= 999):
        raise ProvisionError("机器编号必须在 1–999 之间")
    serial_number = str(req.get("serialNumber") or f"THUNDER-DEV-{robot_num:03d}").strip()
    if not SERIAL_RE.fullmatch(serial_number):
        raise ProvisionError(f"序列号格式无效: {serial_number}")
    display_name = str(req.get("displayName") or f"THUNDER 研发机 {robot_num:03d}").strip()
    model = str(req.get("model") or probe.get("model") or cfg.get("DOSO_DEFAULT_MODEL") or "THUNDER 01").strip()
    hw_rev = str(req.get("hardwareRevision") or probe.get("hardwareRevision") or "rdk-s100p").strip()
    ring = str(req.get("ring") or "canary").strip()
    if ring not in ("dev", "canary", "prod"):
        raise ProvisionError("发布分环必须是 dev / canary / prod")
    tags_raw = req.get("tags")
    if isinstance(tags_raw, str):
        tags = [t.strip() for t in re.split(r"[,，\s]+", tags_raw) if t.strip()]
    elif isinstance(tags_raw, list):
        tags = [str(t).strip() for t in tags_raw if str(t).strip()]
    else:
        tags = [f"dog{robot_num}", hw_rev]
    owner_email = str(req.get("ownerEmail") or cfg.get("DOSO_DEFAULT_OWNER_EMAIL") or "").strip()
    tunnel_port = int(req.get("tunnelPort") or (22000 + robot_num))
    tunnel_name = f"thunder-{robot_num:03d}"

    enable_inventory = bool(req.get("enableInventory", True))
    enable_ota = bool(req.get("enableOta", True))
    enable_tunnel = bool(req.get("enableTunnel", True))
    enable_cloud_keys = bool(req.get("enableCloudLinkKeys", True))
    auto_ecs_sign = bool(req.get("autoEcsSign", True))

    steps: list[dict[str, Any]] = [
        {
            "step": "探测机器狗环境",
            "status": "ok",
            "detail": f"{probe.get('hostname')} · {hw_rev} · {probe.get('brainstemVersion') or 'ready'}",
        }
    ]

    # Step 2: Business inventory on SWAS Supabase
    if enable_inventory:
        inv_detail = _upsert_swas_inventory(
            cfg,
            device_id=device_id,
            serial_number=serial_number,
            display_name=display_name,
            model=model,
            owner_email=owner_email,
        )
        steps.append({"step": "写入云端设备库存", "status": "ok", "detail": inv_detail})

    # Fetch live SWAS assets (OTA pubkey, OTA token, FRP token, optional frpc)
    local_frpc = _find_local_frpc(repo_root) if enable_tunnel else None
    swas_assets = _fetch_swas_ota_and_tunnel_assets(
        cfg, need_frpc_bytes=bool(enable_tunnel and local_frpc is None)
    )
    ota_token = (cfg.get("DOSO_RELEASE_API_TOKEN") or "").strip() or swas_assets["otaToken"]

    # Step 3: Cloud OTA registry + Robot OTA activation
    if enable_ota:
        if not ota_token:
            raise ProvisionError("未获取到云端 OTA 发布令牌")
        ops_mod.api_call(
            cfg["DOSO_OTA_CLOUD_URL"],
            "PUT",
            f"/v1/devices/{urllib.parse.quote(device_id)}",
            ota_token,
            body={
                "model": model,
                "hardwareRevision": hw_rev,
                "enabled": True,
                "ring": ring,
                "tags": tags,
            },
        )
        steps.append({"step": "云端 OTA 登记设备", "status": "ok", "detail": f"{device_id} ({ring})"})

    csr_pem = ""
    signing_pub_pem = ""
    with robot_ops.open_robot_ssh(cfg, target, timeout_s=12) as client:
        if enable_ota:
            ota_state = _install_ota_on_robot(
                client,
                cfg,
                target,
                device_id=device_id,
                model=model,
                hardware_revision=hw_rev,
                brainstem_version=str(probe.get("brainstemVersion") or ""),
                cloud_url=cfg["DOSO_OTA_CLOUD_URL"],
                cloud_token=ota_token,
                ota_pub_pem=swas_assets["otaPubPem"],
            )
            steps.append({"step": "写入机端 OTA 凭据并启动", "status": "ok", "detail": f"doso-ota: {ota_state}"})

        if enable_tunnel:
            tun_state = _install_tunnel_on_robot(
                client,
                cfg,
                target,
                tunnel_name=tunnel_name,
                remote_port=tunnel_port,
                frp_token=swas_assets["frpToken"],
                frpc_path=local_frpc,
                frpc_bytes=swas_assets["frpcBytes"],
            )
            steps.append(
                {
                    "step": "开通 443 云端运维隧道",
                    "status": "ok",
                    "detail": f"{tunnel_name} (:{tunnel_port}) · {tun_state}",
                }
            )

        if enable_cloud_keys:
            csr_pem, signing_pub_pem = _generate_keys_on_robot(client, cfg, target, device_id=device_id)
            steps.append(
                {
                    "step": "机端生成独立密钥与 CSR",
                    "status": "ok",
                    "detail": "私钥保留机端，已导出 CSR 与验签公钥",
                }
            )

    ecs_script = ""
    ecs_status = "skipped"
    if enable_cloud_keys and csr_pem and signing_pub_pem:
        ecs_script = build_ecs_issue_script(device_id, csr_pem, signing_pub_pem)
        has_ecs_ak = bool(
            (cfg.get("DOSO_ECS_ACCESS_KEY_ID") or "").strip()
            and (cfg.get("DOSO_ECS_ACCESS_KEY_SECRET") or "").strip()
        )
        if auto_ecs_sign and has_ecs_ak:
            bundle = run_ecs_issue_via_cloud_assistant(cfg, ecs_script)
            steps.append(
                {
                    "step": "ECS 云助手自动签发证书并写 ACL",
                    "status": "ok",
                    "detail": f"InvokeId {bundle.get('invokeId', '')}",
                }
            )
            fin = finish_mqtt_on_robot(
                cfg,
                {
                    **req,
                    "deviceId": device_id,
                    "deviceCrt": bundle["deviceCrt"],
                    "caCrt": bundle["caCrt"],
                    "gatewaySigningPub": bundle["gatewaySigningPub"],
                    "enableService": True,
                },
            )
            steps.extend(fin["steps"])
            ecs_status = "completed"
        else:
            ecs_status = "manual_required"
            steps.append(
                {
                    "step": "等待 ECS 签发 MQTT 证书",
                    "status": "pending",
                    "detail": "已生成一键签发脚本，粘贴至 ECS 云助手执行后回填回执即可",
                }
            )

    return {
        "ok": True,
        "deviceId": device_id,
        "serialNumber": serial_number,
        "displayName": display_name,
        "model": model,
        "hardwareRevision": hw_rev,
        "ring": ring,
        "tags": tags,
        "tunnelPort": tunnel_port if enable_tunnel else None,
        "ecsStatus": ecs_status,
        "ecsInstanceId": (cfg.get("DOSO_ECS_INSTANCE_ID") or "i-uf6hb2phe34p0mt1xfca").strip(),
        "ecsScript": ecs_script,
        "csrPem": csr_pem,
        "signingPubPem": signing_pub_pem,
        "steps": steps,
    }
