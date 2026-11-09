# Rust 通信架构

文档核对：2026-11-08。本页维护进程与数据归属；完成状态和产品顺序由 [PROGRESS.md](PROGRESS.md) 维护，验证证据由 [TESTING.md](TESTING.md) 汇总。

工程根目录为 `D:\DOSO\robot-cloud`，Cargo workspace 自包含。机器狗云连接、云端Gateway、媒体适配和遥测投递按进程分工，实际部署程度分别见 [PROGRESS.md](PROGRESS.md)。

## 进程与目录

| 目录 / 组件 | 责任与部署位置 |
| --- | --- |
| `robot/` | 机器狗 Linux 主机上的 `doso-cloud-link`，主动连接云端，按请求或有效短期许可只读本地 Brainstem |
| `cloud/gateway/` | 云服务器上的 `doso-cloud-gateway`，ECS 内部 HTTP 服务，发起 MQTT 查询并校验回复 |
| `shared/` | 消息类型、签名、期限、TLS 与状态字段定义，编译进两端 |
| `cloud/broker/` | 云服务器上的 Mosquitto 2.1.x 配置与主题 ACL 模板 |
| `tools/deploy/` | 运维部署时使用的 ECS/机器狗安装脚本、Nginx 模板与历史部署记录 |
| `tools/lab/` | 开发机上的 Rust 模拟服务和联调入口，运行真实云连接服务、Gateway、Broker，模拟只读 Brainstem |
| `cloud/media/` | 云服务器侧Rust视频信令适配候选：对接MediaMTX、业务授权及按需源启停；观看授权已有部署记录，媒体服务仍按候选管理，真机视频待验收 |
| `cloud/telemetry/` | 云服务器侧Rust历史投递：从业务已接受记录领取，复核授权后写ThingsBoard；独立平台及完整链路已本地验证，正式部署待验收 |
| Nginx | ECS 443 终止 HTTPS，转发到 `127.0.0.1:8130`，Gateway 校验服务 Bearer |
| 现有 App / 业务后端 | 独立项目，负责账号、设备归属、用户选择、许可续期与状态入库；11月7日已有实际Web App/数据库配合模拟设备的业务演练 |
| Brainstem | 独立运行时，拥有本地状态与控制真值、运动和硬件安全责任 |

`cloud/gateway/examples/query.rs` 是可信后端 / 开发电脑使用的 Rust HTTPS 查询示例。`lab` 与查询示例均为开发工具，生产部署只交付对应云连接服务或 Gateway 二进制。目标机器使用最终二进制与配置，编译工具链留在开发或构建环境。

目录归类描述源码职责；安装包内部的 `link/`、`gateway/`、`broker/` 布局和已安装的 `/etc/doso/robot-cloud/agent/agent.env` 属于各自交付合同，保持其现有位置。

```text
App ↔ 现有账号/设备后端 ↔ PostgreSQL
               ↑ HTTPS / 服务 apikey，提交状态并取得许可
          Rust Gateway（后台主动同步）
               ↕
         Mosquitto :8883
               ↕ MQTT 5 / mTLS / 狗端主动出站
           云连接服务
               ↓ 只读 gRPC
     Brainstem 127.0.0.1:13146
```

按需查询单独沿用 `业务后端 → HTTPS / 服务 Bearer → ECS Nginx :443 → Gateway 127.0.0.1:8130`。持续状态由 Gateway 主动同步业务接口；两种服务凭据职责不同，均不进入 App 或狗端。

RM500U 提供 IP 网络，拨号和路由由系统管理。云连接服务通过系统路由使用 Wi-Fi 或 5G。

## 状态真源与协议

云连接服务读取 `/brainstem.api.v1.RobotControl/GetControlStatus`。配套 Brainstem 通过 `THUNDER_LOCAL_STATUS_ENABLED=true` 启用本机只读端口，仅注册这个方法；既有 LAN 或 mTLS 控制入口独立保留。该端口适配仍待真机发布。`robot/src/grpc.rs` 按 Brainstem protobuf 字段编号读取快照，三秒超时，字段缺失或未知枚举按上游不可用处理。

