# 真实机器狗云连接交付状态

更新时间：2026-11-13（Asia/Shanghai）

这份记录只写本轮实际执行结果。模拟链路、源码测试、云端公网探测和真机部署分开统计。

## 本轮已验证

| 范围 | 结果 | 说明 |
| --- | --- | --- |
| DOSO Rust SDK | 124 项通过 | 8 个库测试、103 个 gRPC/API 测试、2 个消息格式测试、11 个文档测试；工作区 `D:\inovxio\brain\DOSO-SDK` 的 `rust` 分支 |
| robot-cloud Rust 工作区 | 84 通过，1 ignored | `cargo test --workspace --locked --offline -j 1`；没有失败；包含远程关闭持久化回归 |
| 格式与静态检查 | 通过 | `cargo fmt --all -- --check`、workspace Clippy `-D warnings` |
| 本机模拟云链路 | 通过 | `lab check`：许可、签名状态、状态变化、读失败保留旧时间、关闭、过期、重连、视频、本地关闭持久化 |
| 本机 MQTT 权限 | 9 项通过 | `lab auth-check`：合法证书、无证书、不受信、跨设备、设备控制主题越权和 Gateway 身份检查 |
| 机器狗到公网 MQTT | TLS 通过 | `8.153.169.177:8883`，TLS 1.3，服务端证书校验通过 |
| 真实 Gateway 查询 | 可达但拒绝 | 真实 UUID `d82a831a-2e86-4b46-975b-3d42473583b7` 返回 `403 disabled`，说明云端登记尚未启用 |
| 云端登记变更工具 | 通过 | `render-device-registration.py` 离线校验 UUID v4、重复登记和 ACL 用户，生成 `enabled=false` 的设备条目、公钥副本与六条精确 ACL；不连接 ECS、不改输入、不 reload |
| Gateway 登记路径边界 | 通过 | 拒绝绝对路径、`..` 和其他非普通路径，避免登记文件被篡改后加载登记目录外的公钥；Gateway 27 项测试通过 |

本轮没有发送运动命令、没有使能电机、没有修改 CAN，也没有重启机器狗服务。

本轮新增的离线登记工具测试：`python -m unittest tools/deploy/test_render_device_registration.py -v`，3 项通过。它只生成待审查片段，不能代替 ECS 备份、`mosquitto --test-config`、reload 和真实 Gateway 查询。

本轮新增的远程关闭回归已通过：[local-remote-close-check.log](../artifacts/robot-cloud/real-connect-20261113/local-remote-close-check.log)。模拟设备收到签名 `enabled=false` 后原子保存 `choice=false`、停止采样并退出 cloud-link；重启保持 `cloud_off`，本地 `choice=true` 配合新许可后恢复上报。普通许可到期保持本地选择不变。

历史只读现场复核（2026-11-13 21:29 CST，映射脚本更新前）：`brainstem.service` 已恢复 active，`13145` 可达；`doso-cloud-link` 和 `brainstem-grpc-proxy` 仍 inactive，`13146` 未监听；`can0`–`can3` 均为 UP、ERROR-ACTIVE、1 Mbps，IMU 设备存在。预检以退出码 1 拒绝继续，原因是旧脚本要求 `can0 → USB 1-2`，实际观察为 `can0 → USB 1-1.1`、`can1 → USB 1-2.1`。没有执行 `ip link set`、服务启停或 CAN 配置。[历史原件](../artifacts/robot-cloud/real-connect-20261113/field-preflight-result.json)

历史复核（flowfix11 切换前）：用户随后确认 CAN 已恢复；当时 `brainstem.service` 实际运行 flowfix11，`13145` active，`13146` 尚未监听，cloud-link 和 grpc proxy 仍 inactive。之后当前映射脚本已按实际拓扑恢复，未改动 CAN 或运动配置。[历史原件](../artifacts/robot-cloud/real-connect-20261113/failure-inspection.json)

随后 flowfix11 的当前映射脚本已按实际拓扑恢复，新的只读 preflight 退出 0：`thunder-can.service` active/exited，`127.0.0.1:13146` 正在监听，四路 CAN 均 UP/ERROR-ACTIVE/1 Mbps。[最新 preflight](../artifacts/robot-cloud/real-connect-20261113/field-preflight-result.json)

通过机器狗上的只读 Dart 客户端调用 `127.0.0.1:13146/GetControlStatus` 成功，读回 CMS GROUNDED、输出关闭、控制方 NONE、关键故障 false、gRPC lease inactive；没有调用控制 RPC。[原件](../artifacts/robot-cloud/real-connect-20261113/read-current-13146.json)

