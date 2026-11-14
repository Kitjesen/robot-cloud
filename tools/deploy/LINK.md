# ARM64 云连接服务首次部署

文档核对：2026-11-14。本文保留新名 `link-aarch64.tar.gz` 候选的部署准备资料。机器狗程序源码位于 `robot/`，程序名为 `doso-cloud-link`，本机只读端口为 `13146`。研发机已部署该候选、本机只读桥和 `doso-robot-authorizer`；云端设备登记已启用，但真实 MQTT/App 上报仍需设备恢复网络后验收。包的具体范围见 [BUILD.md](../../BUILD.md)。此前生成的 `agent-aarch64.tar.gz` 保留旧二进制、脚本和服务名称，须使用其包内旧说明。

**当前不能在机器狗不可达时按本文开启真实云连接。** 本地源码已实现远程 `enabled=false` 后原子保存关闭、停止采样并退出 MQTT，以及重启保持关闭和本地重新开启的模拟闭环；研发机上的可信本地管理者入口、证书/ACL 和 `13146` 已部署并有历史只读证据，App 端到端和真实上报仍未验收。`DOSO_CLOUD_ON` 只是进程启动配置，不是用户授权。真实设备恢复可达后，必须先执行只读预检，再调用 `127.0.0.1:13148` authorizer API。

## 1. 核对包与目标机

目标为 Linux aarch64，已知系统基线为 Ubuntu 22.04.5 / glibc 2.35。当前新名候选已核对为 AArch64、最高所需 GLIBC_2.34。每台设备使用独立身份与密钥。

通过已核验主机身份的 SSH / SFTP 上传 `link-aarch64.tar.gz` 到目标机 root 私有目录，保持目录权限 700、包权限 600。解压前核对交付渠道记录的包 SHA256，并查看归档目录。包内校验用于检查传输和文件完整性，交付渠道负责确认来源。

```sh
cd /root
sha256sum link-aarch64.tar.gz
tar -tzf link-aarch64.tar.gz
mkdir -m 700 doso-link-stage
tar -xzf link-aarch64.tar.gz -C doso-link-stage
cd doso-link-stage/link
sha256sum --check --strict SHA256SUMS
uname -s
uname -m
getconf GNU_LIBC_VERSION
sh link.sh
```

归档顶层为 `link/`，包含下列文件。

```text
link.sh / LINK.md / SHA256SUMS / manifest.json
bin/doso-cloud-link
config/env.example / ca.crt / gateway-signing.pub
systemd/doso-cloud-link.service
```

`link.sh` 需要 root、Linux aarch64，检查全部必需文件的校验清单与哈希，拒绝覆盖已有配置目录、二进制或 systemd 单元。脚本创建或复用 `doso-cloud` 系统账户，安装 root 所有的二进制与单元、root 可写且 doso-cloud 组可读的配置和公有材料，最后执行 `systemctl daemon-reload` 并启用开机单元。安装后的 `agent.env` 保持 `DOSO_CLOUD_ON=false`。每次启动仍先检查安装开关和本地 choice，choice 为 false 时退出并保持断云，choice 为 true 时恢复云连接。

若已有安装或脚本中途失败，先检查已安装文件与错误原因。保留现场配置，按维护流程处理部分安装，重复运行仍会拒绝覆盖。

安装脚本同时检查 `doso-agent.service` 与 `doso-cloud-link.service` 以及两种二进制路径。已有旧安装需要在维护窗口单独迁移，避免两个通信进程同时运行。共享配置继续保存在 `/etc/doso/robot-cloud/agent/agent.env`，LingTu 仍从此文件读取 `DOSO_DEVICE_ID`。

启动开关使用 `DOSO_CLOUD_ON`。新程序在该变量缺失时兼容旧的 `DOSO_REMOTE_ENABLED`，新变量存在时以新变量为准。安装样例只写入 `DOSO_CLOUD_ON=false`。

## 2. 先核对 Brainstem 回环入口

本版本接入独立的本机只读状态端口。配套 Brainstem 通过 `THUNDER_LOCAL_STATUS_ENABLED=true` 启用 `127.0.0.1:13146`，只注册 `GetControlStatus`。研发机已有历史只读读取证据；每次现场窗口仍须先检查当前监听和状态。

```sh
ss -ltnp 'sport = :13146'
```

