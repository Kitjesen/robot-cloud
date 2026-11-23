# Shanghai OTA release API installation

This runbook installs the x86_64 release API on the Shanghai ECS without
starting it before its secrets are configured.

## Package evidence

- Bucket: `doso-ota-prod-shanghai`
- Region: `oss-cn-shanghai.aliyuncs.com`
- Archive: `artifacts/robot-cloud/releases/doso-ota-release-x86_64.tar.gz`
- Archive SHA-256: `567e9922673dd908442be32a85bb76485094abceeb6ea3c7f437502b99f68763`
- Binary SHA-256: `bc2ef069aef0dd1ed47513aeba61cf3b3e8df126fe2c2b689bd3ed7512b8ff9c`

## Remote sequence

1. Download the archive from the private OSS object using a short-lived signed URL.
2. Verify the archive SHA-256 before extraction.
3. Extract it to a temporary directory and run `install-ota-release.sh <package-dir>`.
4. On the ECS terminal, run `configure-ota-release.sh`; enter the OSS AccessKey ID and secret only at the local hidden prompts.
5. Run `configure-ota-release.sh --start` after checking the values are correct.
6. Verify `curl --fail http://127.0.0.1:8140/health` and an unauthorized request returns `401`.

The service binds to `127.0.0.1:8140` until an HTTPS reverse proxy and备案
route are separately accepted. The device registry remains `{}` until a real
device record is confirmed; no hardware revision is guessed.

The signing key is generated on the ECS by the configuration script. It is
never included in the App, robot bundle, OSS object, archive, or chat.