随后通过现有 LAN 主控制端口 `192.168.66.64:13145` 做了六个明确的只读 RPC：`GetControlStatus`、`GetProfile`、`GetCmsState`、`GetMotorStatus`、`GetParams` 成功，`GetVoltage` 因源码核对条件不满足而跳过。当前读回 CMS ZERO、输出关闭、控制方 NONE、关键故障 false、16/16 电机 online、无状态码/错误，当前 profile 为 `all_elbow`；这证明主端口读状态正常，不等于 `13146` 只读桥已部署，也不等于物理禁用确认。[只读原件](../artifacts/robot-cloud/sdk-field-20261112/current-read-api-20261112-native-owner.json)

本轮命令原始输出：[local-lab-check.log](../artifacts/robot-cloud/real-connect-20261113/local-lab-check.log) · [local-auth-check.log](../artifacts/robot-cloud/real-connect-20261113/local-auth-check.log)

## 真实交付门

### G1：本机只读源

当前 `brainstem.service` 运行 `flowfix11_prone_20261113_70e3b574`，`13145` active，`127.0.0.1:13146` 已监听。四路 CAN 的当前实际映射通过只读检查，`GetControlStatus` 已真实读通；当前已完成本机只读源验收。尚未对 flowfix11 做停止/回滚生命周期窗口，不能把旧 flowfix10 候选的关闭竞态修复写成已部署。[本机只读原件](../artifacts/robot-cloud/real-connect-20261113/read-current-13146.json)

### G2：机器狗 cloud-link

新 ARM64 候选已安装到 `/opt/doso/bin/doso-cloud-link`，旧版本备份为 `/opt/doso/bin/doso-cloud-link.before-remote-close-20261113T145748Z`。安装后服务仍 inactive/disabled，关闭态运行退出 0、输出 `cloud_off`，候选 SHA-256 为 `15a1603fcd3af3feba71d3481e91f481011bfce0b80f141df6015517dd7bea76`。[安装原件](../artifacts/robot-cloud/real-connect-20261113/cloud-link-install.json)

### G3：云端设备登记

真实设备签名公钥和六条精确 MQTT ACL 已准备在 [real-connect-20261113/cloud-patch](../artifacts/robot-cloud/real-connect-20261113/cloud-patch/)。最新开发机公网 IP 为 `96.44.160.185`，MQTT ECS `8.153.169.177:22` 仍被安全组拦截；业务 ECS 可登录但到 MQTT ECS 的 `22` 也被拦截，不能当跳板。当前不能安全地修改线上 `devices.json` 或 `acl.conf`。[最新探测](../artifacts/robot-cloud/real-connect-20261113/cloud-ssh-preflight-current.json)

恢复受控入口后，执行顺序固定为：

1. 备份线上 `devices.json` 和 `acl.conf`；
2. 追加真实 UUID 的公钥、设备条目和六条 ACL，第一次保持 `enabled=false`；
3. `mosquitto --test-config` 通过后只 reload；
4. 先完成 G1，再将设备登记改为 `enabled=true`；
5. 在有效用户许可下启动 `doso-cloud-link`，只做一次 `status.read`。

不能用 `brainstem-grpc-proxy.service` 绕过 G1；它是完整 `13145` TCP 转发，不是只读状态桥。

## 下一次维护窗口

只执行一次现场窗口，避免反复重启旧服务：

1. 记录当前 PID、端口、云开关和 CAN 只读快照；
2. 安装合并的 local-status + stream-close 候选；
3. 确认 `13145`、`13146`，调用一次 `GetControlStatus`；
4. 停止候选并确认安全关闭流程正常；
5. 回滚到 `flowfix10`，再次确认 `13145` active、云连接保持关闭。

G1 和 G3 同时通过后，才开启真实 `doso-cloud-link`。随后再做周期上报、网络短断、远程关闭、重启保持关闭和本地重新开启验收。

本轮现场 G1 窗口已实际执行但在候选启动前中止：旧服务停止时发现 `can1`/PCAN USB 链路为 DOWN，`usbbus2` 返回 `errno=6`，systemd 最终进入 failed。候选没有启动，`13146` 没有监听。随后只读复核显示 Brainstem 已恢复、四路 CAN 均 UP，但 `can0` 的 USB parent 仍与既有映射脚本不一致；当前必须由现场确认物理总线标签后再进入下一次维护窗口。详见 [g1-field-failure.md](../artifacts/robot-cloud/real-connect-20261113/g1-field-failure.md) 和最新 [field-preflight](../artifacts/robot-cloud/real-connect-20261113/field-preflight-result.json)。

