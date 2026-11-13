# 接入测试

更新：2026-11-14。本页汇总实际验证和可复现入口；每节按其当次源码、部署与硬件范围解释。当前功能状态及产品顺序见 [PROGRESS.md](PROGRESS.md)，本轮真实机器狗交付记录见 [docs/DELIVERY_STATUS_20261113.md](docs/DELIVERY_STATUS_20261113.md)。

## 本轮回归（2026-11-14）

- DOSO Rust SDK：124 项通过。
- robot-cloud workspace：84 项通过、1 项 ignored；无失败。
- `cargo fmt --all -- --check` 和 workspace Clippy `-D warnings` 通过。
- `lab check` 和 `lab auth-check` 通过；两者使用本机模拟 Brainstem/Mosquitto，不是真机证据。
- 真实设备 TLS 到公网 MQTT 的历史探测已通过；云端登记随后已由 ECS 云助手启用为 `enabled=true`，ECS SSH 入口仍未开放。真实机器狗当前不可达，因此尚未完成 MQTT `hello/status` 验收。[启用证据](D:/DOSO/artifacts/robot-cloud/real-connect-20261113/cloud-assistant-enable-20261114.json)
- 使用 Rust Gateway 查询示例做了一次只读查询，HTTP `503`、错误码 `unavailable`；端点可达但没有当前机器狗状态回复，不能解释为登记失败。[查询证据](D:/DOSO/artifacts/robot-cloud/real-connect-20261113/gateway-query-20261114.json)
- 通过 `GET /status/latest` 做了第二次只读投影检查，同样返回 HTTP `503` 和 `unavailable`；这确认当前没有可供 App 读取的实时状态投影，不改变云端登记结果。[latest 证据](D:/DOSO/artifacts/robot-cloud/real-connect-20261113/gateway-latest-20261114.json)
- 云端登记 patch 生成器 3 项通过；Gateway 公钥路径越界回归通过。生成器不连接 ECS、不修改输入文件。
- 远程关闭闭环通过：设备收到签名 `enabled=false` 后原子写入 `choice=false`、停止采样、退出 cloud-link；重启保持 `cloud_off`；本地 `choice=true` 配合新许可后恢复上报。证据：[local-remote-close-check.log](D:/DOSO/artifacts/robot-cloud/real-connect-20261113/local-remote-close-check.log)
- 最新本地 Rust 全工作区回归：`cargo test --workspace --locked --offline -j 1` 共 94 项通过、1 项 ignored、0 项失败；`cargo fmt --all -- --check` 通过。该命令包含当前未提交的 OTA workspace 变更，不能当作已部署或真机证据。

2026-11-14 现场复核：`192.168.66.64` ping、TCP 22 和 SSH 均超时，未到认证阶段；没有执行机端授权写入、服务启停、CAN 配置或运动控制。该失败原因是机器狗当前网络不可达，不是云端登记命令失败。最近一次可达证据仍是 flowfix11 的 `13145`/`13146` 只读读取及 authorizer 部署记录。

本轮在 2026-11-13 20:57 CST 复核了同一工作区：当时 `cargo test --workspace` 为 82 项通过、1 项 ignored，`cargo fmt`、workspace Clippy、`lab check`（8 次模拟读取）和 `lab auth-check`（9 项 MQTT ACL 检查）均通过。之后新增 Gateway 路径回归和远程关闭持久化回归，当前总数为 84 项通过、1 项 ignored。`lab` 使用 `D:/DOSO/toolchains/mosquitto-2.1.2` 的本机 Mosquitto；没有启动真机服务、没有访问 `13146`，也没有修改 CAN 或云端配置。[复核清单](D:/DOSO/artifacts/robot-cloud/real-connect-20261113/offline-regression-20261113T2058+0800.json)

随后 21:29 CST 只读现场复核显示 Brainstem active、13145 可达、四路 CAN 均 UP/ERROR-ACTIVE/1 Mbps、IMU 存在；`field-preflight.sh` 仍因 `can0` 的 USB parent 与既有映射脚本不一致而退出 1。没有启停服务、写 CAN 或访问 13146。

同一现场窗口通过 `192.168.66.64:13145` 完成六个显式只读 RPC，其中五个成功：CMS ZERO、输出关闭、控制方 NONE、关键故障 false、16/16 电机 online、无状态码/错误、profile 为 `all_elbow`；`GetVoltage` 按源码核对条件跳过。该结果只证明现有主端口读状态正常，不替代 13146 验收或物理禁用确认。[原件](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/current-read-api-20261112-native-owner.json)

## 当前 flowfix11 的 13146 与 cloud-link 候选（2026-11-13）

现场最新只读 preflight 通过：`thunder-can.service` active/exited，四路 CAN 均 UP/ERROR-ACTIVE/1 Mbps，`127.0.0.1:13146` 监听。[原件](D:/DOSO/artifacts/robot-cloud/real-connect-20261113/field-preflight-result.json)

通过机器狗上的 staging Dart 客户端调用 `127.0.0.1:13146/GetControlStatus` 成功，读回 CMS GROUNDED、`motorOutputEnabled=false`、owner NONE、critical fault false、lease inactive；没有调用控制 RPC。[原件](D:/DOSO/artifacts/robot-cloud/real-connect-20261113/read-current-13146.json)

新的 ARM64 `doso-cloud-link` 已安装但保持关闭：旧二进制哈希 `d9fe0504...b826d85` 已备份，新二进制哈希 `15a1603f...d7bea76` 与候选一致；以服务用户运行 `DOSO_CLOUD_ON=false` 输出 `cloud_off`，服务未启动、云开关未打开、没有运动命令。[原件](D:/DOSO/artifacts/robot-cloud/real-connect-20261113/cloud-link-install.json)

## G1 关闭竞态候选（2026-11-13）

定位到上一候选的具体竞态：关闭流程在 `Control disposed` 等迟到的输出撤销回调中再次启动 `joint.disable()`，而主关闭流程已经进入资源释放，导致物理禁用确认日志之后仍可能卡在 systemd 停止超时。修复在运行时开始关闭时设置一次性关闭闸门，并移除重复的关闭前 `invalidate`；关闭期间的迟到回调不再启动新的禁用操作，主关闭流程仍先等待现有物理禁用确认。

目标机候选目录 `/home/sunrise/deploy_staging/doso_sdk_stream_close_flowfix10_20261112T143317Z` 已重新构建，ARM64 二进制 SHA-256 为 `7c3a5c044e0335a06878f34e0b19af7e43c73f6b4b1211aaad21241e38634992`。目标机验证结果：只读流/本地状态 9 项通过，native close 定向测试 1 项通过；`localModelAndConfigChecksPassed=true`、`deviceConnections=0`、`enableCommandsSent=false`、`hardwareReady=false`。候选已备份原文件，但 `activation_performed=false`、`service_changed=false`，尚未安装或启动。完整证据：[shutdown-fix-build.json](D:/DOSO/artifacts/robot-cloud/real-connect-20261113/shutdown-fix-build.json)。

这轮只完成候选构建和离线/注入测试，不能替代真机启动、`13146` 监听、真实 `GetControlStatus` 或停止回滚验收；下一步是一次受控维护窗口。

现场前置工具 `tools/deploy/field-preflight.sh` 已加入部署目录并通过 `bash -n`；它只读取服务、端口、CAN、IMU，并调用 `thunder-can-up.py --check`，不会修改 CAN 或重启服务。

12:34 CST 已在真机实际运行该 preflight：它在只读映射检查处拒绝继续，发现脚本要求 `can0→USB 1-2`，实际 `can0` 位于 `1-1.1`，同时 `can1` 仍 DOWN；没有执行 `ip link set` 或服务动作。[原始结果](D:/DOSO/artifacts/robot-cloud/real-connect-20261113/field-preflight-result.json)

当前现场基线的只读对照：`flowfix10_hub_20261113`、PID 12818、`13145` active。10 秒原始双流窗口完成，IMU 997 帧/99.392 Hz，关节 15,943 条合计事件，最大接收间隔 78 ms；这证明当前主端口短窗口可读，不证明 `13146` 已部署。[原始读取](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/raw-trace-10s-20261113T102350Z.json) · [现场快照](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/robot-102012.json)

## G1 现场窗口结果（2026-11-13 18:50 CST）

现场窗口在候选启动前中止。停止旧 `flowfix10_hub_20261113` 时，`usbbus2` 持续返回 `errno=6`，内核记录 `can1 removed`、USB `error -71` 与设备枚举失败；服务随后因 16 个电机没有新鲜禁用反馈而进入 failed。候选没有启动，`13146` 没有监听，候选 drop-in 已移除，云连接保持 inactive。

这次结果不能归因于新候选，也不能把“停止命令已发送”写成 G1 通过。恢复 PCAN/CAN 物理链路和反馈后，才允许再次维护；不通过拉长超时、跳过禁用确认或修改 CAN 配置来绕过。[现场失败记录](D:/DOSO/artifacts/robot-cloud/real-connect-20261113/g1-field-failure.md) · [失败快照](D:/DOSO/artifacts/robot-cloud/real-connect-20261113/failure-inspection.json)

## 最新GitHub上传与本地验证（2026-11-12）

用户授权后，DOSO-SDK已创建并推送 `rust` 分支，最新提交为 `ebc22ed`；GitHub API已确认 `examples/navigate.rs` 在该分支可读取。提交内容包含Rust七域SDK、协议、文档、示例、Android和CI，C++保留于 `brainstem/`、旧Python保留于 `archive/`。新增可运行导航CLI，执行“提交目标 → 收到任务ID → 等待结果”，最新 Rust 回归为124项通过。

发布前远程CI的旧提交记录继续保留；当前提交的本地验证为 124 项测试通过，工作区 clean。源码已上传 `rust` 分支，不代表main已更新、已创建tag/release、机器人部署已改变、导航真机运动验收或七域现场全部验收；下文历史运行和失败证据继续保留各自范围。

## 最新状态检查与60秒同时读取（2026-11-12 22:55–22:57）

只读快照确认brainstem.service为flowfix10、PID10677 active/running，启动22:38:51；13145监听、13146/正式5052无监听，bodyheight-ctrl、lt-host、cloud-link均inactive，云开关false且真实UUID保留。四CAN均UP、1Mbps、berr tx/rx=0，近10分钟严重错误筛选无匹配，不将筛选无条目解释为绝无未记录问题。本轮未修改服务、CAN或运动配置。[快照](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/current-check-20261112T145556Z.json)

SDK/raw约22:56:49同时启动60秒，raw_concurrent_with_sdk=true，二者均exit0；观察桥结束清理与两条测量进程结果分开。SDK60.001253秒completed=true，IMU6,001条/100.015Hz、最大gap104.293ms、源跨度59.996539秒；关节95,946事件，16轴99.931–99.965Hz、最大逐轴gap108.582ms、源跨度59.999781秒。raw completed=true，两流continuous/passed=true、总passed=true，IMU99.958Hz、最大逐轴gap110ms。该60秒窗口无2秒消费超时，不能外推15分钟稳定、严格实时或候选部署已完成；旧GOAWAY、停机、缺轴、消费超时全部保留。SystemInfo三字段仍None，只是电脑观察桥，真实机端型号/固件和正式5052尚未接通。[起止/退出](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/current-concurrent-20261112T145643Z.json) · [SDK原件](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/current-sdk-20261112T145643Z.log) · [raw原件](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/raw-bdp-on-60s-20261112T145650Z.json)

**发布前快照：** 同轮GitHub核对时，远端main及codex分支仍为旧C++提交 `0ae4969c`，无tags/releases；Rust七域、Cargo/proto/docs主要新文件当时仍本地未跟踪、未提交未上传。该检查轮次用户只要求检查与上传状态确认，当时没有commit/push、发布或运行远程CI，功能与历史测试不能写成当时远端已包含这些代码。后续已授权推送 `rust` 分支，最新上传与CI状态见本页首节。

## GOAWAY 定位、候选构建与现场并发复测（2026-11-12）

**最新现场与操作边界：** 22:24:38只读快照已是新boot `1f141a27ab8b4388bee8c55258c61601`，brainstem.service active、PID1647、flowfix10，13145监听，bodyheight-ctrl inactive；13146/正式5052无监听，Host/正式SDK服务未部署。systemd记录ExecMainStartTimestamp=22:15:07，ps lstart显示22:18:26，两种时间口径分开保留，不合并成同一准确启动时刻；后续证据足以说明22:18:41测试窗口前已运行flowfix10。22:16的bodyheight/PID83525属于更早快照，不能给后续测试标版本。用户要求不重启、无需静置，本任务没有重启、切换程序或激活候选，只做只读检查。21:55无监听时未执行的6项API仍不算通过。[最新服务/端口/boot](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/failure-142438.json) · [此前服务切换](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/failure-141609.json) · [21:43真实状态](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/control-flowfix10.json)

**后续22:43短窗口：** 同时SDK/raw 10秒均退出0，所有16轴重新有样本，详细结果见本节末。该窗口的source_elapsed与前面窗口不构成同一已证明的连续生命周期，现场服务/boot已有变化，故不绑定旧PID1647或某固件唯一身份。22:24服务快照仅描述当时，不冒充22:43的新身份核验；短窗口恢复也不证明长稳通过。

增强探针继续保留2秒消费阈值、非零失败和不重连。旧200.319451秒记录只证明消费超时，本地暂停消费者可复现同类报错，不证明当时上游停流。后续实际RPC结果另列，不把不同错误合成一个根因：

| 实际运行 | 结果与边界 | 原件 |
| --- | --- | --- |
| Rust SDK计划900秒 | 316.970859秒、completed=false、Unavailable/GOAWAY error 11；不是消费watchdog错误 | [日志](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/rust-both-900s.log) · [运行记录](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/sdk-900-run.json) |
| 原始直连计划439秒 | 两流约320.52秒Unavailable/GOAWAY 11；20:25:48开始，晚于SDK20:23:25退出，无重合，rawConcurrent=false | [原始源](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/raw-trace-439s-20261112T122548Z.json) |
| 显式禁HTTP代理、BDP on，420秒 | 两流约314.29秒Unavailable/GOAWAY 11，说明禁用代理后仍复现；不能仅由全局HTTP_PROXY或旧日志peer=7890断言代理生成了GOAWAY | [BDP on](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/raw-bdp-on-420s-20261112T125602Z.json) |
| 同样禁代理、BDP off，420秒 | 只交付约167.26/167.27秒源跨度，随后等到420秒deadline；completed=true仅代表窗口截止，不是持续交付成功 | [BDP off](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/raw-bdp-off-420s-20261112T125559Z.json) |

**实际依赖的本地定位：** 下载的grpc 5.2.0/http2 2.3.1复现keepalive比较方向/相邻计时未重置，以及服务端响应DATA未通知activity的问题。候选仅修这两处，保留默认五分钟最小间隔、maxBadPings=2及第三次违规终止规则、正常BDP和原版本/依赖。实际Dart Server＋C-core客户端＋透明字节观察器的loopback中，只将最小间隔缩短至1秒加速检查：原包2.703秒GOAWAY 11，timer-only仍0.718秒早退，完整候选20.078秒到预定deadline、24个客户端PING且无GOAWAY，消息序列连续。4项受控时钟自检及定向分析通过。这是实际库的本地通信证据，不能替代ARM64现场长测，也不解释旧200秒消费超时。[候选说明](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/rpc-compat-candidate/README.md) · [线上帧与结果](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/rpc-compat-candidate/stream-results.json)

