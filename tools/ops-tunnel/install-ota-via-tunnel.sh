#!/usr/bin/env bash
set -euo pipefail

# Provision doso-ota agent onto any robot connected via doso-ops-tunnel (127.0.0.1:<port>)
# directly from the Aliyun SWAS host running doso-ota-release (47.103.89.137).
#
# - Reads DOSO_RELEASE_API_TOKEN & release-signing.pub.pem directly from /etc/doso/ota-release/
#   without ever printing the token.
# - Auto-detects the robot's hardware revision unless explicitly passed.
# - Registers the device atomically in /etc/doso/ota-release/devices.json (with backup).
# - Deploys doso-ota using the standard component-slot layout (/opt/doso/components/ota/...).
# - Never touches motors, CAN, or active brainstem processes.
#
# Usage (as root on 47.103.89.137):
#   sudo bash install-ota-via-tunnel.sh \
#     --port 22001 \
#     --device-id d82a831a-2e86-4b46-975b-3d42473583b7 \
#     --model "THUNDER 01" \
#     --ota-bin /opt/doso/ota-staging/doso-ota-aarch64

PORT=""
DEVICE_ID=""
MODEL="THUNDER 01"
HW_REV="auto"
OTA_BIN="/opt/doso/ota-staging/doso-ota-aarch64"
CLOUD_URL="https://api.dosorobotics.com/ota"
ROBOT_USER="sunrise"
ROBOT_PASS="sunrise"
RELEASE_ENV="/etc/doso/ota-release/release.env"
PUBKEY_SRC="/etc/doso/ota-release/release-signing.pub.pem"
DEVICES_JSON="/etc/doso/ota-release/devices.json"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --port) PORT="$2"; shift 2 ;;
    --device-id) DEVICE_ID="$2"; shift 2 ;;
    --model) MODEL="$2"; shift 2 ;;
    --hw-rev) HW_REV="$2"; shift 2 ;;
    --ota-bin) OTA_BIN="$2"; shift 2 ;;
    --cloud-url) CLOUD_URL="$2"; shift 2 ;;
    --user) ROBOT_USER="$2"; shift 2 ;;
    *) echo "Unknown argument: $1" >&2; exit 2 ;;
  esac
done

test "$(id -u)" -eq 0
test -n "$PORT" && test -n "$DEVICE_ID"
test -f "$OTA_BIN"
test -f "$RELEASE_ENV"
test -f "$PUBKEY_SRC"
test -f "$DEVICES_JSON"

if ! ss -tlnp | grep -q "127\.0\.0\.1:${PORT}\b"; then
  echo "ERROR: Tunnel port 127.0.0.1:${PORT} is not listening (robot offline)." >&2
  exit 3
fi

CLOUD_TOKEN="$(grep -E '^DOSO_RELEASE_API_TOKEN=' "$RELEASE_ENV" | head -n 1 | cut -d= -f2- | tr -d '\r\n')"
if [[ -z "$CLOUD_TOKEN" ]]; then
  echo "ERROR: DOSO_RELEASE_API_TOKEN missing in $RELEASE_ENV" >&2
  exit 1
fi

ASKPASS="$(mktemp /tmp/doso-ota-askpass.XXXXXX)"
LOCAL_ENV="$(mktemp /tmp/doso-ota-env.XXXXXX)"
LOCAL_UNIT="$(mktemp /tmp/doso-ota-unit.XXXXXX)"
LOCAL_APPLY="$(mktemp /tmp/doso-ota-apply.XXXXXX)"
cleanup() {
  rm -f "$ASKPASS" "$LOCAL_ENV" "$LOCAL_UNIT" "$LOCAL_APPLY"
}
trap cleanup EXIT

printf '#!/bin/sh\nprintf "%%s\\n" "%s"\n' "$ROBOT_PASS" > "$ASKPASS"
chmod 700 "$ASKPASS"
export SSH_ASKPASS="$ASKPASS" SSH_ASKPASS_REQUIRE="force" DISPLAY="dummy:0"

SSH_OPTS=(
  -o StrictHostKeyChecking=no
  -o UserKnownHostsFile=/dev/null
  -o LogLevel=ERROR
  -o ConnectTimeout=10
  -o PreferredAuthentications=password
  -o PubkeyAuthentication=no
)

