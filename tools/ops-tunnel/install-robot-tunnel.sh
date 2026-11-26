#!/usr/bin/env bash
set -euo pipefail

# Install and enable the DOSO outbound FRP ops tunnel on an ARM64 Linux robot.
# Connects over WSS on port 443 (works behind 5G CGNAT with zero inbound ports).
# Independent of doso-cloud-link and customer reporting switches.
#
# Usage (as root):
#   sudo bash install-robot-tunnel.sh \
#     --name thunder-003 \
#     --remote-port 22003 \
#     --server api-test.dosorobotics.com \
#     --token-file /tmp/doso-ops-tunnel.token \
#     --frpc-bin /tmp/frpc

FRP_VERSION="0.72.0"
DEVICE_NAME=""
REMOTE_PORT=""
SERVER_ADDR="api-test.dosorobotics.com"
SERVER_PORT="443"
TOKEN_FILE=""
FRPC_BIN=""
LOCAL_SSH_PORT="22"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --name) DEVICE_NAME="$2"; shift 2 ;;
    --remote-port) REMOTE_PORT="$2"; shift 2 ;;
    --server) SERVER_ADDR="$2"; shift 2 ;;
    --server-port) SERVER_PORT="$2"; shift 2 ;;
    --token-file) TOKEN_FILE="$2"; shift 2 ;;
    --frpc-bin) FRPC_BIN="$2"; shift 2 ;;
    --local-ssh-port) LOCAL_SSH_PORT="$2"; shift 2 ;;
    *) echo "Unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [[ "$(id -u)" -ne 0 ]]; then
  echo "Must run as root (use sudo)." >&2
  exit 1
fi

if [[ -z "$DEVICE_NAME" || -z "$REMOTE_PORT" || -z "$TOKEN_FILE" || -z "$FRPC_BIN" ]]; then
  echo "Required: --name <id> --remote-port <22001-22999> --token-file <path> --frpc-bin <path>" >&2
  exit 2
fi

if ! [[ "$DEVICE_NAME" =~ ^[a-z0-9][a-z0-9_-]{1,31}$ ]]; then
  echo "Invalid --name (use lowercase letters, digits, - or _, 2-32 chars): $DEVICE_NAME" >&2
  exit 2
fi

if ! [[ "$REMOTE_PORT" =~ ^[0-9]+$ ]] || (( REMOTE_PORT < 22001 || REMOTE_PORT > 22999 )); then
  echo "Invalid --remote-port (must be 22001..22999): $REMOTE_PORT" >&2
  exit 2
fi

if [[ ! -f "$TOKEN_FILE" ]]; then
  echo "Token file not found: $TOKEN_FILE" >&2
  exit 1
fi

if [[ ! -f "$FRPC_BIN" ]]; then
  echo "frpc binary not found: $FRPC_BIN" >&2
  exit 1
fi

TOKEN="$(tr -d '\r\n' < "$TOKEN_FILE")"
if [[ ${#TOKEN} -lt 24 ]]; then
  echo "Tunnel token too short (<24 chars)." >&2
  exit 1
fi

ARCH="$(uname -m)"
if [[ "$ARCH" != "aarch64" && "$ARCH" != "arm64" && "$ARCH" != "x86_64" ]]; then
  echo "Unsupported architecture: $ARCH" >&2
  exit 1
fi

if ! id doso-tunnel >/dev/null 2>&1; then
  useradd --system --no-create-home --shell /usr/sbin/nologin doso-tunnel
fi

install -d -o root -g root -m 0755 /opt/doso/bin
install -o root -g root -m 0755 "$FRPC_BIN" /opt/doso/bin/frpc
/opt/doso/bin/frpc --version

install -d -o root -g doso-tunnel -m 0750 /etc/doso/ops-tunnel
CONF="/etc/doso/ops-tunnel/frpc.toml"
TMP_CONF="/etc/doso/ops-tunnel/.frpc.toml.tmp.$$"
umask 077
cat > "$TMP_CONF" <<EOF
serverAddr = "${SERVER_ADDR}"
serverPort = ${SERVER_PORT}
loginFailExit = false

[auth]
method = "token"
token = "${TOKEN}"

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
name = "${DEVICE_NAME}-ssh"
type = "tcp"
localIP = "127.0.0.1"
localPort = ${LOCAL_SSH_PORT}
remotePort = ${REMOTE_PORT}
EOF

chown root:doso-tunnel "$TMP_CONF"
chmod 0640 "$TMP_CONF"
mv -f "$TMP_CONF" "$CONF"

/opt/doso/bin/frpc verify -c "$CONF"

cat > /etc/systemd/system/doso-ops-tunnel.service <<'UNIT'
[Unit]
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
UNIT

systemctl daemon-reload
systemctl enable --now doso-ops-tunnel.service
systemctl restart doso-ops-tunnel.service
sleep 2
systemctl is-active doso-ops-tunnel.service
echo "INSTALLED_OK device=${DEVICE_NAME} remote_port=${REMOTE_PORT} server=${SERVER_ADDR}:${SERVER_PORT}"
