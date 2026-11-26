#!/usr/bin/env bash
set -euo pipefail

# Install the release API files without starting it before secrets are configured.
test "$(id -u)" -eq 0

package_dir="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../artifacts/robot-cloud/releases/20261114-ota-release-x86_64" 2>/dev/null && pwd || true)}"
test -n "$package_dir" || { printf 'usage: %s <package-dir>\n' "$0" >&2; exit 2; }
package_dir="$(cd "$package_dir" && pwd)"
binary="$package_dir/doso-ota-release"
service="$package_dir/doso-ota-release.service"
checksum="$package_dir/doso-ota-release.sha256"

test -f "$binary"
test -f "$service"
test -f "$checksum"
expected_sha256="$(awk 'NF { print $1; exit }' "$checksum" | tr -d '\r\n')"
actual_sha256="$(sha256sum "$binary" | awk '{ print $1 }')"
test "${expected_sha256,,}" = "${actual_sha256,,}"

if ! id doso-release >/dev/null 2>&1; then
    useradd --system --home-dir /var/lib/doso-ota-release --shell /usr/sbin/nologin doso-release
fi

install -d -o doso-release -g doso-release -m 750 \
    /var/lib/doso-ota-release \
    /var/lib/doso-ota-release/releases \
    /var/lib/doso-ota-release/jobs \
    /var/lib/doso-ota-release/rollouts \
    /var/lib/doso-ota-release/status \
    /var/lib/doso-ota-release/apps \
    /var/lib/doso-ota-release/apps/android
install -d -o root -g doso-release -m 750 /etc/doso/ota-release
install -d -o root -g root -m 755 /usr/local/lib/doso-ota-release
install -o root -g root -m 755 "$binary" /usr/local/lib/doso-ota-release/doso-ota-release
install -o root -g root -m 644 "$service" /etc/systemd/system/doso-ota-release.service
install -o root -g root -m 640 "$package_dir/env.example" /etc/doso/ota-release/release.env.example

if test ! -e /etc/doso/ota-release/devices.json; then
    printf '{}\n' > /etc/doso/ota-release/devices.json
fi
chown root:doso-release /etc/doso/ota-release/devices.json
chmod 640 /etc/doso/ota-release/devices.json

systemctl daemon-reload
systemd-analyze verify /etc/systemd/system/doso-ota-release.service
printf '%s\n' 'Installed doso-ota-release; service remains stopped until release.env and signing key are configured.'
