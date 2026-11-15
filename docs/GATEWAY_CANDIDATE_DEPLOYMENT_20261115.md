# Gateway 当前候选部署门（2026-11-15）

这份 runbook 只适用于当前已构建的 Linux Gateway 候选，不包含客户绑定、云端运控或机器人 CAN 操作。

## 候选制品

- 本地构建目录：`target/wsl-gateway/release/doso-cloud-gateway`
- SHA-256：`eb25d3521246a693e4d769146164da30ccc40562afeed2c47b61766e902fd0ad`
- ELF x86-64，最高 `GLIBC_2.34`
- [构建 manifest](../artifacts/robot-cloud/real-connect-20261113/cloud-gateway-current-linux-build-20261115.json)

## 执行前只读门

确认目标仍是 MQTT ECS `8.153.169.177`，并记录以下事实：

```sh
uname -m
ldd --version | head -1
systemctl is-active doso-robot-cloud-gateway.service mosquitto.service nginx.service
sha256sum /opt/doso/bin/doso-cloud-gateway
systemctl show doso-robot-cloud-gateway.service -p FragmentPath -p DropInPaths -p ActiveState -p MainPID
```

如果服务不是 active、架构不是 x86-64，或现场运维正在使用 Gateway，停止本流程。

## 受控部署顺序

1. 在 ECS 上为旧二进制和当前 systemd 配置创建带 UTC 时间戳的 root-only 备份。
2. 上传候选到同一文件系统的临时路径；上传后先执行 SHA-256 校验，必须等于上面的候选值。
3. 以 `root:root`、`0755` 原子替换 `/opt/doso/bin/doso-cloud-gateway`；不改 `gateway.env`、`devices.json`、ACL、TLS 或 reporting 状态。
4. `systemctl restart doso-robot-cloud-gateway.service`，等待 `active`，记录新的 PID 和启动时间。
5. 只读检查 `GET /api/v1/robots/{uuid}/status/latest`：
   - HTTP 200 + `permission:null`/空 latest：证明当前实现能返回无许可投影；不等于设备在线。
   - HTTP 503：记录错误体和服务日志，停止继续扩大变更。
   - 不发送 `POST /status`、运动命令或任何设备控制消息。
6. 读取新一轮 `sha256sum`、systemd 状态和 API 响应，写入 `artifacts/robot-cloud/real-connect-20261113/`。

## 回滚

若启动失败、哈希不符或只读回归异常：

```sh
install -o root -g root -m 0755 <backup>/doso-cloud-gateway /opt/doso/bin/doso-cloud-gateway
systemctl restart doso-robot-cloud-gateway.service
systemctl is-active doso-robot-cloud-gateway.service
```

回滚后不重试权限、绑定或状态写入；保留失败日志和两侧哈希。

## 当前状态

本 runbook 已准备，但截至记录时间没有上传、替换或重启线上 Gateway。执行需要明确的线上变更授权；客户账号绑定、真实 reporting permission、机器狗断云和 App 状态仍是后续独立验收门。
