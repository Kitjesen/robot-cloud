#!/usr/bin/env bash
set -euo pipefail

# Configure secrets locally on the ECS host and optionally start the service.
test "$(id -u)" -eq 0

start_service=false
if test "${1:-}" = "--start"; then
    start_service=true
elif test "${1:-}" != ""; then
    printf 'usage: %s [--start]\n' "$0" >&2
    exit 2
fi

config_dir=/etc/doso/ota-release
env_file="$config_dir/release.env"
key_file="$config_dir/release-signing.key.pem"
test -f "$config_dir/devices.json"

read -r -p 'OSS AccessKey ID: ' oss_access_key_id
read -r -s -p 'OSS AccessKey Secret: ' oss_access_key_secret
printf '\n'
test -n "$oss_access_key_id"
test -n "$oss_access_key_secret"

install -d -o root -g doso-release -m 750 "$config_dir"
if test ! -f "$key_file"; then
    openssl genpkey -algorithm ED25519 -out "$key_file" >/dev/null 2>&1
    chown root:doso-release "$key_file"
    chmod 640 "$key_file"
fi

api_token="$(openssl rand -hex 32)"
umask 077
cat > "$env_file" <<EOF
DOSO_RELEASE_ROOT=/var/lib/doso-ota-release
DOSO_RELEASE_DEVICE_REGISTRY=/etc/doso/ota-release/devices.json
DOSO_RELEASE_API_TOKEN=$api_token
DOSO_RELEASE_SIGNING_KEY=/etc/doso/ota-release/release-signing.key.pem
DOSO_RELEASE_SIGNING_KEY_ID=release-key-2026-01
DOSO_OSS_BUCKET=doso-ota-prod-shanghai
DOSO_OSS_ENDPOINT=oss-cn-shanghai.aliyuncs.com
DOSO_OSS_ACCESS_KEY_ID=$oss_access_key_id
DOSO_OSS_ACCESS_KEY_SECRET=$oss_access_key_secret
DOSO_OSS_SIGNED_URL_TTL_SECONDS=1800
DOSO_RELEASE_BIND=127.0.0.1:8140
EOF
chown root:doso-release "$env_file"
chmod 640 "$env_file"
unset oss_access_key_id oss_access_key_secret api_token

systemctl daemon-reload
if "$start_service"; then
    systemctl enable --now doso-ota-release.service
    curl --fail --silent --show-error http://127.0.0.1:8140/health
    printf '\n'
else
    printf '%s\n' 'Configured release.env and signing key; service remains stopped.'
fi