云连接服务固定连接 `127.0.0.1:13146`，仅调用 `/brainstem.api.v1.RobotControl/GetControlStatus`。确认监听属于预期 Brainstem，且只绑定 IPv4 回环。既有 LAN 或 mTLS 控制入口使用其原有配置，设备上的 `192.168.x.x:13145` 监听不能替代本机状态入口。

如果没有确认此端点可达，停在本步，记录 `ss` 结果并保留云连接服务禁用状态。需要另行根据 Brainstem 真实配置决定集成方案。此流程中的安装和验收只管理 `doso-cloud-link`，Brainstem 绑定、CAN 和运动服务有各自变更流程。

## 3. 绑定真实设备与独立凭据

从业务侧设备库存记录取得此狗的不可变 UUID，同时核对物理设备归属。新设备在库存登记时复用 `devices.id` 的 UUID v4 默认生成值；用户绑定只改变归属，不生成新的设备 ID。已有设备继续使用已登记值。

永久 ID 保存在 `/etc/doso/robot-cloud/agent/agent.env` 的 `DOSO_DEVICE_ID`，固件升级和解绑不重新生成。包中 `DOSO_DEVICE_ID` 是示例值，必须替换为业务记录中的真实 UUID。证书 CN、云连接服务的 `DOSO_DEVICE_ID`、云端登记和主题 UUID 必须相同。配套 LingTu 的 `lt-host.service` 已共享加载此文件，DOSO SDK 的 `robot_id` 从同名变量读取；配置在进程启动时生效。未配置时 SDK 返回缺失，非空值不符合规范 UUID v4 时握手报告 `invalid_robot_id`。

由受限签发流程为这台设备准备以下材料。

| 文件 | 用途与位置 |
| --- | --- |
| `device.crt` / `device.key` | 独立 MQTT 客户端证书与私钥，CN 为真实 UUID，客户端认证扩展匹配 Broker 现有受信 CA |
| `device-signing.key` | 此设备独立 Ed25519 签名私钥，PKCS#8 PEM 格式 |
| `device-signing.pub` | 配套 Ed25519 公钥，SubjectPublicKeyInfo PEM 格式，提供给 ECS 登记 |
| 包内 `ca.crt` | MQTT 服务端受信 CA 公有证书，核对当前服务端信任链 |
| 包内 `gateway-signing.pub` | 已部署 Gateway 的 Ed25519 验签公钥，核对运维记录的指纹 |

将前三项设备侧文件放到目标机 root 独占的 `/root/doso-link-private/`，目录 700、文件 600。使用经核验的传输渠道，私钥保持在设备侧和受限签发流程内。CA 私钥留在签发端，Gateway 服务 Bearer 凭据保存在可信查询端。

```sh
for file in device.crt device.key device-signing.key; do
    install -o root -g doso-cloud -m 640 "/root/doso-link-private/$file" /etc/doso/robot-cloud/agent/
done
sudoedit /etc/doso/robot-cloud/agent/agent.env
```

编辑时保持 `DOSO_CLOUD_ON=false`，设置真实 `DOSO_DEVICE_ID`，确认 `DOSO_MQTT_URL=mqtts://8.153.169.177:8883` 和下列路径。配置由 systemd 读取，凭据文件直接从路径加载。

```text
DOSO_MQTT_CA=/etc/doso/robot-cloud/agent/ca.crt
DOSO_MQTT_CERT=/etc/doso/robot-cloud/agent/device.crt
DOSO_MQTT_KEY=/etc/doso/robot-cloud/agent/device.key
DOSO_DEVICE_SIGNING_KEY=/etc/doso/robot-cloud/agent/device-signing.key
DOSO_GATEWAY_PUBLIC_KEY=/etc/doso/robot-cloud/agent/gateway-signing.pub
```

## 4. ECS 最小新增登记与 ACL

先在受限工作目录用 [render-device-registration.py](render-device-registration.py) 生成只读 patch。它校验 UUID v4、现有登记没有重复 ID、ACL 没有同名用户，并输出设备条目、公钥副本和六条精确 ACL；它不会连接 ECS、修改输入文件或 reload Broker。研发机登记和只读启用已由 ECS 云助手完成，不能用本地生成器输出替代线上证据。

