#!/bin/sh
set -eu
umask 077

script=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)/device-keys.sh
work=$(mktemp -d /tmp/doso-device-keys.XXXXXX)
trap 'rm -rf -- "$work"' 0
trap 'exit 1' HUP INT TERM
device_id=11111111-1111-4111-8111-111111111111
directory=$work/keys

sh "$script" "$device_id" "$directory" > "$work/public-paths"
test "$(stat -c %a "$directory")" = 700
for file in device.key device.csr device-signing.key device-signing.pub; do
    test "$(stat -c %a "$directory/$file")" = 600
done
openssl req -in "$directory/device.csr" -verify -noout
test "$(openssl req -in "$directory/device.csr" -noout -subject -nameopt RFC2253)" = "subject=CN=$device_id"
openssl req -in "$directory/device.csr" -noout -pubkey > "$work/csr.pub"
openssl pkey -in "$directory/device.key" -pubout > "$work/tls.pub"
cmp "$work/csr.pub" "$work/tls.pub"
openssl pkey -in "$directory/device-signing.key" -pubout > "$work/signing.pub"
cmp "$directory/device-signing.pub" "$work/signing.pub"
openssl pkey -in "$directory/device.key" -text_pub -noout | grep -q 'NIST CURVE: P-256'
openssl pkey -in "$directory/device-signing.key" -text_pub -noout | grep -q ED25519
printf '%s/device.csr\n%s/device-signing.pub\n' "$directory" "$directory" > "$work/expected-paths"
cmp "$work/public-paths" "$work/expected-paths"

before=$(cksum "$directory/device.key")
if sh "$script" "$device_id" "$directory" > /dev/null 2>&1; then
    printf 'Existing directory was accepted\n' >&2
    exit 1
fi
test "$before" = "$(cksum "$directory/device.key")"
for invalid in bad 11111111-1111-5111-8111-111111111111 11111111-1111-4111-7111-111111111111 "$(printf '%s\nx' "$device_id")"; do
    if sh "$script" "$invalid" "$work/invalid" > /dev/null 2>&1; then
        printf 'Invalid UUID was accepted\n' >&2
        exit 1
    fi
done
test ! -e "$work/invalid"
if sh "$script" "$device_id" relative-path > /dev/null 2>&1; then
    printf 'Relative path was accepted\n' >&2
    exit 1
fi
printf 'device_keys_check_passed\n'
