#!/bin/sh
set -eu
umask 077
cd /root/doso-gateway-20261104

test "$(id -u)" = 0
test -f doso-cloud-gateway
test -f remote-sync.key
test -f gateway-reporting.conf
test ! -e /etc/doso/robot-cloud/gateway/sync.env
test ! -e /etc/systemd/system/doso-robot-cloud-gateway.service.d/doso-sync.conf
test "$(cat /etc/doso-mqtt/acl.conf)" = "$(printf 'user doso-gateway\ntopic write doso/v1/devices/+/status/requests\ntopic read doso/v1/devices/+/status/replies')"
systemctl is-active --quiet mosquitto
systemctl is-active --quiet doso-robot-cloud-gateway

mkdir backup
cp -a /opt/doso/bin/doso-cloud-gateway backup/gateway
cp -a /etc/doso-mqtt/acl.conf backup/acl.conf
rollback() {
    install -m 755 backup/gateway /opt/doso/bin/doso-cloud-gateway.rollback
    mv -f /opt/doso/bin/doso-cloud-gateway.rollback /opt/doso/bin/doso-cloud-gateway
    cp -a backup/acl.conf /etc/doso-mqtt/acl.conf
    rm -f /etc/doso/robot-cloud/gateway/sync.env /etc/systemd/system/doso-robot-cloud-gateway.service.d/doso-sync.conf
    systemctl daemon-reload
    systemctl reload mosquitto || true
    systemctl restart doso-robot-cloud-gateway || true
}
trap rollback EXIT

printf 'DOSO_REMOTE_SYNC_KEY=%s\n' "$(cat remote-sync.key)" > sync.env
install -o root -g doso-cloud -m 640 sync.env /etc/doso/robot-cloud/gateway/sync.env
mkdir -p /etc/systemd/system/doso-robot-cloud-gateway.service.d
cp gateway-reporting.conf doso-sync.conf
printf '%s\n' 'Environment=DOSO_REMOTE_SYNC_URL=https://api-test.dosorobotics.com/functions/v1/remote-state-sync' 'EnvironmentFile=/etc/doso/robot-cloud/gateway/sync.env' >> doso-sync.conf
install -o root -g root -m 644 doso-sync.conf /etc/systemd/system/doso-robot-cloud-gateway.service.d/doso-sync.conf
printf '\n%s\n' 'topic read doso/v1/devices/+/reporting/hello' 'topic write doso/v1/devices/+/reporting/control' 'topic read doso/v1/devices/+/reporting/acks' 'topic read doso/v1/devices/+/status/reports' >> /etc/doso-mqtt/acl.conf
install -m 755 doso-cloud-gateway /opt/doso/bin/doso-cloud-gateway.new
systemctl reload mosquitto
systemctl daemon-reload
mv -f /opt/doso/bin/doso-cloud-gateway.new /opt/doso/bin/doso-cloud-gateway
systemctl restart doso-robot-cloud-gateway
systemctl is-active --quiet doso-robot-cloud-gateway
trap - EXIT
printf '%s\n' 'gateway_upgrade_started'
