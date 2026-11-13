# ECS 部署与私有资料

文档核对：2026-11-08。本页的线上结果来自注明日期的部署记录，本次未重新连接ECS。

ECS `8.153.169.177` 的既有运行环境为Ubuntu 24.04 / x86_64，部署Mosquitto、Rust Gateway和Nginx HTTPS入口。2026-11-04已部署新版Gateway、自动业务同步配置、报告主题ACL和可写许可状态目录；当次三项服务均为active，Gateway `NRestarts=0`，Broker日志确认Gateway通过TLS 1.3连接，公网未授权查询返回HTTP 401。当前线上是否包含全部后续源码改动，需按实际二进制与发布记录核对。

狗端云连接服务源码已采用 `robot/` 和 `doso-cloud-link` 命名，新名 ARM64 离线候选包已生成并核对架构和内容；研发机上的本机只读入口和本地授权服务已部署，真实设备自动同步和 App 端到端查询仍待验收。部署准备资料见 [LINK.md](LINK.md)：真实设备恢复网络后，先运行只读 [field-preflight.sh](field-preflight.sh) 检查 `13146`、`13148`、choice 和安装 gate，再使用 `127.0.0.1:13148` 的 authorizer API 完成本地授权，不要手工写 `choice` 文件。

现场维护前先在机器狗上运行 [field-preflight.sh](field-preflight.sh)。它只读取 systemd、端口、IMU 和四路 CAN，并调用 `thunder-can-up.py --check`；不会启动服务、拉起 CAN 或修改配置。只有四路 CAN 都是 UP 且只读检查通过，才进入 Brainstem 候选维护窗口。

2026-11-03的历史公网模拟查询记录见 [RESULT.md](RESULT.md)，当时Brainstem使用本机模拟服务。2026-11-07的实际Web App、真实业务数据库与模拟设备链路见[模拟设备记录](../../docs/SIMULATED_FLOW.md)；该轮使用隔离Gateway和SSH隧道，不等于真机或独立公网MQTT验收。最新进展见 [PROGRESS.md](../../PROGRESS.md)。

设备库存、证书和签名身份的职责及当前缺口见 [IDENTITY.md](IDENTITY.md)。

## 文件位置

| 位置 | 内容 |
| --- | --- |
| `D:\DOSO\robot-cloud\tools\deploy` | 可共享的安装脚本、HTTPS 模板与历史结果 |
| `D:\DOSO\private\robot-cloud\cloud-stage` | 受限 ACL 的证书、私钥、服务凭据及原始部署资料 |
| `D:\DOSO\artifacts\robot-cloud\linux` | 迁移前生产 Linux 包与 manifest |
| `D:\DOSO\archive\robot-cloud\20261103` | 历史实现与实验资料 |

历史 `RESULT.md` 中的 `.runtime/cloud-stage` 为当时记录路径，对应资料现位于上述私有目录。新本地联调临时文件仍写入工程 `.runtime/`，与长期部署凭据分开管理。

## 服务与权限

- MQTT 使用 TCP 8883 与 mTLS，证书身份绑定主题 ACL。
- Gateway 仅监听 `127.0.0.1:8130`，Nginx 在 443 转发 `/api/v1/robots/` 查询，其余路径返回 404。
- 公网安全组开放 TCP 443、8883，SSH 22 限运维 IP。
- 客户端证书私钥、消息签名私钥和服务 Bearer 分别保管。根 CA 私钥留在受限本地目录，交付包只含公有材料和配置样例。
- 历史模拟设备登记、公钥与主题 ACL 已移除，设备登记恢复为空。真实设备 ID 来自业务侧绑定记录。

## 首次安装脚本

[cloud.sh](cloud.sh) 用于尚无 `/etc/doso-mqtt` 和 Gateway 配置的目标机首次安装。[https.sh](https.sh) 在 Nginx 尚未安装且 80 / 443 空闲时安装 HTTPS 入口。现有 ECS 已部署，后续维护按配置变更范围操作。

运维准备的私有上传目录结构如下。

```text
cloud.sh / SHA256SUMS / https.sh / https.conf
bin/doso-cloud-gateway
broker/mosquitto.conf / acl.conf / tls/{device-ca.pem,server.pem,server.key}
gateway/{gateway.env,devices.json,ca.crt,client.crt,client.key,signing.key,signing.pub}
systemd/doso-robot-cloud-gateway.service
```

