# 真实数据接入：成熟项目复用调研

调研日期：2026-11-08。最初调研范围为官方源码、文档、发布记录、许可证及DOSO本地入口。随后已实施ThingsBoard隔离环境与首批投递代码，当前交付状态以 [PROGRESS.md](../PROGRESS.md) 为准，具体构建与验证见 [MIGRATION.md](MIGRATION.md)。

具体构建、数据出口、代码落点与迁移回退见 [MIGRATION.md](MIGRATION.md)。后续源码核查明确：当前业务同步是每两秒的latest快照；平台出口应在业务数据库最终接受之后。ThingsBoard服务端REST已核实历史/看板路径，告警规则触发需独立验证，不能因写入成功就视为告警接通。

## 建议

云端成品平台优先验证 **ThingsBoard** 的历史数据、看板和告警；机器人数据记录优先复用 **MCAP**，需要实时查看时接 **Foxglove SDK**。**Ditto、OpenRemote** 主要参考状态模型和事件处理。首轮只试一个云平台，避免同时维护多套设备状态与账号系统。

DOSO 已有模拟设备经过实际 Web App、业务数据库和隔离 Gateway 的验证，见 [SIMULATED_FLOW.md](SIMULATED_FLOW.md)。接下来要核验真实数据来源与映射，不重复搭建 mock 流程。通用平台可以处理收到的数据，但不能自动提供 Brainstem 尚未暴露的电量、定位或传感器值。

## ThingsBoard 企业采用与可靠性证据（2026-11-08）

