# 已登记模拟设备的云端状态链路

文档核对：2026-11-08。下文记录2026-11-07的演练及证据；本次未重新确认模拟服务或云端进程仍在运行。当前产品缺口与顺序见 [PROGRESS.md](../PROGRESS.md)，各层测试范围见 [TESTING.md](../TESTING.md)。

2026-11-07 已在实际 DOSO Web App 验证：正常账号绑定、开启采集、真实云端数据库更新、退出页面继续更新、断网恢复，以及关闭停采。

## 当前体验

- App：`http://localhost:8081`，进入“我的”查看远程状态。
- 账号：`cloud-flow@doso.test`，专用测试管理员，正常 Auth 登录和终端登记。管理操作经过正式 MFA；没有把服务端密钥交给 App。
- 模拟库存：`DOSO-SIM-20261107-001`。
- 永久 UUID：`78ec077d-0cde-441b-bddc-022cfc850676`。
- 库存身份：`is_simulated=true`、`is_demo=false`，App 明确显示“模拟设备 / 模拟状态”。
- 该次验收结束时远程状态已关闭，模拟器与隔离Gateway当时保持运行。再次演练前按操作记录核对现有实例及授权，不将这条历史记录当作当前在线状态。

绑定使用正常的 `admin-devices` 创建库存/签发绑定码，再用同一账号的 `device-binding/claim` 认领。此轮通过正常 API 完成绑定，没有验收手机摄像头扫码。

## 本轮实际路径

```text
本机模拟 Brainstem → 正式 Rust doso-cloud-link → 本机 TLS MQTT
  → 公网 SSH 隧道 → ECS 上隔离 Rust Gateway
  → HTTPS remote-state-sync → 真实业务 PostgreSQL
  → 正常账号 remote-state API → 实际 DOSO Web App
```

设备与状态来源是模拟的；云端 Gateway、业务权限、数据库和 App 都实际运行。本轮不是独立公网 MQTT 或 5G 验收。MQTT 不传视频，未调用运动接口。

## 证据

证据目录：`D:\DOSO\artifacts\robot-cloud\20261107-app-flow`。

| 场景 | 实际结果 | 文件 |
|---|---|---|
| 绑定后默认关闭 | 独立 UUID、正常归属、无状态、读取次数 0 | `registered.json`、`connected-off.json`、`uplink.log` |
| App 开启 | `enabled=true`、`activation=on`、`fresh=true`；显示状态和采集时间 | `app-enabled-api.json`、`foreground.json`、`app-live.png` |
| 退出页面 | 浏览器导航到 `about:blank`；数据库序号 141→181，许可继续续期 | `app-closed.png`、`before-background.json`、`after-background.json`、`background-api.json` |
| 断网 | 停止实际 SSH 隧道；`reachable=false`、`fresh=false`；App显示过期 | `disconnected-api.json`、`disconnected-db.json`、`app-disconnected.png` |
| 恢复 | 仅恢复隧道，无需重开许可；新报告会话自动建立、`fresh=true` | `recovered-api.json`、`recovered-db.json`、`app-live.png` |
| App 关闭 | `enabled=false`、`activation=off`、`latest=null`；最后状态时间保持不变；reads=366持续29秒 | `closed-api.json`、`closed-db.json`、`closed-later-db.json`、`app-disabled.png`、`uplink.log` |

该次记录的最后状态固定为 `2026-11-07T15:43:12.953Z`。关闭后Gateway的联系/同步时间仍可变化；不能把停止状态采集描述为MQTT完全断开。该轮在模拟环境预置了本地选择，未验证用户经可信本地入口开启或重新开启机器狗云连接。

## 命令行读取

本机测试凭据以当前 Windows 用户的 DPAPI 加密保存，位于受限目录 `D:\DOSO\private\robot-cloud\flow-account`。不把该目录放入源码或分享。

```powershell
. D:\DOSO\private\robot-cloud\flow-account\auth.ps1
Open-Flow
Invoke-Flow 'functions/v1/remote-state?deviceId=78ec077d-0cde-441b-bddc-022cfc850676' |
  ConvertTo-Json -Depth 10
```

`Open-Flow` 使用保存的普通账号会话；需要时通过正式登录、TOTP 和终端登记重新获取会话。不会伪造 JWT 或绕过服务端校验。

## 程序位置与重新启动

- `tools/lab/src/uplink.rs`：持有模拟状态源、独立 MQTT 与正式 link 子进程；不生成用户许可，不启动本地 Gateway。
- `robot/`：正式设备通信程序。
- `cloud/gateway/`：正式 Gateway 与业务同步。
- App 的 `RemoteStatePanel.tsx`：模拟标记、状态时间与开关；后台不承担续期。
- App 的 `20261107100000_add_simulated_devices.sql`：不可变的模拟库存标记。
- 本机 `flow-account/cloud.ps1`：本轮隔离 Gateway/SSH 的操作脚本，详见同目录 `CLOUD.md`。

重新生成模拟环境时，在工程根目录配置 lab 已有 Mosquitto/OpenSSL 路径后运行：

```powershell
cargo run --locked -p lab -- uplink 78ec077d-0cde-441b-bddc-022cfc850676
```

保持此终端运行，在另一终端按输出的私有 `gateway.json` 运行隔离部署脚本的 `install`、`tunnel-start`、`start`。已有环境先按 `CLOUD.md` 停止/清理，不重复创建库存或覆盖另一轮配置。

## 本次修复与验证

新增模拟库存标记及 App 展示；复用 Fixture 的 MQTT、Gateway、link 配置，减少重复配置构建。正常前台已开启状态改为每秒读取且不并发，保留五秒过期窗口；后台刷新不禁用开关，操作前取消旧 GET，迟到结果不能覆盖新选择。

受影响 Rust 测试与构建、App TypeScript 检查、24项面板/服务测试、24项函数测试和35组隔离 PostgreSQL 检查通过。另验证部署边界与隧道真实停止；线上只新增模拟库存能力与明确的本机测试来源，不改媒体服务。

本轮不包含真机、5G、手机扫码、视频或完整断云/本地重新授权验收。