**独立ARM64候选：** flowfix8与随后保留现场改动的flowfix10各自克隆独立staging；两份候选均完成ARM64构建、5项只读loopback/mock测试，grpc仍5.2.0、http2仍2.3.1，只有候选本地path覆盖，锁定的无关版本未漂移。预检deviceConnections=0、enableCommandsSent=false、hardwareReady=false，不作为硬件就绪或运动通过。flowfix10候选为 `/home/sunrise/deploy_staging/doso_sdk_read_flowfix10_20261112T134521Z`；activation_performed=false，两份均未激活。[flowfix8构建](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/flowfix8-build/candidate-build.json) · [flowfix10构建](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/candidate-build.json) · [5项测试](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/candidate-read-tests.log) · [预检](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/candidate-preflight.json) · [锁定检查](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/candidate-lock-check.log)

候选回环只读服务复用同一native runtime的GetControlStatus/ListenImu/ListenJoint，不新增状态权威或运动RPC；独立取消和关闭均有实际库loopback验证，mock不冒充真实传感器。LingTu `services/lifecycle.py`另修Host deferred模式只延迟HTTP、不漏启gRPC的问题，本地15项通过，尚未部署；不能据源码修复声称正式5052已监听。

**可证实的修改归属：** 本次正式Python启动修复为lifecycle.py中gRPC启动不再受HTTP defer开关阻止；不将共享host.py中其他人的差异归入本次。stage_runtime/build_candidate只复制、修改和构建独立staging，未覆盖活动程序、服务定义或启动配置；measure_current只启动/清理电脑自己创建的观察桥、raw和SDK进程，没有远端systemctl或kill操作。此前真实UUID写入和密钥准备是已单独记录的机端变更，不能笼统说机器上没有改过任何文件。针对实际server.dispose结束读流的stream-close修复仅在独立候选验证通过，未改运行服务。

**第三轮stream-close候选验证：** 新staging为 `/home/sunrise/deploy_staging/doso_sdk_stream_close_flowfix10_20261112T143317Z`。首轮两个测试文件并行共享固定13146端口，8项通过、1项端口冲突失败；第二轮串行暴露path grpc包中6个未复制example workspace声明的解析错误。只从候选fork pubspec移除这6个示例workspace声明，包版本和依赖不变；最终串行9项响应/本地只读测试、1项注入native关闭测试、ARM64构建均通过，预检deviceConnections=0、enableCommandsSent=false、hardwareReady=false，activation_performed=false。前两轮失败没有重写为成功，也不将mock/native注入验证当实际电机或现场停止修复通过。[最终构建](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/stream-build.json) · [最终9项](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/candidate-stream-tests.log) · [预检](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/stream-preflight.json) · [首轮记录](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/stream-build-first.json) · [首轮日志](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/candidate-stream-tests-first.log) · [第二轮](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/stream-build-second.json) · [第二轮日志](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/candidate-stream-tests-second.log)

**flowfix10同时SDK/raw 900秒复测均未通过：** 两者分别于13:51:39.408893/13:51:39.382380 UTC启动，raw_concurrent_with_sdk=true，测试进程禁代理、raw保持BDP on。SDK于13:52:05退出1，消费窗口25.758167秒、IMU/各轴约2秒消费超时、两流no_sample_ready；raw于13:52:23退出1，IMU42.453秒/关节42.437秒Unavailable/GOAWAY 0，源跨度只22.263087/22.273894秒。两边停止交付时段相近，此次未完成900秒，不把no_sample_ready单独当上游停发证明，也不把GOAWAY0并入此前GOAWAY11。[起止记录](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/current-concurrent-20261112T135131Z.json) · [SDK](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/current-sdk-20261112T135131Z.log) · [raw](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/raw-bdp-on-900s-20261112T135141Z.json)

21:55快照只提供旧进程终态；22:16补充的stop_window现已确认PID78009于21:52:02收到systemd Stopping，两次记录16轴Motor disable confirmed；配置TimeoutStopUSec=20s，21:52:22停止超时后被systemd SIGKILL、Result=timeout，与raw21:52:23退出吻合。同秒systemd尝试启动bodyheight-ctrl，其当前Conflicts包含brainstem.service，时序与互斥服务切换吻合；sudo操作者查询无匹配记录，不能指认触发者，也不能把无匹配当作无人操作的证明。21:14 CAN stale属于旧PID70959，不是本次PID78009，不据此归因于SDK或keepalive。用户不重启限制继续有效。

**现有endpoint的60秒同时SDK/raw复测仍未通过：** 两者22:18:41启动，服务端按后续新boot证据为flowfix10/PID1647，不沿用22:16的bodyheight标记。SDK45.604276秒退出1，触发joint[9]消费idle=2001.138ms；8/9/10/11轴均约2秒未消费，其他轴/流仍在更新。失败后joints=sample_ready只表明关节事件流可读，不证明缺轴恢复：诊断未携带该样本轴ID，其余轴仍有新事件。[运行起止](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/current-concurrent-20261112T141836Z.json) · [SDK诊断](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/current-sdk-20261112T141836Z.log)

原始直连IMU 5,997条/60.031秒、99.898Hz；关节8/9/10/11的最后源时间仅56.756–56.763秒，在约43秒bucket后不再出现，尾部间隔分别约17.062/17.062/17.062/17.047秒。其他12轴继续约99.8Hz。raw原件completed=true、退出0只代表等到窗口deadline，不是逐轴连续性通过，也不能用总体关节事件频率遮蔽4轴缺失。[原始逐轴与秒桶](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/raw-bdp-on-60s-20261112T141842Z.json) 后续trace_bdp增加continuous/passed并按同一2秒IMU/逐轴标准决定退出码，保留completed的窗口语义；先通过纯自检，随后本节末10秒真实运行已验证新的passed判定。旧60秒JSON/退出原件不改，不将新判定倒填为旧运行结果。

新boot的stop_window记录Pcan usbbus1 vbus send errno6；新增内核原件直接显示22:19:24 peak_usb can0 Rx urb aborted(-71)、usb1-2断开和can0 removed，22:19:39–40新hub/PCAN重新枚举，22:24快照can0仍DOWN/STOPPED、can1..3 UP/ERROR-ACTIVE。USB断开/重新枚举是已证实事实，但日志中的“EMI?”是内核提示，不能据此断言供电或EMI唯一根因；缺轴到总线的精确映射仍未核对。这与旧boot PID78009停止超时和PID70959 CAN stale分别保留，本任务未配置CAN。[内核原件](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/can-kernel-window.log) · [22:24快照](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/failure-142438.json)

**状态getter与实际流分开验收：** 22:26 GetControlStatus、GetProfile、GetCmsState各调用一次均OK，但部分protobuf只返回空或默认字段，不能证明物理电机输出、姿态或就绪；22:41 GetMotorStatus缓存返回16 online，缺少反馈新鲜度时间，也不能当作之前缺轴已经恢复。[三项状态原件](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/current-status-20261112T142639028438Z.json) · [缓存电机原件](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/current-motors-144123.json)

**实际10秒双流恢复窗口：** SDK与raw约22:43:00同时启动、均退出0。SDK窗口10.001492秒，IMU1,000条/99.985Hz、最大消费gap295.515ms、源跨度9.989859秒；关节15,930事件/1,592.762条每秒，16轴各995–996条、99.485–99.585Hz，最大逐轴gap273.034ms。raw窗口10.015秒，IMU998条/99.651Hz；关节15,952事件，16轴各996–998条、99.451–99.651Hz，最大逐轴gap297ms，两条流continuous=true/passed=true，总passed=true。真实逐轴样本证明本窗口全部16轴重新出现，但不是长期稳定、严格实时、部署候选或物理状态确认；前面所有失败保留。[并发起止](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/current-concurrent-20261112T144254Z.json) · [SDK原件](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/current-sdk-20261112T144254Z.log) · [raw原件](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/raw-bdp-on-10s-20261112T144301Z.json) 本次电脑桥SystemInfo三字段均None，不将此前已验证的UUID桥配置等同本窗口已经装配或机端正式Host已安装。

## 真实身份准备与 SDK 稳定性复测（2026-11-12）

**当前身份与云边界：** 用户明确选定序列号 `THUNDER-DEV-001`，库存创建真实UUID `d82a831a-2e86-4b46-975b-3d42473583b7`，`is_demo=false`、`is_simulated=false`、`owner_id=null`。再次从云端读回同一UUID、真实且未认领。这是真实研发库存登记，不是客户已经认领或设备已在线上报。[创建结果](D:/DOSO/artifacts/robot-cloud/sdk-stability-20261112/inventory-created.json) · [云端复核](D:/DOSO/artifacts/robot-cloud/sdk-stability-20261112/inventory-verified.json)

机端 `/etc/doso/robot-cloud/agent/agent.env` 的样例UUID已原子替换为真实UUID，旧文件备份为 `agent.env.before-identity-20261112T110124Z`，权限root:doso-cloud 0640；`DOSO_CLOUD_ON=false`保持，未重启服务。[首次写入](D:/DOSO/artifacts/robot-cloud/sdk-stability-20261112/identity-110125.json) · [重复执行](D:/DOSO/artifacts/robot-cloud/sdk-stability-20261112/identity-111345.json) 后者changed=false、前后UUID相同、无新备份，身份未旋转。电脑开发桥已加载同一UUID，三次Rust短重连均打印robot_id=Some(真实UUID)、robot_version=None、firmware_version=None；后两项未从真实机端release接通，库存model_code「THUNDER 01」不等于SDK型号已接通，flowfix8构建名/SDK版本也不作为固件值。该握手证明电脑桥字段映射，不能认定机端Host/5052已部署。