目录经验证主机身份的 SSH / SFTP 上传，仅 root 可读。`SHA256SUMS` 覆盖二进制、配置和脚本。首次安装顺序为以 root 执行 `sh cloud.sh`，再执行 `sh https.sh`。迁移前首次安装记录保存在 `RESULT.md`，2026-11-04 在现有服务上执行了 Gateway 增量升级。

脚本从 Mosquitto 维护的签名 PPA 安装软件，以受限用户运行服务。验收需核对 systemd 状态、MQTT 订阅与端口监听，并分别检查未授权的 401、未登记设备的 403，以及已登记在线设备的签名状态回复。真实机器人上线仍需单独完成目标机验收。

## 可信后端查询

接口为 `POST https://8.153.169.177/api/v1/robots/{deviceId}/status`，空 body，服务 Bearer。Rust 客户端位于 [query.rs](../../cloud/gateway/examples/query.rs)。在源码外创建私有查询配置，将凭据替换为实际值。

```json
{
  "baseUrl": "https://8.153.169.177",
  "apiKey": "由运维配置的服务凭据",
  "caFile": "ca.crt"
}
```

`caFile` 相对于配置文件所在目录解析。当前入口使用私有 CA，客户端显式信任 CA 并验证服务器证书。在 workspace 根目录运行，将配置路径和设备 UUID 替换为本轮准备值。

```powershell
cargo run --locked -p doso-cloud-gateway --example query -- 'D:\DOSO\private\robot-cloud\cloud-stage\query.json' '本轮设备UUID'
```

设备必须已登记、启用并连接MQTT，而且已确认当前有效的用户上报许可；否则按当前合同拒绝按需采样。未授权返回401，未登记或禁用返回403，设备不回复会超时，示例遇到非2xx以非0退出。现有旧发布包未必包含后续许可校验，必须部署与[CONTRACT.md](../../CONTRACT.md)相符的版本后再执行新合同验收。模拟公网云连接服务的Rust入口见 [TESTING.md](../../TESTING.md)。2026-11-04已重新确认公网未授权请求返回401，真实设备身份、登记与签名回复仍待验证。

## 维护边界

### 持续状态部署

2026-11-04 已安装 [gateway-reporting.conf](gateway-reporting.conf) 对应的 systemd 覆盖配置，在 `ProtectSystem=strict` 下通过 `StateDirectory` 创建运行用户可写的 `/var/lib/doso-cloud`，许可状态路径为 `/var/lib/doso-cloud/reporting.json`。现场已确认目录归属和服务启动，真实许可保存仍待设备接入后验证。

Broker 已补齐 Gateway 的 reporting 与状态上报权限，保留原按需查询规则。设备主题仍须按真实设备身份分别授权，规则见 [ACL 样例](../../cloud/broker/acl.conf.example)。Gateway 已配置业务 HTTPS 同步地址与专用服务密钥，旧二进制和 ACL 已备份，含密钥的临时上传材料已清理。

业务同步接口为 `https://api-test.dosorobotics.com/functions/v1/remote-state-sync`。2026-11-04本机携带专用凭据发送空设备批次得到HTTP 200和空许可列表，该请求没有传送设备状态；2026-11-07又完成模拟设备的真实业务同步与Web App验证。Gateway对真实机器狗的自动同步仍待验收。

Mosquitto PPA 签名指纹为 `7B2FBD7C964368452615DB74E633D0D8216D1222`，迁移前记录版本为 2.1.2，ACL 插件为 `/usr/lib/x86_64-linux-gnu/mosquitto_acl_file.so`。PPA 更新需主动维护。[PPA 来源](https://launchpad.net/~mosquitto-dev/+archive/ubuntu/mosquitto-ppa)

原部署检查指出 Ubuntu noble 常规源的 `libcjson1 1.7.17-1` 尚需补丁维护，Ubuntu Pro ESM 提供补丁版本。当前配置关闭控制 API，ACL 限制业务状态主题。启用管理 API 前需更新依赖并重新核对暴露范围。

App 设备归属、客户许可、证书续期 / 吊销、真实机器狗状态与 5G 均需各自验收，具体协议见 [CONTRACT.md](../../CONTRACT.md)。
