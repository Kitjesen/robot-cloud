# DOSO OTA release API

This service is the formal domestic release entry for the first OTA object.
It runs on the Shanghai ECS and keeps the release index and job records on the
server. OTA bytes stay in a private Shanghai OSS bucket.

The service does four things:

1. checks a device against the local device registry;
2. lists compatible release templates;
3. creates an idempotent job and signs a short-lived OSS download URL;
4. stores the signed job so the App can query it after a restart.

The release template is stored below `DOSO_RELEASE_ROOT`:

```text
releases/{releaseId}/manifest.template.json
jobs/{jobId}.json
```

The template contains the immutable release metadata and OSS object key. The
service inserts the current signed URL into the manifest and signs the final
manifest with the release private key. The private signing key and OSS secret
must be supplied through a secret manager or protected service environment.

The service is intentionally bound to `127.0.0.1:8140` by default. Put an
HTTPS reverse proxy or API gateway in front of it after the formal domain and
备案 configuration are ready.

Run local checks from the workspace root:

```text
cargo test -p doso-ota-release
cargo check --workspace
```
