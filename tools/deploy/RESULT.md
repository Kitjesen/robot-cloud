# 2026-11-03 云端部署与公网模拟接入记录

2026-11-03（Asia/Shanghai），实例 `i-uf6hb2phe34p0mt1xfca`，上海地域。

2026-11-08文档核对：下文保留当时的二进制、地址、时间和结果，不表示当前线上仍运行同一版本。后续2026-11-04自动同步部署、2026-11-06视频业务授权部署，以及2026-11-07模拟设备与实际Web App链路统一见[PROGRESS.md](../../PROGRESS.md)和[模拟设备记录](../../docs/SIMULATED_FLOW.md)。

## 已运行

| 服务 | 地址 | 结果 |
| --- | --- | --- |
| Mosquitto 2.1.2 | `mqtts://8.153.169.177:8883` | active、enabled，要求客户端证书 |
| Rust Gateway | ECS 内部 `127.0.0.1:8130` | active、enabled，已完成 MQTT 5 / TLS 1.3 连接 |
| Nginx HTTPS | `https://8.153.169.177:443` | active、enabled，TLS 转发状态查询到 Gateway |

部署前通过固定主机公钥的 SSH 确认系统为 Ubuntu 24.04.5 / x86_64，部署目录及 1883、8883、8130 均未占用，UFW 未启用。保留原有 SSH、DNS 与备份服务。
网关二进制 SHA256：`89b67516a2cc28469f2381ebaabaf1ef6b3f5f9d35c9a992adec5e459e146495`，与本机发布包一致。

安全组新增三条入站，原有规则保留：

- `140.207.95.88/32` → TCP 22，`doso-admin-current-ip`。
- `0.0.0.0/0` → TCP 8883，`doso-mqtt-mtls`。
- `0.0.0.0/0` → TCP 443，`doso-status-https`。

没有公开 1883 或 8130。根 CA 私钥留在本地受限目录，未上传 ECS。

## 实际验收

- 01:45:15，本机通过公网 IP 连接 ECS，验证服务器证书并提交短期客户端证书，完成 MQTT 5 / QoS 1 订阅、发布与回收同一测试消息。
- 测试主题独立于机器人状态主题，不发送机器人控制或状态请求。
- 内部 Gateway 无服务凭据返回 `401 unauthorized`；带有效凭据查询未登记 UUID 返回 `403 disabled`，说明 Gateway 已完成 MQTT 订阅并执行登记检查。
- 无客户端证书连接被拒绝：客户端收到 `ECONNRESET`，服务器日志明确记录 `peer did not return a certificate`。
- 测试结束移除临时主题 ACL 并 reload；同一测试证书再次发布被拒绝，MQTT 原因码 `135`。这证明主题权限已移除，不代表证书已吊销或 CONNECT 被禁止。
- 最终两项服务仍为 active、enabled。ACL 文件为 `mosquitto:mosquitto 640`；安装脚本已同步这一要求。

本地原始运行记录已迁到受限目录 `D:\DOSO\private\robot-cloud\cloud-stage` 下的 `install.log` 与 `probe/result.json`；私有凭据目录不作为可共享报告。

## 公网状态查询

02:08:59，运行真正的 Rust 查询示例，经过公网 HTTPS → ECS Gateway → 公网 MQTT → Windows Rust Agent → 模拟 Brainstem，返回 HTTP 200。
完整返回值见 [query-result.json](query-result.json)，`simulated:true` 是验收记录的标识，不是向生产协议添加的字段。
请求编号为 `cde6123d-8bcc-436c-a9dd-500284dfe51d`，采集时间为 `2026-11-02T18:08:59.284Z`（UTC）。

- 使用错误服务凭据经公网返回 401，移除演示设备登记后经公网返回 403；查询程序均以非 0 退出。
- 模拟 GetControlStatus 实际读取次数为 1；演示 Agent 与模拟服务已停止，13145 无监听。
- 演示设备登记、公钥和专属主题权限已移除，保留三项云服务运行。没有真实机器人参与，也未启用运动或命令 worker。
- `transport:local-insecure` 只描述 Agent 到本机模拟 gRPC；公网 HTTPS 与 MQTT 均校验证书并使用 TLS。

## 当时的剩余工作

设备登记仍为 `{"devices":[]}`。真实机器狗 Agent 尚未部署，未验收真实 GetControlStatus、5G、App 账号/归属与客户许可，也未启用旧命令 worker。
公网 HTTPS 入口已可供可信后端调用；客户 App 的账号、设备归属与许可仍需接线。证书续期/吊销及依赖补丁维护边界见 [README.md](README.md)。

本节描述2026-11-03时点。当前业务接口与模拟设备App链路已有后续验证，真实机器狗开通、本地云连接授权及关闭后持久化断云仍待完成；以根目录进度和合同中的当前说明为准。
