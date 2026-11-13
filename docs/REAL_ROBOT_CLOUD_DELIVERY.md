# 真实机器狗云连接交付计划

更新：2026-11-13。目标是交付一条真实、只读、可回滚的机器狗云状态链路。云端不获得运动控制能力；站起、坐下、遥操作、External 运控和导航继续只由本地 SDK 与机器人安全链负责。

## 最终交付

```text
Brainstem 只读状态
  → 127.0.0.1:13146
  → 机器狗 doso-cloud-link
  → MQTT 5 / mTLS
  → 云端 Gateway
  → 业务后端 / PostgreSQL
  → App 最新状态
  → telemetry → ThingsBoard 历史
```

交付不包含云端远程运控、任意 gRPC 透传、原始 Brainstem 控制接口暴露或 App 直连 MQTT。

## 验收门

| 门 | 通过条件 | 当前状态 | 责任范围 |
| --- | --- | --- | --- |
| G0 设备身份 | UUID、设备 CSR、MQTT 客户端证书、状态签名公钥和云端登记使用同一真实设备 ID | UUID 已登记；CSR/证书已生成；云端公钥登记未完成 | 业务库存、签发端、ECS 运维 |
| G1 本机只读源 | 稳定 Brainstem 版本监听 `127.0.0.1:13146`，只注册 `GetControlStatus`，启动/退出不破坏原有安全关闭 | 候选关闭竞态已修复并构建通过；现场窗口在候选启动前暴露 PCAN/CAN 链路故障，旧服务停止超时并进入 failed；尚未现场启动 | Brainstem 候选与现场部署 |
| G2 机器狗云连接 | cloud-link 安装、关闭态自检、证书加载、MQTT 连接和许可检查通过 | 二进制已安装；`DOSO_CLOUD_ON=false`；未启动 | robot-cloud/robot |
| G3 云端通道 | Broker 接受真实设备证书；真实 UUID 有精确 ACL；Gateway 验证设备签名并返回状态 | TLS 握手通过；Gateway 对 UUID 返回 `403 disabled` | cloud/broker、cloud/gateway、ECS |
| G4 业务与 App | 用户许可开启后，Gateway 状态进入业务库，App 显示状态与时间；历史投递不越权 | 模拟链路已通过；真机未通过 | 业务后端、App、telemetry |
| G5 关闭与回滚 | 用户关闭后停止新读取/上报并断云；重启保持关闭；移除候选可回到旧 Brainstem | 远程关闭持久化、MQTT 退出、重启保持关闭和本地重新开启已通过模拟链路；真机断云未验收 | robot/cloud/业务后端 |

只有 G0–G5 全部通过，才把真实机器狗云连接写成已交付。

## 当前事实

- 真实机器狗 `192.168.66.64` 可达，Brainstem 主端口 `13145` active。
- flowfix10 不包含 `13146` 实现；设置环境变量不会创建端口。
- 现有 `brainstem-grpc-proxy` 是完整 `13145` TCP 转发，不是只读桥，禁止启用。
- 本地 ARM64 候选已构建并通过硬件前置检查，曾真实监听 `13146`；退出时在物理电机禁用确认阶段超时，已移除 drop-in 并恢复 flowfix10。
- 新候选已修复关闭竞态：运行时开始关闭后忽略迟到的输出撤销回调，避免 `Control disposed` 再次启动电机禁用；目标机 ARM64 构建哈希为 `7c3a5c044e0335a06878f34e0b19af7e43c73f6b4b1211aaad21241e38634992`，仍未安装或启动。
- 最新维护窗口没有启动候选：旧 `flowfix10_hub_20261113` 在停止时遇到 `usbbus2 errno=6`、`can1` USB 断开和 `CAN link is DOWN`，随后进入 failed；候选 drop-in 已移除。[现场失败记录](../../artifacts/robot-cloud/real-connect-20261113/g1-field-failure.md)
- 当前现场状态需先恢复硬件：`can0`、`can2`、`can3` 为 UP，`can1` 为 DOWN/STOPPED；`brainstem.service` 当前 failed、没有 `13145`/`13146` 监听。未执行 CAN 配置或强制重启。[硬件快照](../../artifacts/robot-cloud/real-connect-20261113/failure-inspection.json)
- 11:36 CST 只读复核确认上述状态没有自行恢复：`brainstem.service=failed`、三个端口无监听、cloud-link inactive，`can1` 仍 DOWN/STOPPED。[最新现场复核](../../artifacts/robot-cloud/real-connect-20261113/field-recheck-20261113T113631Z.json)
- 12:29 CST 新一轮只读复核仍显示同样状态，且 `thunder-can.service=failed`、`thunder-dualpose-can.service=active/exited`，Brainstem 的 `can-check.py --check` 继续返回 `CAN link is DOWN`。[最新复核](../../artifacts/robot-cloud/real-connect-20261113/fresh-recheck-20261113T122918Z.json)
- 12:34 CST 已在真机执行新增的 `field-preflight.sh`（只读）。它正确拒绝继续：`can1` DOWN，且现有映射脚本要求 `can0→USB 1-2`，实际 `can0` 当前挂在 `1-1.1`，说明 USB/PCAN 拓扑已变化；没有执行任何 `ip link set` 或服务动作。[原始结果](../../artifacts/robot-cloud/real-connect-20261113/field-preflight-result.json)
- 最新 udev 只读信息显示四个 PCAN-USB 都没有唯一序列号（`ID_SERIAL=_XCAN-USB`），因此不能从软件自动判断逻辑总线角色；当前接口路径为 `can0→1-1.1`、`can1→1-2.1`、`can2→1-1.2`、`can3→1-1.3`。必须由现场确认物理总线标签后才能调整映射，不能猜测或用电机报文试探。[拓扑快照](../../artifacts/robot-cloud/real-connect-20261113/failure-inspection.json)
- cloud-link ARM64 候选已安装到 `/opt/doso/bin/doso-cloud-link`，关闭态运行输出 `cloud_off`。
- 真实设备证书已用当前 Broker CA 签发，机器人到 `8.153.169.177:8883` 的 TLS 1.3 握手通过。
- Gateway 真实 UUID 查询返回 `403 disabled`，表示云端登记/ACL/启用尚未完成。
- 10:20 CST 最新只读现场复核显示当前基线为 `flowfix10_hub_20261113`、PID 12818、`13145` active；`13146`、`5052` 和 `doso-cloud-link` 仍未运行。[现场快照](../../artifacts/robot-cloud/sdk-field-20261112/robot-102012.json)
- 当前 `13145` 只读基线补测 10 秒完成：IMU 997 帧、99.392 Hz；关节 15,943 条合计事件，最大接收间隔 78 ms；两流均按预定窗口正常结束。[原始读取](../../artifacts/robot-cloud/sdk-field-20261112/raw-trace-10s-20261113T102350Z.json)
- 当前 `13145` 单次只读 API 复核通过：`GetControlStatus` 为 `ZERO`、电机输出 false、控制方 NONE、无关键故障；`GetMotorStatus` 返回 16 行，其中 12 online、4 offline。该结果仅是现有主控制端口的只读对照，不是 `13146` 验收。[API 原件](../../artifacts/robot-cloud/sdk-field-20261112/current-read-api-20261112-native-owner.json)
- 现场没有发送运动命令、使能电机或修改 CAN 配置。