| 字段 | 内容 |
| --- | --- |
| `state.kind` | 初始化、落地、站立、行走或过渡状态 |
| `state.transition` | 状态转换类型 |
| `control.owner` | 当前控制方 |
| `control.motorOutputEnabled` | 电机输出标志 |
| `control.criticalMotorFault` | 关键电机故障标志 |
| `transport` | 云连接服务到本地 gRPC 的传输类型 |

云端字段与合法值集中在 `shared/src/status.rs`，新增许可、握手、确认和报告类型集中在 `shared/src/reporting.rs`；没有增加另一套状态枚举。签名信封与 HTTP 行为见 [CONTRACT.md](CONTRACT.md)。Brainstem 版本与现场状态需在目标机验收。

## 狗端生命周期

1. 启动时同时检查安装开关 `DOSO_CLOUD_ON` 和本地持久化选择文件，均开启才加载身份并连接 MQTT。安装开关兼容旧的 `DOSO_REMOTE_ENABLED`，两者同时存在时以新变量为准。本地选择缺失或无效时关闭；可信本地授权写入方仍待实现。
2. 使用本设备证书建立 mTLS 连接，生成新连接编号，订阅自己的查询与许可主题。
3. 查询核验签名、设备 ID、期限与重复编号；许可另外核验连接、授权代次、版本和截止时间。
4. 按需读取需有效且设备确认的许可；有效许可下每秒最多一次周期读取，只有一个本地 future，忙时跳过周期 tick。
5. 签名回复或报告；报告使用严格递增序号，只保留一个未确认的 QoS 1 报告。
6. 许可关闭或到期取消周期读取；本地选择关闭则取消读取并断开云连接。重连丢弃读取与 MQTT 队列，重新取得许可，不补发离线状态。远程关闭触发本地持久化并断云的协议尚未完成。

`robot/src/link.rs` 在重连循环外持有 `Device`，去重记录保留到请求过期。`session.rs` 同时等待 MQTT 事件、读取结果和退出信号，读取 future 属于本次会话。

单次建连最多五秒。网络、DNS、握手或服务暂不可用时，以 2、4、8、16、30 秒的基数加少量抖动重试，实际等待最多三十秒。订阅成功并稳定在线六十秒后，退避回到起点。重试等待可被退出信号打断。

缺失配置、密钥、明确的本地 TLS 材料错误或身份 / 主题权限被拒绝时，云连接服务记录原因并退出 78，systemd 对此等待人工修正。暂时的 TLS 握手或证书验证失败仍走退避。

环境变量在启动时读取。更新配置后重启云连接服务生效，持久化选择文件在运行中继续检测。关闭选择会退出云连接进程；再次开启需要可信本地入口同时保存选择并启动服务，这段接线仍待实现。既有Brainstem与CAN服务保持各自生命周期，云连接仅使用状态查询接口。

## 网关生命周期与权限

Gateway 完成回复主题订阅后接受查询。会话持有在线状态及待完成查询，断线丢弃会话，等待者通过通道关闭收到不可用结果。HTTP 请求取消后，查询 guard 清理对应待处理项。

一次查询最多等待八秒，最多同时处理十六次查询。Gateway 在发起查询和收到回复时重读设备登记与公钥，校验设备开关、签名、请求编号和期限。

- 客户端通过服务器 CA 与证书验证 HTTPS，Gateway 校验独立服务 Bearer。
- MQTT 使用客户端证书，证书 CN 绑定连接身份，ACL 限制设备自己的状态主题。
- 网关服务凭据只交给可信后端。狗端保存本设备私钥和网关公钥。
- 云连接服务配置和网关 `enabled` 属于部署接入开关，客户许可须由现有业务后端完成账号、设备归属与用户许可核对。
- 有效证书但缺少 ACL 的客户端仍可能建立 MQTT 连接，主题读写会被拒绝。登记关闭会阻止查询，已建立连接的撤权、动态踢下线和完整证书吊销流程待实现。

