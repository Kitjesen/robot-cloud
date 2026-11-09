#!/bin/sh
# First installation on the empty ECS host; credentials arrive in a private bundle.
set -eu
test "$(id -u)" = 0
cd "$(dirname "$0")"
test ! -e /etc/doso-mqtt
test ! -e /etc/doso/robot-cloud/gateway
test -f bin/doso-cloud-gateway
sha256sum -c SHA256SUMS

export DEBIAN_FRONTEND=noninteractive
apt-get update -q
apt-get install -y --no-install-recommends ca-certificates curl gnupg
curl --fail --silent --show-error --location 'https://keyserver.ubuntu.com/pks/lookup?op=get&search=0x7B2FBD7C964368452615DB74E633D0D8216D1222' -o mosquitto-ppa.asc
fingerprint=$(gpg --batch --show-keys --with-colons mosquitto-ppa.asc | awk -F: '$1 == "fpr" { print $10; exit }')
test "$fingerprint" = 7B2FBD7C964368452615DB74E633D0D8216D1222
install -m 644 mosquitto-ppa.asc /usr/share/keyrings/doso-mosquitto-ppa.asc
cat > /etc/apt/sources.list.d/doso-mosquitto.sources <<'EOF'
Types: deb
URIs: https://ppa.launchpadcontent.net/mosquitto-dev/mosquitto-ppa/ubuntu
Suites: noble
Components: main
Signed-By: /usr/share/keyrings/doso-mosquitto-ppa.asc
EOF
apt-get update -q
apt-get install -y --no-install-recommends mosquitto mosquitto-clients
test -f /usr/lib/x86_64-linux-gnu/mosquitto_acl_file.so

if ! id doso-cloud >/dev/null 2>&1; then
    useradd --system --home-dir /opt/doso --shell /usr/sbin/nologin doso-cloud
fi
install -d -m 755 /opt/doso /opt/doso/bin
install -m 755 bin/doso-cloud-gateway /opt/doso/bin/doso-cloud-gateway
install -d -o root -g mosquitto -m 750 /etc/doso-mqtt /etc/doso-mqtt/tls
install -o root -g mosquitto -m 640 broker/mosquitto.conf /etc/doso-mqtt/
install -o mosquitto -g mosquitto -m 640 broker/acl.conf /etc/doso-mqtt/
install -o root -g mosquitto -m 640 broker/tls/* /etc/doso-mqtt/tls/
install -d -o root -g doso-cloud -m 750 /etc/doso/robot-cloud/gateway
install -o root -g doso-cloud -m 640 gateway/* /etc/doso/robot-cloud/gateway/
install -m 644 systemd/doso-robot-cloud-gateway.service /etc/systemd/system/
install -d -m 755 /etc/systemd/system/mosquitto.service.d
cat > /etc/systemd/system/mosquitto.service.d/doso.conf <<'EOF'
[Service]
ExecStart=
ExecStart=/usr/sbin/mosquitto -c /etc/doso-mqtt/mosquitto.conf
EOF
mosquitto --test-config -c /etc/doso-mqtt/mosquitto.conf
systemctl daemon-reload
systemctl enable mosquitto doso-robot-cloud-gateway
systemctl restart mosquitto
systemctl start doso-robot-cloud-gateway
systemctl is-active mosquitto doso-robot-cloud-gateway
dpkg-query -W mosquitto libcjson1
