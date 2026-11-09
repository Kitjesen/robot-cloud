#!/bin/sh
# Run on the device, with the destination under trusted local storage.
set -eu
umask 077

if test "$#" != 2; then
    printf 'Usage: device-keys.sh <device-uuid-v4> <new-absolute-directory>\n' >&2
    exit 1
fi
device_id=$1
directory=$2
if test "${#device_id}" != 36 || ! printf '%s\n' "$device_id" | LC_ALL=C grep -Eq '^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$'; then
    printf 'Device ID must be a lowercase UUID v4\n' >&2
    exit 1
fi
case "$directory" in
    /*) ;;
    *) printf 'Destination must be an absolute path\n' >&2; exit 1 ;;
esac
test "$(uname -s)" = Linux
command -v openssl >/dev/null

# Exclusive creation keeps retries from replacing an existing device identity.
mkdir -m 700 -- "$directory"
cd "$directory"
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out device.key
openssl req -new -sha256 -key device.key -subj "/CN=$device_id" -out device.csr
openssl req -in device.csr -verify -noout >/dev/null
openssl genpkey -algorithm ED25519 -out device-signing.key
openssl pkey -in device-signing.key -pubout -out device-signing.pub

directory=$(pwd -P)
printf '%s/device.csr\n%s/device-signing.pub\n' "$directory" "$directory"
