# 云端系统：当前进度与产品推进

**2026-11-15 15:45 CST 现场只读复核：** 机器狗 `192.168.66.64` 当前可达，`brainstem.service`、`doso-cloud-link.service`、`doso-robot-authorizer.service`、`go2rtc.service` 和 `lt-video-gateway.service` active；`127.0.0.1:13146`、`13148` 与视频 `5050` 监听，正式 `lt-host/5052` 仍 inactive/未监听。使用机器人现有 Dart gRPC 客户端对 `127.0.0.1:13146/GetControlStatus` 连续读取 20 次、每秒一次，20/20 成功，均为 grounded、输出关闭、控制方 none、无严重电机故障、无 gRPC 租约。该窗口证明本机只读状态接口短时稳定，不证明 reporting permission、云端 Gateway 投影、正式 `5052` 或 App 可读状态已完成；没有发送运动/电机/CAN 命令。[20 次读取证据](artifacts/robot-cloud/real-connect-20261113/real-readonly-status-loop-20261115T0745Z.json)

**2026-11-15 17:16–18:23 CST 云端只读快照：** 阿里云 ECS 云助手确认 Gateway、Mosquitto 和 Nginx active，443/8130/8883 监听正常；systemd drop-in 的权威 reporting 路径 `/var/lib/doso-cloud/reporting.json` 尚不存在，但源码 `Reporting::load` 对首次启动的 `NotFound` 会按空许可状态处理，因此这本身不是 `GET /status/latest` 503 的充分原因。进一步确认研发 UUID 已 `enabled=true`、公钥文件存在且路径匹配；线上 Gateway 二进制 mtime 为 2026-11-04，早于当前 11月13日源码基线，需核对线上制品版本/实现后再解释 503。没有修改线上文件、重载服务、绑定账号或输出密钥。[reporting 路径快照](artifacts/robot-cloud/real-connect-20261113/cloud-gateway-reporting-path-snapshot-20261115T095940Z.json) · [登记/制品快照](artifacts/robot-cloud/real-connect-20261113/cloud-gateway-registry-binary-snapshot-20261115T102328Z.json)

当前源码的 Gateway reporting HTTP 边界回归 `1 passed`；本机无法生成可部署的 Linux 新 Gateway（缺少 `x86_64-linux-gnu-gcc` 与 `cargo-zigbuild`），所以没有上传或替换线上制品。[制品审计](artifacts/robot-cloud/real-connect-20261113/cloud-gateway-binary-audit-20261115.json)

随后改用 WSL2 Ubuntu 完成当前源码的 Linux release 构建：ELF x86-64、最高 GLIBC 2.34，SHA256 `eb25d3521246a693e4d769146164da30ccc40562afeed2c47b61766e902fd0ad`。它尚未上传、安装或重启线上 Gateway；下一步是按部署门核对配置备份、上传审批和 `/status/latest` 回归。[当前 Linux 制品](artifacts/robot-cloud/real-connect-20261113/cloud-gateway-current-linux-build-20261115.json)

云端 Gateway journal 只读筛选仅看到 `robot_cloud_gateway_listening`，没有启动错误或 reporting 活动日志；服务能启动，但当前线上旧制品没有给出状态投影原因。[journal 快照](artifacts/robot-cloud/real-connect-20261113/cloud-gateway-journal-snapshot-20261115.json)

候选 Gateway 的上传、哈希校验、原子替换、回滚和只读 `/status/latest` 验收顺序已整理为 [部署 runbook](docs/GATEWAY_CANDIDATE_DEPLOYMENT_20261115.md)，目前只准备未执行。

**2026-11-14 当前交付状态：** [真实机器狗云连接交付状态](docs/DELIVERY_STATUS_20261113.md)。云端只读登记已通过阿里云 ECS 云助手启用（设备 UUID `d82a831a-2e86-4b46-975b-3d42473583b7`，远程运控仍关闭）；机器狗曾通过 authorizer 开启 cloud-link 并建立 MQTT TCP 会话，但研发库存未绑定账号，业务同步返回 `permission:null`，没有形成状态上报。设备随后再次不可达。Rust Gateway 的按需查询和 `status/latest` 投影返回 `503 unavailable`，当前结论是“传输层通过、业务许可和状态投影待完成”。[现场证据](artifacts/robot-cloud/real-connect-20261113/real-cloud-activation-20261114.json) · [查询证据](artifacts/robot-cloud/real-connect-20261113/gateway-query-20261114.json) · [latest 证据](artifacts/robot-cloud/real-connect-20261113/gateway-latest-20261114.json)

**2026-11-14 真实云连接窗口补充：** 设备在窗口开始时可达，`13146` 只读状态为 grounded、输出关闭、控制方 none；通过本机 authorizer 开启后，`doso-cloud-link` active，MQTT TCP 到 `8.153.169.177:8883` established，日志出现 `cloud_connected`。业务同步对该 UUID 返回 `permission:null`，研发库存尚未绑定账号，Gateway 未下发 reporting lease，所以 `status` 与 `status/latest` 仍为 `503 unavailable`。设备随后再次离开局域网，云状态上报、App 读取和关闭回退保持待验收。没有发送运动命令或修改 CAN。[现场证据](artifacts/robot-cloud/real-connect-20261113/real-cloud-activation-20261114.json)

**2026-11-15 开机恢复检查：** 机器狗恢复可达，Brainstem 和 13146 只读状态通过，`choice=true` 已保留。原有 `doso-cloud-link.service` 为 disabled，已执行 `systemctl enable --now`，当前 enabled/active，MQTT TCP 已建立。预检的 USB 映射要求仍是旧路径，当前观察为 `can0→1-1.2`、`can1→1-2.1`、`can2→1-1.3`、`can3→1-1.4`，四路 CAN 实际均 UP/ERROR-ACTIVE/1Mbps。下一步是 App 绑定账号并生成短期 reporting permission，再验证 Gateway/App 状态上报。没有发送运动命令或修改 CAN。[开机证据](artifacts/robot-cloud/real-connect-20261113/cloud-link-enable-20261115.json)

**2026-11-15 视频验收补充：** 机器狗的 Orbbec、go2rtc relay 和局域网 WHEP 查看器已有只读帧证据；Rust SDK 通过临时只读桥收到单帧及连续 10 帧 JPEG，App 视频合同回归为 60/60。正式 SDK Gateway `5052` 仍未监听，隔离视频包已完成真机局域网接收，但主 App 控制页和公网 WHEP 路由仍未验收。分层结果与下一步见 [视频链路验收状态](docs/VIDEO_ACCEPTANCE_20261115.md)。

