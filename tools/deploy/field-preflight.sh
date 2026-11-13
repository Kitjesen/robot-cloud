#!/usr/bin/env bash
set -euo pipefail

# Read-only field gate. It must not bring CAN links up or restart a service.
imu_port="${HAN_DOG_IMU_PORT:-/dev/serial/by-id/usb-Silicon_Labs_CP2102N_USB_to_UART_Bridge_Controller_0e3a84e11af0ef11b457535a6d9880ab-if00-port0}"

printf 'observed_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
printf '\n[services]\n'
for unit in brainstem.service doso-cloud-link.service brainstem-grpc-proxy.service thunder-can.service; do
  printf '%s ' "$unit"
  systemctl show "$unit" -p ActiveState -p SubState -p MainPID -p Result --no-pager || true
done

printf '\n[ports]\n'
ss -ltn | awk '$4 ~ /:(13145|13146|5052)$/ {print}' || true

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
