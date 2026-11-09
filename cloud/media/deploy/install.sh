#!/bin/sh
# First installation. Review the private staging files before invoking as root.
set -eu
umask 077
test "$(id -u)" = 0
test "$(uname -s)" = Linux
test "$(uname -m)" = x86_64
test "$#" = 1
cd "$1"
for file in bin/mediamtx bin/doso-cloud-media mediamtx.yml media.json tls/media.crt tls/media.key doso-mediamtx.service doso-cloud-media.service; do
    test -f "$file"
    test ! -L "$file"
    awk -v file="$file" '($2 == file || $2 == "*" file) && length($1) == 64 { found = 1 } END { exit !found }' SHA256SUMS
done
sha256sum --check --strict SHA256SUMS
if grep -Eq 'REPLACE|PLACEHOLDER' media.json mediamtx.yml; then
    printf 'Unresolved deployment inputs\n' >&2
    exit 1
fi
python3 -c 'import json; c=json.load(open("media.json")); assert c["port"] == 19890; assert c["mediaOrigin"] == "http://127.0.0.1:19889"; assert c["mediaApiOrigin"] == "http://127.0.0.1:19997"; assert c["authorizeUrl"] == "https://api-test.dosorobotics.com/functions/v1/video-authorize"; assert c["sourceGatewayUrl"] == "http://127.0.0.1:8130"; assert 32 <= len(c["sourceGatewayKey"]) <= 256'
openssl x509 -in tls/media.crt -checkend 86400 -noout
openssl x509 -in tls/media.crt -checkhost media-test.dosorobotics.com -noout
openssl x509 -in tls/media.crt -pubkey -noout > cert.pub
openssl pkey -in tls/media.key -pubout > key.pub
cmp cert.pub key.pub
rm cert.pub key.pub
for path in /etc/doso-media /opt/doso/bin/mediamtx /opt/doso/bin/doso-cloud-media /etc/systemd/system/doso-mediamtx.service /etc/systemd/system/doso-cloud-media.service; do
    if test -e "$path" || test -L "$path"; then
        printf 'Refusing to overwrite %s\n' "$path" >&2
        exit 1
    fi
done
for unit in doso-mediamtx.service doso-cloud-media.service; do
    if systemctl cat "$unit" >/dev/null 2>&1; then
        printf 'Unit already exists: %s\n' "$unit" >&2
        exit 1
    fi
done
if ! id doso-media >/dev/null 2>&1; then
    useradd --system --home-dir /nonexistent --shell /usr/sbin/nologin doso-media
fi
getent passwd doso-media | awk -F: '$3 > 0 && $3 < 1000 && ($7 == "/usr/sbin/nologin" || $7 == "/sbin/nologin" || $7 == "/bin/false") { ok = 1 } END { exit !ok }'
test "$(id -gn doso-media)" = doso-media
install -d -o root -g root -m 755 /opt/doso/bin
install -d -o root -g doso-media -m 750 /etc/doso-media /etc/doso-media/tls
install -o root -g root -m 755 bin/mediamtx bin/doso-cloud-media /opt/doso/bin/
install -o root -g doso-media -m 640 mediamtx.yml media.json /etc/doso-media/
install -o root -g doso-media -m 640 tls/media.crt tls/media.key /etc/doso-media/tls/
install -o root -g root -m 644 doso-mediamtx.service doso-cloud-media.service /etc/systemd/system/
systemctl daemon-reload
printf 'Media files installed. Services remain stopped until network and authorization checks.\n'