```sh
python3 tools/deploy/render-device-registration.py \
  --device-id <真实设备UUID> \
  --public-key /受限路径/device-signing.pub \
  --devices /受限路径/devices.json \
  --acl /受限路径/acl.conf \
  --output-dir /受限路径/cloud-patch
```

先检查 `registration-report.json`、`device-entry.json` 和 `acl-entry.conf`，再将它们作为一次受控变更上传。输出目录中的公钥是公开材料；私钥不应出现在目录中。

ECS 入口恢复后可使用 [apply-device-registration.sh](apply-device-registration.sh) 执行同一变更。脚本要求 root，先备份现有登记/ACL，拒绝重复 UUID 和非预期六行 ACL，原子写入设备登记，运行 `mosquitto --test-config` 后仅 reload；任一步失败会恢复备份。云端登记开关与机端授权是两个步骤；启用云端登记不会启动机器狗 cloud-link。

```sh
sh tools/deploy/apply-device-registration.sh /受限路径/cloud-patch
```

准备启用真实设备时，由运维完成以下 ECS 配置。先备份现有登记和 ACL，核对现有内容，并保留所有原条目。设备公钥存为 `/etc/doso/robot-cloud/gateway/device-真实UUID-signing.pub`，所有者 `root:doso-cloud`、权限 640。

在现有 `/etc/doso/robot-cloud/gateway/devices.json` 的 `devices` 数组中追加一个条目，确认同一 UUID 只有一条。下面为单个条目的结构，替换占位符后再加入现有数组。

```json
{
  "deviceId": "真实设备UUID",
  "enabled": false,
  "publicKeyPath": "device-真实UUID-signing.pub"
}
```

在现有 `/etc/doso-mqtt/acl.conf` 末尾新增此设备的精确主题权限，将全部占位符替换成相同 UUID。

```text
user 真实设备UUID
topic read doso/v1/devices/真实设备UUID/status/requests
topic write doso/v1/devices/真实设备UUID/status/replies
topic write doso/v1/devices/真实设备UUID/reporting/hello
topic read doso/v1/devices/真实设备UUID/reporting/control
topic write doso/v1/devices/真实设备UUID/reporting/acks
topic write doso/v1/devices/真实设备UUID/status/reports
```

保留原有 `doso-gateway` 身份与主题权限，并按 `cloud/broker/acl.conf.example` 核对其六条对应读写方向。新增设备条目只授予上面六条精确主题权限。ACL 负责消息通道访问，持续采集的用户许可由业务后端逐设备单独决定，云连接服务依据收到的有效短期许可采样上报。

ACL 文件保持 `mosquitto:mosquitto`、权限 640。复核修改差异并检查 Broker 配置，通过后仅 reload Mosquitto。

```sh
mosquitto --test-config -c /etc/doso-mqtt/mosquitto.conf
systemctl reload mosquitto
```

配置检查验证语法，ACL 还需人工核对身份与读写方向。Gateway 每次查询读取登记文件，完成 JSON 校验后原子替换该文件即可生效。保留 root 所有、doso-cloud 组可读权限，所有已有登记和公钥继续保留。

## 5. 真机云端验收（暂不可执行）

先完成本地管理者验证、远程管理开关及持久化，并验证关闭时 MQTT 确实断开；还需完成真实设备身份、凭据和业务许可对接，并部署包含查询许可校验的匹配版本。用户在狗旁开启远程管理后，才能验证设备认证、状态上报和 App 显示；关闭后需验证云连接断开及不再上报。不得把安装开关或 HTTP 受理当作用户授权或设备确认。

## 6. 回退

目标机回退仅停止此云连接服务。

```sh
systemctl stop doso-cloud-link
systemctl is-active doso-cloud-link
```

预期 `inactive`，`is-active` 此时返回非 0。保留安装文件与配置供排查。运行中的 MQTT 证书吊销和连接撤权须按既有运维流程处理。

本文交付边界为离线准备与待执行的首次真机验收。当前待验证 ARM64 加载与启动、目标机回环入口、真实 GetControlStatus 和本轮公网回复；App 设备归属 / 客户许可接线与 5G 验收分别跟踪。

2026-11-07已登记模拟设备经正式cloud-link、MQTT、隔离云Gateway、真实业务数据库和Web App验证状态流程，证据见[模拟设备链路](../../docs/SIMULATED_FLOW.md)。该轮未安装到真实机器狗，也未验证手机扫码、完整断云或断云后的本地重新授权。
