#!/bin/sh
# First installation only; provision each device's credentials separately.
set -eu
test "$#" = 0
if test "$(uname -s)" != Linux || test "$(uname -m)" != aarch64 || test "$(id -u)" != 0; then
    printf 'Requires Linux aarch64 and root\n' >&2
    exit 1
fi
cd "$(dirname "$0")"

for file in link.sh LINK.md manifest.json bin/doso-cloud-link config/env.example config/ca.crt config/gateway-signing.pub systemd/doso-cloud-link.service; do
    test -f "$file"
    awk -v file="$file" '($2 == file || $2 == "*" file) && length($1) == 64 { found = 1 } END { exit !found }' SHA256SUMS
done
sha256sum --check --strict SHA256SUMS
test "$(grep -c '^DOSO_CLOUD_ON=' config/env.example)" = 1
grep -qx 'DOSO_CLOUD_ON=false' config/env.example

for path in /etc/doso/robot-cloud/agent /opt/doso/bin/doso-cloud-link /opt/doso/bin/doso-robot-agent /etc/systemd/system/doso-cloud-link.service /etc/systemd/system/doso-cloud-link.service.d /etc/systemd/system/doso-agent.service /etc/systemd/system/doso-agent.service.d; do
    if test -e "$path" || test -L "$path"; then
        printf 'Refusing to overwrite %s\n' "$path" >&2
        exit 1
    fi
done
for unit in doso-cloud-link.service doso-agent.service; do
    if systemctl cat "$unit" >/dev/null 2>&1; then
        printf '%s already exists\n' "$unit" >&2
        exit 1
    fi
done

if ! id doso-cloud >/dev/null 2>&1; then
    useradd --system --home-dir /opt/doso --shell /usr/sbin/nologin doso-cloud
fi
getent passwd doso-cloud | awk -F: '$3 > 0 && $3 < 1000 && ($7 == "/usr/sbin/nologin" || $7 == "/sbin/nologin" || $7 == "/bin/false") { found = 1 } END { exit !found }'
test "$(id -gn doso-cloud)" = doso-cloud
install -d -m 755 /opt/doso /opt/doso/bin
install -d -o root -g doso-cloud -m 750 /etc/doso/robot-cloud/agent
install -o root -g root -m 755 bin/doso-cloud-link /opt/doso/bin/doso-cloud-link
install -o root -g doso-cloud -m 640 config/env.example /etc/doso/robot-cloud/agent/agent.env
install -o root -g doso-cloud -m 640 config/ca.crt config/gateway-signing.pub /etc/doso/robot-cloud/agent/
install -o root -g root -m 644 systemd/doso-cloud-link.service /etc/systemd/system/doso-cloud-link.service
systemctl daemon-reload
printf 'Cloud link installed, disabled by configuration. Complete LINK.md before starting it.\n'
