# DOSO OTA Production Runbook

Canonical end-to-end operations guide for `doso-ota` (robot), `doso-ota-release` (Shanghai ECS), private Aliyun OSS (`doso-ota-prod-shanghai`), and `tools/deploy/doso-ota-ops.py`.

## 1. Architecture and Boundaries

| Component | Location / Bind | Role | Notes |
| --- | --- | --- | --- |
| `doso-ota-release` | ECS `127.0.0.1:8140` -> Nginx `/ota/` | Release templates, Ed25519 manifest signing, OSS URL signing, canary rollouts, device status inventory | State under `/var/lib/doso-ota-release` (`releases/`, `jobs/`, `rollouts/`, `status/`, `apps/android/`) |
| Private OSS | `doso-ota-prod-shanghai.oss-cn-shanghai.aliyuncs.com` | Immutable `.tar.gz` and `.apk` byte storage | Private ACL; robot downloads via short-lived `Expires` signed HTTPS URLs |
| `doso-ota` | Robot `127.0.0.1:13147` + cloud poller | Polls `next-job`, downloads with byte journal, enforces safety window, swaps slots / pushes policy, auto-rolls back | State under `/var/lib/doso-ota`; slots under `/opt/doso/components/<comp>/{releases,current,last-known-good}` |
| `doso-brainstem` | Robot `:5054` (`/health`, `/v1/policies/{push,rollback}`) | Safety pre-check source, Channel A health proof, Channel B zero-restart policy hot-OTA | `THUNDER_ALLOW_MOTOR_ENABLE` can be `true` on real robots; `doso-ota` checks `!motor_output_enabled && cms_state=="grounded"` |
| `cloud/console` | ThingsBoard CE `4.3.1.8` (`127.0.0.1:18081`) | Telemetry / device management UI only | **Zero `doso-ota-release` integration**: proxies only `/api`, `/static/*`, `/oauth2`. Do **not** use ThingsBoard stock `/features/otaUpdates` or embed `DOSO_RELEASE_API_TOKEN` in browser JS. |

## 2. Key Custody and First-Time ECS / Robot Provisioning

### 2.1 ECS Installation & Signing Keys

1. Build `doso-ota-release` (`x86_64-unknown-linux-gnu.2.31`), bundle binary + `doso-ota-release.service` + `doso-ota-release.sha256` + `env.example`, and run on ECS as root:
   ```sh
   bash tools/deploy/install-ota-release.sh /path/to/release-pkg
   ```
2. Provision `/etc/doso/ota-release/devices.json` (see §2.2), then configure OSS credentials, API token, and Ed25519 signing keys interactively on ECS:
   ```sh
   bash tools/deploy/configure-ota-release.sh --start
   ```
   This generates `/etc/doso/ota-release/release-signing.key.pem` (`0640 root:doso-release`), exports `/etc/doso/ota-release/release-signing.pub.pem` (`0644`), and prints `der_sha256`.
3. Copy **only** `release-signing.pub.pem` to every robot (`DOSO_OTA_VERIFY_KEY`, e.g. `/etc/doso/robot-cloud/ota/release-signing.pub.pem` or `/etc/doso/ota-release-signing.pub.pem` on Dog3). Never copy `.key.pem`, OSS AccessKey, or `DOSO_RELEASE_API_TOKEN` into git or robot artifacts.

### 2.2 Device Provisioning (Release Registry vs Gateway Registry)

`doso-ota-release` uses a **map** in `/etc/doso/ota-release/devices.json` (`ring` is `dev | canary | prod`), distinct from Gateway's `{"devices": [...]}` array. Always generate a fresh random RFC 4122 UUID v4 (`python3 -c "import uuid; print(uuid.uuid4())"`) for every new or re-enrolled robot (do not copy IP-suffix placeholder UUIDs such as legacy Dog3 `1313...`):

```bash
python tools/deploy/doso-ota-ops.py provision-device \
  --devices-file /tmp/devices.json \
  --device-id <device-uuid-v4> \
  --model "THUNDER 01" \
  --hardware-revision rdk-s100p \
  --ring canary \
  --tag rdk-s100p
```

Validate on ECS before replacing `/etc/doso/ota-release/devices.json` (`root:doso-release 0640`). `doso-ota-release` hot-reloads `devices.json` on each request without restart.

### 2.3 Robot `/etc/doso/ota.env`

Render a clean template without embedded secrets using:

```bash
python tools/deploy/doso-ota-ops.py render-robot-env \
  --device-id <device-uuid-v4> \
  --brainstem-version 0.0.3 \
  --brainstem-release v0.0.3 \
  --verify-key /etc/doso/ota-release-signing.pub.pem \
  --cloud-url https://api.dosorobotics.com/ota \
  --output /tmp/ota.env
```