**2026-11-15 视频部署修复：** 新鲜只读复核发现 `/etc/go2rtc/go2rtc.yaml` 的 `cam` producer 先为空，恢复后又引用了机端不存在的 `lingtu_camera_webrtc`。已备份并切换到机端现有的 `lingtu_camera_go2rtc_mpegts.py`，只重启 `go2rtc.service`；WHEP 201、640×480 解码、连续 5 个有效且不同的 JPEG 均通过，查看器退出后编码子进程自动回收。没有改 Brainstem/CAN、电机、运动命令或云连接。[最终验收证据](artifacts/robot-cloud/real-connect-20261113/video-producer-pdeath-20261115.json)

**2026-11-15 部署准备补充：** 通过 SSH 只读维护窗口在机器狗源码 build tree 生成了 `lingtu_gnss_dds` ARM64 候选（未安装、未启动、未改服务）；现有 `lingtu_camera_dds` 已存在。完整 native release 仍缺导航、driver、SLAM、maps 等组件和标准 `install/` 树，正式 `lt-host/5052` 不能由当前 video probe 替代。Rust robot-cloud workspace 本轮 `95 passed, 1 ignored, 0 failed`。[构建记录](../artifacts/robot-cloud/real-connect-20261113/remote-gnss-build-20261115.json) · [部署门](docs/FORMAL_GATEWAY_5052_DEPLOYMENT_20261115.md)

正式 Gateway 预检也已补入 `scripts/deploy/thunder/verify_gateway_grpc_preflight.py`：manifest 的 `requiredLibraries` 是唯一库清单，独立 gRPC key 不能复用 HTTP key，TLS 权限、session 和 `lt-host` 安装状态缺一即失败。该脚本只读，不启动服务、不创建 session；在机器狗 Linux 环境运行配套测试为 2 项通过。正式 native release 和 `lt-host` 仍未形成。

本轮又在同一源码 build tree 生成 `lingtu_dds_probe` 和 `livox_sdk2_stream`；`lingtu_driver` 仅完成 CMake 前置后因目标机缺少 `BrainstemClientConfig.cmake` 0.6 停止。所有构建均未安装、未启动或替换现场服务。[ARM 构建记录](../artifacts/robot-cloud/real-connect-20261113/remote-native-builds-20261115.json)

随后尝试只构建导航 endpoint（关闭测试执行），约 60% 编译期间机器狗 SSH/ping 暂时不可达，未能读回最终结果；没有重试、重启或安装。该窗口单独记为不确定，不把中间进度写成导航构建通过。[导航构建记录](../artifacts/robot-cloud/real-connect-20261113/remote-nav-build-20261115.json)

逐项交付审计见 [DELIVERY_AUDIT_20261114.md](docs/DELIVERY_AUDIT_20261114.md)。

本地回归补充：当前工作区 `cargo test --workspace --locked --offline -j 1` 为 94 passed、1 ignored、0 failed，格式检查通过；未执行真机变更。另发现 authorizer ARM64 本地重建 SHA256 与既有部署记录不一致，已暂停制品一致性声明，待机器狗恢复可达后只读核对远端二进制。[哈希审计](artifacts/robot-cloud/real-connect-20261113/authorizer-hash-audit-20261114.json)

App/业务合同补充：App 远程状态相关 Node 测试 30/30 通过，Supabase `remote-state-user` 与 `remote-state-sync` Deno 测试 26/26 通过；当前客户端会把 Gateway `503 unavailable` 显示为不可用/等待状态并保留合法的上次成功时间，不会伪造在线状态。该结果是源码合同证据，仍不替代真实机器狗上报验收。

制品准备补充：已固化待部署的 ARM64 authorizer 发布包和 manifest，远端部署状态保持 pending；不会用未核对的旧哈希开启机器狗。[发布 manifest](artifacts/robot-cloud/releases/20261114-authorizer-aarch64/manifest.json)

发布包自校验已通过：manifest 中的 binary/package SHA256 与实际文件一致；这仍是本地制品证据，不代表远端已部署。[自校验证据](artifacts/robot-cloud/real-connect-20261113/authorizer-release-self-check-20261114.json)

**2026-11-13 本轮交付记录：** [真实机器狗云连接交付状态](docs/DELIVERY_STATUS_20261113.md)。Rust SDK 124 项、robot-cloud 工作区 84 项通过且 1 项 ignored，格式与 Clippy 通过；本机模拟云链路和 MQTT ACL 检查通过，远程关闭持久化和本地重新开启已补齐。现场 G1 窗口在候选启动前曾暴露 `usbbus2 errno=6`、`can1` USB 断开和 CAN link DOWN，旧服务停止超时并进入 failed；随后当前 flowfix11 的映射检查恢复，`127.0.0.1:13146` 已真实读通。公网 MQTT TLS 已通，真实 Gateway 曾对研发 UUID 返回 `403 disabled`；该历史状态随后已通过 ECS 云助手修正，不能把历史拒绝写成当前登记状态。

**11:36 CST 只读复核：** 现场状态没有自行恢复：`brainstem.service=failed`、`13145/13146/5052` 无监听、`doso-cloud-link` inactive，`can0/2/3` UP 而 `can1` DOWN/STOPPED。没有执行 start/stop 或 CAN 配置。[复核证据](artifacts/robot-cloud/real-connect-20261113/field-recheck-20261113T113631Z.json)

**12:34 CST 只读前置检查：** 新增的 `field-preflight.sh` 已在真机运行并安全拒绝继续；现有 `thunder-can-up.py` 要求 `can0→USB 1-2`，但实际 `can0` parent 为 `1-1.1`，同时 `can1` 仍 DOWN。没有执行 `ip link set`、服务启动或候选激活。[前置检查原件](artifacts/robot-cloud/real-connect-20261113/field-preflight-result.json)

**21:29 CST 最新只读复核：** `brainstem.service` 已 active，`13145` 可达，四路 CAN 均 UP/ERROR-ACTIVE/1 Mbps，IMU 存在；`13146`、cloud-link 和 grpc proxy 仍未运行。预检仍退出 1，因为 `can0→USB 1-1.1` 与既有脚本要求的 `can0→USB 1-2` 不一致；未执行 CAN 配置或服务动作。[最新原件](artifacts/robot-cloud/real-connect-20261113/field-preflight-result.json)

**用户确认 CAN 恢复后的只读复核：** 当前 Brainstem 为 `flowfix11_prone_20261113_70e3b574`、PID 86758，13145 active；四路 CAN 均 UP，但既有 ROUTES 检查仍拒绝 `can0→USB 1-1.1`。没有切换当前 Brainstem、修改 CAN 或启动 13146。[复核原件](artifacts/robot-cloud/real-connect-20261113/failure-inspection.json)

**22:55 CST 当前只读复核：** flowfix11 的实际映射脚本已按当前 USB 拓扑通过，四路 CAN 为 UP/ERROR-ACTIVE/1 Mbps，`127.0.0.1:13146` 已监听；机器狗本机 `GetControlStatus` 读回 GROUNDED、输出关闭、控制方 NONE、无关键故障。新 ARM64 cloud-link 候选已安装但保持 `DOSO_CLOUD_ON=false`，未连接云端。[preflight](artifacts/robot-cloud/real-connect-20261113/field-preflight-result.json) · [13146 读取](artifacts/robot-cloud/real-connect-20261113/read-current-13146.json) · [cloud-link 安装](artifacts/robot-cloud/real-connect-20261113/cloud-link-install.json)

