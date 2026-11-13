#!/usr/bin/env bash
set -euo pipefail

# Read-only field gate. It must not bring CAN links up or restart a service.
imu_port="${HAN_DOG_IMU_PORT:-/dev/serial/by-id/usb-Silicon_Labs_CP2102N_USB_to_UART_Bridge_Controller_0e3a84e11af0ef11b457535a6d9880ab-if00-port0}"
choice_path="${DOSO_CLOUD_CHOICE:-/var/lib/doso-cloud/choice}"
agent_env="${DOSO_AGENT_ENV:-/etc/doso/robot-cloud/agent/agent.env}"

printf 'observed_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
printf '\n[services]\n'
for unit in brainstem.service doso-cloud-link.service doso-robot-authorizer.service brainstem-grpc-proxy.service thunder-can.service; do
  printf '%s ' "$unit"
  systemctl show "$unit" -p ActiveState -p SubState -p MainPID -p Result --no-pager || true
done

printf '\n[ports]\n'
ss -ltn | awk '$4 ~ /:(13145|13146|13148|5052)$/ {print}' || true

printf '\n[cloud_choice]\n'
printf 'choice_path=%s\n' "$choice_path"
if [ -f "$choice_path" ]; then
  choice="$(tr -d '\r\n' < "$choice_path")"
  case "$choice" in
    true|false) printf 'choice=%s\n' "$choice" ;;
    *) printf 'choice=invalid\n' ;;
  esac
else
  printf 'choice=missing\n'
fi
if [ -f "$agent_env" ]; then
  cloud_gate="$(sed -n 's/^DOSO_CLOUD_ON=//p' "$agent_env" | tail -n 1)"
  case "$cloud_gate" in
    true|false) printf 'install_gate=%s\n' "$cloud_gate" ;;
    *) printf 'install_gate=missing_or_invalid\n' ;;
  esac
else
  printf 'install_gate=env_missing\n'
fi

printf '\n[can]\n'
for name in can0 can1 can2 can3; do
  printf '===%s===\n' "$name"
  ip -details link show "$name" 2>&1 || true
done

printf '\n[imu]\n'
if [ -e "$imu_port" ]; then
  printf 'present=%s\n' "$imu_port"
else
  printf 'missing=%s\n' "$imu_port"
fi

printf '\n[read_only_can_check]\n'
if [ -x /usr/local/libexec/thunder-can-up.py ]; then
  HAN_DOG_IMU_PORT="$imu_port" python3 /usr/local/libexec/thunder-can-up.py --check
else
  printf 'missing=/usr/local/libexec/thunder-can-up.py\n'
  exit 2
fi