echo "=== 1. PROBING ROBOT HARDWARE & LAYOUT ON 127.0.0.1:${PORT} ==="
PROBE="$(ssh "${SSH_OPTS[@]}" -p "$PORT" "${ROBOT_USER}@127.0.0.1" \
  'MODEL_DT=$(tr -d "\0" < /proc/device-tree/model 2>/dev/null || echo unknown);
   ARCH=$(uname -m);
   HAS_DOSO_BS=$(systemctl list-unit-files doso-brainstem.service --no-legend 2>/dev/null | wc -l);
   HAS_LEGACY_BS=$(systemctl list-unit-files brainstem.service --no-legend 2>/dev/null | wc -l);
   PROD_CODE=$(curl -sS --max-time 5 -o /dev/null -w "%{http_code}" https://api.dosorobotics.com/ota/health || echo 000);
   TEST_CODE=$(curl -sS --max-time 5 -o /dev/null -w "%{http_code}" https://api-test.dosorobotics.com/ota/health || echo 000);
   echo "MODEL_DT=$MODEL_DT";
   echo "ARCH=$ARCH";
   echo "HAS_DOSO_BS=$HAS_DOSO_BS";
   echo "HAS_LEGACY_BS=$HAS_LEGACY_BS";
   echo "PROD_CODE=$PROD_CODE";
   echo "TEST_CODE=$TEST_CODE"')"
echo "$PROBE"

ARCH="$(echo "$PROBE" | awk -F= '$1=="ARCH"{print $2}')"
MODEL_DT="$(echo "$PROBE" | awk -F= '$1=="MODEL_DT"{print $2}')"
HAS_DOSO_BS="$(echo "$PROBE" | awk -F= '$1=="HAS_DOSO_BS"{print $2}')"
PROD_CODE="$(echo "$PROBE" | awk -F= '$1=="PROD_CODE"{print $2}')"
TEST_CODE="$(echo "$PROBE" | awk -F= '$1=="TEST_CODE"{print $2}')"

if [[ "$ARCH" != "aarch64" ]]; then
  echo "ERROR: Expected aarch64 robot, got $ARCH" >&2
  exit 1
fi

if [[ "$HW_REV" == "auto" ]]; then
  if echo "$MODEL_DT" | grep -qi "s100"; then
    HW_REV="rdk-s100p"
  elif echo "$MODEL_DT" | grep -qi "x5"; then
    HW_REV="rdk-x5"
  else
    HW_REV="rdk-s100p"
  fi
fi

if [[ "$PROD_CODE" != "200" && "$TEST_CODE" == "200" ]]; then
  CLOUD_URL="https://api-test.dosorobotics.com/ota"
fi

BRAINSTEM_UNIT="doso-brainstem.service"
if [[ "$HAS_DOSO_BS" == "0" ]]; then
  # Keep compatible with component-slot Rust Brainstem once upgraded via OTA
  BRAINSTEM_UNIT="doso-brainstem.service"
fi

echo "RESOLVED hw_rev=${HW_REV} cloud_url=${CLOUD_URL} brainstem_unit=${BRAINSTEM_UNIT}"

echo "=== 2. REGISTERING DEVICE IN CLOUD /etc/doso/ota-release/devices.json ==="
cp -a "$DEVICES_JSON" "${DEVICES_JSON}.bak.$(date -u +%Y%m%dT%H%M%SZ)"
python3 - "$DEVICES_JSON" "$DEVICE_ID" "$MODEL" "$HW_REV" <<'PY'
import json, os, sys, tempfile
from pathlib import Path

path, device_id, model, hw_rev = sys.argv[1:]
p = Path(path)
data = json.loads(p.read_text(encoding="utf-8"))
data[device_id] = {
    "model": model,
    "hardwareRevision": hw_rev,
    "enabled": True,
}
fd, tmp = tempfile.mkstemp(prefix=".devices.json.", dir=str(p.parent))
try:
    with os.fdopen(fd, "w", encoding="utf-8") as fh:
        json.dump(data, fh, indent=2, ensure_ascii=False)
        fh.write("\n")
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp, p)
finally:
    if os.path.exists(tmp):
        os.unlink(tmp)
print(f"REGISTERED_OK device_id={device_id} model={model} hw_rev={hw_rev}")
PY
chown root:doso-release "$DEVICES_JSON"
chmod 0640 "$DEVICES_JSON"

echo "=== 3. GENERATING ROBOT ota.env & doso-ota.service ==="
umask 077
cat > "$LOCAL_ENV" <<EOF
DOSO_DEVICE_ID=${DEVICE_ID}
DOSO_MODEL=${MODEL}
DOSO_HARDWARE_REVISION=${HW_REV}
DOSO_BOOTLOADER_VERSION=1.0.0
DOSO_BRAINSTEM_VERSION=0.0.1
DOSO_BRAINSTEM_RELEASE=v0.0.1
DOSO_BATTERY_PERCENT=100
DOSO_OTA_VERIFY_KEY=/etc/doso/ota-release-signing.pub.pem
DOSO_DEVICE_ONLINE=true
DOSO_TASK_RUNNING=false
DOSO_OTA_ROOT=/var/lib/doso-ota
DOSO_OTA_STAGING_DIR=/ota/staging
DOSO_OTA_RELEASES_DIR=/opt/doso/releases
DOSO_OTA_CURRENT_LINK=/opt/doso/current
DOSO_OTA_LAST_GOOD_LINK=/opt/doso/last-known-good
DOSO_OTA_BIND=127.0.0.1:13147
DOSO_BRAINSTEM_UNIT=${BRAINSTEM_UNIT}
DOSO_OTA_BOOT_ID=/run/doso/brainstem.boot_id
DOSO_OTA_BRAINSTEM_HEALTH_URL=http://127.0.0.1:5054/health
DOSO_POLICY_ADMIN_URL=http://127.0.0.1:5054
DOSO_OTA_CLOUD_URL=${CLOUD_URL}
DOSO_OTA_CLOUD_TOKEN=${CLOUD_TOKEN}
DOSO_OTA_POLL_INTERVAL_SECS=30
DOSO_OTA_COMPONENTS_ROOT=/opt/doso/components
EOF

cat > "$LOCAL_UNIT" <<'UNIT'
[Unit]
Description=Domain B - DOSO Dual-Channel OTA Agent (Cores 0-3)
After=network-online.target doso-brainstem.service
Wants=network-online.target

[Service]
Type=simple
User=root
Group=root
CPUAffinity=0 1 2 3
EnvironmentFile=/etc/doso/ota.env
ExecStart=/opt/doso/ota/bin/doso-ota serve
Restart=on-failure
RestartSec=2
NoNewPrivileges=true
PrivateTmp=true
ProtectHome=true
ReadWritePaths=/opt/doso /ota /var/lib/doso-ota /run/doso

[Install]
WantedBy=multi-user.target
UNIT

cat > "$LOCAL_APPLY" <<'REMOTE'
#!/usr/bin/env bash
set -euo pipefail
trap 'rm -f /tmp/doso-ota.bin /tmp/ota-release-signing.pub.pem /tmp/doso-ota.env /tmp/doso-ota.service /tmp/doso-ota-apply.sh' EXIT
install -d -o root -g root -m 0755 /opt/doso/components/ota/releases/v0.1.0 /opt/doso/ota/bin /opt/doso/releases /etc/doso /var/lib/doso-ota /ota/staging /run/doso
install -o root -g root -m 0755 /tmp/doso-ota.bin /opt/doso/components/ota/releases/v0.1.0/doso-ota
ln -sfn /opt/doso/components/ota/releases/v0.1.0 /opt/doso/components/ota/current
ln -sfn /opt/doso/components/ota/current/doso-ota /opt/doso/ota/bin/doso-ota
install -o root -g root -m 0644 /tmp/ota-release-signing.pub.pem /etc/doso/ota-release-signing.pub.pem
install -o root -g root -m 0600 /tmp/doso-ota.env /etc/doso/ota.env
install -o root -g root -m 0644 /tmp/doso-ota.service /etc/systemd/system/doso-ota.service
systemctl daemon-reload
systemctl enable --now doso-ota.service
systemctl restart doso-ota.service
sleep 3
systemctl is-active doso-ota.service
curl -fsS http://127.0.0.1:13147/health
echo ""
journalctl -u doso-ota.service -n 10 --no-pager
REMOTE

echo "=== 4. UPLOADING & INSTALLING doso-ota ON ROBOT ==="
scp "${SSH_OPTS[@]}" -P "$PORT" "$OTA_BIN" "${ROBOT_USER}@127.0.0.1:/tmp/doso-ota.bin"
scp "${SSH_OPTS[@]}" -P "$PORT" "$PUBKEY_SRC" "${ROBOT_USER}@127.0.0.1:/tmp/ota-release-signing.pub.pem"
scp "${SSH_OPTS[@]}" -P "$PORT" "$LOCAL_ENV" "${ROBOT_USER}@127.0.0.1:/tmp/doso-ota.env"
scp "${SSH_OPTS[@]}" -P "$PORT" "$LOCAL_UNIT" "${ROBOT_USER}@127.0.0.1:/tmp/doso-ota.service"
scp "${SSH_OPTS[@]}" -P "$PORT" "$LOCAL_APPLY" "${ROBOT_USER}@127.0.0.1:/tmp/doso-ota-apply.sh"

ssh "${SSH_OPTS[@]}" -p "$PORT" "${ROBOT_USER}@127.0.0.1" \
  "chmod +x /tmp/doso-ota-apply.sh && printf '%s\n' '${ROBOT_PASS}' | sudo -S bash /tmp/doso-ota-apply.sh"

echo "=== 5. VERIFYING CLOUD OTA-RELEASE RECOGNIZES DEVICE ==="
sleep 2
curl -fsS -H "Authorization: Bearer ${CLOUD_TOKEN}" "http://127.0.0.1:8140/v1/devices/${DEVICE_ID}/releases" || true
echo ""
echo "OTA_INSTALL_COMPLETE device_id=${DEVICE_ID} port=${PORT}"
