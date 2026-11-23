# OTA ECS deployment record

The Shanghai release API was installed on ECS `47.103.89.137` through the
existing `admin` SSH deployment key.

## Observed

- Binary path: `/usr/local/lib/doso-ota-release/doso-ota-release`
- Unit: `doso-ota-release.service`
- Service account: `doso-release`
- Signing key: `/etc/doso/ota-release/release-signing.key.pem`
- Device registry: `/etc/doso/ota-release/devices.json`, currently `{}`
- Release state: `/var/lib/doso-ota-release`
- Binary SHA-256: `bc2ef069aef0dd1ed47513aeba61cf3b3e8df126fe2c2b689bd3ed7512b8ff9c`
- Temporary local smoke test: `/health` returned `200`, an unauthorised request returned `401`, and an authorised request for an unregistered device returned `404`.
- Temporary smoke credentials were removed after the test. The service is currently stopped and disabled until the real OSS credentials are configured.

## HTTPS route

The existing certificate for `api-test.dosorobotics.com` is valid and Nginx
now proxies `/ota/` to `127.0.0.1:8140`. The route returns `502` while the
release service is stopped, which confirms the route reaches the local
upstream. After the service starts, use `/ota/health` and
`/ota/v1/devices/{deviceId}/firmware/releases` for the public test checks.

## Remaining acceptance

1. Enter the real OSS AccessKey ID and Secret locally on ECS and start the service.
2. Verify public HTTPS health, `401`, registered-device compatibility, and a real signed OSS URL.
3. Build the ARM64 agent with the Linux cross linker and record its ELF architecture and SHA-256.
4. Produce a real Brainstem bundle, install the public verification key, and perform one physical upgrade and rollback run.

An ARM64 build attempt was started on ECS after installing
`gcc-aarch64-linux-gnu`. The system Cargo package is Rust 1.75 and cannot
consume this workspace's Rust 2024 manifest. The rustup download from
`static.rust-lang.org` did not complete during this run, so no ARM64 binary is
being recorded as a release artifact.
