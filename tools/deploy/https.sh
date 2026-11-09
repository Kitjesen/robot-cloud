#!/bin/sh
# First HTTPS installation after cloud.sh; leave existing web servers alone.
set -eu
cd "$(dirname "$0")"
test "$(id -u)" = 0
test ! -e /etc/nginx
test -f /etc/doso-mqtt/tls/server.pem
test -f /etc/doso-mqtt/tls/server.key
test -z "$(ss -H -lnt '( sport = :80 or sport = :443 )')"
export DEBIAN_FRONTEND=noninteractive
apt-get install -y --no-install-recommends nginx
# This symlink belongs to the package just installed; retain its available-site file.
test "$(readlink /etc/nginx/sites-enabled/default)" = /etc/nginx/sites-available/default
rm /etc/nginx/sites-enabled/default
install -m 644 https.conf /etc/nginx/conf.d/doso-cloud.conf
nginx -t
systemctl enable nginx
systemctl reload-or-restart nginx
systemctl is-active nginx
