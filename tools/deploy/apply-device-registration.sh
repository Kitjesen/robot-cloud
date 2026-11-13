#!/usr/bin/env bash
set -euo pipefail

# Apply one reviewed device patch on the existing MQTT ECS.
# The first application is disabled-only; it never starts the Gateway or robot link.

patch_dir=${1:?usage: apply-device-registration.sh /path/to/cloud-patch}
gateway_dir=${DOSO_GATEWAY_DIR:-/etc/doso/robot-cloud/gateway}
acl_file=${DOSO_ACL_FILE:-/etc/doso-mqtt/acl.conf}
registry_file="$gateway_dir/devices.json"
mosquitto_conf=${DOSO_MOSQUITTO_CONF:-/etc/doso-mqtt/mosquitto.conf}

test "$(id -u)" = 0
test -d "$patch_dir"
test -f "$patch_dir/device-entry.json"
test -f "$patch_dir/acl-entry.conf"
test -f "$registry_file"
test -f "$acl_file"

device_id=$(python3 - "$patch_dir/device-entry.json" <<'PY'
import json, sys, uuid
from pathlib import Path

value = json.loads(Path(sys.argv[1]).read_text())
if not isinstance(value, dict) or value.get("enabled") is not False:
    raise SystemExit("device entry must be disabled on first application")
device_id = value.get("deviceId")
if not isinstance(device_id, str):
    raise SystemExit("device entry is missing deviceId")
parsed = uuid.UUID(device_id)
if parsed.version != 4 or parsed.variant != uuid.RFC_4122 or str(parsed) != device_id:
    raise SystemExit("deviceId must be canonical UUID v4")
key_path = value.get("publicKeyPath")
if not isinstance(key_path, str) or Path(key_path).name != key_path:
    raise SystemExit("publicKeyPath must be a file name")
print(device_id)
PY
)

key_name=$(python3 - "$patch_dir/device-entry.json" <<'PY'
import json, sys
from pathlib import Path
value = json.loads(Path(sys.argv[1]).read_text())
print(value["publicKeyPath"])
PY
)
test -f "$patch_dir/$key_name"

python3 - "$patch_dir/acl-entry.conf" "$device_id" <<'PY'
import sys
from pathlib import Path

path, device_id = sys.argv[1:]
lines = Path(path).read_text().splitlines()
expected = [
    f"user {device_id}",
    f"topic read doso/v1/devices/{device_id}/status/requests",
    f"topic write doso/v1/devices/{device_id}/status/replies",
    f"topic write doso/v1/devices/{device_id}/reporting/hello",
    f"topic read doso/v1/devices/{device_id}/reporting/control",
    f"topic write doso/v1/devices/{device_id}/reporting/acks",
    f"topic write doso/v1/devices/{device_id}/status/reports",
]
if lines != expected:
    raise SystemExit("ACL patch is not the expected six-line device block")
PY

stamp=$(date -u +%Y%m%dT%H%M%SZ)
backup_dir=/root/doso-device-register-$stamp
mkdir -m 700 "$backup_dir"
cp -a "$registry_file" "$backup_dir/devices.json"
cp -a "$acl_file" "$backup_dir/acl.conf"
if test -e "$gateway_dir/$key_name"; then
    cp -a "$gateway_dir/$key_name" "$backup_dir/$key_name"
fi

rollback() {
    cp -a "$backup_dir/devices.json" "$registry_file"
    cp -a "$backup_dir/acl.conf" "$acl_file"
    if test -e "$backup_dir/$key_name"; then
        cp -a "$backup_dir/$key_name" "$gateway_dir/$key_name"
    else
        rm -f "$gateway_dir/$key_name"
    fi
    mosquitto --test-config -c "$mosquitto_conf" >/dev/null
    systemctl reload mosquitto || true
}
trap 'rollback' ERR

python3 - "$registry_file" "$patch_dir/device-entry.json" <<'PY'
import json, os, sys, tempfile, uuid
from pathlib import Path

registry_path, entry_path = map(Path, sys.argv[1:])
registry = json.loads(registry_path.read_text())
entry = json.loads(entry_path.read_text())
devices = registry.get("devices") if isinstance(registry, dict) else None
if not isinstance(devices, list):
    raise SystemExit("devices.json must contain a devices array")
target = entry["deviceId"]
ids = [item.get("deviceId") for item in devices if isinstance(item, dict)]
if len(ids) != len(set(ids)):
    raise SystemExit("existing devices.json contains duplicate device IDs")
if target in ids:
    raise SystemExit("device ID already exists in devices.json")
devices.append(entry)
fd, temporary = tempfile.mkstemp(prefix="devices.json.", dir=str(registry_path.parent))
try:
    with os.fdopen(fd, "w") as stream:
        json.dump(registry, stream, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, registry_path)
finally:
    if os.path.exists(temporary):
        os.unlink(temporary)
PY
chown root:doso-cloud "$registry_file"
chmod 640 "$registry_file"

if grep -Fqx "user $device_id" "$acl_file"; then
    echo "device ID already exists in ACL" >&2
    exit 1
fi
cat "$patch_dir/acl-entry.conf" >> "$acl_file"
install -o root -g mosquitto -m 640 "$patch_dir/$key_name" "$gateway_dir/$key_name"
chown mosquitto:mosquitto "$acl_file"
chmod 640 "$acl_file"

mosquitto --test-config -c "$mosquitto_conf"
systemctl reload mosquitto
trap - ERR
printf 'device_registration_applied device_id=%s enabled=false backup=%s\n' "$device_id" "$backup_dir"