现场原始证据：[real-connect-20261113](../../artifacts/robot-cloud/real-connect-20261113/README.md)。

## 推进顺序

### 1. 修复 G1，不扩大权限

合并 local-status 与 stream-close 的最小生命周期修复，保留已有真实电机禁用确认。新候选已通过目标机 ARM64 构建、9 项只读流测试和 native close 测试；现场窗口在候选启动前因 PCAN/CAN 物理链路故障中止。恢复硬件链路后再重试一次；不延长超时、不跳过禁用确认、不打开云连接。候选证据：[shutdown-fix-build.json](../../artifacts/robot-cloud/real-connect-20261113/shutdown-fix-build.json)，现场失败：[g1-field-failure.md](../../artifacts/robot-cloud/real-connect-20261113/g1-field-failure.md)。

### 2. 完成 G0/G3 云端登记

由受控 ECS 运维入口追加真实 UUID 的 `device-signing.pub`，为该 UUID 增加六条 MQTT 主题 ACL，保持原 Gateway 身份和历史条目不变。应用前保存配置差异，执行 Broker 配置检查；应用后用 Gateway 真实查询确认从 `403 disabled` 变为设备不可达或上游状态，而不是伪造成功。

### 3. 完成 G2 本机 cloud-link

在 `DOSO_CLOUD_ON=false` 和本地选择关闭时保持 inactive；本机只读源通过后，先用有效证书但关闭许可验证不会连接/上报。云连接启动只在 G0/G1/G3 通过后进行，避免把配置问题混成网络问题。

### 4. 完成 G4 单次真实读取

通过可信后端为该研发设备开启短期状态许可，Gateway 发起一次 `status.read`，核对设备签名、requestId、observedAt、状态字段和 App 展示。一次成功不等于长稳；保留云端和机器人两侧证据。

### 5. 完成 G4/G5 周期与关闭

验证周期状态、许可续期、网络短断、云端拒绝、用户关闭、机器狗进程退出、重启保持关闭和本地重新开启。关闭后的在途请求按合同处理，不把旧缓存冒充新鲜状态。

## 交付规则

- 任何候选二进制都使用新目录和新 systemd drop-in，先保留旧版本回滚入口。
- 现场服务改动前记录 active PID、端口、当前配置摘要和回滚命令。
- 现场验证期间云开关默认关闭，运动输出保持关闭；云端只读链路不得调用控制方法。
- 私钥只在机器狗或受限签发目录存在，不下载到 App，不提交 Git，不写入测试报告。
- 代码测试、模拟联调、ECS 公网测试和真机验收分开记录；任一层通过不能替代其他层。

## 下一次可执行动作

1. 恢复并只读确认四路 PCAN/CAN 链路和新鲜电机反馈，保持运动输出关闭。
2. 在新的维护窗口用候选只读启动，完成单次本机 `GetControlStatus`、停止和回滚。
3. 恢复 ECS 受控运维入口，提交真实 UUID 公钥和 ACL 差异，完成 G0/G3。
4. 通过 G1/G3 后才开启一次真实 cloud-link 许可，完成 App 状态验收。
