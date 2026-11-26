#!/usr/bin/env bash
set -euo pipefail

# Deploy DOSO loopback-only frps behind existing Nginx 443 WSS proxy.
# Usage (as root on cloud server):
#   sudo bash install-frps-server.sh --token-file /path/to/doso-ops-tunnel.token --frps-bin /path/to/frps

TOKEN_FILE=""
FRPS_BIN=""
BIND_PORT="7000"
PORT_START="22001"
PORT_END="22999"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --token-file) TOKEN_FILE="$2"; shift 2 ;;
    --frps-bin) FRPS_BIN="$2"; shift 2 ;;
    --bind-port) BIND_PORT="$2"; shift 2 ;;
    --port-start) PORT_START="$2"; shift 2 ;;
    --port-end) PORT_END="$2"; shift 2 ;;
    *) echo "Unknown argument: $1" >&2; exit 2 ;;
  esac
done

test "$(id -u)" -eq 0
test -f "${TOKEN_FILE:?--token-file required}"
test -f "${FRPS_BIN:?--frps-bin required}"

TOKEN="$(tr -d '\r\n' < "$TOKEN_FILE")"
(( ${#TOKEN} >= 24 ))

if ! id doso-tunnel >/dev/null 2>&1; then
  useradd --system --no-create-home --shell /usr/sbin/nologin doso-tunnel
fi

install -d -o root -g root -m 0755 /opt/doso/bin
install -o root -g root -m 0755 "$FRPS_BIN" /opt/doso/bin/frps

install -d -o root -g doso-tunnel -m 0750 /etc/doso/ops-tunnel
CONF="/etc/doso/ops-tunnel/frps.toml"
umask 077
cat > "$CONF" <<EOF
bindAddr = "127.0.0.1"
bindPort = ${BIND_PORT}
proxyBindAddr = "127.0.0.1"

allowPorts = [
  { start = ${PORT_START}, end = ${PORT_END} }
]

[auth]
method = "token"
token = "${TOKEN}"

[transport]
heartbeatTimeout = 90
tcpMux = true
tcpMuxKeepaliveInterval = 30

[log]
to = "console"
level = "info"
EOF
chown root:doso-tunnel "$CONF"
chmod 0640 "$CONF"
/opt/doso/bin/frps verify -c "$CONF"

cat > /etc/systemd/system/doso-frps.service <<'UNIT'
[Unit]
Description=DOSO Robot Ops Reverse Tunnel Server (frps loopback)
Wants=network-online.target
After=network-online.target
StartLimitIntervalSec=0

[Service]
Type=simple
User=doso-tunnel
Group=doso-tunnel
ExecStart=/opt/doso/bin/frps -c /etc/doso/ops-tunnel/frps.toml
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
systemctl enable --now doso-frps.service
systemctl restart doso-frps.service
systemctl is-active doso-frps.service