**云端入口复核（登记前历史状态）：** 当前开发机公网 IP 为 `96.44.160.185`，MQTT ECS `8.153.169.177:22` 仍不可达；业务 ECS 到 MQTT ECS 的 22 也被拦截。已改用已登录的阿里云 ECS 云助手完成受控登记，不改安全组、不开放 SSH；当时设备登记保持 `enabled=false`，随后已由 ECS 云助手启用为只读 `enabled=true`。[入口探测](artifacts/robot-cloud/real-connect-20261113/cloud-ssh-preflight-current.json) · [初始登记](artifacts/robot-cloud/real-connect-20261113/cloud-assistant-registration-20261114.json) · [启用证据](artifacts/robot-cloud/real-connect-20261113/cloud-assistant-enable-20261114.json)

**本地授权入口部署：** 真实机器狗已安装 `doso-robot-authorizer.service`，监听 `127.0.0.1:13148`，Bearer token 校验通过，安装 gate 已切换为 `DOSO_CLOUD_ON=true`，choice 仍为 `false`，cloud-link 保持 inactive/disabled。授权调用开始前机器狗从局域网消失（无 ARP、ping、TCP 22），因此尚未声称 MQTT hello/status 已发送。[部署证据](artifacts/robot-cloud/real-connect-20261113/authorizer-deploy-20261114.json)

更新：2026-11-12。本页是当前完成程度、缺口和推进顺序的汇总入口；协议见 [CONTRACT.md](CONTRACT.md)，测试命令与证据边界见 [TESTING.md](TESTING.md)。各条记录区分源码、实际部署与真机证据。

## 当前完成程度

**最新GitHub状态（2026-11-13）：** DOSO-SDK `rust` 分支最新提交为 `ebc22ed`，已推送 GitHub；本地最新 Rust 回归 124 项通过。提交包含Rust七域SDK、协议、文档、示例、Android和CI；C++位于 `brainstem/`，旧Python位于 `archive/`。新增可运行导航CLI，流程为“提交目标 → 收到任务ID → 等待结果”。此前远程CI对应的旧提交记录继续保留，不能替代当前提交的现场部署或七域真机验收。此次上传针对 `rust` 分支，不代表main已更新、已创建tag/release、已部署到机器人或七域现场全部验收。

**本轮最新检查（2026-11-12 22:55–22:57，北京时间）：flowfix10的brainstem.service active/running、PID10677，22:38:51启动；13145在线，13146/正式5052无监听，bodyheight、lt-host和cloud-link均inactive。** 四条CAN均UP、1Mbps、berr计数0，近10分钟严重错误筛选无匹配；这次检查没有修改服务、CAN或运动配置，云开关false、真实UUID保持。[现场快照](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/current-check-20261112T145556Z.json)

随后同时SDK/raw 60秒均退出0：SDK窗口60.001253秒，IMU6,001条/100.015Hz、最大gap104.293ms，16轴99.931–99.965Hz、最大逐轴gap108.582ms，关节95,946事件、源跨度59.999781秒；raw两流continuous/passed均true、总passed=true，IMU99.958Hz、最大逐轴gap110ms。本窗口没有2秒消费超时，但不是15分钟长期稳定、严格实时或已部署候选的证明，旧失败保留。电脑观察桥SystemInfo三字段仍None，真实型号/固件接入和正式5052仍缺。[运行记录](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/current-concurrent-20261112T145643Z.json) · [SDK](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/current-sdk-20261112T145643Z.log) · [raw](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/raw-bdp-on-60s-20261112T145650Z.json)

**发布前快照（22:55–22:57检查轮次）：** 当时实际检查的远端main与codex分支仍为旧C++基线 `0ae4969c`，没有tags/releases；Rust七域、Cargo、proto、docs等主要新文件当时仍在本地未跟踪、未提交、未上传。该检查轮次仅检查，没有commit或push；此后已按上方最新GitHub状态推送 `rust` 分支。七域本地实现与源码上传均不等于现场七域全部验收。

