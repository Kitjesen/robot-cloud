# 机器狗云连接

文档核对：2026-11-13。功能状态和下一阶段统一见 [PROGRESS.md](PROGRESS.md)，测试范围见 [TESTING.md](TESTING.md)；本轮真实交付证据见 [docs/DELIVERY_STATUS_20261113.md](docs/DELIVERY_STATUS_20261113.md)。核对日期不代表重新部署或真机验收。

独立 Rust 工程位于 `D:\DOSO\robot-cloud`，源码按运行位置组织：`robot/` 运行在机器狗 Linux 主机，`cloud/` 部署到云服务器，`shared/` 编译进两端，`tools/` 放开发联调与部署工具。

```text
robot-cloud/
├── robot/                机器狗端：doso-cloud-link
├── cloud/                云服务器端
│   ├── gateway/          doso-cloud-gateway：状态查询、许可与业务同步
│   ├── broker/           Mosquitto：MQTT配置与主题ACL模板
│   ├── media/            doso-cloud-media：视频信令与观看会话候选
│   ├── telemetry/        doso-cloud-telemetry：已授权状态历史投递、ThingsBoard隔离部署
│   └── console/          DOSO管理后台：ThingsBoard定制前端、构建与独立部署
├── shared/               两端共用的协议、签名与状态定义
├── tools/                开发或运维时使用
│   ├── lab/              本地模拟与通信联调
│   └── deploy/           安装脚本、部署准备与历史记录
├── Cargo.toml            统一构建入口
└── Cargo.lock            统一依赖锁定
```

`robot/` 的程序名仍为 `doso-cloud-link`；`cloud/gateway/` 的程序名仍为 `doso-cloud-gateway`。媒体服务是视频信令适配候选，部署和真机观看验收范围见其说明。根目录保留的 `agent/.omx/` 仅为历史工具元数据，没有运行程序。

命名、抽象和职责标准见 [AGENTS.md](AGENTS.md)，用户操作与后台自动行为的边界见 [CONTRACT.md](CONTRACT.md)，当前进度和后续步骤见 [PROGRESS.md](PROGRESS.md)。

## 主流程

```text
DOSO App ↔ 账号与设备后端 ↔ PostgreSQL
                  ↑ HTTPS / 服务 apikey，提交状态并取得许可
             Rust Gateway（后台主动同步）
                  ↕ MQTT / mTLS
               Mosquitto
                  ↕ 狗端主动出站
               云连接服务
                  ↓ 只读 gRPC
               Brainstem
```

产品目标是：机主在本地开启一次云连接，离开现场后仍可查看状态；主动关闭后机器狗保存关闭并断云，重启保持关闭，再次开启通过本地授权完成。已有App远程状态开关、云端许可保存和后台续期；机器狗本地可信开启、服务唤起及远程持久化断云仍待实现。本地SDK使用独立于云端开关。

2026-11-07已用已登记模拟设备跑通实际DOSO Web App、真实业务数据库和ECS隔离Gateway，验证绑定API、开启、退出页面持续更新、隧道断网恢复与关闭停采。设备来源为模拟Brainstem，MQTT通过本机和SSH隧道接入；未验证真实机器狗、手机扫码、独立公网MQTT或5G。关闭停采时连接仍可保持，不能据此宣布完全断云。[模拟设备业务流程](docs/SIMULATED_FLOW.md)

有效查询触发 `GetControlStatus`，云连接服务在三秒期限内读取本地 `127.0.0.1:13146`，签名返回状态、控制方和电机标志。配套 Brainstem 本机只读入口仍为待发布候选。持续状态通过最长 30 秒、需显式续期的采集许可开启；狗端确认后每秒最多一次周期采样。两种读取共用同一个本地请求，Wi-Fi 与 5G 共用该程序。当前源码额外要求本地持久化选择开启；可信授权写入及远程关闭后断云仍待实现，不能手工写文件冒充产品授权。

| 可信后端接口 | 当前源码行为 |
| --- | --- |
| `POST /api/v1/robots/{id}/status` | 按需查询，需设备确认的有效用户许可；线上版本须按部署记录核对 |
| `PUT /api/v1/robots/{id}/reporting` | 设置带版本的短期许可；202 是受理，设备确认另查 |
| `GET /api/v1/robots/{id}/status/latest` | 读取最近连接、许可确认、最新报告和最后成功状态；不触发采样 |