Set `DOSO_OTA_CLOUD_TOKEN` locally on the robot (`0600`), keep `DOSO_OTA_COMPONENTS_ROOT=/opt/doso/components`, `DOSO_OTA_REQUIRE_CAN_READ_ONLY=false`, `DOSO_OTA_MIN_VBUS_V=44.0`, and verify `/opt/doso/ota/bin/doso-ota` points to `/opt/doso/components/ota/current/...` with `last-known-good` intact. Anti-replay sequences are stored independently per component (`components/<comp>/last-sequence`), per policy slot (`policies/<slot>/last-sequence`), and for bundles (`bundle-last-sequence`).

## 3. Production Release Workflow

All operator commands use `tools/deploy/doso-ota-ops.py` (stdlib-only Python 3; secrets are read exclusively from environment variables).

### Step 1 — Cross-Compile Robot Binary (for binary/component releases)

```powershell
$env:CARGO_ZIGBUILD_ZIG_COMMAND = 'D:\DOSO\toolchains\robot-cloud\zig\ziglang\zig.exe'
D:\DOSO\toolchains\robot-cloud\zigbuild\bin\cargo-zigbuild.exe build --release --locked --target aarch64-unknown-linux-gnu.2.35 -p doso-ota
```

### Step 2 — Build & Verify Immutable Tarball

Channel A (`brainstem_release`), standalone component (`component_release`, e.g. `ota`, `cloud-link`, `voice`, `nav`), or Channel B (`policy_package`):

```bash
# Brainstem release (requires bin/doso-brainstem inside source-dir)
python tools/deploy/doso-ota-ops.py package \
  --type brainstem_release \
  --source-dir ./staging/brainstem-0.0.4 \
  --output ./dist/brainstem-0.0.4.tar.gz

# Standalone component (e.g. doso-ota self-upgrade)
python tools/deploy/doso-ota-ops.py package \
  --type component_release --component ota \
  --source-dir ./staging/ota-0.1.0 \
  --output ./dist/ota-0.1.0.tar.gz

# Zero-restart policy package (requires policy.yaml + ONNX model + optional golden.json)
python tools/deploy/doso-ota-ops.py package \
  --type policy_package \
  --source-dir ./staging/flat_w6 \
  --output ./dist/flat_w6.tar.gz
```

`package` generates POSIX `+x` permissions on `bin/` and ELF files, LF `SHA256SUMS`, rejects path traversal/symlinks, and outputs `{sizeBytes, sha256}`.

### Step 3 — Upload Immutable Object to Private Shanghai OSS

```bash
# Dry-run first
python tools/deploy/doso-ota-ops.py oss-upload \
  --file ./dist/brainstem-0.0.4.tar.gz \
  --object-key firmware/qa/bs-0.0.4/brainstem-0.0.4.tar.gz \
  --dry-run

# Live upload (requires DOSO_OSS_ACCESS_KEY_ID & DOSO_OSS_ACCESS_KEY_SECRET in env; refuses overwrite by default)
python tools/deploy/doso-ota-ops.py oss-upload \
  --file ./dist/brainstem-0.0.4.tar.gz \
  --object-key firmware/qa/bs-0.0.4/brainstem-0.0.4.tar.gz
```

### Step 4 — Render & Install Release Template on ECS

`doso-ota-release` currently reads release templates from `<DOSO_RELEASE_ROOT>/releases/<releaseId>/manifest.template.json`:

```bash
python tools/deploy/doso-ota-ops.py render-template \
  --release-id bs-0.0.4 \
  --version 0.0.4 \
  --sequence 4 \
  --channel qa \
  --type brainstem_release \
  --object-key firmware/qa/bs-0.0.4/brainstem-0.0.4.tar.gz \
  --artifact ./dist/brainstem-0.0.4.tar.gz \
  --output ./dist/bs-0.0.4.manifest.template.json
```

Upload `./dist/bs-0.0.4.manifest.template.json` to `/var/lib/doso-ota-release/releases/bs-0.0.4/manifest.template.json` (`doso-release:doso-release 0640`).

> **Note on `release_bundle` and `delta`**: For `release_bundle`, pass `--bundle-spec <items.json>` to `render-template`; it keeps top-level `objectKey` empty and populates `bundleObjectKeys` (plus optional `--delta-object-key` for `deltaObjectKey`), matching `cloud/release/src/lib.rs` `build_signed_envelope`.

### Step 5 — Canary-to-Prod Rollout & Circuit Breaker Operations

