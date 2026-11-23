# 远程控制产品交付

更新于 2026-11-17。当前代码实现远控会话及 Claim、Hold、Release；验收结果维护在 TESTING.md，部署状态维护在 PROGRESS.md。

## 完整代码路径

| 责任 | 当前路径 |
| --- | --- |
| Rust SDK 与七域 API | `D:\inovxio\brain\DOSO-SDK\` |
| SDK 连接与握手 | `D:\inovxio\brain\DOSO-SDK\src\client\mod.rs` |
| SDK 本地遥操作 | `D:\inovxio\brain\DOSO-SDK\src\motion\teleop.rs` |
| SDK 原始消息定义 | `D:\inovxio\brain\DOSO-SDK\proto\` |
| 机器狗云连接 | `D:\DOSO\robot-cloud\robot\` |
| 机器狗远控桥 | `D:\DOSO\robot-cloud\robot\src\control\` |
| 云端 Gateway 与中继 | `D:\DOSO\robot-cloud\cloud\gateway\` |
| 远控会话与 gRPC 服务 | `D:\DOSO\robot-cloud\cloud\gateway\src\control\` |
| 远控 gRPC 协议 | `D:\DOSO\robot-cloud\shared\proto\control.proto` |
| 远控签名与校验 | `D:\DOSO\robot-cloud\shared\src\control.rs` |
| App 账号授权入口 | `D:\DOSO\app-workspace\doso-app\supabase\functions\remote-control\` |
| App 会话服务 | `D:\DOSO\app-workspace\doso-app\src\services\remoteControl.ts` |
| Rust 远控调用工具 | `D:\DOSO\robot-cloud\tools\lab\src\control_check.rs` |
| 机端 OTA agent | `D:\DOSO\robot-cloud\ota\` |
| 云端 OTA 发布服务 | `D:\DOSO\robot-cloud\cloud\release\` |

机端桥通过 Cargo Git 依赖引用 Rust SDK，提交锁定为 `67634645d49a22cb45927d5d2dc8c0d5ac1d2e9a`。SDK 原始消息由该依赖生成。云中继协议由本项目 shared/proto/control.proto 生成。

## 本轮接入流程

1. App 用已有账号调用 `POST /functions/v1/remote-control`，请求 `{"action":"create","deviceId":"UUID"}`。
2. 业务函数验证账号、终端会话、设备归属和授权版本，使用服务端凭据调用 Gateway 创建会话。
3. 机器人主动连接 `RemoteControl.DeviceLink`，完成设备签名与一次性挑战校验。
4. App 或 Rust 调用者提交短期 grant，建立 `RemoteControl.Operate`。
5. Claim、Hold、Release 经中继到机端桥，再调用既有 Rust SDK。本地 SDK 握手身份必须匹配授权设备。
6. 机端原样返回接纳结果、持权状态、最终输出确认和未知提交语义。释放、撤销、到期与断链结束对应本地流。

创建结果包含 `deviceId`、`sessionId`、`expiresAt`、`grant` 和 `endpoint`。grant 是短期凭据，调用方仅在会话期间保管。关闭请求为 `{"action":"close","deviceId":"UUID","sessionId":"UUID"}`。

当前会话最长 30 秒，结束后需重新申请。每台设备同时只有一个有效会话，序号严格递增，同一时刻最多等待一个机端回执。账号归属检查发生在申请和关闭时；在途归属变更的主动撤销仍需接入业务事件。

## 开发验证入口

在 `D:\DOSO\robot-cloud` 执行，使用 Rust 1.94 与 protoc。

```powershell
cargo test --locked --offline --workspace -j 1
cargo run --locked --offline -p lab -- control-check
```

`control-check` 在随机本机端口运行真实 HTTP 会话入口、双向 gRPC 中继、机端桥和锁定的 Rust SDK。本地 Motion 服务使用模拟实现，输出含 simulated 标记。该命令验证软件接入和回执，物理动作与停止时限由真机验收记录证明。

由账号 API 获取的新会话可在有效期内保存到受保护的 session.json，再运行以下命令验证 Claim、Hold、Release。文件包含 grant，应按凭据处理。

```powershell
cargo run --locked --offline -p lab -- control D:\private\session.json
```

## 部署配置

云端 Gateway 配置 `DOSO_CONTROL_PORT` 后增加回环 gRPC 监听。HTTPS 代理需支持 HTTP/2，并将 `/doso.remote.v1.RemoteControl/` 转到该监听。既有 Gateway HTTP 服务继续接收受信业务函数的会话创建和关闭请求。

业务函数配置以下值。

```text
DOSO_CONTROL_GATEWAY_URL
DOSO_CONTROL_GATEWAY_KEY
DOSO_CONTROL_PUBLIC_URL
```

机端使用已有设备身份、设备签名密钥、Gateway 公钥与本地云选择，另配置以下值并运行 `doso-cloud-link control`。

```text
DOSO_CONTROL_URL
DOSO_CONTROL_CA                  optional PEM CA path
DOSO_CONTROL_LOCAL_ENDPOINT
DOSO_CONTROL_LOCAL_API_KEY
DOSO_CONTROL_LOCAL_CA            optional PEM CA path
```

状态进程与 control 模式可作为两个 systemd 实例运行，分别承担状态上报和远控传输。两者读取同一用户云选择；关闭选择后退出。机端部署前需先完成正式 SDK Gateway 的 TLS、凭据与设备身份配置。

## 产品验收顺序

| 阶段 | 交付条件 |
| --- | --- |
| 会话与释放 | 本轮软件闭环、跨设备拒绝、撤销、到期、断线及重连回归通过 |
| 正式设备接入 | 研发账号归属、机端正式 SDK Gateway、云端 TLS 中继、ARM64 安装与哈希可核对 |
| 真机持权 | App/Rust 申请、机端持权、释放和状态读回完成，记录实际最终输出确认 |
| 速度与视频 | 50Hz 输入保留机器人签发的原始输入窗口，视频与控制权状态可见，App 前后台行为完成 |
| 弱网与撤权 | 延迟、乱序、蜂窝断网、Wi-Fi/5G 切换、云重启及归属变更的实际停止时间通过验收 |
| OTA | 云任务领取和回传、机端自动执行、真实启动槽、App 进度、成功升级与回滚均有研发机证据 |

远控速度进入正式协议前，需把原始机器人输入窗口传到操作者并原样返回。桥接时重新获取窗口会改变旧输入的有效期，需要在速度实现中保留原语义。

OTA 首包沿用 Brainstem 与配套 policy。详细发布与回滚工作记录在 `D:\DOSO\app-workspace\doso-app\docs\production\DOSO_OTA_IMPLEMENTATION_PLAN.md`。