复用 `tools/deploy/device-keys.sh` 在机端生成独立TLS私钥/CSR及Ed25519状态签名key。CSR CN为真实UUID，公私钥一致性与权限断言通过；device.key、device-signing.key、device-signing.pub安装为root:doso-cloud 0640，私钥未下载。`device.crt`尚未签发/安装、云端Gateway公钥登记与Broker ACL尚未做；制造认领密钥未生成，不能用状态签名或MQTT密钥替代。只导出CSR与状态签名公钥，不因准备凭据开启远程云。[密钥准备记录](D:/DOSO/artifacts/robot-cloud/sdk-stability-20261112/device-keys.json) · [身份与开通边界](tools/deploy/IDENTITY.md#首台研发机身份准备2026-11-12)

**此前flowfix8目标快照：** Brainstem PID41773当时active/running，启动时间18:14:14，北京时间；实际程序 `/home/sunrise/deployments/thunder_dual56501_flowfix8_20261112_a2398ada/bundle/bin/thunder`，SHA256 `a2398ada37abb4bd6f25227335cb21472c0b200aba7079f127cac31b356413fd`。当时两次完整端口检查只有13145，13146/5052未监听、lt-host未安装、cloud-link inactive/disabled。后续现场flowfix10及用户不重启限制见本页首节，本段各窗口保留原版本范围。[18:51快照](D:/DOSO/artifacts/robot-cloud/sdk-stability-20261112/robot-105152.json) · [19:05快照](D:/DOSO/artifacts/robot-cloud/sdk-stability-20261112/robot-110510.json)

`measure_observations`已加入第三个可选参数 `both|imu|joints`，原endpoint/seconds用法默认both兼容；只建立所选RPC，不订阅、不统计、不检查未选流。固定窗口、逐选定流/轴2秒消费超时失败、EOF/错误传播、实际源时间、首尾最大间隔与drop取消保留，不自动重连。初版单项模式测试与原统计测试共2项、cargo check、构建、rustfmt和帮助检查通过；后续诊断增强验证见下节。本轮真实运行记录如下：

后续只增加输出：连接后打印一次握手SystemInfo三字段，保留原统计行，再为每项单独打印已有source_first_ns/source_last_ns，缺失保持None，不新增状态或根据接收时间推断源时钟；rustfmt与示例构建通过，没有重跑无关测试。

最后顺序运行三次both模式10秒短重连，均退出0、completed=true，窗口分别10.002908/10.000787/10.001114秒，握手均为同一真实UUID。IMU的源首/末纳秒为3985886727000→3995876574000、3996176567000→4006176429000、4006296441000→4016265732000；关节事件为3985878298000→3995831602000、3996173607000→4006166641000、4006293135000→4016221685000。两条流每次重连首值均超过上次末值，表明读取的是继续前进的源时间，不能只凭平均Hz或稳定UUID作判断。该30秒分段证据不抵销300秒首轮消费超时失败。[重连1日志](D:/DOSO/artifacts/robot-cloud/sdk-stability-20261112/rust-reconnect-1.log) · [退出结果](D:/DOSO/artifacts/robot-cloud/sdk-stability-20261112/reconnect-1-run.json) · [重连2日志](D:/DOSO/artifacts/robot-cloud/sdk-stability-20261112/rust-reconnect-2.log) · [退出结果](D:/DOSO/artifacts/robot-cloud/sdk-stability-20261112/reconnect-2-run.json) · [重连3日志](D:/DOSO/artifacts/robot-cloud/sdk-stability-20261112/rust-reconnect-3.log) · [退出结果](D:/DOSO/artifacts/robot-cloud/sdk-stability-20261112/reconnect-3-run.json)

| 模式与计划窗口 | 实际结果 | 证据 |
| --- | --- | --- |
| imu，60秒 | 60.012434秒，6,001条、99.996Hz，最大接收间隔60.738ms，源跨度59.998687秒，线加速度存在0/6,001；该窗口完成 | [IMU单流](D:/DOSO/artifacts/robot-cloud/sdk-stability-20261112/rust-imu-60s.log) |
| joints，60秒 | 60.001822秒，95,940事件、1,598.951条/秒，各轴5,995–5,997条、99.914–99.947Hz，最大逐轴间隔152.323ms，源跨度59.999019秒；该窗口完成 | [关节单流](D:/DOSO/artifacts/robot-cloud/sdk-stability-20261112/rust-joints-60s.log) |
| both，300秒 | **200.319451秒触发2秒消费超时失败，completed=false**；IMU 19,833条、源跨度198.315270秒；关节317,130事件、源跨度198.331038秒，不能当作300秒稳定通过；原日志措辞保留 | [双流原始失败](D:/DOSO/artifacts/robot-cloud/sdk-stability-20261112/rust-both-300s.log) |

**SDK与原始流并行300秒诊断：** SDK实际300.001840秒、completed=true，IMU 30,001条、100.003Hz，最大gap184.444ms、源跨度299.992651秒；关节478,138事件、1,593.784条/秒，各轴29,880–29,888条、99.599–99.626Hz，最大逐轴gap638.590ms，关节源跨度299.024850秒，较墙钟约少1秒。平均近100Hz不能等同无积压。[SDK五分钟日志](D:/DOSO/artifacts/robot-cloud/sdk-stability-20261112/rust-both-paired-300s.log)

同期原始直连300.032秒收到IMU 29,997条、99.979Hz，最大gap187ms；关节479,641事件、1,598.633条/秒，各轴29,973–29,982条，源跨度299.964362秒、总体事件最大gap156ms，记录中没有超过500ms的事件间隔。该原始间隔是整条关节事件流口径，不能直接冒充逐轴gap。[同期原始直连](D:/DOSO/artifacts/robot-cloud/sdk-stability-20261112/direct-sync-300s-20261112T111332Z.json)

该次对照未重现2秒消费超时，不能抹去首轮在200.319451秒失败。此前IMU-only仅20帧后读取未继续的记录也保留；单流60秒与本次五分钟完成各自只证明当次窗口。首轮失败后短时原始直连正常、Brainstem PID/哈希未变，SDK五分钟仍有逐轴长gap和源时间推进落后，故障尚未定位，稳定验收仍未通过，不将原因直接归给源、SDK或网络。远程云、客户认领、机端完整SDK服务与运动不在本轮通过范围。

### 消费超时语义与本地复现（2026-11-12）

`Stats.last` 在消费循环处理样本后更新，2秒watchdog不能证明上游停发；biased选择先处理到期timer，有缓冲消息也可同样失败。未找到180/200秒固定截止，但完整flowfix8源/实际依赖未核对；Windows无匹配日志也不排除未记录暂停。示例保持阈值、选择顺序、非零失败、不重连，新增 `trigger`、`consumer_idle_ms`、`last_reported_source_ns`，失败后每条选定read单次零等待分类为 `sample_ready(next_source_ns=...)`、`eof`、`rpc_error`、`no_sample_ready`；诊断样本不计入原统计，也不改判成功。4项单测、check/build、格式与帮助入口通过。本地真实SDK→生产只读Gateway→模拟Brainstem实验中，baseline3秒退出0；仅暂停自有Rust客户端2.25秒，源handler计数IMU325→552、关节4624→7800仍前进，恢复后约2.3秒消费超时、退出1，两流均sample_ready。handler产出不证明全部数据已到客户端；**机制复现不证明历史根因**，原失败/日志保留，本轮无新真机长测。下一步先分层对照再真机验证，随后机端SDK及真实型号/固件元数据，最后证书、公钥/ACL及获许可云状态。[结果](D:/DOSO/artifacts/robot-cloud/stream-idle-20261112/result.json) · [暂停日志](D:/DOSO/artifacts/robot-cloud/stream-idle-20261112/paused-consumer.log) · [完整说明](D:/DOSO/artifacts/robot-cloud/stream-idle-20261112/README.md)

## 真实机器狗关闭态与本地 SDK 观测（2026-11-12）

**此前阶段快照（17:40，北京时间，当前已为上节flowfix8）：** 当时SSH可达，Brainstem恢复为flowfix7b，PID35151 active/running、13145监听。[快照](D:/DOSO/artifacts/robot-cloud/live-sdk-20261112/current-094036.json) 记录17:37:20启动，实际程序为 `/home/sunrise/deployments/thunder_dual56501_flowfix7b_20261112/bundle/bin/thunder`。恢复/部署由现场或其他操作完成，本轮SDK任务未执行。生产 `source.py` 已直接设置single-thread channel选项；关闭环境开关后的原45项本地测试与Ruff通过。flowfix7b默认代码20秒IMU99.646Hz、每轴93.397–93.447Hz，只证明当时读通且未稳定达到100Hz；本节各版本证据保留原范围。

**恢复前的故障时序：** 先前默认代码Rust建流在2秒内未启动，退出并报告 `Observation subscriptions did not start within 2 seconds`；紧随其后的原始直连约4秒，IMU与关节均为0样本、source span为null。[默认代码失败日志](D:/DOSO/artifacts/robot-cloud/live-sdk-20261112/rust-observations-default-20s.log) · [直连0样本结果](D:/DOSO/artifacts/robot-cloud/live-sdk-20261112/direct-sync-4s.json)

[17:36只读SSH快照](D:/DOSO/artifacts/robot-cloud/live-sdk-20261112/current-093608.json) 当时显示服务failed/failed、最后启动17:03:24、PID 28915已退出、13145无监听；该次启动非本轮SDK任务操作。journal记有17:18:06云卓H使能、17:18:10取得控制/L1接纳，随后17:18:44模式切换超过2秒预算并停止硬件控制；17:33:18进入停止流程，报告 `MOTOR DISABLE UNCONFIRMED` 与物理disable未确认。这是恢复前历史状态，后续flowfix7b恢复及本页首节flowfix8快照分别记录。现有证据不建立SDK读取与现场失败的因果关系，也不能将该次disable错误当作物理关闭确认；SDK任务没有擅自重启或执行动作。

本节保留同日较早构建阶段“TCP不可达、未安装”之后的检查。真实目标为 `192.168.66.64`；该次记录时UUID仍是模板、设备凭据尚未准备。后续真实库存/UUID及机端密钥更新见本页首节，远程云仍未开启。

**机端云连接关闭态：** ARM64 `doso-cloud-link` 候选已安装，机端 `/opt/doso/bin/doso-cloud-link` 的 SHA256 为 `d9fe0504b0c4fab27236e31a6171a6947cf85eece8125a69730e2e602b826d85`，与安装候选一致。以 `sudo -u doso-cloud`、`DOSO_CLOUD_ON=false` 运行，退出 0，stdout 为 `cloud_off`，stderr 为空；前后 `systemctl` 均显示 inactive/disabled。报告中的 after 组合命令退出 1 对应 inactive/disabled 的状态查询，不是程序运行失败。本证据只证明实际安装的关闭分支，不证明已 provision、开启、MQTT 上报或远程关闭闭环。[原始关闭态结果](D:/DOSO/artifacts/robot-cloud/live-sdk-20261112/cloud-off.json)

**此前测量时的机器人端点：** 主 Brainstem `192.168.66.64:13145` 曾以明确的LAN development plaintext监听，电脑IP已在既有白名单。此前flowfix5检查确认`13146`环境配置虽已写入，二进制缺少只读入口接线、未监听，机端SDK Gateway `5052`也未安装监听。[早先实机检查快照](D:/DOSO/artifacts/robot-cloud/live-sdk-20261112/robot-before.json) 当时flowfix7b的`ss`只截取前15行，只能确认13145、不能排除13146/5052；本页首节后续flowfix8完整检查已单独复核端口，机端完整SDK服务仍未完成部署验收。

`13146` 已准备未应用的 [native local-status补丁候选](D:/DOSO/artifacts/robot-cloud/live-sdk-20261112/local-status-candidate/README.md)，复用既有状态真源，仅回环提供 `GetControlStatus`，不提供IMU/关节流。只检查了flowfix5旧基线的替换锚点与隔离 `git apply --check` 适用性，未编译、分析、部署或验证监听生命周期；候选准备时匹配源码中的native entry尚不完整。现场已切换flowfix7b，旧候选不可直接部署到新版，须按新版重新核对。这是可审查补丁，不能视为可运行替换制品。

**电脑上的只读链路：** LingTu 新增 `tools/sdk/observe.py`，仅在 `127.0.0.1:50051` 注册 SDK Handshake、WatchImu、WatchJoints，复用生产 `Observations` adapter 与 Gateway handlers，不启动硬件 Host 或 driver、不注册控制/导航/产品切换 RPC。连接本次既有主接口须显式 `--lan-development`，映射到只读源 `LINGTU_OBSERVATIONS_TRANSPORT=lan-dev`；仅接受字面 RFC1918 IPv4，拒绝与 TLS 混配。未选择该模式时，远端仍要求 mTLS，无失败降级。Python 本地相关 45 项通过；这是本地合同证据，不与下列实测样本数相加。

```powershell
# 在 LingTu 根目录且已具备其 Python 运行依赖时启动；上游必须是已授权的 LAN 开发接口。
$env:PYTHONPATH = 'D:\inovxio\brain\lingtu\src'
python tools/sdk/observe.py --target 192.168.66.64:13145 --lan-development
# 另一个终端运行已构建的真实 Rust SDK 只读示例。
& 'D:\inovxio\brain\DOSO-SDK\target\debug\examples\measure_observations.exe' http://127.0.0.1:50051 20
```

Rust 示例 `measure_observations` 的单项统计自检、构建、`cargo check --example` 和 `--help` 已通过。它在同一墙钟窗口并发统计 IMU/每关节事件，不逐帧打印，源时间仅使用实际 `source_elapsed`，缺失保持未知；任一源或关节 2 秒无数据、源提前结束或报错均失败，结束时丢弃订阅。

**首轮真实 Rust 20 秒测量：** 实际窗口 20.001201 秒，IMU 2,002 条、100.094 Hz，源时间跨度 20.009644 秒，最大接收间隔 73.569 ms，线加速度字段存在 0/2,002。关节事件 24,921 条、1,245.975 条/秒；每轴 1,557–1,558 条、77.845–77.895 Hz，关节事件源时间跨度 15.581882 秒，逐轴最大接收间隔最高 75.634 ms。[原始汇总日志](D:/DOSO/artifacts/robot-cloud/live-sdk-20261112/rust-observations-20s.log)

`completed=true` 仅表示测量窗口完成且没有触发2秒空闲/断流失败，**不是关节100Hz性能目标通过**。首轮关节源时间在20秒墙钟中仅前进约15.58秒，未达到目标；这份原始失败与此前IMU-only仅收到20帧后停流的结果均保留。

**flowfix5直连与转发对照：** 同一Python环境直接读取真实Brainstem，20.031秒收到IMU 1,999条、99.795Hz，关节31,902事件、1,592.631条/秒，关节源时间跨度19.984995秒。直连源保持正常速率而首轮转发落后，将当时瓶颈定位在转发路径，而非将机器狗源解释为约78Hz。[flowfix5对照归档](D:/DOSO/artifacts/robot-cloud/live-sdk-20261112/direct-sync-flowfix5-20s.json) 原 `direct-sync-20s.json` 后被新版测量覆盖，旧档从此前工具原始输出恢复，来源说明随归档保存，不将恢复件称为未改动的原始文件。

用gRPC已有single-thread unary-stream环境开关做A/B后，Rust 20秒达到IMU99.858Hz、每轴99.758–99.808Hz。该轮曾并行离线负载，IMU最大接收间隔115.743ms，保留为定位证据；以下无该并行负载的60秒测量为flowfix5阶段主要性能证据，当前新版结论另列。[20秒A/B日志](D:/DOSO/artifacts/robot-cloud/live-sdk-20261112/rust-observations-single-thread-20s.log)

**干净60秒真实Rust测量：** 实际窗口60.002805秒，IMU 5,999条、99.979Hz，最大接收间隔70.889ms，源时间跨度59.979214秒，线加速度存在0/5,999。关节95,923事件、1,598.642条/秒；每轴5,994–5,996条、99.895–99.929Hz，逐轴最大接收间隔最高76.383ms，关节事件源时间跨度59.989973秒。[60秒原始日志](D:/DOSO/artifacts/robot-cloud/live-sdk-20261112/rust-observations-single-thread-60s.log)

这证明该设置下真实Rust SDK链路在flowfix5当时60秒窗口内接近100Hz，源时间与墙钟推进接近，未见首轮的明显持续积压；70–76ms级最大接收间隔仍在，不能描述为严格实时、硬件同步采样或长期稳定，也不能推广为当前新版结论。

**最终默认代码、无环境开关、flowfix7b 20秒：** 已通过建流及完整20.000744秒读取，IMU 1,993条、99.646Hz，最大接收间隔120.424ms、源跨度19.919941秒，线加速度存在0/1,993。关节29,900事件、1,494.944条/秒，每轴1,868–1,869条、93.397–93.447Hz；逐轴最大接收间隔最高280.441ms，关节事件源时间跨度18.699046秒。[最终Rust汇总](D:/DOSO/artifacts/robot-cloud/live-sdk-20261112/rust-observations-default-flowfix7b-20s.log)

flowfix7b原始直连对照中，IMU 1,993条/20.047秒、99.416Hz；关节31,871事件/20.031秒、1,591.084条/秒，每轴1,991–1,992条、约99.4Hz，关节源时间跨度19.929782秒。[flowfix7b直连归档](D:/DOSO/artifacts/robot-cloud/live-sdk-20261112/direct-sync-flowfix7b-20s.json) 对比显示，双gRPC边界转发在变动负载下仍有吞吐不足与积压风险：本轮默认实现已真实读通，但各轴约93.4Hz、源时间推进落后，100Hz目标尚未稳定达成，不能写为瓶颈已完全解决。`completed=true`仍只表示窗口完成且未触发2秒无数据失败。45项本地测试与Ruff通过证明相关合同与静态检查，不替代该性能限制；SDK任务未启用远程云、完成机端SDK 5052部署或发送动作，现场云卓操作不属于SDK运动测试。

## 云权限与客户公开信息（2026-11-12）

用户明确设备无云管理权、云端禁止远程运控、本地SDK保留操控，见 [CONTRACT.md](CONTRACT.md#设备云端与本地-sdk-的权限边界2026-11-12-确认)。本轮只改源码、回归与文档，未部署服务或连接真机。

**云权限：** `cargo run -p lab --locked --offline -j 1 -- auth-check` 的9项实际本机Mosquitto检查通过，覆盖证书认证、合法自身上报、跨设备发布/订阅隔离、设备写云控制/视频/请求主题被PUBACK拒绝及伪装Gateway身份失败。订阅隔离通过实际消息投递验证，未将SUBACK成功当作可读证明。临时证书与进程正常清理。

`cargo test -p doso-cloud-gateway --locked --offline device_ -j 1` 3项通过（含2新增及1既有）；`cargo test -p doso-cloud-link -p doso-cloud-gateway --locked --offline cloud_ -j 1` 3项通过，其中1项与前批重叠。验证设备签名不能替代管理Bearer或把关闭许可改为开启；合法云凭据请求运动/遥操作/电机/通用gRPC路由404；即使签名合法且许可有效，运动kind与注入method/arguments/command字段在Device接纳前拒绝，合法`status.read`仍通过。唯一Brainstem状态调用仍固定`GetControlStatus`，无新分发入口。生产ACL及本地SDK运动逻辑未改。

**SDK公开系统信息：** Rust全量105项通过（8单元、84gRPC、2格式、11文档），Clippy与格式通过；示例收紧输出后单独构建及Clippy通过。新增11项真实Python gRPC测试与69项相邻回归通过，共80项，验证六个System Snapshot按公开字段投影、不更改诊断源、保留false/null/缺省与精确CAS、未预期异常只返回固定INTERNAL/internal_error而服务日志保留traceback。Ruff通过。[公开投影](D:/inovxio/brain/lingtu/src/gateway/rpc/system_view.py) · [Rust类型化读取](D:/inovxio/brain/DOSO-SDK/src/system/product.rs)

真实Rust `read_status` CLI调用生产Python System handlers/system_view，输入为模拟诊断源字典，退出0。ProductStatus为map/active/available=true，Readiness保留false，能力中仅公开features与Product可用性；内部marker、端点、进程和CAS均未进入CLI输出。原始诊断字典未改，server/端口/Rust进程清理通过。[结果](D:/DOSO/artifacts/robot-cloud/public-sdk-20261112/result.json) · [复现命令](D:/DOSO/artifacts/robot-cloud/public-sdk-20261112/commands.ps1)

**客户状态与历史：** 原生Deno2.5.6使用现有缓存、冻结锁文件，类型检查与38项通过（history11、state-user13、内部sync14）；App五组51项与`tsc --noEmit`通过。前期Node适配运行仅为初查，最终原生Deno已覆盖Request流、权限与模块解析。隔离PostgreSQL17.11验证旧单参RPC删除、新三参RPC只有service_role能执行、活跃账号/终端/Auth会话/归属校验、historyId回填不改授权和投递真值、换主变更数据集及公共采样去transport且保留未知精度。任务集群已停止。

公开历史使用不透明historyId保证分页一致性、breakBefore表达轨迹断段、业务availability/syncState代替映射与队列计数。重叠样本只在服务端用于跨页判断，同授权连续、授权变化断开、缺前置样本保守开新段，分页途中historyId变化拒绝混拼。公共state移除transport，内部报告/sync仍严格检查transport。[验证说明与命令](D:/DOSO/artifacts/robot-cloud/customer-data-20261112/README.md) · [16文件变更清单](D:/DOSO/artifacts/robot-cloud/customer-data-20261112/verification.json) · [PG原始结果](D:/DOSO/artifacts/robot-cloud/customer-data-20261112/postgres-verification.json)

**发布边界：** 新SDK `product_status()`返回类型及客户history/RemoteStatus合同存在开发预览兼容变更。App、增量SQL和remote-history/remote-state Edge函数需在协调窗口发布；既有上线迁移未改。发布前旧线上接口仍保留旧行为。本轮不证明生产客户/人员访问隔离、其他领域全部诊断字段或真机运动已验收。

## SDK 地理位置与云端投影（2026-11-11）

Rust SDK 新增 `localization.location()`、`watch_location()`，LingTu gRPC 复用已有 GNSS 快照读取器。真实地理坐标与 SLAM map 位姿分别提供；1Hz 快照流通过缺 fix、过期和恢复持续运行。协议与字段见 [SDK API](D:/inovxio/brain/DOSO-SDK/docs/api/localization.md#地理位置)，云端数据源和后续 IP/告警边界见 [MONITORING.md](docs/MONITORING.md)。

`cargo test --locked` 在 SDK 根目录通过 103 项（8 单元、82 gRPC、2 格式、11 文档）；LingTu 的位置、原地理读取、RPC telemetry 与 security 相关 Python 测试 42 项通过。真实跨语言检查发现当代 epoch 浮点整值乘纳秒会损失 64ns，改为分别计算整数秒与小数秒后，重新通过 11 项位置专项与 Ruff。其他现有 GNSS freshness/验证规则未改。

使用生产 `gateway.rpc.server.start(GatewayModule(), ...)` 与已构建 Rust `watch_location.exe` 实际回环联调，输入为明确模拟的临时 GNSS 文件。约 4 秒收到 Ready、Ready、NoFix、Stale、Ready 五份样本，间隔约 1 秒，源时间的 125000000ns、未知精度和恢复后的新坐标保留。主动停止本地测试 server 后，Rust 按预期收到 Unavailable 并退出 1，测试进程与端口清理通过。[结果](D:/DOSO/artifacts/robot-cloud/location-20261111/cross-language.json) · [复现命令](D:/DOSO/artifacts/robot-cloud/location-20261111/commands.ps1) · [脚本](D:/DOSO/artifacts/robot-cloud/location-20261111/verify_cross_language.py)

云端 `cargo test --locked -p doso-cloud-telemetry` 10 项通过。位置展开、GNSS 独立时间、有精度转未知、缺位置、读取失败及原完整报告保真均有回归，格式与 source diff 检查通过。每条明确写位置和精度可用位，TB latest 的旧坐标仍需结合可用位和源时间判断。本轮未部署线上 worker，未创建 TB 地图或告警规则，不表示真实机器人已上报位置。

## 最新验证范围

11月12日真实关闭态和 Rust 观察短窗见本页首节，通知源码的生产构建与页面证据见下方「通知收纳与紧凑浮层」；此前侧栏、登录页和业务链的记录保留各自原始范围。

| 验证层 | 最近记录 | 覆盖与边界 |
| --- | --- | --- |
| GOAWAY定位与未激活候选 | 2026-11-12 | 历次GOAWAY/消费超时/停机/4轴缺失均保留；内核证实USB can0断开及重枚举，物理根因未确定；stream-close候选最终9+1测试/ARM64构建通过、前两轮失败保留且未激活；最新SDK/raw 10秒全16轴有样本并退出0，仅短窗口恢复，长稳未通过；无任务重启或CAN配置 |
| 真实身份准备与SDK稳定性复测 | 2026-11-12 | flowfix8 PID41773/哈希不变、13145在线，13146/5052未监听、lt-host未安装；真实库存UUID/机端密钥已准备，重复写身份不旋转，云关/未认领/证书公钥ACL待完成；单流60秒完成，首轮both300在200.319451秒失败；后续并行300秒完成仍有638.590ms逐轴gap和约1秒源跨度落后，稳定验收未通过 |
| SDK 地理位置与云端位置投影 | 2026-11-11 | Rust103项、相关Python42项、修复后11项位置专项、实际Rust/Python gRPC五帧回环通过；telemetry10项。输入为模拟GNSS，未部署真机或线上worker |
| 目录调整后的Rust workspace | 2026-11-07 | 离线锁定构建、61项测试与格式检查通过；这是当次源码快照的结果 |
| 本机通信链路 | 2026-11-07 `lab check` | 真实Gateway、MQTT、cloud-link；Brainstem为模拟。覆盖许可、查询、上报、关闭停采和本地选择关闭退出 |
| 实际Web App与真实业务数据库 | 2026-11-07 [模拟设备流程](docs/SIMULATED_FLOW.md) | 正常账号和已登记模拟设备；绑定API、开启、退出页面持续更新、隧道断网恢复、关闭停采已验证 |
| GNSS扩展与地球地图 | 2026-11-08 [GNSS接入](docs/GNSS.md) | Rust56项、App32项、Deno26项、隔离PG36组通过；真实云库接收两坐标；实际Web App渲染地球/街道且标记移动；Linux原子快照通过；无真机或手机实测 |
| 遥测历史与ThingsBoard | 2026-11-08 | Rust9项、Deno31项；真实PG37组加实际平台4组=41组通过。实际SQL、生产handler、Rust和TB执行，源为模拟设备，PostgREST传输替换为白名单psql适配；无生产部署或真机验收 |
| GPS跟随与日期轨迹 | 2026-11-09 [TRACKS.md](docs/TRACKS.md) | 测试ECS实际SQL/HTTPS接口、ECS Rust投递、本机真实TB和实际Web App分段验证；关闭停采、后台更新与恢复均有实测，详见下节；不是独立常驻云平台或真机验收 |
| DOSO管理后台 | 2026-11-11 | 紧凑铃铛最终生产编译414.538秒、本机18081已部署；9项源码自检、45项HTTP/API/WS及202资源、Chromium桌面/手机10项检查通过。保留此前侧栏与登录验证记录；浏览器双账号切换、空态、其他浏览器和负载另验 |
| 完全断云与本地重新开启 | 模拟链路已实现并验证 | 真机 cloud-link、真实远程关闭和 App/业务许可端到端仍待验证 |
| 真实机器狗和正式手机/G20 | 部分只读证据，产品交付待验收 | 11月12日仅候选安装关闭态与本地SDK真实观察短窗；正式身份、真实云状态、扫码、独立公网MQTT、手机/G20与5G仍未验收 |

11月7日本地联调由Rust工具运行，云连接服务、Gateway与Mosquitto使用真实实现，Brainstem使用只读模拟服务；11月12日SDK观察短窗的上游则为真实机器狗，范围见本页首节。每条证据按其实际业务后端、网络和客户端组成解释，模拟返回不能作为真机验收证据。

## GPS跟随与日期轨迹（2026-11-09）

后续滑动摘要修正：地图按钮和状态移到React工具条，轨迹线细化，起末点实测8px；数字字体统一MiSans-Medium24px/32行高。新增专用TrackSheet，移除旧二次详情折叠状态。TrackPanel11项、TrackSheet5项、地图与桥接/控件12项共28项通过，类型检查通过。实际Web App在430/360视口验证鼠标上下拖动、Chrome触摸上下拖动、Enter切换与日期切换；196/340px两档及地图剩余尺寸正确，成功历史读取11次、远程状态写入0次、JS错误0。证据见 [滑动摘要验证](D:/DOSO/artifacts/robot-cloud/20261109-track-sheet/verification.json)。前次失败与RNW手势诊断保留；最终修复不依赖被View丢弃的click-capture，未新增手势库。原生手机手势未实机验收。

同日UI改版：采用Inspora参考的单卡、日期行、双指标摘要和轻量地图控件。RemoteStatePanel17项、TrackPanel11项、地图8项共36项通过，App类型检查通过。实际App在430/360/1440视口无横向溢出，控件不挡版权；日期、详情与深色预设重载检查通过，成功读取真实状态19次/历史16次，远程状态写入0次、JS错误0。代码与本地Metro已更新，正式手机包未发布。证据见 [UI验证](D:/DOSO/artifacts/robot-cloud/20261109-ui/verification.json) 和 [参考说明](D:/DOSO/artifacts/robot-cloud/20261109-ui/reference.md)。运行中系统主题切换未立即更新App主题，已记录并保留为全局主题问题；本轮未修改该hook。

源码验证通过：App地图脚本7项、轨迹统计6项、摘要组件最终10项、历史分页服务6项及原远程状态回归；Expo `tsc --noEmit`通过。历史接口Deno9项，连同相邻回归39项通过；隔离PG41组验证原样发布SQL、权限、换主/epoch与重复apply拒绝。部署边界4项通过。Rust telemetry9项及模拟圆环1项通过，x86_64 Linux遥测程序锁定离线zigbuild通过。

测试业务ECS已备份并应用遥测/历史两份迁移，新增两个HTTPS函数且原21个Functions文件保持原字节；主私有环境与其他核心服务保持。三个接口 `remote-history`、`telemetry-export`、原 `remote-state` 的无凭据请求均返回401。数据库备份为 `/opt/doso-backend-shadow/env/remote-history-database-34qyge0l`，运行时备份为 `remote-history-runtime-qzx44bt0`。发布包与本地验证见 [remote-history-20261109](<C:/Users/99563/Documents/ChatGPT/New project/doso-app/deploy/backend/remote-history-20261109/local-verification.json>)。

真实浏览器使用正常账号和模拟UUID `78ec077d-0cde-441b-bddc-022cfc850676`，验证了实际HTTPS历史读取、新轨迹增长、拖动释放跟随、全览起末点、刷新保留、离开App后台更新、关闭停采与保留摘要。关闭稳定后模拟源读取次数保持2050，历史保持276条；重新开启后的独立App检查读到373条以及更新的时间。各步骤汇总见 [acceptance.json](D:/DOSO/artifacts/robot-cloud/20261109-track-flow/acceptance.json)。后续采样仍会增长，数字是验收时快照。

完整自动化脚本在最后恢复步骤前曾切到首页，最终等待超时；该失败原件保存在 `browser-before-resume.json`，恢复步骤另一次实际App运行通过，不能把两次结果描述为单次不间断全通过。源码检查发现同账号会话重新准备可能触发回首页，但该次Auth事件未记录，具体触发尚未确认；未为此改动账号或导航代码。

本次联调还实测到开发机Docker历史平台中断、SSH临时通道断开和本机worker请求云端超时。恢复既有卷后旧历史可读；已把Rust worker放到ECS，使用同机回环业务入口减少往返，原3秒超时保留。错误日志现保留失败阶段与HTTP状态，不输出密钥或原始报告。ThingsBoard仍在本机经SSH私有通道接入，当前不是常驻生产部署。没有发送运动指令，未验证真实GNSS、原生手机或5G。

### 本次文档检查（2026-11-08）

已核对云连接工程全部16份Markdown，以及App/SDK的19份相关说明，共35份。292处本地文件链接可解析；组件说明中的3个Cargo包名、7种lab命令模式和9个部署/检查脚本入口与现有文件及参数匹配。更新了目录、当前状态、历史边界和产品推进顺序，未改生产代码、协议字段、依赖或部署配置，也未重跑功能测试。以上是文档静态检查，不能累加为新的运行测试或部署证据。

## DOSO管理后台（2026-11-09）

### 通知收纳与紧凑浮层（2026-11-11）

继续参考已下载 shadcn/ui 与 shadcn-admin 的菜单分组和紧凑布局。铃铛复用平台 WS 通知，并合计当前会话本地提示的未读数。本地提示最多30条，按内容与类型合并，支持单条/全部已读和清除；刷新、退出或换账号清空。账号变化通过现有 popover 可见性路径立即销毁旧弹层和平台订阅，不等待离场动画；普通点击关闭仍保留动画。

Toast 正文13 px，半透明淡色面板；未传 duration 时普通提示6秒、错误/警告8秒，显式0保留原持续显示语义。铃铛浮层360 px宽、内容最多480 px高并适应视口，空态改为简短文字，高级管理增加轻分隔。未改登录页或增加依赖。

9项实际服务/组件源码自检通过，外部Store、Angular注入和WS/UI边界使用替身，覆盖容量/去重、读清、换号清空、时长、立即销毁、旧关闭回调不重订阅，以及换行归一、原toast内容保留与文本插值边界。脚本保存于 [本地提示](D:/DOSO/artifacts/robot-cloud/console-20261111-notifications/doso-session-notifications.spec.cjs) 与 [铃铛生命周期](D:/DOSO/artifacts/robot-cloud/console-20261111-notifications/doso-notification-bell.spec.cjs)。三模板解析、三TS语法、三Sass编译、两语言JSON及source diff检查通过。

中间候选Angular完整生产编译317.986秒通过，结束时间2026-11-11T08:41:54.427Z，目录`.build/20261111-084228-0b9e1a42`。该候选45项HTTP/API/WS检查与202个资源通过，并在实际页面验证了本地404记录、铃铛5=4平台+1本地、已读回落4和单条清除。实际页面发现本地消息露出字面`<br/>`及平台正文预览过长，随后补换行归一和仅弹层内三行预览；最终构建与验收在本轮完成后更新。[中间接口结果](D:/DOSO/artifacts/robot-cloud/console-20261111-notifications/http-before-preview-fix.json) · [中间页面记录](D:/DOSO/artifacts/robot-cloud/console-20261111-notifications/browser-intermediate.json)

中间页面最初假设通知为空，但实验租户实际已有4条平台通知，且第一次测量读的是滚动内容宿主高度；该空态断言已标为superseded，改为检查实际360×480 px滚动面板及rgba白底/12 px模糊。未为验证空态清除既有平台通知。更早一轮编译由本任务主动中止以修复账号切换的即时销毁，未部署该中间候选。

最终Angular生产编译414.538秒通过，结束2026-11-11T09:11:36.440Z（北京时间17:11:36）。发行目录`.build/20261111-091215-19db7ffd`，manifest时间09:16:15.5142563Z，patch SHA256 `2878a76661a83024b20c67484e4255a0ed340a5b18c4b7dd971c6c91758fcc92`，镜像`sha256:d56cf988d3171ff48f4f89e8afbaece4d948d6492749b1ad12067f9b29b6cf59`。补丁逆向检查、Compose更新与容器`nginx -t`通过，已部署本机18081；TB、数据库及原有卷保持。

最终45项HTTP/API/WS和202个静态资源全部通过，覆盖构建指纹、鉴权、既有接口与字节一致性。[HTTP结果](D:/DOSO/artifacts/robot-cloud/console-20261111-notifications/http.json)

Chromium实际页面10项检查通过，1280×720与390×844两种尺寸。确认360×480 px玻璃面板、滚动区、原4条平台通知正文预览三行；计算样式通过`getPropertyValue('-webkit-line-clamp')`读取为3，实际正文48 px（原组件行高16 px），上限54 px。初始camel-case属性探针未暴露该前缀属性，改用标准属性读取后确认，说明保存在结果中。只读访问不存在的测试设备产生1条本地404提示，原换行标记已变为普通换行，白空白样式pre-line，未读数4→5→标已读回4，清除后本地行消失。未改平台已读状态。手机面板左右20/380 px、底部545.5 px，文档宽390 px，无横向越界。视口已重置，总览保留可见。[页面结果](D:/DOSO/artifacts/robot-cloud/console-20261111-notifications/browser.json) · [桌面](D:/DOSO/artifacts/robot-cloud/console-20261111-notifications/notifications.jpg) · [手机](D:/DOSO/artifacts/robot-cloud/console-20261111-notifications/mobile.jpg)

运行数据仍为隔离实验租户及模拟记录。空态未在实际页面触发，浏览器双账号切换仅有源码/组件替身验证；真实触屏、其他浏览器、负载表现、云端位置worker与真机仍按各自范围验收。本轮没有运动操作或开启云采集。

### 侧栏调宽与图标（2026-11-11）

本次日期采用Asia/Shanghai。保留白底和无卡片外框布局，补回侧栏、顶部工具栏、品牌区与总数区底部的极浅分界。主导航、主要入口面包屑及手机侧栏按钮接入八份Lucide原始SVG，复用现有图标注册服务，完整ISC与Feather MIT归属随资源发行并在NOTICE署名。参考三个GitHub项目的17份原始文件，固定提交、文件哈希和许可集中在 [导航参考说明](D:/DOSO/references/console-design/navigation-20261111/README.md) 与 [清单](D:/DOSO/references/console-design/navigation-20261111/manifest.json)。

HomeComponent拥有桌面侧栏宽度，默认240 px，拖动范围200–360 px。可聚焦分隔条使用左右键每次调节8 px。模板按side/over模式应用桌面宽度，拖动期间同步Material内容margin并关闭其margin过渡，小屏取消拖动状态。宽度在当前组件会话内保存，刷新恢复默认值。

TypeScript完整 `tsc -p src/tsconfig.app.json --noEmit`、模板解析、Sass编译、SVG/注册映射与许可字节检查通过。Angular生产编译483.449秒，于2026-11-10 16:41:39.076 UTC结束，即北京时间11月11日00:41:39。发行目录 `.build/20261110-164231-962a9030`，清单时间16:46:20.1181143 UTC。补丁SHA256为 `1131e6d9b848e96c3d7e3f223c9229b6a66aac6d5e4b2042df4ecb169ec72c7b`，镜像ID为 `sha256:0227ec05d265e56c5870895b270ebf1203f79c08fab448d4b43d107cb7702ba7`。补丁逆向应用、源码差异检查、Compose和容器内 `nginx -t` 通过，本机18081已部署。

最终HTTP/API/WebSocket 45项及202个静态资源通过，运行于2026-11-10 17:53:57.861–17:54:52.896 UTC。包含既有35项检查，以及八个SVG和完整许可的源文件/发行物/HTTP字节一致性、NOTICE署名。首次登录请求出现20秒超时，第二轮登录通过但7 MB级JS资源读取失败。保留两轮原件，将静态检查并发从6降至2后，完整45项复测通过。三轮检查均使用20秒请求截止时间，负载与延迟表现仍待独立验证。[最终HTTP报告](D:/DOSO/artifacts/robot-cloud/console-20261111/console-navigation-http.json)、[首次结果](D:/DOSO/artifacts/robot-cloud/console-20261111/console-navigation-http-first.json)、[第二次结果](D:/DOSO/artifacts/robot-cloud/console-20261111/console-navigation-http-second.json)

Chromium IAB三种尺寸21项检查通过，运行于2026-11-10 16:58:56.680–17:48:03.590 UTC，尺寸为1440×900、960×720和390×844。正常指针拖动240→320、超出手柄区域拖至200/360、松手后停止、菜单展开时调宽、左右键8 px和上下限均通过。各宽度下内容左缘与侧栏宽度一致，主导航行随宽度伸缩。拖动状态下margin过渡为none；拖动中跨手机断点后状态清除、手柄移除、内联宽度清除。手机抽屉宽334 px并可正常导航，返回桌面恢复会话宽280 px，刷新复位240 px。960 px桌面最大侧栏宽360 px时文档与卡片无横向越界。[浏览器行为自检报告](D:/DOSO/artifacts/robot-cloud/console-20261111/browser-navigation.json)

浏览器还核对七个主要导航SVG均为24×24 viewBox、2 px描边和圆端点，容器20 px；手机panel-left同源SVG可见，白底和极浅分界正确。平台将交互分隔条呈现为splitter，自动化改用观察到的DOM选择器。一次只读几何查询超时后独立重读通过，展开菜单行宽断言补计6 px滚动条后通过，查询修正保留在报告中。最终截图宽280 px，视口覆盖已重置，页面保留在总览。其他浏览器、完整键盘遍历、触控笔和负载表现待验收。

[桌面侧栏与图标](D:/DOSO/artifacts/robot-cloud/console-20261111/console-navigation-desktop.jpg)、[手机导航](D:/DOSO/artifacts/robot-cloud/console-20261111/console-navigation-mobile.jpg)。数据来自既有实验租户与模拟设备，真实机器狗上报和业务隔离仍按各自交付范围验收。

### 白底无框工作台（2026-11-10）

依据用户要求观察当次Codex桌面工作区，主内容截图中白色为 `#ffffff`、侧栏主色为 `#fdfdfd`、浅灰选中区域为 `#f1f1f1`。这是桌面画面采样，具体组件文本色由DOSO选择，记录属于当次视觉参考。[脱敏颜色采样](D:/DOSO/artifacts/robot-cloud/console-20261110/workspace-color-samples.json)

主内容与表面调整为纯白，文字为中性灰黑。移除总览卡、快捷面板、空态和列表外框、设备卡阴影、卡片底线及外壳分隔。总览“查看全部设备”使用无框文本按钮，设备卡和快捷入口在默认、悬停及焦点状态覆盖上游全局锚点下线。列表采用极浅中性行分隔，悬停采用中性灰，40 px导航与56 px数据行延续紧凑布局。

最终Angular生产编译205.838秒，于2026-11-10 15:36:23.821 UTC结束。发行目录为 `.build/20261110-153644-0a372f25`，清单生成于15:40:10.6623273 UTC。补丁SHA256为 `bd1f6ac5508c20868c5977b9d326a464b8c82e4b7867768ed063554307048cc5`，镜像ID为 `sha256:9d874ec8fdb09a8cbda7b7d4b79b4fef74b59948b7e191c74f36dfc8ae7f4189`。Sass编译、Angular模板解析、源码差异空白检查、补丁逆向应用、Compose配置与容器内 `nginx -t` 通过，已部署本机18081。

最终HTTP/API/WebSocket 35项及193个静态资源通过，运行于15:46:24.210–15:46:30.234 UTC。验证既有实验租户的认证、设备列表/详情/搜索/历史、错误返回、原版入口数据一致性、属性订阅、字体/图片字节与许可。[HTTP报告](D:/DOSO/artifacts/robot-cloud/console-20261110/console-white-http.json)

Chromium IAB四种尺寸18项检查通过，运行于15:46:51.759–15:52:43.193 UTC，尺寸为1440×900、1280×800、390×844和320×568。实际计算样式确认主背景为 `rgb(255,255,255)`，总数卡、设备卡、快捷面板、链接、卡片底部与列表外框各侧border均为0，列表shadow为none、radius为0。设备卡和快捷入口悬停采用 `rgb(247,247,247)`，无边框和下线。键盘焦点保留2 px可见轮廓，无全局锚点底线，Enter可从无框总览入口进入设备列表。[浏览器报告](D:/DOSO/artifacts/robot-cloud/console-20261110/browser-white.json)

其余运行检查覆盖56 px表格行、独立滚动容器、设备搜索结果、手机真实登记计数、导航按钮可见性、小屏告警时间窗/搜索/分页和文档宽度。临时视口覆盖已重置，浏览器保留在总览。其他浏览器、完整键盘遍历、真实机器狗上报与真实告警需独立验收。

[白底桌面总览](D:/DOSO/artifacts/robot-cloud/console-20261110/console-white-desktop.jpg)、[白底设备列表](D:/DOSO/artifacts/robot-cloud/console-20261110/console-white-devices.jpg)、[白底手机总览](D:/DOSO/artifacts/robot-cloud/console-20261110/console-white-mobile.jpg)。下节保留紧凑有框版的历史证据。

### 紧凑工作台与设计参考（2026-11-10）

按用户截图删除总数卡、每台设备卡与快捷入口中的重复装饰图标，快捷入口采用文字链接。布局采用中性色、4/8/12/16/24 px主要间距，顶部64 px、导航40 px、表格行56 px、表头44 px。总览与列表沿原数据和交互入口读取既有测试设备。

实际检索GitHub并下载Ant Design官方参考，固定提交 `0572a24a2ada6021a55d93fceca93a67b2dc6e2f`，源码版本6.7.0。共14份上游原始文件、82,662字节，加本地说明和manifest共16文件。内容覆盖中文设计价值观、布局、色彩、字体、数据展示、尺寸/紧凑算法、表格token及MIT许可。下载时逐文件核对字节数和SHA256，原文图示外链保留。DOSO色值及具体组件尺寸为本项目选择，应用构建继续使用现有Angular/Material组件。[本地参考说明](D:/DOSO/references/console-design/README.md)、[下载与校验清单](D:/DOSO/references/console-design/manifest.json)

最终Angular生产编译260.809秒，于2026-11-10 14:59:22.438 UTC结束。发行目录为 `.build/20261110-145945-fd0e9f6d`，清单生成于15:02:39.3588519 UTC。补丁SHA256为 `33280aaea13d3f72ef9a07193103aa80a6f9acb45444c7d876470026cc5be7f6`，镜像ID为 `sha256:4f880a9d19bb44b972326e88d9202b581a0b4c53e879c398f7714725d79cb6a8`。补丁逆向应用、Sass编译、Angular模板解析、源码差异空白检查、Compose配置与容器内 `nginx -t` 通过，已部署本机18081。

最终HTTP/API/WebSocket 35项及193个静态资源通过，运行于15:06:20.853–15:06:30.942 UTC。覆盖认证、既有设备列表/搜索/详情/历史、错误响应、原版与定制入口数据一致性、属性订阅、字体、图片和许可文件。[HTTP报告](D:/DOSO/artifacts/robot-cloud/console-20261110/console-compact-http.json)

Chromium IAB四种尺寸21项检查通过，运行于15:06:45.838–15:16:15.606 UTC。尺寸为1440×900、1280×800、390×844和320×568。实际测量桌面设备卡229.390625→177.390625 px，总数卡126→80 px，快捷行57→40 px，顶栏72→64 px，主导航48→40 px。总览重复装饰图标从当前两设备情况下的7个减为0。桌面与手机共享表格数据行均56 px，桌面工具栏64 px、表头44 px。[浏览器报告](D:/DOSO/artifacts/robot-cloud/console-20261110/browser-compact.json)

浏览器验证还覆盖纯文字快捷入口点击与Enter激活、设备搜索和空结果、详情遥测读取、40 px高级菜单展开、手机行操作菜单、独立横向滚动、抽屉开关、告警筛选/时间窗/分页、小屏长名称与文档宽度。一次紧接菜单关闭的滚动输入被消耗，等待浮层关闭后的独立复测滚至660 px，记录保存在报告中。临时视口覆盖已重置，浏览器保留在总览。其他浏览器、键盘完整遍历、真实设备上报与真实告警仍需独立验收。

[紧凑桌面总览](D:/DOSO/artifacts/robot-cloud/console-20261110/console-compact-desktop.jpg)、[紧凑设备表](D:/DOSO/artifacts/robot-cloud/console-20261110/console-compact-devices.jpg)、[紧凑手机总览](D:/DOSO/artifacts/robot-cloud/console-20261110/console-compact-mobile.jpg)。下节保留紧凑调整前内部版的构建与验收证据。

### 内部工作台重构（2026-11-10）

本轮调整总览设备卡、共享实体表格、侧栏MDI图标与白色工具栏。设备、告警、数据看板和客户列表继续复用原实体表格组件及页面配置。导航高级子项保持40 px，与原展开高度计算一致。测试使用既有实验租户与两条测试设备记录，数据来源和平台活跃语义按实际内容展示。

最终Angular生产编译295.030秒，于2026-11-10 14:08:17.711 UTC结束。发行目录为 `.build/20261110-140842-d98ce73b`，清单生成于14:11:42.8345514 UTC。补丁SHA256为 `69aa91ef9bb6d7c9a8753a913657d46fbb5cddbb5205244acabd8ddf6aa5cd6c`，镜像ID为 `sha256:e0ab6e85f914150dfdca3326a982e710b760c06adf8f71e4ba9473a56e3bd8a2`。归档补丁与源码、编译指纹和清单一致，源码逆向应用、源码差异空白检查、Compose配置及容器内 `nginx -t` 通过。本机18081已使用最终镜像。

最终HTTP/API/WebSocket 35项及193个静态资源全部通过，运行于2026-11-10 14:16:30.436–14:16:34.932 UTC。覆盖认证、设备列表/详情/搜索、历史时间点、原版与新入口数据一致性、错误响应、WebSocket属性订阅、字体与景观素材字节及许可文件。[最终HTTP报告](D:/DOSO/artifacts/robot-cloud/console-20261110/console-interior-http.json)

最终Chromium IAB四种尺寸22项检查通过，运行于14:16:59.531–14:26:59.953 UTC。尺寸为1440×900、1280×800、390×844和320×568。覆盖总览真实登记数与设备卡、MiSans、240 px侧栏/72 px工具栏、MDI资源、40 px高级菜单展开、设备搜索及空结果、排序、选择工具栏、详情遥测、主要页面入口、手机抽屉、告警筛选/时间窗/分页、表格横向滚动和登录往返。桌面表格可见数据行72 px，手机68 px。页面不产生文档级横向溢出，长表格由内部滚动容器承接。[最终浏览器报告](D:/DOSO/artifacts/robot-cloud/console-20261110/browser-interior.json)

登录页桌面左卡尺寸549.59375×572.484375 px、位置120/163.75 px，与已有基线差异小于0.1 px。手机左卡342×220 px、位置24/24 px。一次桌面测量发生于原入场动画期间，等待该动画结束后的复测通过。一次表格行查询包含隐藏详情中的0 px行，改用可见数据行复测通过。这两项验收查询修正保存在报告中。临时视口覆盖已重置，页面保留在总览。

候选版曾通过25项交互检查，发现顶部工具栏仍继承白色图标，最终通过限定选择器修复后重新构建、部署和验收。候选与最终发行物的96个JavaScript包逐字节一致，候选结果仅作历史参考。[候选报告](D:/DOSO/artifacts/robot-cloud/console-20261110/browser-interior-candidate.json)、[包对照](D:/DOSO/artifacts/robot-cloud/console-20261110/interior-bundle-comparison.json)

[桌面总览](D:/DOSO/artifacts/robot-cloud/console-20261110/console-overview-desktop.jpg)、[桌面设备表](D:/DOSO/artifacts/robot-cloud/console-20261110/console-devices-desktop.jpg)、[手机总览](D:/DOSO/artifacts/robot-cloud/console-20261110/console-overview-mobile.jpg)、[手机告警](D:/DOSO/artifacts/robot-cloud/console-20261110/console-alarms-mobile.jpg)。本轮运行证据覆盖上述页面和只读操作。其他浏览器、键盘完整遍历、reduced-motion运行时、真实告警生成与高级配置操作另行验收。云端新数据投递和真实机器狗上报的状态见下节。

### 私有数据通道恢复（2026-11-10）

本轮检查于2026-11-10 14:00:16.205 UTC结束。管理后台运行于本机 `http://127.0.0.1:18081`，Nginx代理本机Docker中的ThingsBoard，TB仅映射 `127.0.0.1:18080`。业务API、业务数据库与遥测投递程序运行于测试ECS，数据经私有SSH反向通道写入本机TB。

恢复前，ECS的 `doso-track-export.service` 与 `doso-track-tb-relay.service` 均为active/running，但Functions网络内的 `127.0.0.1:28080` 返回HTTP 000、curl退出码52。远端 `/home/admin/.doso-track/tb-3.sock` 属主admin、权限600，实际已无监听。核实后仅清理该失效socket，重建转发至本机18080的原私有通道，沿用既有relay与export服务。[恢复前证据](D:/DOSO/artifacts/robot-cloud/console-20261110/cloud-connection-audit-before-recovery.json)

恢复后，Functions网络内经桥接读取TB，未鉴权返回401，使用既有worker凭据返回200。socket重新监听，属主admin、权限600，两项ECS临时服务保持active/running。公网 `remote-state` 与 `remote-history` 未鉴权GET均返回401，`telemetry-export` GET返回405、未鉴权POST返回401。

本机TB、云端业务数据库与经云端桥接鉴权读取的最新记录一致，业务设备UUID为 `78ec077d-0cde-441b-bddc-022cfc850676`，标记为模拟设备。序号为38192，观察时间为2026-11-10T03:48:09.582Z，即北京时间11:48:09.582。业务快照中remoteEnabled和exportEnabled均为true，outbox为delivered 26888、cancelled 463，无pending或failed记录。因此本轮验证覆盖已有数据可读与通道恢复，新记录投递及真机上报仍待验收。TB内两个既有测试设备的platformActive均为false，该字段不能证明机器狗的物理上电或运动就绪。[恢复后证据](D:/DOSO/artifacts/robot-cloud/console-20261110/cloud-connection-audit.json)

临时通道由本机 `ssh.exe` 进程PID 88028维持，未配置自启动。关闭前先核对该PID仍是转发 `tb-3.sock` 的SSH进程，再执行 `Stop-Process -Id 88028`，避免PID复用后误停其他进程。当前使用依赖开发机在线、本机Docker中的TB和该SSH通道运行，独立常驻云平台部署仍待完成。

### 原左卡与延伸玻璃（2026-11-10）

用户明确要求恢复原左侧景观，当前以 [等大版浏览器记录](D:/DOSO/artifacts/robot-cloud/console-20261110/browser-aligned.json) 为左卡基线，核对图源、尺寸、构图、圆角与两行文字。右侧玻璃向左填满原列间距并藏入左图后28 px，移动布局向上藏入24 px，左图层级2、右侧层级1，右侧保留15 px辅助标题与0.75白底填充输入框。

开发预览使用18082，首次编译275.761秒，后续组件更新225.229秒。四种尺寸的左图边界与原基线一致，桌面为549.59×572.48 px、位置120/163.75 px，1280、960与390视口也分别匹配，报告内四项边界检查通过。开发服务已停止并确认18082无监听。[开发预览报告](D:/DOSO/artifacts/robot-cloud/console-20261110/browser-overlay-dev.json)、[开发预览截图](D:/DOSO/artifacts/robot-cloud/console-20261110/login-overlay-dev-desktop.jpg)。这些证据仅覆盖开发预览，生产版本结果分别记录如下。

最终 Angular 生产编译188.048秒，于2026-11-10 10:42:02.454 UTC结束。发行目录为 `.build/20261110-104222-a4caabfd`，清单生成于10:45:17.8783165 UTC。补丁 SHA256 为 `ded9a5150ed838f6387aad89172ffc5658d4037d83e41e0adfe5c8836c725405`，镜像 ID 为 `sha256:2821edda806aed5b6f83f54d983e4b908122d70c19df1db70d61474dc0b31972`。原始补丁与清单SHA一致，源码逆向应用与差异空白检查通过。本机18081部署、Compose与容器内 `nginx -t` 通过。

最终 HTTP/API/WebSocket 35项及193个静态资源通过，运行于2026-11-10 10:50:10.771–10:50:17.873 UTC，覆盖既有模拟设备只读接口、未授权错误、属性订阅及图片、字体和许可文件检查。[最终HTTP报告](D:/DOSO/artifacts/robot-cloud/console-20261110/login-overlay-http.json)

Chromium IAB生产浏览器25项检查通过，视口为1440×900、1280×800、960×720、390×844、320×568和900×500。前四种尺寸左图的left、top、width、height、right、bottom均与等大版基线差异小于0.1 px。桌面左图549.59375×572.484375 px、位置120/163.75 px，使用竖图、28 px圆角、48 px文字内边距与50%/60%裁切位置。手机左图342×220 px、位置24/24 px，使用横图、18 px圆角、24 px文字内边距与65%/50%裁切位置，原两行文字恢复。[生产浏览器报告](D:/DOSO/artifacts/robot-cloud/console-20261110/browser-overlay.json)

桌面右侧玻璃起点641.59375 px，比左图右缘669.59375 px向内28 px；移动下方玻璃顶部220 px，比图片底部244 px向内24 px。左图层级2高于玻璃层级1，右侧实际为0.4白底、20 px模糊、边框none，内容居中且最大宽380 px，辅助标题15 px。输入框实际为0.75白底及12 px圆角。六尺寸无横向溢出，320×568滚动175 px后找回入口底部515.5 px可达，900×500回顶部后左图顶部32 px、高240 px。

无效邮箱提示、密码显隐、Tab聚焦密码框、Enter登录总览与退出回登录页通过，临时视口已重置，空登录页保留。[最终完整桌面截图](D:/DOSO/artifacts/robot-cloud/console-20261110/login-overlay-desktop.jpg)、[最终移动截图](D:/DOSO/artifacts/robot-cloud/console-20261110/login-overlay-mobile.jpg)。以上记录属于技术验证，用户视觉反馈单独记录。愿景文字仍为试排，其他浏览器、reduced-motion与不支持背景模糊模式的运行时效果尚未实测，兼容样式仅检查源码。

### 登录页连续场景（2026-11-10）

该轮采用统一圆角主容器，山海背景贯穿左右，右侧为更透明的无外框玻璃整栏，「管理后台」收为辅助标题，输入框为填充样式。桌面使用横图，窄屏用竖图，图文与自然高度表单上下排列并共用背景。改动位于登录HTML、组件SCSS及现有全局DOSO样式的登录作用域。用户随后明确否定了扩大山海背景的视觉方向，要求恢复原左侧景观，以下仅保留当次技术证据。

候选浏览器检查发现上游主题将填充框背景强制改为透明，并叠加主色伪元素。最终在 `doso.scss` 的登录作用域复用表单背景和圆角token，覆盖主题层并移除两个输入框的旧 `tb-autofilled` 类。修复前候选HTTP结果保留为 [输入框修复前报告](D:/DOSO/artifacts/robot-cloud/console-20261110/login-merged-before-field-fix-http.json)。

最终 Angular 生产编译212.083秒，于2026-11-10 05:41:01.819 UTC结束。发行目录为 `.build/20261110-054123-a3264d3d`，清单生成于05:44:36.2361116 UTC。补丁 SHA256 为 `e35fa545175b93d851340768039b23d86016246441c6dcfdfdbcd8fb9eb2d0b0`，镜像 ID 为 `sha256:8b04f36c869df0bc0aa288450644f64afbf4ae6661a469e453361606527b8724`。原始补丁与清单SHA一致，源码逆向应用检查及三个改动源文件的差异空白检查通过。最终制品已部署到本机18081。

最终 HTTP/API/WebSocket 35项及193个静态资源通过，运行于2026-11-10 05:51:43.453–05:51:47.794 UTC，覆盖现有模拟设备只读接口、未授权错误、属性订阅及图片、字体与许可文件检查，报告内发行指纹与最终补丁一致。[最终HTTP报告](D:/DOSO/artifacts/robot-cloud/console-20261110/login-merged-http.json)

Chromium IAB实际浏览器22项检查通过，视口为1440×900、1280×800、960×720、390×844、320×568和900×500。1440视口主场景1200×620 px，背景完整覆盖容器，右侧玻璃宽540 px，实际样式为0.52白底、20 px模糊、边框none、圆角0、阴影none。「管理后台」为15 px、字重500，两输入框均为0.75白底、12 px圆角，主色伪元素none。桌面实际加载横图，手机加载竖图，移动图文区与下方自然高度表单共用背景，无横向溢出。320×568滚动129 px后找回入口底部515.5 px可达，900×500回顶部后内容顶部32 px，安全居中有效。[浏览器报告](D:/DOSO/artifacts/robot-cloud/console-20261110/browser-merged.json)

无效邮箱提示、密码显隐、Tab聚焦密码框、2 px焦点下划线、Enter登录总览、退出回登录页及重置密码入口跳转通过，重置密码请求未提交。临时视口已重置，最终截图为刷新后的空表单。最终验收使用Chromium IAB，[完整桌面截图](D:/DOSO/artifacts/robot-cloud/console-20261110/login-merged-desktop.jpg)、[移动截图](D:/DOSO/artifacts/robot-cloud/console-20261110/login-merged-mobile.jpg) 和 [场景区域截图](D:/DOSO/artifacts/robot-cloud/console-20261110/login-merged-scene.jpg) 均来自最终实际页面。场景截图直接截取页面区域。愿景文字仍为试排，其他浏览器、reduced-motion与不支持背景模糊模式的运行时效果尚未实测，相关兼容样式仅检查源码。以下等大布局数据保留为前一版历史。

### 登录页左右等大布局（2026-11-10）

本轮桌面改为两列等宽、拉伸同高，右侧玻璃卡片与左侧景观等大，内部 `.login-inner` 整体居中并最大宽380 px。网格采用 `repeat(2, minmax(0, 1fr))` 与显式 stretch，两卡 `min-width: 0`，移除原面板440 px上限。959 px及以下使用单列自然高度。首次候选的浏览器测量发现左图宽高比会在内容撑高时将左卡扩大到500 px，右卡为445 px。增加景观 `width: 100%` 后重新构建并完成下述最终验收。候选HTTP结果保留为 [修宽前候选报告](D:/DOSO/artifacts/robot-cloud/console-20261110/login-aligned-before-width-fix-http.json)。

最终 Angular 生产编译208.136秒，于2026-11-10 04:18:53.913 UTC结束。发行目录为 `.build/20261110-041913-4272336f`，清单生成于04:22:15.6270163 UTC。补丁 SHA256 为 `bf50351840f182049ba4066a6ab4d3da0f143e7e8df23209f5a08f1f3dcf5b5d`，镜像 ID 为 `sha256:bd74067fa97ad079cf643a820975d63bc95065bccbc39f77f8c69b58648a9c42`。实际补丁与清单SHA一致，逆向应用检查、源码差异空白检查、Compose 与容器内 `nginx -t` 通过。

最终 HTTP/API/WebSocket 35项及193个静态资源通过，运行于2026-11-10 04:27:08.484–04:27:14.893 UTC，覆盖既有模拟设备只读接口、未授权错误、属性订阅及图片、字体与许可文件检查。[最终HTTP报告](D:/DOSO/artifacts/robot-cloud/console-20261110/login-aligned-http.json)

Edge扩展通道的实际浏览器17项检查通过，视口为1440×900、1280×800、960×720、390×844、320×568和900×500。1440视口左右卡片均约549.6×572.5 px，顶部163.75 px、底部736.23 px对齐，圆角均为28 px。1280视口各547.20×570 px，960视口各398.41×521 px。三个桌面尺寸的内容组居中且最大宽380 px，无横向溢出。移动布局上下排列，选用横向图片。320×568滚动194 px后找回入口底部519.39 px可达，900×500回顶部后内容顶部32 px，安全居中有效。[浏览器报告](D:/DOSO/artifacts/robot-cloud/console-20261110/browser-aligned.json)

无效邮箱提示、Tab聚焦密码框、Enter登录总览及退出回登录页通过，22 px背景模糊保留，临时视口已重置。[双卡片区域截图](D:/DOSO/artifacts/robot-cloud/console-20261110/login-aligned-cards.jpg) 直接截取实际页面区域，完整 [桌面截图](D:/DOSO/artifacts/robot-cloud/console-20261110/login-aligned-desktop.jpg) 与 [移动截图](D:/DOSO/artifacts/robot-cloud/console-20261110/login-aligned-mobile.jpg) 含用户已安装Kimi扩展的悬浮按钮。愿景文字沿用试排文案。其他浏览器、reduced-motion与不支持背景模糊模式的运行时效果尚未实测，相关兼容样式仅检查源码。

### 白底蓝色流光与毛玻璃

本轮仅调整登录SCSS。画布为 `#FFFFFF`，两条 transform 蓝色流光周期为26秒和30秒。表单默认0.94白底，支持 backdrop-filter 时采用0.72白底与22 px模糊，桌面最大宽440 px、内边距36 px。沿用山海图、试排标语、低高度滚动与原认证绑定。正式 Angular 生产编译通过，构建阶段313.978秒，于2026-11-09 14:57:18.781 UTC结束，补丁 SHA256 为 `af3d9ceb425622713ed42b6849afc2b9fdb954b8d13e540448a5dc7b68b49793`。发行目录为 `.build/20261109-145800-bcdd484b`，Compose 与容器内 `nginx -t` 通过。补丁逆向应用检查、源码差异空白检查通过，当前补丁 SHA256 与发行清单一致，清单时间为2026-11-09 15:01:57.1645910 UTC。镜像 ID 为 `sha256:391d593d53ffc97ced6ee224e5d77f54519b608f4faa3574a4bc1a11d060ecba`。

HTTP/API/WebSocket 35项和193个静态资源全部通过，运行于2026-11-09 15:06:01–15:06:09 UTC，涵盖原图片、字体及许可三方 SHA256、NOTICE、登录、只读API、401/404与WebSocket回归。[玻璃登录页HTTP报告](D:/DOSO/artifacts/robot-cloud/console-20261109/login-glass-http.json)

Chromium IAB 浏览器19项布尔检查通过，视口为1440×900、390×844、320×568和900×500。实际计算样式为纯白画布、0.72白底和22 px背景模糊，4.2秒观察中两条流光 transform 均发生变化，装饰层 `pointer-events: none`，桌面及移动视口无横向溢出。320×568下滚动194 px后找回入口底部为519.39 px，900×500回顶部后内容顶部32 px，安全居中有效。[浏览器报告](D:/DOSO/artifacts/robot-cloud/console-20261109/browser-glass.json)、[桌面截图](D:/DOSO/artifacts/robot-cloud/console-20261109/login-glass-desktop.jpg)、[移动截图](D:/DOSO/artifacts/robot-cloud/console-20261109/login-glass-mobile.jpg)

无效邮箱提示、Tab聚焦密码框、Enter登录总览、退出回登录页与 MiSans Demibold 检查通过。reduced-motion 与不支持 backdrop-filter 的回退仅检查源规则，未模拟运行时媒体条件或不支持环境。本轮浏览器范围为 Chromium IAB，生产 OAuth 与真实硬件未验收。

### 山海星空登录页

本轮将四卡画板替换为 `figure`、`picture` 和 `figcaption`，桌面与窄屏分别使用竖向、横向山海星空素材。图像为内置 imagegen 生成的虚构景观，提示词保存在 [素材生成记录](D:/DOSO/artifacts/robot-cloud/console-20261109/vision-image-prompts.md)。试排文案分两行显示「探索未知」「拓展人类的可能」，尚未确认为正式公司愿景。源码保留原认证绑定、低高度滚动和安全居中，增加24秒两轮轻漂移与一次入场，reduced-motion 为静态样式。正式 Angular 生产编译通过，构建阶段308.048秒，发行目录为 `.build/20261109-135840-8e3f554f`，补丁 SHA256 为 `5ccb24647b8285bb6bbdc63d2ebce1b5ac895daf6857c0341231deceb23f8583`。Node v24.11.1、Yarn锁文件沿用下述基线。已完成本机镜像更新、`up -d --force-recreate` 与容器内 `nginx -t`，镜像 ID 为 `sha256:c05a1df620bad69054328f1166a76fe9cf6256336ec45244758bf72fed147367`。

最终版本 HTTP/API/WebSocket 35项及193个静态资源全部通过，运行于2026-11-09 14:12:41–14:12:54 UTC。新增两幅景观图的 HTTP/发行目录/源文件 SHA256、PNG标识与类型检查通过，保留四个 OTF、许可 PDF、NOTICE 和既有只读回归。[山海星空 HTTP报告](D:/DOSO/artifacts/robot-cloud/console-20261109/login-horizon-http.json)

真实浏览器检查覆盖1440×900、390×844、320×568与900×500。桌面实际加载 `horizon.png`，原图1122×1402，窄屏加载 `horizon-wide.png`，原图1859×846。实际画面已确认星空、山脉、海面和白字，原四卡元素数量为0。轻漂移在24秒、两轮样式下观察到缩放从1.015变为1.01569及小幅平移。[浏览器报告](D:/DOSO/artifacts/robot-cloud/console-20261109/browser-horizon.json)、[桌面截图](D:/DOSO/artifacts/robot-cloud/console-20261109/login-horizon-desktop.jpg)、[移动截图](D:/DOSO/artifacts/robot-cloud/console-20261109/login-horizon-mobile.jpg)

320×568视口真实滚轮后 `scrollTop=144`，找回入口底部为544 px。900×500下 `scrollTop=0`、顶部32 px，安全居中有效。错误邮箱提示、Tab聚焦密码框、Enter登录进入总览和退出回登录页通过，MiSans 保持正确。reduced-motion 仅检查源CSS静态规则，运行时媒体模拟未完成。

### Bento 登录页与动效

本轮源码采用浅灰画布、四格原创机器人 SVG/CSS 画板和右侧单一标题表单，保留 MiSans 与既有登录流程。新增入场、关节节奏、hover 及 reduced-motion 样式。正式 Angular 生产编译通过，构建阶段415.577秒，发行目录为 `.build/20261109-122207-b8b46f7d`。补丁 SHA256 为 `9306ea25efa151cd149519c4f9252e516d1b0df2509b5f60079d6b6553a925bd`，Node 与锁文件沿用下述基线。已完成本机镜像更新、`up -d --force-recreate` 与容器内 `nginx -t`。镜像 ID 为 `sha256:e51f755f8c650744c52cb802146eb3a4168cb762d74287fb01337537141470a5`。

最终版本 HTTP/API/WebSocket 33项及191个静态资源检查通过，运行于2026-11-09 12:32:09–12:32:18 UTC，涵盖既有只读回归、四个 OTF 与原许可 PDF 的 HTTP/发行目录/App 三方 SHA256，以及 NOTICE 署名。[Bento HTTP报告](D:/DOSO/artifacts/robot-cloud/console-20261109/login-bento-http.json)

最终浏览器检查覆盖1440×900、390×844、320×568和900×500。初次检查发现320×568下找回密码入口裁切，已补页面滚动与安全居中。最终真实滚轮操作后 `scrollTop=32`，入口底部为544 px，可在568 px高视口内到达，900×500下顶部位置也保持非负。直接浏览器截图保留桌面与移动布局。[浏览器报告](D:/DOSO/artifacts/robot-cloud/console-20261109/browser-bento.json)、[桌面截图](D:/DOSO/artifacts/robot-cloud/console-20261109/login-bento-desktop.jpg)、[移动截图](D:/DOSO/artifacts/robot-cloud/console-20261109/login-bento-mobile.jpg)

关节动效在4.2秒观察中由无变换进入旋转矩阵，周期为5.6秒、重复4轮。真实鼠标 hover 后画板位移为 `translate(-4px, -4px)`。错误邮箱提示、Tab聚焦密码输入、Enter登录进入总览、账号菜单退出回登录页均通过。MiSans 和 Material Icons 计算字体正确。reduced-motion 源样式已检查，工具不支持该次 CDP 媒体模拟方法，运行时媒体条件未验收。

参考站内容为 MP4 展示，原组件和动效源码未知。REA 返回部分捕获及 `cleanup_incomplete`，没有成功 Evidence ID。后续任务进程检查未发现 owned Chrome 残留。具体参考范围见 [观察记录](D:/DOSO/artifacts/robot-cloud/bento-reference-20261109/analysis.md)。

### 字体与登录文案调整

复用 App 的 MiSans Regular、Medium、Demibold、Bold 原始 OTF，400/500/600/700 字重统一用于主题和正文。原许可 PDF 与 NOTICE 署名随发行物保留。字体文件及许可与 App 来源的 SHA256 一致，局部 Sass 编译与字体断言通过。登录页删除重复的「管理后台」「登录DOSO管理后台」可见文案，保留表单可访问名称。

正式 Angular 生产编译通过，构建阶段245.695秒。[本轮发行清单](cloud/console/.build/20261109-100457-b0ad94c5/manifest.json) 的补丁 SHA256 为 `8db3a2e5b365270c5e2f630edf8358b7e30f647b7edebe7b5c5d8663acf4d061`，Node 与锁文件沿用下述基线。打包阶段 Docker Engine 曾退出，原因未确定。处理 socket 启动阻塞并恢复后，已用该发行目录完成镜像构建和 `up -d --force-recreate`，容器内 `nginx -t` 通过。镜像 ID 为 `sha256:c36fce30d6fa886ad2af019f5ab738948cb416001c0762bde626128f3edd932e`。

本轮 HTTP/API/WebSocket 检查为33项全部通过、191个静态资源全部可读取，运行于2026-11-09 10:32:23–10:33:05 UTC。覆盖初版27项，并增加四个 OTF、原始许可 PDF 的 HTTP/发行目录/App 原件三方 SHA256 与文件标识检查，以及 MiSans NOTICE 署名和许可路径检查。[HTTP检查报告](D:/DOSO/artifacts/robot-cloud/console-20261109/login-font-http.json)

真实浏览器使用现有账号完成登录→总览→设备页→账号菜单退出→登录页。两处重复文案已从可见文字删除，输入框、按钮与设备表头的计算字体为 MiSans。CDP 确认用户名标签实际使用 MiSans-Regular 渲染10个字形，`customFont=true`，图标计算字体仍为 Material Icons。视口为663×2364，多断点和高级页面未覆盖。[浏览器报告](D:/DOSO/artifacts/robot-cloud/console-20261109/browser-font.json)、[登录页截图](D:/DOSO/artifacts/robot-cloud/console-20261109/login-misans.jpg)

### 初版部署与接口验证

以下27项、187个资源及构建指纹属于字体与登录文案调整前的版本。

定制基于ThingsBoard CE 4.3.1.8，固定上游提交 `ebbfc6789b81a4e04cae2711208130fb2841b31f`。正式生产前端编译通过，构建阶段367.226秒。实际构建使用Node v24.11.1、Yarn 1.22.22，设置单构建worker和关闭并行TypeScript编译。源改动包括DOSO品牌、中文默认、导航、设备总览与列表样式。

可重复构建入口为 [build.ps1](cloud/console/build.ps1)，补丁归档为 [doso.patch](cloud/console/patches/doso.patch)。脚本检查源码改动与补丁完全匹配，成功编译后写入 `doso-build.json`。`-SkipCompile`在缺少编译记录时拒绝复用的行为已实际检查。生成的 [构建清单](cloud/console/.build/20261109-090556-c50e83e5/manifest.json) 记录上游提交、补丁与锁文件SHA256、Node版本和固定Nginx镜像。

本轮补丁SHA256为 `fafa8e6294d5072226234dfa59b620f99d257757df49b5d6c77febcd4ce090ea`，上游Yarn锁文件SHA256为 `45240db20dcd80e71a733f528c95295494f67d45b2ed9377a2113dc94fc84486`。

最终源码补全了3处DOSO修改声明后再次编译。本机独立前端已部署，打包使用 [发行清单](cloud/console/.build/20261109-091358-9bde3eb3/manifest.json)。`docker compose config --quiet`、`up -d --force-recreate` 和容器内 `nginx -t` 通过。容器 `doso-console-console-1` 为running，端口映射为 `127.0.0.1:18081 → 8080`，镜像ID为 `sha256:239b5bf8ea9d76122d34956c9801838800500158c262480be071e1e35a627978`。已核对进程用户为 `nginx`、根文件系统只读及 `cap_drop ALL`。

部署期间宿主重启后Docker Engine未自动恢复，经恢复Engine后完成最终镜像部署。既有ThingsBoard的18080首页返回200，未授权API返回401，PostgreSQL健康且沿用同一数据库卷。

最终镜像的 [HTTP/API/WebSocket检查报告](D:/DOSO/artifacts/robot-cloud/console-20261109/http-smoke.json) 为27项全部通过，187个静态资源全部可读取。实际检查覆盖下列内容。

- 首页、登录页、设备列表和详情的直接路由返回200，标题为DOSO管理后台，首页使用 `no-store`；缺失JS返回404，未授权API保留401及JSON响应。
- 现有账号通过真实登录API取得会话，读取租户管理员身份、设备列表、命中和空搜索、设备详情、模拟来源属性、两点历史及latest。看板、客户、告警列表API也返回200，当前数据为空。
- 新旧入口返回同一设备数据；`/api/ws`建立连接并收到一次设备属性订阅响应，读回模拟标记。
- 服务端发行清单与编译记录的提交、补丁、锁文件和Node版本一致；中英文locale资源都含DOSO修改声明。

数据来自既有模拟设备和历史点，本轮仅创建登录会话。按用户决定，本轮验收止于部署、HTTP/API及上述WebSocket属性订阅，浏览器显示、登录界面、搜索操作和退出交互暂缓。生产OAuth、公网部署、客户数据隔离和真实机器狗仍待各自验收。

## 遥测投递与实际平台（2026-11-08）

新增 `doso-cloud-telemetry` 的9项定向测试、Clippy和格式检查通过，Windows可执行程序构建通过。业务Edge入口及受影响鉴权/同步共31项Deno测试和类型检查通过。Compose配置、bootstrap/smoke脚本行为夹具通过；workspace metadata正确解析6个成员。

修正版Linux x86_64 release交叉构建成功，实际Linux容器验证ELF加载与缺少配置时安全退出；这是加载/启动边界检查，不等于目标云主机systemd验收。候选与锁文件/二进制校验见 [BUILD.md](BUILD.md) 和 [linux-smoke.json](D:/DOSO/artifacts/robot-cloud/telemetry-20261108/linux-smoke.json)。

本机运行固定摘要的ThingsBoard 4.3.1.8与独立PostgreSQL 18.1，完成初始化、私有凭据保存、默认密码更换、无邮件技术账号引导。实际 [API读回证据](D:/DOSO/artifacts/robot-cloud/telemetry-20261108/thingsboard-smoke.json) 核验两点历史、同点重试及较旧数据不倒退latest。平台只映射 `127.0.0.1:18080`，正式云端未安装本次组件。

[完整链路证据](D:/DOSO/artifacts/robot-cloud/telemetry-20261108/telemetry-chain.json) 使用实际业务SQL接受一个明确模拟设备的报告，经生产Edge handler、Rust程序写入真实TB，验证：原观察时间和完整location保留；重复sync不重复入队；读取失败只保存错误，不在该时刻生成正常状态；关闭许可后取消未发送记录。专用key缺失的401也在该链路断言中验证。PostgREST传输被白名单psql适配替换，未运行用户Auth登录、真实机器狗或手机App。

[数据库及集成总报告](D:/DOSO/artifacts/robot-cloud/telemetry-20261108/database-chain.json) 为41组通过（现有/扩展数据库37组、实际平台4组），临时PG、Deno和Rust进程均确认关闭，完整链路临时TB设备已删除。独立API smoke设备保留在本机隔离租户，标记为模拟。这些计数不能与历史基线简单相加当成不同测试数量。

首次完整链路发现TB查询范围为 `[startTs,endTs)`，原精确毫秒查询漏掉结束点；修正为 `[ts,ts+1)`，HTTP夹具按真实区间过滤，回归及实际链路均通过。首次失败证据保留于App `artifacts/multi-device/2026-11-08T05-47-26-015Z`，不能用后续成功抹去当时失败。高并发构建曾内存不足，单任务 `-j 1` 构建恢复成功。

可重复入口：先按 [telemetry README](cloud/telemetry/README.md) 准备隔离平台与私有环境；在App工程设置 `DOSO_THINGSBOARD_URL/KEY` 和 `DOSO_TELEMETRY_INTEGRATION_HOOK` 指向 [verify-telemetry-live.mjs](<C:/Users/99563/Documents/ChatGPT/New project/doso-app/scripts/verify-telemetry-live.mjs>)，再运行既有 `node scripts/verify-multi-device-ownership.mjs`。此hook仅允许本机平台，使用本机已有Deno和PostgreSQL工具链，不发布生产SQL或访问机器人。未设置hook时，原runner只做数据库验证。

运行代码检查：

```powershell
cargo test -p doso-cloud-telemetry --locked --offline -j 1
cargo clippy -p doso-cloud-telemetry --all-targets --locked --offline -- -D warnings
cargo fmt -p doso-cloud-telemetry -- --check
cargo build -p doso-cloud-telemetry --locked --offline -j 1
```

## 运行位置目录重组验证（2026-11-07）

源码迁入 `robot/`、`cloud/`、`shared/`、`tools/` 后，沿用原有测试完成以下检查：

- `cargo metadata --locked --offline --no-deps` 正确解析5个workspace成员及其新路径。
- `cargo build --workspace --locked --offline` 成功；`cargo test --workspace --locked --offline` 为61项通过，1个既有子进程夹具测试保持ignored。
- `cargo fmt --all -- --check` 通过。当次构建提示lab中的三个既有辅助方法未使用；该目录整理未调整其行为，后续源码变化需单独核对。
- `cargo run --locked --offline -p lab -- check` 通过，覆盖许可、查询、状态变化、读取失败、关闭、到期、重连、模拟视频源启停和本地关闭持久化。真实Gateway、MQTT和云连接进程使用回环网络，Brainstem为模拟源，模拟读取8次；退出后本轮13146监听已释放。
- 74个本地Markdown链接和7个主要源码入口有效。与迁移前87个源文件的快照对照，无文件遗漏；非文档内容只调整Cargo路径及lab目录定位、模板路径和格式。

未运行安装脚本、重新交叉编译Linux发布包或部署云端/机器狗。迁移前源码快照位于 `D:\DOSO\archive\robot-cloud\20261107-layout\source-before-225159.zip`；历史发布包继续按其原目录使用。

## 本机准备

在 `D:\DOSO\robot-cloud` 根目录使用 PowerShell。Rust 依赖由 `Cargo.lock` 锁定，外部工具需要 Mosquitto 2.1.x、对应 ACL 插件和 OpenSSL。

```powershell
$env:MOSQUITTO_BIN = 'D:\DOSO\toolchains\mosquitto-2.1.2\mosquitto.exe'
$env:MOSQUITTO_ACL_PLUGIN = 'D:\DOSO\toolchains\mosquitto-2.1.2\mosquitto_acl_file.dll'
$env:OPENSSL_BIN = 'C:\Users\99563\miniconda3\Library\bin\openssl.exe'
cargo build --locked -p doso-cloud-link -p doso-cloud-gateway
```

Linux 使用本机安装的工具路径设置相同环境变量，ACL 插件通常为 `mosquitto_acl_file.so`。未设置时，工具从 PATH 查找 Mosquitto / OpenSSL，从 Mosquitto 所在目录查找 ACL 插件。生产程序默认读取本工作区 `target/debug`，设置 `CARGO_TARGET_DIR` 时使用该目录的 `debug` 输出。

本地模拟服务监听回环，`13146`被占用时失败；不要为重跑模拟而停止正在运行的他人服务。每轮临时证书、日志和配置写入本工程 `.runtime/lab-*`，其中包含本轮开发凭据，目录限制为当前用户访问，结束时关闭所拥有的子进程并清理本轮目录。`public`和`uplink`可参与外部联调，按对应章节明确配置。

## 自动检查本地链路

设备证书和主题权限可单独检查：

```powershell
cargo run --locked -p lab -- auth-check
```

该检查在回环地址启动独立的真实 Mosquitto，生成临时 CA 和设备证书，检查合法证书可向自己的主题发布、无证书和不受信任证书被拒绝、合法设备向其他设备主题发布被拒绝。它使用同目录的 `mosquitto_pub` 和 MQTT 5 QoS 1 回执，不连接真实设备或公网，不能代替证书签发、吊销或真机验收。

历史环境说明：2026-11-06运行 `auth-check` 时曾因锁文件与当时清单不一致，在独立副本中完成验证。随后11月7日原工作区已完成 `--locked --offline` 构建、测试和 `lab check`；旧锁文件问题不再作为当前操作指引。本次文档核对未重跑 `auth-check`。

```powershell
cargo test --workspace --locked
cargo run --locked -p lab -- check
```

`check` 创建临时证书、签名密钥和设备登记，启动真实 Rust Gateway、云连接服务与 Mosquitto，验证以下行为后退出并关闭本轮服务。

- 用户许可缺失时返回 HTTP 403，模拟 gRPC 不发生读取。
- 用户许可经设备确认后，HTTP 200 经 `Gateway → MQTT/mTLS → 云连接服务 → 模拟 gRPC → 签名回复 → Gateway` 返回模拟状态。
- 未带服务凭据返回 HTTP 401。
- 设备登记禁用后返回 HTTP 403，模拟 gRPC 读取次数保持不变。

随后同一 `check` 流程恢复测试设备登记，验证新的持续状态路径：

- 没有采集许可时不会自动读取；有效许可被设备确认后，从模拟 GROUNDED 变化到 STANDING，报告序号推进。
- gRPC 读取失败单独报告错误，最后成功状态保留原始时间，`fresh=false` 与 `reachable=true` 可以同时成立。
- 关闭收到设备确认后停采，旧许可返回 409，已关闭设备的按需查询返回 403。
- 短期许可到期停采，过期会话不能续租；云连接服务重启产生新连接编号，旧连接许可不能重新开启。
- 本地选择文件关闭后，正在上报的云连接进程退出，读取次数停止；保留同一份关闭文件重启时输出 `cloud_off`，不连接 MQTT。

模拟状态为 `GROUNDED`、控制方 `NONE`、电机输出关闭、无关键电机故障。开发工具输出模拟标识，生产协议沿用 [CONTRACT.md](CONTRACT.md)。

## 手工请求本地环境

第一个终端保持运行。

```powershell
cargo run --locked -p lab -- local
```

复制输出的 `client.json` 绝对路径，在第二个 PowerShell 终端执行下列命令，将路径替换为本轮输出。

```powershell
$client = Get-Content '.runtime\lab-本轮UUID\client.json' -Raw | ConvertFrom-Json
Invoke-RestMethod -Method Post -Uri $client.url -Headers @{ Authorization = "Bearer $($client.apiKey)" }
```

请求 body 为空。`local` 默认没有用户许可，因此本次查询应返回 HTTP 403；要验证设备确认许可后的 HTTP 200，使用上面的 `lab check`，或运行 `lab demo` 并在模拟业务确认开启后使用它输出的 `client.json`。`simulated` 标识说明使用模拟 Brainstem，`url` 和 `apiKey` 仅供本轮本地请求。返回第一个终端按 `Ctrl+C` 结束服务。

## 后台自动同步联调

使用同样的本机工具配置和构建步骤，运行：

```powershell
cargo run --locked -p lab -- auto-check 'D:\DOSO\artifacts\robot-cloud\auto-sync-wire.json'
```

此入口运行真实 Gateway 自动同步任务、MQTT 和云连接服务；业务 API 使用 Rust 内存模拟，Brainstem 也为模拟源，不接真实数据库或账号服务。模拟源采用 8 秒许可和 4 秒续期阈值加速检查，生产合同仍为 30 秒／15 秒。

检查默认关闭零采集、用户只开启一次后的自动续期与最新状态保存、云连接服务重启后自动恢复、业务源不可用时到期停采，以及用户关闭后拒收旧状态和停采。命令不会用管理 PUT 冒充自动续期，也没有 App 保活定时器。

可选输出文件只保存真实 Gateway 发出的同步请求/响应 body 供业务侧验证格式，不保存 apikey、证书或私钥。这里的模拟保存不能替代真实数据库事务、托管鉴权或 App 端到端验收。

## 公网模拟联调

本节是独立公网模拟的操作入口；2026-11-07的实际Web App流程使用本机MQTT与SSH隧道，单独记录在 [SIMULATED_FLOW.md](docs/SIMULATED_FLOW.md)。二者不能互相替代。

历史链路为 `HTTPS → ECS Nginx → Rust Gateway → 公网 MQTT/mTLS → 本机云连接服务 → 模拟 Brainstem`。

公网运行前，需要运维重新准备短期设备证书、签名密钥、设备登记及主题 ACL。历史测试身份已清理，旧 UUID 与配置不能直接作为在线设备复用。私有准备目录位于 `D:\DOSO\private\robot-cloud\cloud-stage`，接入边界见 [tools/deploy/README.md](tools/deploy/README.md)。

`link.json` 是字符串值的 `DOSO_*` 配置对象，字段对应 [robot/env.example](robot/env.example)。其中 `DOSO_CLOUD_ON` 必须为字符串 `"true"`，证书、设备签名私钥及网关公钥使用实际路径。旧配置缺少新变量时可继续使用 `DOSO_REMOTE_ENABLED`，新变量优先。工具拒绝包含 `DOSO_GATEWAY_API_KEY` 或 `DOSO_REMOTE_SYNC_KEY` 的云连接服务配置；查询凭据独立放在查询配置中，同步凭据仅供 Gateway 使用。

还须指定 `DOSO_CLOUD_CHOICE`，指向本轮私有模拟配置目录内的选择文件，内容仅为 `true` 或 `true` 加 LF。该持久化输入不能替代正式用户身份与本地授权；真实设备的可信写入入口尚未实现。启动开关即使为 `true`，缺失或无效的本地选择仍会阻止联网。

完成准备并核对目标Gateway制品后，在两个终端分别运行，将示例路径和UUID替换为本轮私有配置。当前源码查询需要业务侧有效许可并由设备确认；旧公网成功记录不能证明目标服务器已运行同一版本。

```powershell
cargo run --locked -p lab -- public 'D:\DOSO\private\robot-cloud\cloud-stage\link.json'
cargo run --locked -p doso-cloud-gateway --example query -- 'D:\DOSO\private\robot-cloud\cloud-stage\query.json' '本轮设备UUID'
```

第一个终端保持模拟服务和云连接服务运行，第二个终端使用真实 Rust HTTPS 客户端。查询配置包含 `baseUrl`、`apiKey`、`caFile`，相对 CA 路径按配置文件所在目录解析。查询结束后对第一个终端按 `Ctrl+C`，随后由运维清理云端临时设备登记、公钥及主题 ACL。

此流程会连接已配置的公网服务。目录迁移本身只调整本地工程，ECS 保留现有 Mosquitto、Rust Gateway、Nginx 部署。

2026-11-04 新版 Gateway、业务同步和报告主题 ACL 已部署到 ECS。云连接服务与 Brainstem 本机只读入口仍待真机发布；Linux 包的具体版本见 [BUILD.md](BUILD.md)，历史模拟结果不代替真实上云验收。

## 2026-11-03历史验证记录

以下保留各次运行当时的源码、环境和结论。诸如“待接线”“未部署”描述该历史时点；当前完成程度见本文顶部及 [PROGRESS.md](PROGRESS.md)。

2026-11-03 后台自动同步验证：Gateway 原 17 项定向测试及新增“单设备许可过期不阻断另一台关闭”回归通过，模拟业务模型边界测试通过，工作区构建/格式/Clippy 通过。`lab auto-check` 实际验证了一次开启后自动跨首份许可到期、状态保存、Agent 重启自动恢复、业务源 503 后到期停采及恢复，以及一次关闭后拒旧报告和停采。

本轮使用 8 秒／4 秒的加速模拟许可，生产仍为 30 秒／15 秒。业务 API 与 Brainstem 为模拟，Gateway、MQTT 和 Agent 为真实进程；退出后进程和临时凭据已清理。证据：[自动运行结果](D:/DOSO/artifacts/robot-cloud/auto-sync-result.json)、[运行日志](D:/DOSO/artifacts/robot-cloud/auto-sync-check.log)、[五阶段真实同步报文](D:/DOSO/artifacts/robot-cloud/auto-sync-wire.json)。实际业务数据库、托管鉴权和 App 验收以业务侧独立记录为准，不能由模拟存储替代。

同日完成工程标准收敛：共享计数边界、会话身份比较和报告匹配只维护一份，HTTP/JSON 契约不变。受影响三个包的 31 项单元测试（Agent 14、Gateway 12、shared 5）、格式与 Clippy 通过；未重跑无变化的 Broker/公网基线。

2026-11-03 持续状态扩展已完成本地验证：

- 三个包定向单元测试合计 30 项通过：Agent 14、Gateway 12、shared 4。
- 工作区构建、格式检查、全目标 Clippy 通过。
- `lab check` 使用真实 Rust Gateway、MQTT/mTLS、Rust Agent 和模拟 Brainstem，通过旧查询兼容、缺少凭据、登记禁用、许可确认、状态变化、读取失败、关闭、到期和重连行为检查。
- 实际报告从 GROUNDED 更新为 STANDING，序号递增；读取失败不改变最后成功采集时间；关闭后旧许可 409、按需查询 403；到期和重连都不自动恢复采样。此次模拟总读取次数为 7。
- 退出后确认无本轮 Lab/Agent/Gateway/Broker 进程及临时 Lab 目录。可查看 [运行日志](D:/DOSO/artifacts/robot-cloud/reporting-check.log) 与 [报告样例](D:/DOSO/artifacts/robot-cloud/reporting-result.json)。

该次新增功能当时未安装或部署，未连接机器狗或启用5G。该记录尚不包含后来完成的业务许可事务、数据库写入与Web App演练，不能将通信层模拟本身当成这些业务能力的证据。

2026-11-03 迁移前的记录包括 14 项 Rust 单元测试，以及本地和公网模拟链路的 HTTP 200、401、403。公网独立记录保存在 [tools/deploy/RESULT.md](tools/deploy/RESULT.md) 与 [tools/deploy/query-result.json](tools/deploy/query-result.json)，它们描述迁移前运行结果。

2026-11-03 在新目录完成本次迁移验证：

- `cargo build --workspace --locked` 成功；`cargo test --workspace --locked` 的 14 项单元测试全部通过。
- `cargo run --locked -p lab -- check` 通过 HTTP 200、401、403 三项检查，模拟 `GetControlStatus` 总读取次数为 1。
- `lab local` 启动后按 Ctrl+C 退出，确认本轮子进程、私有临时目录和 13145 监听均已清理。Windows 终端因 Ctrl+C 返回 1，清理结果通过进程、目录和监听检查确认。
- `cargo fmt --all --check`、`cargo clippy -p lab --no-deps -- -D warnings` 通过。新增工具使用当前依赖，没有 Node.js 或旧实验目录运行依赖。

本次没有重新连接公网；`lab public` 已编译，公网成功记录仍属于迁移前那次模拟验证。

上述历史运行未覆盖真实机器狗、目标机Agent启动、5G、App账号/设备归属/客户许可及运行中的MQTT连接撤权。后续模拟设备Web演练补充了账号与业务流程证据，真机和完全断云仍待验收。迁移前Node脚本保存在 `D:\DOSO\archive\robot-cloud\20261103`，当前源码与测试入口由本独立工程提供。


## 本地版本管理基线（2026-11-09）

本轮在 `D:\DOSO\robot-cloud` 初始化独立本地 Git 仓库，并补充本地凭据、缓存和工具状态的忽略规则。忽略边界在隔离仓库及实际仓库分别验证；未配置远端或推送。

`cargo fmt --all -- --check` 通过。首次直接使用现有 `target/` 执行离线工作区测试时，正在运行的 `doso-cloud-link.exe` 占用文件，Windows 返回拒绝访问（os error 5）；本轮没有停止该进程。改用独立的 `D:\DOSO\build-cache\robot-cloud-git-baseline-20261109` 构建目录后，`cargo test --workspace --locked --offline -j 1 --quiet` 实际通过 77 项，1 项保持 ignored。

为保护控制台构建记录使用的补丁指纹，`.gitattributes` 禁止 `cloud/console/patches/doso.patch` 的换行转换。在 `core.autocrlf=true` 和 `false` 下分别使用 `git checkout-index` 恢复，文件 SHA-256 均与原件一致。

本次为本地代码和测试验证，不代表云端部署、真实机器狗或视频端到端验收。

## 账号登录 IP 自查（2026-11-12）

本轮核对了 App 真源 `C:\Users\99563\Documents\ChatGPT\New project\doso-app` 的迁移、Edge Function、会话服务、AuthContext 和账号安全页：

- `capture_login` 不接收 `ip`/`ipAddress`/`p_ip`，服务端 RPC 只按当前认证用户和 JWT `session_id` 从 `auth.sessions` 取 `created_at` 与 `ip`，按 session ID 幂等写 `account.login`。
- `list_login_history` 只按当前 `target_user_id` 查询，倒序最多 50 条，返回 `id`、`loggedInAt`、`ipAddress`（可为 `null`）和 `method`（当前为 `null`）；账号安全页显示本人记录，未知值显示“未记录”。
- App 本地回归命令：

  ```powershell
  node --test --test-timeout=15000 scripts/session-service-response.test.mjs scripts/account-security-navigation.test.mjs
  ```

本次运行 46/46 通过。另以项目锁定的 Deno 2.5.6 离线运行器执行 `supabase/functions/tests/shared_test.ts`，15/15 通过；这些结果覆盖账号安全页、会话服务和 Edge 源码约束，不连接真实 Supabase，不证明线上 IP 来源可信。

**发布边界：** 本轮源码和本地回归完成，未部署 `20261112020000_add_login_history.sql` 或新版 `account-sessions`，未完成真实账号登录、真实 JWT 读回或公网代理链验收。仓库中的 Nginx 模板已覆盖客户端伪造的 `X-Forwarded-For`、注入 `Sb-Forwarded-For` 并将网关限制在 loopback；真实部署仍需核对拓扑并禁止绕过入口。GoTrue/Auth 只在可信链配置正确后才能提供可信 `auth.sessions.ip`。

登录 IP 与机器人 MQTT IP 的验证不能合并：本轮没有 MQTT Broker 连接事件采集或真实机器人接入，不能把账号登录 IP、Gateway HTTP 调用方或 GNSS 坐标当作机器人出口 IP 证据。生产前还需用真实 JWT 验证本人隔离、伪造 XFF、未知 IP、重复 capture 幂等和 50 条上限。
