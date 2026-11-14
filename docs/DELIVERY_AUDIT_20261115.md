# 真实只读云连接交付审计（2026-11-15）

本页是当前真实研发机窗口的最新审计，补充并覆盖早期“设备暂时不可达”的观察。它仍然区分真实设备、云端业务许可和本地模拟合同，不把其中一层替代另一层。

## 真实机器狗窗口

目标设备：`THUNDER-DEV-001`，UUID `d82a831a-2e86-4b46-975b-3d42473583b7`，地址 `192.168.66.64`。

只读快照显示：

- 开发机到机器狗 ping、SSH 22、Brainstem 13145 均可达。
- Brainstem 回环 `127.0.0.1:13146` 可读，`GetControlStatus` 返回 `GROUNDED`、电机输出关闭、控制方 `CONTROL_OWNER_NONE`、无严重故障、无 gRPC lease。
- `doso-cloud-link.service` 为 active，持久化 `choice=true`，到 MQTT `8.153.169.177:8883` 的 mTLS TCP 为 `ESTABLISHED`。
- `13148` authorizer active；正式 SDK Gateway `5052` 没有监听。
- 未携带 Bearer 的完整 choice 写请求返回 `401`，前后 choice、authorizer 和 cloud-link 均不变；本次没有读取 token，也没有执行授权写入。[边界证据](D:/DOSO/artifacts/robot-cloud/real-connect-20261113/authorizer-readonly-boundary-20261115.json)
- 云端远程运控继续禁止；本窗口没有发送运动、电机、导航、External 或通用 gRPC 命令。

证据：[real-readonly-snapshot-20261115.json](D:/DOSO/artifacts/robot-cloud/real-connect-20261113/real-readonly-snapshot-20261115.json)。

## 云端业务窗口

同一 UUID 的设备登记已经启用，但开发库存仍然 `owner_id=null`，没有绑定账号。业务同步因此返回 `permission:null`，Gateway 没有向设备下发短期 `reporting.control`。当前观察为：

| 项目 | 结果 |
| --- | --- |
| MQTT/mTLS 传输 | 通过（TCP 已建立） |
| `reporting.hello` | 未观察到可读证据 |
| `status.report` | 未观察到 |
| Gateway `/status` | HTTP 503 `unavailable` |
| Gateway `/status/latest` | HTTP 503 `unavailable` |
| 业务库 / App 真实状态 | 尚未证明 |
| 云端远程运控 | 禁止 |

这不是设备网络失败，而是当前产品归属/许可边界的预期结果。必须先完成研发设备账号绑定，再验证：

```text
hello → reporting.control → reporting.ack → status.report
      → remote-state-sync → PostgreSQL → App
```

## 本地合同回归

执行：

```text
cargo test --workspace --locked --offline -j 1
```

结果：95 passed、0 failed、1 ignored（仅 subprocess fixture）。Gateway、cloud-link、authorizer、报告许可、关闭/重开、租期过期、旧会话拒绝和远程运控拒绝均覆盖。

另外执行：

```text
cargo run --locked --offline -p lab -- check
```

模拟 Brainstem → Rust cloud-link → TLS MQTT → Gateway 的闭环通过：无许可 403、签名状态 200、读取失败保留 lastSuccess、远程关闭持久化 `choice=false`、重启保持关闭、本地重新开启需要新许可、租期过期停止采集且不改写本地选择、旧 connection/session 被拒绝。

证据：[cloud-link-regression-20261115.json](D:/DOSO/artifacts/robot-cloud/real-connect-20261113/cloud-link-regression-20261115.json) · [lab-check-20261115.json](D:/DOSO/artifacts/robot-cloud/real-connect-20261113/lab-check-20261115.json)。

## 交付判断

当前已经交付并证明：

1. 机器狗本机只读状态接口和安全状态边界。
2. 机器狗 cloud-link 的安装、持久化选择、mTLS MQTT 连接和开机恢复。
3. 云端设备只读登记和远程运控拒绝边界。
4. 云链路协议及关闭/重开合同的本地可重复回归。
5. App 远程状态读取合同与 `503`/过期/lastSuccess 展示逻辑（模拟和代码回归）。

尚未交付：

1. 真实物理设备绑定到账号后的短期 reporting permission。
2. 真实 `status.report` 入 Gateway、业务库和 App 的端到端读回。
3. 真机远程关闭后断 MQTT、重启保持关闭、重新本地开启的实测闭环。

下一次需要一个明确的外部业务动作：将 `THUNDER-DEV-001` 绑定到授权研发账号。绑定前不应伪造 `permission`、手工写 `status.report` 或把 MQTT TCP 连接显示成设备在线。
