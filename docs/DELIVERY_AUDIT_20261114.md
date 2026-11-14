# 真实只读云连接交付审计

更新时间：2026-11-14（Asia/Shanghai）。本页按最终交付目标逐项区分源码、模拟、部署和当前真机证据。

本日现场窗口补充：设备曾可达并通过 authorizer API 开启 `choice=true`。`doso-cloud-link` 与 MQTT 建立 TCP 会话并记录 `cloud_connected`，但业务同步对该未绑定研发库存返回 `permission:null`，Gateway 未下发短期 reporting lease，设备未产生可供 App 读取的 `status.report`。设备随后再次离线，以下结论按“传输层通过、业务状态未通过”记录。

2026-11-15 开机检查确认 13146 仍可读，`choice=true` 已保留。原有 unit 为 disabled，已在真机执行 `systemctl enable --now doso-cloud-link.service`，当前为 enabled/active 且 MQTT TCP 已建立。预检的 CAN USB 路径校验因当前 `can0→1-1.2` 与旧路径不一致而退出，四路 CAN 实际均为 UP/ERROR-ACTIVE/1Mbps，物理拓扑未被猜测修改。

| 要求 | 当前结论 | 证据与边界 |
| --- | --- | --- |
| Brainstem 本机 `13146` 只读状态 | 现场窗口通过，当前离线 | 窗口内 `GetControlStatus` 返回 GROUNDED、输出关闭、控制方 NONE、无严重故障、无 lease；设备随后无 ARP，不能把窗口状态当当前状态。[读取](../artifacts/robot-cloud/real-connect-20261113/read-current-13146.json) |
| `doso-cloud-link` | 传输层现场通过，状态上报待许可 | 已恢复为 enabled/active，MQTT TCP `ESTABLISHED`；业务许可为空，未形成 `status.report`。[开机恢复](../artifacts/robot-cloud/real-connect-20261113/cloud-link-enable-20261115.json) |
| authorizer | 现场 API 调用通过 | 远端二进制 SHA256 与发布值归一化后匹配。API 返回 `{"enabled":true}`，choice 持久化成功。预检比较已改为大小写不敏感。 |
| MQTT / mTLS | 真实 TCP 会话通过，应用消息待许可 | 云端 MQTT TCP 建立并记录 `cloud_connected`；尚未观察到 Gateway 可读的 `status.report`。[观察](../artifacts/robot-cloud/real-connect-20261113/real-cloud-observe-20261114.json) |
| 云端 Gateway | 服务可达，业务许可为空 | 真实 UUID 的按需查询和 `status/latest` 返回 `503 unavailable`；登记已启用，但 `remote-state-sync` 返回 `permission:null`，研发库存未绑定账号。[查询](../artifacts/robot-cloud/real-connect-20261113/real-cloud-activation-20261114.json) |
| 业务同步与 App | 源码合同和模拟链路通过，真机数据未验证 | App Node 30/30、业务 Deno 26/26；模拟设备业务流程通过。真实机器狗上报后仍需验证业务库入库和 App 时间/状态展示。[测试记录](../TESTING.md) |
| 云端远程运控禁止 | 已通过源码和权限回归 | Gateway 只接受 `status.read`；MQTT ACL、cloud-link 和 App 合同拒绝运动、遥操作、电机使能及通用 gRPC 请求。 |
| 远程关闭 / 断云 / 本地重开 | 模拟闭环通过，真机闭环未验收 | 模拟设备已验证持久化 `choice=false`、退出 MQTT、重启保持关闭、本地重新开启；真机需在真实会话建立后执行。 |
| 测试与文档记录 | 已完成当前轮记录 | 状态、证据、预检和发布包均已提交；工作区原有 `Cargo.toml`、`Cargo.lock`、`ota/` 变更未纳入本轮。 |

## 当前唯一现场门

设备当前已恢复可达。下一步先由 App 完成研发机账号绑定，再由业务授权流程生成短期 reporting permission，观察 `hello → control → ack → status.report`。CAN USB 路径需要现场确认物理标签后再更新，不能凭当前枚举顺序改映射。整个过程中保持云端运控关闭。
