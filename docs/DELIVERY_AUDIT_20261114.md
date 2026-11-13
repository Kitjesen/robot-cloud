# 真实只读云连接交付审计

更新时间：2026-11-14（Asia/Shanghai）。本页按最终交付目标逐项区分源码、模拟、部署和当前真机证据。

| 要求 | 当前结论 | 证据与边界 |
| --- | --- | --- |
| Brainstem 本机 `13146` 只读状态 | 历史真机通过，当前待复核 | flowfix11 曾监听 `127.0.0.1:13146`，`GetControlStatus` 返回 GROUNDED、输出关闭、控制方 NONE；当前机器狗无 ARP，不能把历史状态当当前状态。[历史读取](../artifacts/robot-cloud/real-connect-20261113/read-current-13146.json) |
| `doso-cloud-link` | ARM64 候选已安装记录，当前运行未证明 | 关闭态 `cloud_off` 和安装记录通过；尚未确认当前远端二进制哈希与源码一致。[安装记录](../artifacts/robot-cloud/real-connect-20261113/cloud-link-install.json) |
| authorizer | 本地候选已部署记录，制品一致性待复核 | 本地 ARM64 重建已固化发布包；历史部署哈希与本地重建哈希不一致，设备恢复后必须先读远端哈希。[哈希审计](../artifacts/robot-cloud/real-connect-20261113/authorizer-hash-audit-20261114.json) · [发布 manifest](../artifacts/robot-cloud/releases/20261114-authorizer-aarch64/manifest.json) |
| MQTT / mTLS | 云端端点和历史 TLS 探测通过，真实设备会话未证明 | 云端 MQTT ECS 可达，Gateway HTTPS 可达；没有真实设备 `reporting.hello` 或 `status.report`。[Gateway latest](../artifacts/robot-cloud/real-connect-20261113/gateway-latest-20261114.json) |
| 云端 Gateway | 服务可达，当前无设备状态 | 真实 UUID 的按需查询和 `status/latest` 返回 `503 unavailable`，不是 `403 disabled`；登记已启用，但没有设备会话。[启用证据](../artifacts/robot-cloud/real-connect-20261113/cloud-assistant-enable-20261114.json) |
| 业务同步与 App | 源码合同和模拟链路通过，真机数据未验证 | App Node 30/30、业务 Deno 26/26；模拟设备业务流程通过。真实机器狗上报后仍需验证业务库入库和 App 时间/状态展示。[测试记录](../TESTING.md) |
| 云端远程运控禁止 | 已通过源码和权限回归 | Gateway 只接受 `status.read`；MQTT ACL、cloud-link 和 App 合同拒绝运动、遥操作、电机使能及通用 gRPC 请求。 |
| 远程关闭 / 断云 / 本地重开 | 模拟闭环通过，真机闭环未验收 | 模拟设备已验证持久化 `choice=false`、退出 MQTT、重启保持关闭、本地重新开启；真机需在真实会话建立后执行。 |
| 测试与文档记录 | 已完成当前轮记录 | 状态、证据、预检和发布包均已提交；工作区原有 `Cargo.toml`、`Cargo.lock`、`ota/` 变更未纳入本轮。 |

## 当前唯一现场门

`192.168.66.64` 当前没有 ARP，ping/TCP 22 超时。设备恢复后，先只读执行 [field-preflight.sh](../tools/deploy/field-preflight.sh)，再核对远端 authorizer/cloud-link SHA256，确认 `13146` 和 `choice=false`，最后才允许通过 authorizer API 开启云连接。整个过程中保持云端运控关闭。