Gateway后台自动同步在2026-11-04有部署记录，11月7日模拟设备业务流程验证了真实业务入库与后台续期。当前线上运行状态未在本次文档核对中重新查询，真实设备的自动同步仍待验收。用户许可和入库事务由业务数据库决定，Gateway的文件登记开关不是客户许可。消息格式以 [CONTRACT.md](CONTRACT.md) 为准。

2026-11-03的公网模拟记录、11月7日目录重组验证和模拟设备Web流程分别保留其证据范围。开发验证、部署记录和真机验收按 [TESTING.md](TESTING.md) 查阅。

## 读代码入口

GPS地图、实时跟随、日期轨迹和摘要卡见 [TRACKS.md](docs/TRACKS.md)，其中列出App、历史接口、Rust投递程序各自的源码入口与本轮部署范围。

1. [robot/src/link.rs](robot/src/link.rs) 编排狗端连接、会话与重试，状态读取见同目录 `grpc.rs`。
2. [cloud/gateway/src/lib.rs](cloud/gateway/src/lib.rs) 管理查询、MQTT 会话、设备登记与回复校验，HTTP 入口见同目录 `http.rs`。
3. [shared/src/lib.rs](shared/src/lib.rs) 定义签名消息与 TLS 配置，状态字段见同目录 `status.rs`。
4. [cloud/gateway/src/sync.rs](cloud/gateway/src/sync.rs) 在后台自动同步业务许可和最新报告，用户无需处理续期或重连。
5. [cloud/media/README.md](cloud/media/README.md) 说明视频信令及按需启停候选；业务观看授权有11月6日部署记录，媒体服务、正式App和真机视频仍待对应交付验收。
6. [cloud/telemetry/README.md](cloud/telemetry/README.md) 说明新增独立Rust历史投递程序、业务增量迁移、ThingsBoard隔离环境和回退。11月8日实际平台与完整投递链已本地验证，数据源明确为模拟设备，正式云端和真机仍待部署验收。
7. [cloud/console/README.md](cloud/console/README.md) 说明DOSO管理后台的源码补丁、品牌与中文导航、构建和本机独立入口。11月9日生产前端编译和本机独立部署通过，入口为 `http://127.0.0.1:18081`。HTTP/API/WebSocket定向检查27项及187个静态资源通过，浏览器显示与交互按用户决定暂缓，详见测试记录。

## 启动与测试

在工程根目录运行。开发需要 Rust、Mosquitto 2.1.x 及 OpenSSL，Windows 工具路径见 [TESTING.md](TESTING.md)。

```powershell
cargo build --locked -p doso-cloud-link -p doso-cloud-gateway
cargo test --workspace --locked
cargo run --locked -p lab -- check
cargo run --locked -p lab -- local
```

`check` 检查按需查询及持续状态的完整模拟流程后退出，`local` 保持模拟环境运行供手工请求。两者运行真正的云连接服务、Gateway 和 Broker，只模拟 Brainstem。未授予采集许可时不会自动采样，关闭或到期后停报。生产配置见 [robot/env.example](robot/env.example) 和 [cloud/gateway/env.example](cloud/gateway/env.example)。

| 文档 | 内容 |
| --- | --- |
| [ARCHITECTURE.md](ARCHITECTURE.md) | 进程边界、状态真源、权限与生命周期 |
| [CONTRACT.md](CONTRACT.md) | HTTP / MQTT 协议与签名格式 |
| [PROGRESS.md](PROGRESS.md) | 当前完成程度、产品目标与下一阶段顺序 |
| [TESTING.md](TESTING.md) | Rust 本地模拟、公网联调与验证范围 |
| [docs/SIMULATED_FLOW.md](docs/SIMULATED_FLOW.md) | 实际Web App与云端数据库上的模拟设备演练证据 |
| [docs/REUSE.md](docs/REUSE.md) | 真实数据接入的成熟项目选型、许可证、源码参考与最小验证步骤 |
| [docs/MIGRATION.md](docs/MIGRATION.md) | 保留现有业务链的遥测平台构建、代码落点、数据迁移与回退方案 |
| [BUILD.md](BUILD.md) | Linux 构建、现有包及兼容基线 |
| [cloud/console/README.md](cloud/console/README.md) | DOSO管理后台的前端源码、编译、启动与回退 |
| [tools/deploy/README.md](tools/deploy/README.md) | ECS 部署资料与凭据边界 |
| [tools/deploy/LINK.md](tools/deploy/LINK.md) | 狗端离线安装包、设备登记和开机后首次真实查询 |