| 项目 | 已核查事实 | 来源与边界 |
| --- | --- | --- |
| Schwarz IT / IoT.SCHWARZ | EHI 的2024年 reta 获奖说明明确采用ThingsBoard，覆盖32国、13,000多家超市，连接500,000多台设备 | [EHI获奖项目原文](https://www.reta-europe.com/best-enterprise-solution-2024-schwarz-it/)，独立于软件厂商的颁奖方披露；是2024项目规模，不是今天实时在线数或单机吞吐保证 |
| Wiifor / Toulouse Métropole | 部署方官网明确使用其ThingsBoard应用，项目管理100多个传感器；2023年继续扩展人数统计、空气质量和设备监测 | [Wiifor自有项目说明](https://www.wiifor.com/project/solution-iot-multi-capteurs-pour-collectivite-toulouse-metropole/)，具名现场项目；未公开CE/PE版本、SLA或长期故障数据 |
| ÖBB-Infrastruktur AG | 铁路资产状态和时序记录，使用ThingsBoard Edge、内部网络与云端可视化 | [ThingsBoard案例](https://thingsboard.io/case-studies/obb-infra/)，本轮来源为厂商发布，不能据此声称其铁路安全控制由ThingsBoard承担 |
| T-Mobile Czech Republic | 创新团队用来快速完成PoC、原型和新设备可视化 | [厂商案例与具名客户评价](https://thingsboard.io/case-studies/t-mobile-cz/)，用途明确是原型验证，不当作大规模生产可靠性的证明 |

结论：已有可核查的商业现场采用和持续维护，足以继续作为DOSO云端历史/管理后台的成熟产品选择；这些案例不能替代DOSO具体部署的容量、数据完整性和恢复验收，也不能把不同版本/部署拓扑的结果等同。

本地 [Compose](../cloud/telemetry/deploy/compose.yml) 使用1GiB的单节点与`TB_QUEUE_TYPE=in-memory`，用于接入验证。[官方消息队列建议](https://thingsboard.io/docs/reference/architecture/queue/) 将内存队列定位为开发/演示、生产建议Kafka；内存队列的未处理消息可能在重启时丢失，不等于已写入PostgreSQL的历史全部丢失。当前REST投递路径还需结合数据库写入确认单独验证故障恢复，不能仅凭替换队列宣布端到端可靠。

当前4.3.x在 [官方支持表](https://thingsboard.io/docs/releases/releases-table/) 为Active LTS，4.3.1.8于2026-10-27发布，支持至2027-07-20。正式部署前锁定受支持版本及升级/许可安排，完成预计负载下的持续运行、进程和数据库重启、断网恢复与备份还原。此处是生产采用要求，本次只核查证据，未改变运行配置或增加Kafka。

## 候选比较

| 项目 | 已核查发布基线 | 能省掉什么 | 本次建议 |
| --- | --- | --- | --- |
| [ThingsBoard](https://github.com/thingsboard/thingsboard) | v4.4.0.2，2026-10-30 | 遥测历史、曲线、设备看板、规则与告警 | 云端成品平台首选试验；先接已验证的状态副本，保留 DOSO 账号和设备归属真源 |
| [ThingsBoard IoT Gateway](https://github.com/thingsboard/thingsboard-gateway) | 3.8.6，2026-10-29 | 多协议连接器、字段转换、离线存储 | 当前 Rust 已能读取 gRPC 并发 MQTT，无需再增加 Python 采集层；有实际 CAN/Modbus 等接入需求时再采用 |
| [Eclipse Ditto](https://github.com/eclipse-ditto/ditto) | 3.9.7，2026-09-01 | 设备当前状态、期望设置、订阅和字段级权限 | 状态模型与 MQTT 映射参考；若需要独立状态服务，再作为 ThingsBoard 的替代方案比较 |
| [OpenRemote](https://github.com/openremote/core) | 1.34.0，2026-11-06 | 资产、属性、事件、历史、规则和管理界面 | 重点参考迟到数据处理；整套采用会增加 Manager、Keycloak 等维护面 |
| [Foxglove SDK](https://github.com/foxglove/foxglove-sdk) / [MCAP](https://github.com/foxglove/mcap) | SDK 0.30.3，2026-10-26；MCAP Rust 0.26.0，2026-10-19 | 实时调试、消息 schema、录制、索引和回放 | 可直接复用 Rust 库，作为现有 SDK 的只读消费者，不改变客户 SDK 接口 |

维护判断依据为持续发布和具体源码，不把 star 或下载数等同于生产规模。

## 许可与部署成本

品牌定制补充（2026-11-08）：当前固定CE 4.3.1.8使用Apache-2.0，可修改名称、logo、favicon、样式和菜单后重建；分发时保留许可证、适用版权/NOTICE与修改说明。现成White-Labeling设置属于历史PE能力，不能按官网当前统一版页面推断CE 4.3已有该菜单。[固定版LICENSE](https://github.com/thingsboard/thingsboard/blob/v4.3.1.8/LICENSE)、[CE/PE功能对照](https://thingsboard.io/ce-vs-pe-diff/)、[PE白标说明](https://thingsboard.io/docs/pe/user-guide/white-labeling/)。

当前CE修改入口集中在`ui-ngx`的环境appTitle、index.html、logo资源和menu.models.ts；需要重建前端/镜像，不是给现有Docker增加一个DOSO名称变量即可。首版网页控制台可先小范围品牌定制和菜单精简，复用原设备、历史与规则组件；DOSO App继续沿用现有产品界面。隐藏菜单不替代后端数据权限。

4.4+无偿生产许可要求保留ThingsBoard品牌，完整白标需匹配相应商业授权。另写DOSO前端调用其API也不能未经核对就视为自动规避该版本许可；前端定制和后端许可分别核对。[4.4.0.2 LICENSE](https://github.com/thingsboard/thingsboard/blob/v4.4.0.2/LICENSE)。本次已登录本机原版界面并保留截图，未修改品牌配置或采购授权。

- **ThingsBoard 4.4.0.2 为 BUSL-1.1**。无偿商业生产使用有不超过 100 台设备、单台生产服务器、品牌标识及有效免费许可密钥等条件，不能沿用“最新版 CE 均为 Apache、无限制商用”的旧结论。[固定版本 LICENSE](https://raw.githubusercontent.com/thingsboard/thingsboard/v4.4.0.2/LICENSE)、[2026-09-29 官方说明](https://thingsboard.io/blog/one-thingsboard-source-available/)。
- **ThingsBoard 4.3.1.8 仍为 Apache-2.0**，2026-10-27 发布，官方安全支持到 2027-07-20。可作固定版本评估，但长期产品必须计入升级与维护安排。[LICENSE](https://raw.githubusercontent.com/thingsboard/thingsboard/v4.3.1.8/LICENSE)、[支持表](https://thingsboard.io/docs/releases/releases-table/)。[Gateway 3.8.6](https://raw.githubusercontent.com/thingsboard/thingsboard-gateway/3.8.6/LICENSE) 仍为 Apache-2.0。
- **Ditto 为 EPL-2.0**，独立服务集成与分发修改后的源码需分别评估许可义务；其 [Docker 部署](https://github.com/eclipse-ditto/ditto/blob/3.9.7/deployment/docker/docker-compose.yml) 引入 Java 服务和 MongoDB。[LICENSE](https://github.com/eclipse-ditto/ditto/blob/3.9.7/LICENSE)。
- **OpenRemote 核心为 AGPL-3.0-or-later**，附带第三方组件有各自许可。修改后通过网络提供服务等场景需按条款处理，不能直接视为任意闭源复制；[部署文件](https://github.com/openremote/core/blob/1.34.0/docker-compose.yml) 包含 Manager、PostgreSQL、Keycloak 和代理。[LICENSE](https://github.com/openremote/core/blob/1.34.0/LICENSE.txt)。
- **Foxglove SDK 与 MCAP Rust 库为 MIT**。这不代表 Foxglove 完整客户端或托管服务也适用 MIT。SDK 0.30.3 要求 Rust 1.88，MCAP 0.26.0 要求 Rust 1.81；DOSO SDK 声明 Rust 1.94，本机实测 rustc 1.94，满足这两个最低版本，尚未编译验证依赖组合。[SDK 清单](https://github.com/foxglove/foxglove-sdk/blob/sdk/v0.30.3/Cargo.toml)、[MCAP 清单](https://github.com/foxglove/mcap/blob/releases/rust/v0.26.0/rust/mcap/Cargo.toml)。

## 最值得读和复用的实现

### ThingsBoard：遥测、设备属性、活跃状态

[MQTT 遥测 API](https://thingsboard.io/docs/reference/mqtt-api/telemetry/) 接收带 Unix 毫秒 `ts` 的测量与批量数据；[属性 API](https://thingsboard.io/docs/reference/mqtt-api/attributes/) 分开设备资料和共享配置；[连接状态](https://thingsboard.io/docs/user-guide/connectivity-status/) 区分连接、断连与活动时间。可直接用现成历史、曲线和告警能力。

其原生 topic、认证和 payload 与 DOSO 不同。DOSO 的 `{payload, signature}` 信封不能直接当成普通遥测 JSON。试验转换应位于现有 Rust 验签、设备归属和许可校验之后，不能让第三方连接器绕过这些入口。

[Gateway MQTT connector](https://thingsboard.io/docs/iot-gateway/config/mqtt/) 可参考 topic 到字段的转换；[自定义连接器](https://thingsboard.io/docs/iot-gateway/custom/) 和 [GrpcConnector](https://github.com/thingsboard/thingsboard-gateway/blob/3.8.6/thingsboard_gateway/gateway/grpc_service/grpc_connector.py) 可作扩展示例，但不是能自动理解任意 Brainstem proto 的现成读取器。

### Ditto：期望设置与实际状态

[Feature](https://eclipse.dev/ditto/basic-feature.html) 分开 `properties` 和 `desiredProperties`。适合参考“用户要求开启”和“设备实际执行”如何独立表达；不能因云端保存成功就显示设备已经执行。

官方示例可直接对照：

- [mqtt-quick-introduction](https://github.com/eclipse-ditto/ditto-examples/tree/master/mqtt-quick-introduction)：外部 MQTT broker、消息映射、状态存储与通知。
- [mqtt-bidirectional](https://github.com/eclipse-ditto/ditto-examples/tree/master/mqtt-bidirectional)：双向消息和前端订阅。
- [payload-mapping-testing](https://github.com/eclipse-ditto/ditto-examples/tree/master/payload-mapping-testing)：原始消息、转换代码、预期结果三类夹具，适合验证真实数据映射。

Ditto 可连接 [外部 MQTT 5 broker](https://eclipse.dev/ditto/connectivity-protocol-bindings-mqtt5.html)，无需更换 Mosquitto。其 [历史能力](https://eclipse.dev/ditto/basic-history.html) 用于实体历史，不是保存全部 100 Hz 传感器数据的时序数据库。旧示例只参考流程，版本采用已修复安全问题的 [3.9.7](https://eclipse.dev/ditto/release_notes_397.html) 或后续已核验版本；默认官网文档可能跟随开发分支。

### OpenRemote：旧数据不覆盖当前值

[整体架构](https://docs.openremote.io/docs/architecture/overall-architecture/) 通过带值和时间的属性事件处理数据；[1.34.0 AssetProcessingService](https://github.com/openremote/core/blob/1.34.0/manager/src/main/java/org/openremote/manager/asset/AssetProcessingService.java) 将过时事件与当前值更新分开。迟到数据可以进入历史，不能倒退当前显示。DOSO 已有会话和序号去重，只有扩展历史存储时才需要借鉴这一部分，不能再维护第二套顺序判断。

它也有 [外部 MQTT Agent](https://docs.openremote.io/docs/user-guide/agents-protocols/mqtt/)。支持 MQTT 不代表自动支持 DOSO 签名、许可或机器人字段。

### Foxglove / MCAP：真实样本查看与回放

[Rust SDK](https://github.com/foxglove/foxglove-sdk/tree/sdk/v0.30.3/rust/foxglove) 提供通道、实时服务和文件记录；[MCAP 回放示例](https://github.com/foxglove/foxglove-sdk/blob/sdk/v0.30.3/rust/examples/ws_stream_mcap/src/main.rs) 可参考播放、暂停、时间定位与变速。[标准消息](https://docs.foxglove.dev/docs/sdk/schemas) 已有 JointStates、PointCloud、图像、Odometry 和坐标变换。只在工具边界做必要转换，DOSO 对外 proto 继续由现有协议目录拥有。

MCAP 的 [log_time / publish_time](https://mcap.dev/spec#message-op0x05) 不会自动生成硬件采样时间。原消息的时钟含义、序号、坐标系与单位必须保留，未知的采样时间不能用云接收时间伪装。

## DOSO 的准确接入位置

当前真实代码路径为：

```text
Brainstem GetControlStatus
  → robot/src/grpc.rs
  → robot/src/session.rs、reporting.rs
  → MQTT / mTLS / Mosquitto
  → cloud/gateway/src/reporting.rs（验签、许可、序号和新鲜度）
  → cloud/gateway/src/sync.rs
  → 现有业务后端 / PostgreSQL / DOSO App
```

| 责任 | 现有代码 | 下一步如何复用 |
| --- | --- | --- |
| 云端状态来源 | [robot/src/grpc.rs](../robot/src/grpc.rs) | 已读取本地 `127.0.0.1:13146` 的 gRPC；核对真机安装与来源，不再搭一个模拟读取器 |
| 状态格式 | [shared/src/status.rs](../shared/src/status.rs) | 核心为运行/过渡状态、控制方、电机使能、严重电机故障；后续迁移复核时发现并行新增可选location，保留其原始字段，部署及真机定位另验；电量未进入该格式 |
| 许可、顺序与时间 | [shared/src/reporting.rs](../shared/src/reporting.rs)、[cloud/gateway/src/reporting.rs](../cloud/gateway/src/reporting.rs) | 已有 observedAt、receivedAt、会话、seq 与过期判断；平台适配复用已校验结果 |
| 用户、归属与入库 | [cloud/gateway/src/sync.rs](../cloud/gateway/src/sync.rs) 连接现有业务后端 | 用户与设备归属仍由业务数据库拥有；平台只能保存所需投影 |
| SDK 传感器订阅 | [imu.rs](D:/inovxio/brain/DOSO-SDK/src/observations/imu.rs)、[joints.rs](D:/inovxio/brain/DOSO-SDK/src/observations/joints.rs)、[cloud.rs](D:/inovxio/brain/DOSO-SDK/src/observations/cloud.rs) | 独立工具消费 `watch_imu()`、`watch_joints()`、`watch_point_cloud()` 后写 MCAP；不重写底层采集器 |
| 现有读取示例 | [read_observations.rs](D:/inovxio/brain/DOSO-SDK/examples/read_observations.rs)、[watch_cloud.rs](D:/inovxio/brain/DOSO-SDK/examples/watch_cloud.rs) | 前者有 API key 读取；后者接受保护端点时需要沿用客户端 Config 的认证支持 |
| 视频 | [cloud/media/README.md](../cloud/media/README.md) | 已选用 MediaMTX/WebRTC 候选，继续接真实摄像头，不另写媒体服务器 |

云状态链目前直接调用 Brainstem，并不经过 SDK observations。新增记录工具是独立消费者，不应画成当前 cloud-link 已经读取全部传感器。

当前云状态 `observedAt` 由 `session.rs` 在本次 gRPC 读取完成后取机端时间，代表本次观察时间；它不是底层传感器的硬件采样时刻。`receivedAt` 是 Gateway 接收时间。引入遥测平台时应保留这一差别。

SDK 的 IMU/关节 `source_elapsed` 为源事件映射时的相对时长，不能直接当 Unix 采样时间。点云 watch 是已有显示帧，存在采样、量化、缓存和慢消费者丢旧帧语义；先用现有 `CloudFrame::decode()` 解码 PCLD，再映射到查看器。该记录不能宣传为原始全量激光雷达数据。完整语义见 [observations API](D:/inovxio/brain/DOSO-SDK/docs/api/observations.md)。

## 最小推进顺序与验收

1. **先取得一份真实状态样本。** 沿已授权的本地只读接口核对设备 ID、原始响应、字段来源和读取时间；云状态先维持四类已知数据。缺失项保持未知。安装及完整断云流程仍按 PROGRESS 推进。
2. **只试一个云平台。** 在隔离 ThingsBoard 实例中接收业务数据库最终接受的状态副本，保留观察时间；先验证设备详情、历史查询和看板，告警链单独验收。试验不迁移 DOSO 用户、设备绑定和 App；采用版本与部署方式须满足对应许可。
3. **需要排查高频数据时使用 MCAP。** 复用 SDK 的只读流录下一段真实 IMU/关节样本，保留原消息语义，再用现成读写库与回放示例复现解析问题。云状态最高 1 Hz 与本地传感器 100 Hz 属于不同用途，不默认全量上云。
4. **以实际节省的功能决定采用。** 核验迟到/重复消息不刷新当前值，停报后明确过期，读取失败不伪造正常值，重启可恢复最新状态，关闭许可后无新增上传。分别记录“停止上传”和“机端完全断云”，后者仍需现有产品代码补全。

若现阶段只需四类状态进入现有 App，维持当前 Rust + PostgreSQL 链路成本更低。若很快需要批量设备看板、历史曲线与告警，ThingsBoard 才能替代足够多的自研工作。Foxglove 是工程调试入口，客户继续使用 DOSO App；直接在 App 中嵌入完整第三方查看器属于另一个选型问题。

## 暂不采用与验证边界

- [Zenoh 1.10.2](https://github.com/eclipse-zenoh/zenoh/releases/tag/1.10.2) 适合发布订阅与跨设备路由；当前 MQTT/gRPC 已通，新增总线不能解决缺少真实字段来源的问题。[MQTT bridge](https://github.com/eclipse-zenoh/zenoh-plugin-mqtt) 自身承担 MQTT broker 入口，并非现有 broker 的无成本插件。
- [EMQX 5.9 起的许可变更](https://github.com/emqx/emqx/discussions/15212) 及 [当前 LICENSE](https://github.com/emqx/emqx/blob/master/LICENSE) 需要按商业使用方式核对；当前 Mosquitto 已承担转发，无必要仅为真实数据接入换 broker。
- 这些项目具备完整源码，本次直接分析官方实现；REA 更适合后续有具体已交付二进制、前端包或运行行为需要解释时使用，本次未调用 REA 分析工具。
- 最初调研阶段只新增文档；后续平台及代码实施结果见上述进度与验证入口。尚未做真机验证、容量压测或完整安全审计。版本/许可证按所列证据核对；正式采用仍需对具体集成方式及客户端条款确认。按授权epoch隔离会增加平台逻辑实体数量，不能直接把物理机器人台数当作新版许可的全部设备计数。