Set `DOSO_RELEASE_API_TOKEN` in your local shell (`DOSO_OTA_CLOUD_URL` defaults to `https://api.dosorobotics.com/ota`).

```bash
# 1. Create canary -> prod-10 -> prod-100 rollout (circuit trips on 1st failure or >5%)
python tools/deploy/doso-ota-ops.py rollout create \
  --release-id bs-0.0.4 \
  --rollout-id ro-bs-0.0.4 \
  --tag dog3 \
  --canary-max-devices 1 \
  --canary-bake-seconds 1800 \
  --max-failure-count 1 \
  --max-failure-rate 0.05

# 2. Monitor rollout stage counts, per-device phase, and circuit breaker
python tools/deploy/doso-ota-ops.py rollout status ro-bs-0.0.4
python tools/deploy/doso-ota-ops.py fleet-status

# 3. Promote canary -> prod-10 -> prod-100, or pause / resume / abort
python tools/deploy/doso-ota-ops.py rollout promote ro-bs-0.0.4
python tools/deploy/doso-ota-ops.py rollout pause ro-bs-0.0.4
python tools/deploy/doso-ota-ops.py rollout resume ro-bs-0.0.4
python tools/deploy/doso-ota-ops.py rollout abort ro-bs-0.0.4
```

For single-robot acceptance before a fleet rollout, trigger an idempotent single-device job:

```bash
python tools/deploy/doso-ota-ops.py job start \
  --device-id <device-uuid-v4> \
  --release-id bs-0.0.4 \
  --idempotency-key accept-bs-0.0.4
```

## 4. Production Cutover Status (`https://api.dosorobotics.com/ota`)

The formal OTA domain cutover completed on **2026-11-26**:

1. **DNS & TLS**: Authoritative `A` record `api.dosorobotics.com -> 47.103.89.137` configured in Aliyun ESA (`siteId=187245473806416`, DNS-only mode, mirrored in `dnsnext`); Let's Encrypt certificate issued via `--webroot` (valid through `2027-02-24`).
2. **Minimal Nginx Exposure**: `/etc/nginx/sites-available/doso-api-prod-ota` redirects HTTP `:80` (except ACME) to HTTPS `:443`, and proxies **only `/ota/` -> `http://127.0.0.1:8140/`** (all other paths return `404`). Note: `api-test.dosorobotics.com` remains active separately for Supabase test functions and the 443 WSS ops tunnel (`/~!frp`).
3. **Service Hardening & Live Acceptance**: `doso-ota-release.service` runs as `doso-release` with `NoNewPrivileges=true`, `PrivateTmp=true`, `ProtectHome=true`, `ReadWritePaths=/var/lib/doso-ota-release`. Verified `/ota/health` (`200`), unauthenticated `/ota/v1/devices/status` (`401`), and Dog3 end-to-end live policy OTA over `https://api.dosorobotics.com/ota` (Job `a8e1835d-dd45-4ab3-87a1-636a2a46bb72`, `sequence=202`, `724464 B`, `phase=succeeded`, `motor_output_enabled=false`, `vbus_min_v=53.27V`).
4. **Remaining Robot Onboarding**: Migrate/onboard additional robots (e.g., `THUNDER-DEV-001` once reachable) to `DOSO_OTA_CLOUD_URL=https://api.dosorobotics.com/ota` and execute Channel A `brainstem_release` + bad-hash rollback acceptance on Dog3.

## 5. Git Release Hygiene (`D:\DOSO\robot-cloud`)

Before committing and tagging an OTA production release (`ota-v0.1.0`):
- **Include**: `Cargo.lock`, `ota/**` (including `src/download.rs`), `cloud/release/**` (`src/lib.rs`, `src/rollout.rs`, `src/main.rs`, `doso-ota-release.service`, `Cargo.toml`), `CONTRACT.md`, `tools/deploy/**`, `docs/OTA_PRODUCTION_RUNBOOK.md`, `docs/DOSO_OTA_RELEASE_BOT.md`, `cloud/console/README.md`.
- **Exclude (never commit)**: `target/` cross-compiled ELFs, `.runtime/`, `cloud/console/.build/`, `tools/deploy/__pycache__/`, `artifacts/robot-cloud/releases/*.tar.gz`, any `*.pem` / `*.key` / `.env`, and delete the empty stray `\` (`U+F05C`) root directory before tagging.
- **Verify**: `cargo fmt --all -- --check`, `cargo clippy -p doso-ota -p doso-ota-release --all-targets -- -D warnings`, `cargo test -p doso-ota -p doso-ota-release`, `python -m unittest discover -s tools/deploy -p "test_*.py" -v`, and `bash -n tools/deploy/*.sh`.