**最新真实读取（2026-11-12 22:43，北京时间）：13145的同时SDK/raw 10秒窗口均退出0，全16轴重新有样本；这只是短窗口恢复，长期稳定验收仍未通过。** SDK IMU99.985Hz、各轴99.485–99.585Hz、逐轴最大gap273.034ms；raw两流continuous/passed均为true。前一60秒窗口4轴尾部约17秒缺数据与SDK超时保留，不由16 online缓存或本次短测抹去。不同窗口源时间已出现重置、现场程序/boot有变化，最新10秒只按endpoint和采样时段记录，不绑定旧PID1647或单一固件身份。用户要求不重启、无需静置，本任务只读检查和独立构建、未配置CAN或激活候选；13146/正式5052未部署。[最新现场与测试](TESTING.md#goaway-定位候选构建与现场并发复测2026-11-12)

增强探针计划900秒，在316.970859秒明确收到Unavailable/GOAWAY 11；随后另一次原始直连也约320秒收到同码，两次并不重合。禁用代理后的BDP on仍约314秒复现，BDP off只交付约167秒样本后等待deadline，不能当成功或作为关闭BDP的修法。实际grpc 5.2.0的keepalive计时与响应DATA activity缺陷已在真实库loopback定位，完整候选保留五分钟最小间隔、第三次违规保护和正常BDP。本地定位不等于机端修复已部署，也不解释旧200.319秒仅消费超时的失败。稳定验收仍未通过。[现场与本地分层证据](TESTING.md#goaway-定位候选构建与现场并发复测2026-11-12)

旧boot中PID78009的900秒同时复测、旧200秒消费超时、新60秒缺轴均保留各自证据，不混因。内核已直接记录22:19:24 can0 USB Rx aborted -71、usb1-2断开/can0 removed，22:19:40重新枚举，22:24仍DOWN；这是USB设备断开事实，不足以断定供电或EMI唯一物理根因，也未完成缺轴到总线的因果映射。新的独立stream-close候选最终串行9项响应/只读测试、1项注入native关闭测试与ARM64构建通过，预检零硬件连接/enable=false，activation=false；首轮端口冲突、第二轮fork示例workspace解析错误均保留，未把候选成功写成现场修复。[USB内核证据](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/can-kernel-window.log) · [最终候选构建](D:/DOSO/artifacts/robot-cloud/sdk-field-20261112/stream-build.json)

用户已明确选择研发机序列号 `THUNDER-DEV-001`，真实库存已登记UUID `d82a831a-2e86-4b46-975b-3d42473583b7`，非demo、非模拟、未认领。UUID已原子写入机端agent.env并备份旧样例，权限root:doso-cloud 0640；重复执行changed=false、UUID未旋转，云开关保持false、未重启。机端已用既有device-keys.sh生成独立TLS私钥/CSR与Ed25519状态签名密钥，私钥未下载，CSR CN与UUID一致；device.crt尚未签发安装、Gateway公钥登记和Broker ACL尚未做，制造认领密钥未生成，不能混用。电脑开发桥已配置同一UUID，不能据此声称机端Host/5052已安装；身份准备不等于开启云、客户认领或真实上报通过。[身份准备](tools/deploy/IDENTITY.md#首台研发机身份准备2026-11-12)

三次Rust双流10秒短重连均退出0、完成窗口，读回电脑桥相同真实robot_id，IMU与关节事件源时间在连接之间继续前进；这不消除首轮长读失败。robot_version与firmware_version均为None，尚未从真实机端release接通。库存model_code「THUNDER 01」仅是库存型号，不能当作SDK型号字段已配置；Brainstem的flowfix8构建名或SDK版本也不能填充整机固件版本。这两项仍是当前缺口。

2026-11-12 已明确设备无云管理权、云端禁止远程运控、本地SDK保留操控。设备证书/云管理凭据隔离、MQTT主题隔离及拒绝云运动请求的本地回归通过。SDK六项System公开查询已收紧，新增类型化ProductStatus；客户历史/状态接口取消公开平台映射、授权版本、投递统计和内部transport，新增服务端专用历史RPC并保留分页/断段及并发校验。Rust105项、Python gRPC80项、原生Deno38项、App51项及隔离PG验证通过；实际Rust/Python只读回环通过。**这些改动尚未部署，历史/状态合同与App需协调发布，旧线上入口不会因源码更新自动收口。** 证据与范围见 [TESTING.md](TESTING.md)。

2026-11-12 已按当前 `robot-cloud` 源码重新生成 ARM64 `doso-cloud-link` 候选包，完成 ELF/GLIBC/包校验和及狗端 26 项测试。此前构建阶段的目标地址 TCP 不可达结论已被同日后续实机检查更新：真实机器人 `192.168.66.64` 可达，候选已安装且二进制 SHA256 一致；以服务用户 `doso-cloud` 和 `DOSO_CLOUD_ON=false` 运行退出 0、输出 `cloud_off`，前后服务均为 inactive/disabled。该次关闭态检查时UUID仍是模板；后续已登记真实库存并准备机端密钥，当前尚缺证书签发安装、云侧公钥登记与ACL，未开启远程云连接或完成真实上报。候选见 [BUILD.md](BUILD.md#当前-arm64-云连接候选2026-11-12)，实机关闭证据见 [TESTING.md](TESTING.md#真实机器狗关闭态与本地-sdk-观测2026-11-12)。

同日真实Rust SDK已通过电脑回环上的Python只读观察服务读取 `.64:13145` 的真实IMU/关节。flowfix5阶段首轮每轴77.845–77.895Hz、源时间落后的记录保留，直连对照定位当时转发瓶颈；single-thread模式干净60秒曾达到IMU99.979Hz、每轴99.895–99.929Hz。flowfix7b默认代码20秒已读通但每轴约93.4Hz；这些都是各自版本和窗口证据，不能替代最新flowfix8长读结论。此前IMU-only仅收20帧后停流的结果保留；本轮IMU单流60秒成功也不证明长期停流问题已消失。13146的flowfix5候选仅通过旧基线适用性检查、未编译部署，不可直接用于新版本；机端完整SDK服务未完成部署验收，本次不是运动验收。

同日补跑云端候选回归：Gateway 26 项、Media 9 项、Telemetry 10 项通过；这些是本地协议/生命周期测试，不代表 ThingsBoard 常驻云、MediaMTX 公网服务或真实设备已经部署。

2026-11-11 本轮已完成后台通知收纳并部署本机18081，最终45项接口/202资源和桌面、手机10项页面检查通过。SDK/机端地理位置查询与1Hz流、本地Rust/Python实际gRPC联调和云端位置投影已补齐；完整结果见 [TESTING.md](TESTING.md)。连接 IP、位置与活动异常的数据来源、现有差距及产品顺序集中在 [机器人云端管理](docs/MONITORING.md)。本轮未将SDK改动安装到真机或发布线上worker，也未接通告警规则。

| 部分 | 已有事实 | 剩余工作 |
| --- | --- | --- |
| 目录与开发工程 | `robot/`、`cloud/`、`shared/`、`tools/`已整理；11月7日锁定构建、61项测试、格式和本地联调通过 | 新Linux交付包需按实际候选重新构建、验收 |
| 云端基础服务 | 业务API、Gateway自动同步、Mosquitto和HTTPS有11月4日部署记录 | 当前正式制品与最新源码是否一致需核对；部署记录不等于今天的在线检查 |
| 真实Web App与业务数据库 | 11月7日已登记模拟设备，用正常账号完成绑定API、开启、退出页面更新、断网恢复、关闭停采 | 真实机器狗、正式手机/G20、手机扫码、独立公网MQTT和5G待验收 |
| GNSS位置与轨迹地图 | 11月9日新增街道跟随、自由查看、分段轨迹全览、日期与摘要卡；实际Web App已从真实HTTPS接口读回模拟轨迹并验证刷新保留；[实现与范围](docs/TRACKS.md) | 真实GNSS硬件、机端部署、原生手机和国内底图长期可达性待验收；长时间历史目前最多展示2000条 |
| 机器狗云连接程序 | 新远程关闭持久化 ARM64 候选已安装并以关闭态验证；真实 13146 已读通；真实 UUID 与机端独立 TLS/状态签名密钥已准备 | Gateway 公钥/ACL 未登记，云连接保持关闭；真实 MQTT 上报和 App 状态仍待验收 |
| 用户主动关闭云连接 | 本地关闭、重启保持关闭、远程 `enabled=false` 持久化 `choice=false`、停止采样并退出 MQTT 已在模拟 mTLS 链路验证 | 真机远程关闭、真实服务用户权限和 App 端到端仍待验收 |
| 设备身份与归属 | 用户选定THUNDER-DEV-001，真实库存UUID已登记并写入机端；非demo/非模拟、未认领，TLS CSR与独立状态签名密钥已准备 | 客户认领、制造证明密钥、证书签发安装、云侧公钥/ACL与上云验收未完成；已有状态签名密钥不能当制造认领密钥 |
| 视频 | 观看授权有11月6日部署记录；media/MediaMTX与模拟源已有验证 | 媒体候选部署、真实摄像头、正式App观看及公网连接待验收 |
| 状态历史与ThingsBoard | 测试业务ECS已发布遥测/历史接口，Rust投递程序在ECS临时服务运行，App已接入历史；11月10日恢复原SSH私有通道，鉴权读取200、未鉴权401，云库与TB模拟记录序号38192及时间一致 | TB仍依赖开发机隔离Docker与临时SSH通道，独立常驻部署待完成；本轮无待投递记录，新投递、真机数据、告警链与客户生产许可另验 |
| DOSO管理后台 | 11月11日紧凑铃铛与会话提示版已部署本机18081，保留可调侧栏、Lucide与轻分隔；9项服务/组件自检、生产编译414.538秒、最终45项接口及202资源、桌面/手机10项页面检查通过 | 浏览器双账号切换、通知空态、其他浏览器与负载表现另验；真实告警、客户访问隔离及独立云部署待完成；先前接口超时记录保留 |
| 客户数据与告警责任 | 11月12日源码已收紧客户历史/状态投影，历史映射RPC仅服务端可执行，本地SQL权限与真实接口回归通过；平台映射、版本、队列及transport移出普通客户响应 | 本轮需App/SQL/Edge协调发布后才在线生效；数据类别/用途授权、DOSO人员查看隔离和售后分享仍待实现，见[产品合同](CONTRACT.md#客户数据与告警责任) |

11月7日业务演练使用本机模拟Brainstem、本机TLS MQTT、SSH隧道和ECS隔离Gateway；云端业务数据库与DOSO Web App为实际实现。关闭后仅验证了停采，联系时间仍可更新。完整证据见 [SIMULATED_FLOW.md](docs/SIMULATED_FLOW.md)，不能把它当成完全断云或真实设备交付。

**下一阶段建议交付：一台真实机器狗由机主在本地开通一次，离开现场后仍能看见带时间的真实状态；主动关闭后保存关闭并断云，重启保持关闭，再次开启通过本地授权完成。**既有“完全断云”产品决定沿用；下面的实施顺序和验收安排是本次建议。

本地SDK功能继续独立推进。量产工厂平台、多成员共享、完整证书自动生命周期、5G切换和云端运控不作为首台已授权研发设备交付的统一前置条件。真实视频可并行准备摄像头和安装候选，在状态交付后做完整观看验收。

遥测历史已按 [构建与迁移方案](docs/MIGRATION.md) 接入测试业务ECS与App，业务数据库最终接受快照后异步投递，现有SDK和许可链保持。现有两秒latest同步只能形成快照历史；ThingsBoard仍依赖开发机隔离平台，不改变本页真机与完整断云交付顺序。

## 历史进展记录

以下保留各日期当时的结果与限制。旧记录中的“当前”“未部署”“库存为0”只描述该次检查，当前汇总以上表为准。

2026-11-11 侧栏调宽与Lucide图标已部署本机18081。白底上补回侧栏、顶部与总数分区的极浅分界，主要导航使用八份原始Lucide SVG并保留完整许可。桌面侧栏默认240 px，边缘拖动限制200–360 px，左右键每次8 px，宽度归HomeComponent当前会话，刷新复位。鼠标松手、超出手柄范围、菜单展开、拖动中切小屏及返回桌面已验证。最终45项接口与202个资源、21项页面检查通过。首次登录检查超时，第二次大文件读取失败，降低验证并发后的完整复测通过，原件保留。GitHub三个项目的原始参考文件已保存，见 [TESTING.md](TESTING.md#侧栏调宽与图标2026-11-11)。

2026-11-10 白底无框工作台已部署本机18081。按用户要求参考当前Codex桌面画面，主背景采用纯白，导航采用近白底与浅灰选中态。删除总览卡、快捷入口、空态和列表容器外框与阴影，清理全局锚点规则带来的链接下线，列表使用极浅中性行分隔。生产编译205.838秒，HTTP/API/WebSocket 35项及193个静态资源、Chromium IAB四种尺寸18项检查通过。颜色观察、构建指纹与截图见 [TESTING.md](TESTING.md#白底无框工作台2026-11-10)。

2026-11-10 紧凑版工作台已部署本机18081。按用户截图删除总数卡、设备卡和快捷入口中的重复装饰图标，快捷入口改为纯文字。桌面设备卡实际从229.390625 px收至177.390625 px，总数卡126→80 px，快捷行57→40 px，导航48→40 px，表格行72→56 px。官方Ant Design中文规范、布局、色彩、紧凑token与许可共14份原文件已按固定提交下载至 [本地参考目录](D:/DOSO/references/console-design/README.md)。生产编译260.809秒，最终HTTP/API/WebSocket 35项及193个静态资源、Chromium IAB四种尺寸21项检查通过，证据见 [TESTING.md](TESTING.md#紧凑工作台与设计参考2026-11-10)。

2026-11-10 内部工作台重构已部署本机18081。总览改为真实登记总数、最近设备卡与快捷入口，移除重复宣传文案和告警快捷入口。设备、告警及其他实体列表共用表格布局，长接入标识单行省略，窄屏在表格内滚动，侧栏主要入口与高级入口统一使用现有MDI图标。最终修复顶部白色工具栏的图标可见性和高级菜单箭头留白。最终编译295.030秒，HTTP/API/WebSocket 35项、193个静态资源和Chromium IAB四种尺寸22项检查通过。登录页原左卡桌面与手机尺寸回归通过。现有记录仍是测试设备，真机数据与独立云部署分别验收，证据见 [TESTING.md](TESTING.md#内部工作台重构2026-11-10)。

2026-11-10 状态历史私有通道恢复。确认旧Unix socket已无监听后，清理该socket并重建原SSH反向转发，沿用ECS临时relay与export服务。云端桥接未鉴权读取401、既有凭据读取200，本机TB、云库和云端桥接读回的模拟记录序号38192、观察时间2026-11-10T03:48:09.582Z一致。检查时无pending或failed记录，未验证新数据投递。当前仍需开发机、Docker与临时SSH进程运行，独立云平台部署和真机上报待验收。证据与停止方式见 [TESTING.md](TESTING.md#私有数据通道恢复2026-11-10)。

2026-11-10 DOSO管理后台恢复原左卡已部署。按用户要求恢复等大版左侧图源、尺寸、构图、圆角与文字，右侧玻璃覆盖原列间距并藏入左图后28 px，移动布局藏入24 px，左图处于前景。右侧采用0.4白底与20 px模糊，保留小标题与填充输入框。最终生产编译188.048秒，HTTP/API/WebSocket 35项及193个资源、Chromium IAB六种尺寸25项技术检查通过，四种尺寸左卡边界与原基线差异小于0.1 px。开发服务已停止，用户视觉反馈单独记录。愿景文字仍为试排，其他浏览器与两种兼容模式运行时效果尚未实测，证据见 [TESTING.md](TESTING.md)。

2026-11-10 DOSO管理后台登录页连续场景曾完成技术部署。该轮将山海背景贯穿统一圆角主容器，右侧采用0.52白底与20 px模糊的无外框玻璃整栏，「管理后台」为15 px辅助标题。修正主题对填充输入框的覆盖后，最终生产编译212.083秒，HTTP/API/WebSocket 35项及193个资源通过，Chromium IAB六种尺寸22项检查通过。背景贯穿、输入框白底、移动排列、低高度滚动、密码显隐、焦点和登录退出均通过，重置密码仅检查入口跳转。用户随后明确要求恢复原左侧景观。愿景文字仍为试排，其他浏览器与两种兼容模式的运行时效果尚未实测，证据见 [TESTING.md](TESTING.md)。

2026-11-10 DOSO管理后台登录页左右等大已部署。桌面两列等宽并拉伸同高，右侧内容组最大宽380 px并整体居中，959 px及以下单列自然高度。补景观 `width: 100%` 修正候选中宽高比撑宽的问题后，最终生产编译208.136秒，HTTP/API/WebSocket 35项及193个资源通过。Edge六种尺寸17项检查通过，覆盖等大对齐、表单居中、滚动和键盘登录、退出。其他浏览器、reduced-motion及背景模糊回退运行时尚未实测，结果见 [TESTING.md](TESTING.md)。

2026-11-09 DOSO管理后台白底流光与毛玻璃。仅调整登录SCSS，纯白底叠加26/30秒蓝色流光，表单采用可回退的背景模糊与更宽松桌面间距，沿用景观和试排文字。生产编译313.978秒，本机镜像已更新，HTTP/API/WebSocket 35项及193个静态资源通过。Chromium四种视口、流光变化、玻璃效果、低高度滚动、错误提示、键盘登录和退出19项通过。reduced-motion与不支持背景模糊的运行时回退仅检查源规则，证据见 [TESTING.md](TESTING.md)。

2026-11-09 DOSO管理后台山海星空登录页。以完整生成景观替换四卡画板，桌面竖图与窄屏横图分别构图，保留原认证绑定和低高度滚动。两行试排「探索未知」「拓展人类的可能」，尚未定为正式公司愿景。生产编译308.048秒，本机镜像已更新，HTTP/API/WebSocket 35项及193个静态资源通过。浏览器四种视口、响应式景观、轻漂移、低高度滚动、错误提示、键盘登录与退出通过，reduced-motion 运行时模拟未完成，证据见 [TESTING.md](TESTING.md)。

2026-11-09 DOSO管理后台 Bento 登录页重构。新增浅灰画布、四格原创机器人 SVG/CSS 画板、单一标题与原表单布局，加入入场、关节节奏和 hover 动效，支持 reduced-motion。最终生产编译415.577秒，本机镜像已更新，HTTP/API/WebSocket 33项及191个静态资源通过。浏览器完成四种视口、动效、hover、错误提示、键盘登录和退出。修复320×568下底部入口裁切，reduced-motion 运行时模拟尚未完成，证据见 [TESTING.md](TESTING.md)。

2026-11-09 DOSO管理后台字体与登录页调整。复用 App 已有 MiSans 四字重原始 OTF，主题和正文统一字体，保留原许可 PDF、NOTICE 署名及用户菜单许可入口。删除登录页重复的「管理后台」「登录DOSO管理后台」可见文案，保留表单可访问名称。本轮生产编译通过，构建阶段245.695秒，本机镜像已更新；HTTP/API/WebSocket 33项与191个静态资源通过。浏览器在663×2364视口完成登录、总览、设备页和退出，确认实际 MiSans 渲染及两处文案删除；多断点和高级页面仍待验收，结果见 [TESTING.md](TESTING.md)。

2026-11-09 DOSO管理后台初版完成定制前端编译。基于ThingsBoard CE 4.3.1.8固定提交，复用现有设备和账号API，增加DOSO品牌、中文默认、精简导航和实际设备API总览。最终构建通过并记录源码、补丁、锁文件及Node版本指纹，构建阶段为367.226秒。独立Nginx已在本机 `127.0.0.1:18081` 启动，沿用既有ThingsBoard后台与数据库。最终镜像的HTTP/API/WebSocket定向检查27项和187个资源全部通过，覆盖实际账号API、模拟设备列表/搜索/详情/历史和一次属性订阅。浏览器显示与交互按用户决定暂缓，证据见 [TESTING.md](TESTING.md)。

2026-11-08 遥测首批实现：新增 `cloud/telemetry` workspace成员，业务数据库增量迁移、专用Edge入口、固定镜像的ThingsBoard 4.3.1.8隔离部署、Linux服务单元和可重复验收脚本。Rust9项与Deno31项定向检查通过；真实PG37组加实际平台4组，共41组验证通过，本轮临时进程已关闭。真实平台验证保留原时间/位置、重复不入队、读取失败不伪造状态和关闭取消未发；修复实测发现的历史查询半开时间区间问题。数据源为明确模拟设备，PostgREST传输在本次验收中由白名单psql适配，未发布生产业务或连接真机。证据见 [TESTING.md](TESTING.md)。

2026-11-08 完成遥测平台构建与迁移方案，核查App/数据库接受事务、Gateway两秒快照、ThingsBoard服务端REST及设备映射。首轮建议隔离验证4.3.1.8的历史与看板，生产版本和告警链另核；不迁旧账号、不回填不存在的历史。当前Docker CLI可用但Linux Engine不可连接；未启动平台、运行迁移或修改运行代码。

2026-11-08 完成真实数据接入的成熟项目调研，见 [REUSE.md](docs/REUSE.md)。建议云端历史、看板和告警优先隔离验证 ThingsBoard，机器人记录复用 MCAP/Foxglove，Ditto/OpenRemote 参考状态与事件模型；保留既有 DOSO 授权、归属和真实数据适配入口。已核对版本、许可证和本地源码落点，未安装或采用新平台，也不构成新增运行测试或真机验收。

2026-11-07 源码按运行位置归类：机器狗程序在 `robot/`，云服务器程序及配置在 `cloud/`，共享协议在 `shared/`，联调和部署工具在 `tools/`。同步Cargo成员/依赖、lab根目录与Broker模板路径，以及App的当前源码入口。离线锁定构建、61项测试、格式检查和现有 `lab check` 通过，74个文档链接有效；细节见 [TESTING.md](TESTING.md)。本次为目录整理，没有部署服务或新增云连接授权能力。

2026-11-06 视频业务授权已部署到业务服务器：无凭据访问视频函数返回 401，未知观看凭据返回 403，既有设备管理与状态同步无凭据请求仍返回 401。真实账户签发凭据、媒体服务器部署、设备推流与 App 播放尚未验收。

2026-11-06 设备登记进展：森哥确认唯一现有账号为本人并授权作为设备管理员后，已备份业务数据库并完成一次性管理员引导，审计与角色变更同事务；设备管理数据库迁移和接口已发布，带有效 action 的无凭据和无效凭据请求均返回 401。研发机编号 `DOSO-RD-20261105-001` 已分配并保存在本机，尚未创建业务库存或绑定；真实管理员 AAL2 会话、型号确认和登记操作尚待完成。

2026-11-06 MQTT 认证本地检查：用独立的真实 Mosquitto 和临时证书，验证合法设备可向自己主题发布、无证书及不受信任证书被拒绝、设备向另一设备主题发布被拒绝。新增 `lab auth-check` 可重复检查；这不代表真实机器狗已签发证书、连接云端或完成吊销。

2026-11-05 本地进展：云连接源码新增持久化选择文件读取，缺失或无效时不连接；运行中本地选择关闭会取消读取并断开连接，本地模拟验证了关闭和重启保持关闭。此文件仍缺可信本地授权写入方；远程明确关闭后的持久化断云未实现，不能将此视为产品授权闭环或真机上线。当前联网查询确认业务库存无设备；机器狗未安装云连接服务。RM500U 已注册广电 5G SA n41，但 USB 数据接口未连接，未验证 5G 上网。

2026-11-05/06 视频本地进展：Rust `media` 信令适配候选与 MediaMTX 串接后，浏览器实际通过 WebRTC 解码模拟 H.264，帧数持续增长；观看授权在该次验证中使用本机替身。App 四路入口、业务观看授权和按需源启停均有本地候选；真实 MQTT 联调验证了签名消息启动及停止模拟源。尚未完成这些部分的真实产品端到端验收、线上部署、真实摄像头或公网验收，细节见 [cloud/media/README.md](cloud/media/README.md)。

2026-11-04 G20 真实只读显示：通过 USB 反向通道和电脑临时 `GetControlStatus` 只读中继，App 显示机器狗落地、无控制者、电机未使能、无严重故障。该结果不是 Wi-Fi 直连或云端上报；G20 Wi-Fi 直连被现有来源限制拒绝。临时转发和中继已清理，App 测试地址恢复，证据见 `C:\Users\99563\Documents\ChatGPT\New project\doso-app\artifacts\robot-live-20261104\g20-live-20261104\result.json`。未扩展狗端控制权限或发送运动指令。本机只读入口切换方案保留既有 LAN 白名单，见 [候选方案](D:/DOSO/artifacts/robot-cloud/20261104-g20-status-switch.md)。

2026-11-04 无线只读检查：机器狗 Wi-Fi 网卡列出未禁用的 5 GHz 信道 36/40/44/48；此前 `wlan0` 实际连接在 2.4 GHz，未切换网络。USB 识别到 RM500U-CNV；检查时 `usb0` 显示 unavailable，ModemManager 未返回可用信息，不能据此判定 SIM 状态或蜂窝已注册。未拨号或改变默认路由。

2026-11-04 后续：用户确认关闭远程管理时应完全断开云连接。一次性状态查询现已在本地源码要求设备确认的有效许可；Gateway 和狗端均拒绝未授权查询，归属变化时取消旧查询。工作区测试和本机通信回归通过，新的 ARM64/x86-64 可执行候选位于 `D:\DOSO\artifacts\robot-cloud\releases\20261104-status-consent`；Gateway、狗端及 App 均未发布本次改动。该历史记录之后的本地选择进展见上文；本地授权入口与远程关闭闭环仍未实现。下述历史候选包早于本次查询许可修改，不包含该修改。

2026-11-04 真机测试：机器狗 `192.168.66.64` 曾通过 Wi-Fi 到达云端 MQTT TCP 端口，业务 HTTPS 未授权返回 401；既有局域网 `GetControlStatus` 返回落地、电机输出关闭、无控制方及关键电机故障。云连接服务与回环只读入口未安装。隔离目录 `cloud_status_stage_20261104` 的配套源码通过目标机静态分析；处理复制缓存的旧路径问题后，目标机第二次构建成功，候选位于 `build/cloud-status-candidate-3/bundle/bin/thunder`，依赖锁文件与原目录一致。SSH 间歇超时，运行中的 Brainstem 未切换或重启；该记录不能视为云端真机状态已接通。

当前源码的狗端程序已采用 `robot/`、`doso-cloud-link` 和 `DOSO_CLOUD_ON`。新名 ARM64 候选包位于 `D:\DOSO\artifacts\robot-cloud\releases\20261104-cloud-link-naming\link-aarch64.tar.gz`，已核验架构和包内容，尚未在狗端启动或安装。旧名 `agent-aarch64.tar.gz` 保持原有内容；共享设备身份配置路径继续保留。

更新：2026-11-04。目标是用户在 DOSO App 开启一次后，机器狗主动连接云端，App 能看到带采集时间的真实只读状态；后台负责续期和恢复。业务 API 和新版 MQTT Gateway 已部署，真实机器狗链路正在接入，尚未验收。

真机已通过既有局域网 `13145` 接口读取一次真实状态。配套的本机只读 `127.0.0.1:13146` 接口和云连接服务修改仍在准备，尚未安装到机器狗。

新版 ARM64 云连接服务候选包已生成，默认关闭且不含私钥；机器狗当前原地址不可达，尚未做真机启动。设备侧生成独立密钥和 CSR 的工具已准备，库存中的真实序列号、型号与设备 ID 尚待核对。产品证书签发、续期和撤销流程尚未完成，见 [设备身份与首次开通](tools/deploy/IDENTITY.md)。

2026-11-04 已只读核对当前上海业务库，非演示实体设备库存为 0；这不代表机器狗本机或其他系统没有已有身份。森哥同意先进行本地模拟，真机恢复供电和网络后再核对身份及登记。

同日本地模拟重新通过 `lab check` 和 `lab auto-check`：分别使用模拟 Brainstem 与模拟业务接口，验证签名查询、自动续期、状态保存、重启恢复和关闭或到期停采；不代表真实机器狗或线上数据库已验收。设备密钥工具的隔离验证通过，临时 CA 可为 CSR 签发客户端证书，消息篡改后验签失败。

库存与认领相关的本地验证已推进：业务侧已将创建库存、重置绑定码和解绑与各自审计改为数据库事务，并在本地真实 PostgreSQL 验证回滚及并发；管理员接口 Deno 测试和部署工具测试也已通过。复核发现 `device-admin-20261104` 归档还包含未纳入上述测试范围的 `issue_factory_claim` 分支，整个包暂不可发布，须先明确其归属并重新审查；上述测试不能视为归档整体验收。当前尚无真实设备库存登记，线上未验证。

## 产品目标与用户体验

以下是完整云连接开关的产品目标，缺失实现按下一节推进；现有App按钮只完成远程状态许可流程。

- 每台设备默认关闭云连接。机主或明确委托的管理者在本地授权开启；设备绑定和打开页面不自动联网。产品保留一个清楚的云连接开关，协议参数由系统处理。
- 首版远程价值是查看运行状态、控制方、电机输出和关键故障及采集时间。沿现有最高每秒一次的状态采集，不将本机高频控制数据默认全量上云；电量等无来源时保持未知。
- 保存选择、设备执行和数据新鲜度分别展示为可理解的等待或结果。当前联系失败显示“暂时无法联系”，不能推断机器狗物理关机。
- App退出不改变已授权选择，后台续期不依赖手机保持前台。普通网络中断恢复时仍按原选择与有效许可工作。
- 用户主动关闭后，云端立即拒收，机器狗收到明确关闭后保存关闭并断云；设备未收到时显示“关闭请求已保存，等待设备确认”。停采确认与完全断云确认分别以证据判断。
- 在关闭操作旁说明“关闭后需在机器狗附近重新开启”。借用者的本地运动权限不自动等于云管理权限。关闭云连接后，已授权的本地SDK和机身控制继续按本地规则运行。

## 职责

| 位置 | 负责什么 | 入口 |
| --- | --- | --- |
| DOSO App | 用户操作和结果展示；不保存服务密钥、不负责续期 | 独立 App 工程 `src/components/RemoteStatePanel.tsx` 和 `src/services/remoteState.ts` |
| 业务后端和数据库 | 用户身份、设备归属、用户选择、许可续期和最新状态入库 | App 工程 `supabase/functions/remote-state*/` 与 `supabase/migrations/20261103150000_add_remote_state.sql` |
| Rust Gateway | 向业务后端同步状态和许可，管理 MQTT 连接并验证设备消息 | [cloud/gateway/src/sync.rs](cloud/gateway/src/sync.rs)、[cloud/gateway/src/reporting.rs](cloud/gateway/src/reporting.rs) |
| MQTT Broker | 转发消息，并按设备身份限制主题 | [cloud/broker/](cloud/broker/) |
| 云连接服务 | 主动连接云端，核对许可，读取本机状态并签名上报 | [robot/src/link.rs](robot/src/link.rs)、[robot/src/reporting.rs](robot/src/reporting.rs) |
| Brainstem | 机器狗运行状态真源；运动和安全保持由本地系统负责 | 既有机器狗运行时 |
| shared | 定义通信状态、许可和报告；编译进云连接服务和 Gateway | [shared/src/status.rs](shared/src/status.rs)、[shared/src/reporting.rs](shared/src/reporting.rs) |

状态字段包括运行/过渡状态、控制方、电机输出与关键电机故障；11月8日新增可选GNSS位置及独立采集时间。定位已有模拟端云证据，真实接收机仍待验收；电量等无来源字段保持未知。

## 步骤与完成标准

| 顺序 | 产品交付 | 主要责任与代码入口 | 完成证据 |
| --- | --- | --- | --- |
| 1：P0开关闭环 | 本地授权开启/重新开启，远程明确关闭后持久化断云 | `robot/src/choice.rs`、`main.rs`、`link.rs`、`session.rs`；`shared/src/reporting.rs`；`cloud/gateway/src/reporting.rs`；App及业务后端 | 默认关闭无MQTT；授权保存后实际启动；明确关闭持久化并退出；重启不自开；许可到期和普通断网不改写用户选择 |
| 2：首台设备开通 | 核对既有身份，按真实UUID完成库存、归属、证书和安装候选 | 既有业务设备管理及 `tools/deploy/IDENTITY.md`、`LINK.md` | 真实设备UUID、证书CN、消息身份及云端登记一致；经产品入口开启，不以手工写true文件作为产品验收 |
| 3：真实状态可用 | 将已演练的Web业务链换成真实机器狗和正式手机/G20 | 本机Brainstem只读入口、cloud-link、云服务、现有App状态页 | 显示真实四类状态与采集时间；退出页面后跨至少两次许可续期仍更新；读失败保留旧时间并明确过期；用户不输入通信参数 |
| 4：关闭和恢复交付 | 验证网络中断、设备重启、主动关闭、离线关闭等待及本地重开，形成可重复安装包 | 同一端云链路和现有发布工具 | 网络恢复可自动恢复有效上报；主动关闭重启仍关；离线关闭云端立即拒收，设备恢复后先处理待执行关闭；本地重开成功且本地SDK独立可用 |
| 5：真实视频 | 复用既有观看授权与media候选接入摄像头 | `cloud/media/`、`robot/src/source.rs`及App视频入口 | 先验一条真实摄像头流持续解码、停止观看后源退出，再验所需多路并发；视频使用WebRTC，MQTT仅传源启停 |

第1阶段代码与第2阶段身份/安装准备可以并行；真实安装须先具备相应授权和可审查候选。第3、4阶段通过后即形成首个远程状态交付，不等待第5阶段视频或量产扩展。

下一项代码应先接通机器狗本地云连接设置入口：核对机主/受托管理权限，持久化选择，并让已退出的cloud-link实际启动。公共接口若由SDK暴露，归入 `system`，状态与连接生命周期继续由robot-cloud持有。具体接口和明确断云消息在实现前更新 [CONTRACT.md](CONTRACT.md)，复用已有App会话与设备授权，避免再维护一套账号系统。

远程关闭协议同时覆盖设备离线：云端保存明确关闭意图并拒收，设备下次建立联系时执行该意图，持久化后退出；确认不足时保留等待状态。短期上报许可到期、普通撤权和用户要求彻底断云必须保持可区分。这是待实现目标，不是已有 `enabled=false` 的额外承诺。

交付只沿用已约定的机器人ID、机器人版本和一个整机固件版本；组件构建信息留在发布制品中。安装、排错和验收记录指向具体设备与制品，不增加面向用户的版本体系。

## 业务范围与后续扩展

历史单设备限制已有解除迁移记录，App已有设备列表、选择、添加和解绑。真实多设备、成员共享、生产证书续期/吊销和量产工厂流程继续按对应项目推进；不把这些全部列为首台已授权设备的阻塞条件。真实身份与本次访问授权仍须满足现有合同。

以上只覆盖远程状态。地图上传下载、视频、远程任务及运动控制是独立功能，不因开启远程状态而自动启用。

## 账号登录 IP 自查（2026-11-12）

源码已完成：App 在认证后的会话准备阶段调用 `capture_login`，服务端迁移 `20261112020000_add_login_history.sql` 从已验证 `auth.sessions` 读取会话 IP/时间并幂等写入 `account.login`；账号安全页调用 `list_login_history`，按当前账号过滤并最多返回 50 条，未记录 IP 保持 `null`。客户端不能提交 IP，且查询不授予设备云管理或机器人控制权限。API 字段和动作见 [DOSO_ACCOUNT_LOGIN_IP_API.md](<C:/Users/99563/Documents/ChatGPT/New project/doso-app/docs/production/DOSO_ACCOUNT_LOGIN_IP_API.md>)。

本地 Node 账号安全/会话回归 46/46 通过，Deno 2.5.6 Edge 源码约束 15/15 通过。**迁移和新版 `account-sessions` 尚未部署，真实账号、线上可信代理链和公网 IP 读回尚未验收。** 仓库代理模板已覆盖伪造 XFF、注入 SBFF 并限制 loopback，实际部署仍需核对拓扑并禁止绕过入口；Auth 无法确认来源时显示未记录。机器人 MQTT 出口 IP 是 Broker 连接事实，尚未采集，和账号登录 IP 分开处理。
