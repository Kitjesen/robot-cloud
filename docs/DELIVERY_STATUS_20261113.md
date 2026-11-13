# 真实机器狗云连接交付状态

更新时间：2026-11-13（Asia/Shanghai）

这份记录只写本轮实际执行结果。模拟链路、源码测试、云端公网探测和真机部署分开统计。

## 本轮已验证

| 范围 | 结果 | 说明 |
| --- | --- | --- |
| DOSO Rust SDK | 124 项通过 | 8 个库测试、103 个 gRPC/API 测试、2 个消息格式测试、11 个文档测试；工作区 `D:\inovxio\brain\DOSO-SDK` 的 `rust` 分支 |
| robot-cloud Rust 工作区 | 83 通过，1 ignored | `cargo test --workspace --locked --offline -j 1`；没有失败；新增登记公钥路径越界回归 |
| 格式与静态检查 | 通过 | `cargo fmt --all -- --check`、workspace Clippy `-D warnings` |
| 本机模拟云链路 | 通过 | `lab check`：许可、签名状态、状态变化、读失败保留旧时间、关闭、过期、重连、视频、本地关闭持久化 |
| 本机 MQTT 权限 | 9 项通过 | `lab auth-check`：合法证书、无证书、不受信、跨设备、设备控制主题越权和 Gateway 身份检查 |
| 机器狗到公网 MQTT | TLS 通过 | `8.153.169.177:8883`，TLS 1.3，服务端证书校验通过 |
| 真实 Gateway 查询 | 可达但拒绝 | 真实 UUID `d82a831a-2e86-4b46-975b-3d42473583b7` 返回 `403 disabled`，说明云端登记尚未启用 |
| 云端登记变更工具 | 通过 | `render-device-registration.py` 离线校验 UUID v4、重复登记和 ACL 用户，生成 `enabled=false` 的设备条目、公钥副本与六条精确 ACL；不连接 ECS、不改输入、不 reload |
| Gateway 登记路径边界 | 通过 | 拒绝绝对路径、`..` 和其他非普通路径，避免登记文件被篡改后加载登记目录外的公钥；Gateway 27 项测试通过 |

本轮没有发送运动命令、没有使能电机、没有修改 CAN，也没有重启机器狗服务。

本轮新增的离线登记工具测试：`python -m unittest tools/deploy/test_render_device_registration.py -v`，3 项通过。它只生成待审查片段，不能代替 ECS 备份、`mosquitto --test-config`、reload 和真实 Gateway 查询。

最新只读现场复核（2026-11-13 21:29 CST）：`brainstem.service` 已恢复 active，`13145` 可达；`doso-cloud-link` 和 `brainstem-grpc-proxy` 仍 inactive，`13146` 未监听；`can0`–`can3` 均为 UP、ERROR-ACTIVE、1 Mbps，IMU 设备存在。预检仍以退出码 1 拒绝继续，原因是现有映射脚本要求 `can0 → USB 1-2`，实际观察为 `can0 → USB 1-1.1`、`can1 → USB 1-2.1`。没有执行 `ip link set`、服务启停或 CAN 配置。[最新原件](../artifacts/robot-cloud/real-connect-20261113/field-preflight-result.json)

本轮命令原始输出：[local-lab-check.log](../artifacts/robot-cloud/real-connect-20261113/local-lab-check.log) · [local-auth-check.log](../artifacts/robot-cloud/real-connect-20261113/local-auth-check.log)

## 真实交付门

### G1：本机只读源

当前 `brainstem.service` 已恢复 `flowfix10`，`13145` active；`13146` 没有监听。合并候选曾经成功绑定 `127.0.0.1:13146`，但停止时进入既有物理电机禁用确认流程并超时，因此没有安装为正式版本。候选的离线预检通过不等于真机生命周期通过。最新现场四路 CAN 已重新为 UP，但物理角色映射仍未确认，不能进入候选维护。

### G3：云端设备登记

真实设备签名公钥和六条精确 MQTT ACL 已准备在 [real-connect-20261113/cloud-patch](../artifacts/robot-cloud/real-connect-20261113/cloud-patch/)；当前 ECS SSH 入口仍被安全组拦截，不能安全地修改线上 `devices.json` 或 `acl.conf`。本轮只读探测记录在 [cloud-ssh-preflight.json](../artifacts/robot-cloud/real-connect-20261113/cloud-ssh-preflight.json)。

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