当前接口范围为状态查询，运动与安全仍走既有本地链路，旧命令 worker 保持关闭。

## 持续状态与用户许可

业务后端拥有用户选择、设备归属、授权代次和许可版本。Gateway 的 `reporting.json` 只保存已接收决定的版本下限和开关，不保存用户账号，也不在重启后自动恢复采集。许可须由可信后端显式发起和续期，最长 30 秒；新决定先原子保存，设备签名确认另行记录。关闭立即拒收报告和按需查询结果，设备不可达时仍显示待确认。

`robot/src/reporting.rs` 拥有许可、单调截止时间和序号；`session.rs` 拥有单个读取与发布确认。`cloud/gateway/src/reporting.rs` 拥有许可投影、连接最近可见时间、最新报告及最后成功报告。读取失败保留成功报告的旧时间，`fresh` 只表示当前会话内五秒以内的成功采样；`reachable` 表示十五秒内有可验证消息，不代表物理上电判断。

`GET /status/latest` 只读取缓存。`cloud/gateway/src/sync.rs` 每两秒批量向业务接口提交最新投影，并应用业务端返回的许可；同值已确认时不重复发送。业务数据库在事务内复核归属、epoch、version、session及序号；用户关闭提交后的拒收不依赖 Gateway 通知成功。App 许可保存和显示由业务侧负责，用户不维护短期租期或技术编号。

## 状态历史投递

业务库在接受新latest的同一事务中写入私有待投递表，逐设备出口默认关闭。独立 `doso-cloud-telemetry` 通过专用服务鉴权领取完整报告，再经ThingsBoard服务端REST保存历史；平台网络请求不进入许可同步事务。现有App实时状态、用户与设备归属继续由DOSO业务库拥有。

新历史是每两秒同步实际接受的快照，不保证每个机端报告。映射按设备UUID和授权epoch隔离，外发前复核当前机主、epoch、许可版本、用户选择与账号有效性；原采集租约到期不抹掉已接受历史。已开启的第三方HTTP可能在关闭后完成，停止新采集、取消待发送与在途完成分别处理。具体实现、部署和容量边界见 [telemetry](cloud/telemetry/README.md) 及 [合同](CONTRACT.md)。

## 开发与文件边界

本地模拟由 `cargo run --locked -p lab -- check` 或 `local` 启动。公网模拟用 `public <link.json>` 启动本机云连接服务和模拟 Brainstem，HTTPS 请求由 Rust 查询示例执行。配置方法见 [TESTING.md](TESTING.md)。

| 位置 | 内容 |
| --- | --- |
| 工程 `.runtime/` | 每次联调的临时证书、日志和客户端配置，按私有临时文件处理 |
| 工程 `target/` 或 `CARGO_TARGET_DIR` | Cargo 构建输出 |
| `D:\DOSO\private\robot-cloud\cloud-stage` | 受限 ACL 的私有凭据和原始部署资料 |
| `D:\DOSO\artifacts\robot-cloud\linux` | 迁移前已构建的 Linux 发布包 |
| `D:\DOSO\toolchains\robot-cloud` | Zig 与 cargo-zigbuild 构建工具 |
| `D:\DOSO\toolchains\mosquitto-2.1.2` | Windows 本地 Broker 与 ACL 插件 |
| `D:\DOSO\archive\robot-cloud\20261103` | 迁移时保留的历史实现与资料 |

源码、测试入口和发布包各自独立管理。历史公网模拟见 [tools/deploy/RESULT.md](tools/deploy/RESULT.md)，实际Web App/真实数据库配合模拟设备的流程见 [SIMULATED_FLOW.md](docs/SIMULATED_FLOW.md)。真实机器狗、正式手机/G20、独立公网MQTT、5G及完整断云/本地重新授权仍待验收。Linux构建和现有产物来源见 [BUILD.md](BUILD.md)。
