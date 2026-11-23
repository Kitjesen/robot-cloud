# DOSO Robot OTA Agent

`doso-ota` is the robot-side installer for the first OTA object: a
Brainstem application bundle and its matching policy package. It is separate
from the motion service and the cloud link.

The agent owns the local update lifecycle:

1. verify the Ed25519-signed manifest and device compatibility;
2. refuse expired, downgraded, low-battery, offline, or busy updates;
3. resume a signed-URL download from a persisted byte journal;
4. verify exact size and SHA-256;
5. install into the inactive release slot and atomically move the active-slot pointer;
6. restart only the allow-listed Brainstem systemd unit;
7. require a device-signed Brainstem proof plus strategy, CAN read-only, and boot evidence before success;
8. switch back to the last-known-good slot when health fails, or report
   `recovery_required` when rollback health also fails.

The default management API binds to `127.0.0.1:13147`. Set
`DOSO_OTA_TOKEN` before exposing it to another local service. It accepts only
the OTA job operations; it never executes an arbitrary shell command.

State and artifacts are kept below `DOSO_OTA_ROOT` (default `/var/lib/doso-ota`):

```text
staging/{jobId}.artifact.part
staging/{jobId}.artifact.journal
slots/a/brainstem.bundle
slots/b/brainstem.bundle
current-slot
last-known-good-slot
job.json
```

The signed manifest is stored inside `job.json` so the job, release sequence,
and idempotency key are committed together.

The public verification key is installed separately. The signing private key
never belongs on the robot or in the App.

Build and test from the workspace root:

```text
cargo test -p doso-ota
cargo run -p doso-ota -- inspect
cargo run -p doso-ota -- serve
```

The local Windows release binary is only a host test artifact. The robot
package must be built with `--target aarch64-unknown-linux-gnu` in a Linux or
cross-toolchain environment that provides `aarch64-linux-gnu-gcc`; this
Windows checkout currently does not have that linker.

The health URLs and marker files in `env.example` are required evidence, not
demo defaults. If they are absent, the API keeps serving status but cannot
claim an update succeeded.
