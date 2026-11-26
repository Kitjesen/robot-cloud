# DOSO OTA 与机器狗项目交接手册（2026-11-26）

本文档面向接手 **OTA 发布体系** 与 **机器狗（THUNDER）机载运行时 / 云端联调** 的研发同事，汇总当前仓库全景、云服务器架构、已完成与已验收能力、现役样机状态、后续待办优先级以及日常运维操作命令。

> **安全边界提示**：本仓库（`Kitjesen/robot-cloud`）当前为公开状态（Public），本文档与脚本中**不包含**任何私钥、Token、OSS AccessKey、SSH/sudo 密码或个人账号信息；线下凭据移交见文末[第七节](#七线下私密交接清单严禁入-git)。

---

## 一、代码仓库全景与版本基线

当前系统按“**云端与机载通信/OTA (`robot-cloud`) + 机载实时运控 (`brainstem`) + 对外开发套件 (`DOSO-SDK`) + App/语音/硬件固件**”拆分维护：

| 仓库 | 可见性 | 主分支 / 最新基线 | 核心职责 | 本地常用目录（开发机） |
| --- | --- | --- | --- | --- |
| [`Kitjesen/robot-cloud`](https://github.com/Kitjesen/robot-cloud) | Public | `main`（`ota-v0.1.0` + 2026-11-26 运维通道与切流更新） | 云端 OTA 发布中心（`cloud/release`）、机载 OTA Agent（`ota`）、云端网关（`cloud/gateway`）、机载云连接（`robot`）、本地授权开关（`robot-authorizer`）、443 WSS 反向运维通道（`tools/ops-tunnel`） | `D:\DOSO\robot-cloud` |
| [`Kitjesen/brainstem`](https://github.com/Kitjesen/brainstem) | Private | `master`（`d9617d6`, tag `v0.0.3`） | 纯 Rust 四足轮腿主控运行时 `doso-brainstem`（200Hz/50Hz 硬实时、SocketCAN RS04/RS02、Hi91 IMU、ONNX 策略热部署、动作库/站高/跌倒恢复）与硬件调测工具 `doso-cli`；旧 Dart v2 已归档于 tag `v2.2.0-dart-final` | `D:\inovxio\brain\brainstem` |
| [`Kitjesen/DOSO-SDK`](https://github.com/Kitjesen/DOSO-SDK) | Public | `rust`（`f1f1d89`） | 统一 SDK（`doso-sdk`、Proto 协议、Python/C++/Android/FFI 绑定、`:5052` 桥接 `brainstem-dds-bridge`、共享内存 `doso-shm-ipc`、外部 RL 运行器 `doso-rl-runner`） | `D:\inovxio\brain\DOSO-SDK` |
| `Kitjesen/doso-app` | Public | `main` | React Native / Expo Android App 与 Supabase Edge Functions / 数据库迁移 | `D:\DOSO\app-workspace\doso-app` |
| `Kitjesen/doso-voice` | Private | `master` | 机载全双工语音对话、安全门控与语义记忆运行时 | — |
| `Kitjesen/doso-bms` | Private | `main` | BMS 固件（GD32 + ESP32-C3） | — |

### 端到端数据与控制流

```text
[开发机 / 运维 CLI] (tools/deploy/doso-ota-ops.py)
   ├─ 上传不可变 .tar.gz / .apk ──────────▶ 阿里云私有 OSS (doso-ota-prod-shanghai)
   └─ HTTPS / Bearer 发布与灰度 ───────────▶ SWAS 47.103.89.137 (api.dosorobotics.com/ota)
                                                  │  签名 Manifest + 短时效 OSS 签名 URL
                                                  ▼
[机器狗端 (RDK S100P ARM64)]
   ├─ doso-ota.service (:13147) ◀─── HTTPS 轮询 / 断点续传 / Ed25519 验签 / 槽位切换 & 回滚
   │     └─ 本机 HTTP (:5054) ───▶ doso-brainstem (:5052 / :5054 / :13145 / :13146 / SHM)
   ├─ doso-cloud-link.service ───▶ ECS 8.153.169.177:8883 (MQTT mTLS) ───▶ Gateway ───▶ Supabase
   └─ doso-ops-tunnel.service ───▶ wss://api-test.dosorobotics.com:443/~!frp ───▶ SWAS 127.0.0.1:2200x
```

---

## 二、云服务器与基础设施拓扑（2026-11-26 实测现状）

### 2.1 域名与权威解析

- 域名：`dosorobotics.com`
- 权威 DNS：托管在 **阿里云 ESA**（`siteId=187245473806416`），云解析 `dnsnext` 保持同步记录。
- 当前已配置 A 记录（均指向 `47.103.89.137`，仅 DNS 模式，TTL `600s`）：
  - `api.dosorobotics.com` → `47.103.89.137`（**OTA 正式生产域名**）
  - `api-test.dosorobotics.com` → `47.103.89.137`（业务 Supabase 测试接口 + 443 WSS 运维隧道入口 `/~!frp`）

### 2.2 阿里云上海 SWAS：`47.103.89.137`

- 实例 ID：`e86b553bc0de4181ac17f0ecc0fce86a`（Ubuntu 22.04 `x86_64`，Nginx `1.18.0`）
- 承载服务清单：

| 服务 / 组件 | 监听地址 | 运行用户 / 配置与状态目录 | 用途与当前状态 |
| --- | --- | --- | --- |
| Nginx `doso-api-prod-ota` | `:80`, `:443` (`api.dosorobotics.com`) | `/etc/nginx/sites-available/doso-api-prod-ota`（Let's Encrypt 证书有效期至 `2027-02-24`） | **已切流上线**：80 除 ACME 外 `308` 跳 HTTPS；443 **仅代理 `/ota/` → `127.0.0.1:8140/`**，其余路径返回 `404` |
| Nginx `doso-api` | `:443` (`api-test.dosorobotics.com`) | `/etc/nginx/sites-available/doso-api` | 代理 Supabase 业务接口与 `location = /~!frp` → `http://127.0.0.1:7000`（WSS） |
| `doso-ota-release.service` | `127.0.0.1:8140` | 用户 `doso-release`；二进制 `/usr/local/lib/doso-ota-release/doso-ota-release`；配置 `/etc/doso/ota-release/`；状态 `/var/lib/doso-ota-release/` | **在线运行 (`active`)**：发布模板、Ed25519 签名、OSS URL 签名、灰度熔断、设备库存上报 |
| `doso-frps.service` | `127.0.0.1:7000` + `127.0.0.1:22001–22999` | 用户 `doso-tunnel`；二进制 `/opt/doso/bin/frps` (`v0.72.0`)；配置 `/etc/doso/ops-tunnel/frps.toml` | **在线运行 (`active`)**：`proxyBindAddr = 127.0.0.1`，公网零暴露 SSH |
| Supabase Docker 栈 | `127.0.0.1:18000` | 容器 `supabase-envoy`, `supabase-edge-functions`, `supabase-auth`, `supabase-rest`, `supabase-db` 等 | 账号、设备归属、许可同步（`remote-state-sync` 等） |
| `doso-flow-gateway.service` | `127.0.0.1:28130` | `/opt/doso-flow/41e5ada5fcae4804bf841b197518b539/doso-cloud-gateway` | **注意**：仍为 `2026-11-08` 旧版二进制，尚未升级至支持远控的最新源码 |
| `mediamtx` | `127.0.0.1:28554`, `127.0.0.1:28889` | — | 媒体转发进程（公网 WHEP/WebRTC 与 App 观看尚未正式验收） |

> **SSH 运维排障提示**：SWAS 机内启用了 `ufw` 防火墙与 `fail2ban`（`sshd` jail）。若本机 SSH 登录触发频繁失败导致 IP 被临时封禁，不要关安全工具，通过**阿里云轻量应用服务器控制台 → 命令助手**执行 `fail2ban-client set sshd unbanip <你的公网IP>` 即可解封。

### 2.3 阿里云上海 ECS：`8.153.169.177`

- 系统：Ubuntu 24.04 `x86_64`，SSH 22 对外受限，日常通过**阿里云 ECS 云助手**运维。
- 承载服务：
  - **Mosquitto `2.1.2`**：TCP `8883`（MQTT 5 + mTLS 客户端证书），配置与主题 ACL 位于 `/etc/doso-mqtt/`。
  - **`doso-robot-cloud-gateway.service`**：监听 `127.0.0.1:8130`，运行用户 `doso-cloud`，配置/设备公钥在 `/etc/doso/robot-cloud/gateway/`，状态文件 `/var/lib/doso-cloud/reporting.json`。
    - **现状注意**：线上二进制仍为 `2026-11-04` 构建版本；`2026-11-26` 已修正研发机 1 公钥属组权限（`root:doso-cloud 0640`）并恢复 `GET /status/latest` HTTP `200`，但**尚未升级至支持远控及最新协议的 Gateway 二进制**。
  - **Nginx**：`:443` 仅反代 `/api/v1/robots/` 至 `127.0.0.1:8130`。

### 2.4 阿里云私有 OSS（上海）

- Bucket：`doso-ota-prod-shanghai`
- Endpoint：`oss-cn-shanghai.aliyuncs.com`
- 访问控制：**Private ACL**。仅 SWAS 上的 `doso-ota-release` 持有 RAM AccessKey 为已认证设备动态签发短时效（默认 `1800s`）HTTPS 下载 URL；机端与 App 不保存任何 OSS 密钥。

---

## 三、已完成并验收的能力清单

> 以下严格区分**“源码已实现”**、**“云端已部署”**与**“真机已实测”**。

### 3.1 OTA 发布与升级体系（`robot-cloud` `ota-v0.1.0` + 2026-11-26 正式切流）

1. **四类升级载体（源码 + 单测/E2E 完备）**：
   - `policy_package`（Channel B 零重启 RL 策略包）：下载验签后推送至 `doso-brainstem` `:5054/v1/policies/push`，校验 SHA-256、Golden 数值精度与 50 帧 Shadow 试跑，不重启主控进程。
   - `brainstem_release`（Channel A 底座整包）：A/B 原子槽位切换 + 重启 allow-list unit + `:5054/health` 签名健康校验 + 失败自动回滚 `last-known-good` / `recovery_required`。
   - `component_release`（多组件独立槽位 `/opt/doso/components/<comp>/`）：支持 `ota` 自升级、`cloud-link`、`voice`、`nav` 独立升级与历史 GC（`DOSO_OTA_KEEP_HISTORY=2`），不重启 `doso-brainstem`。
   - `release_bundle`（整车多组件编排包）与 `android_apk`（`/v1/apps/android/{releases,latest}` 元数据与 OSS 签名分发）。
2. **云端灰度与熔断引擎（`cloud/release`）**：
   - 支持设备分环（`dev` / `canary` / `prod`）与标签筛选、多阶段灰度（`canary -> prod-10 -> prod-100`）、稳定哈希分桶、故障率/故障次数自动熔断（`paused_circuit_open`）、人工 `promote/pause/resume/abort` 及设备状态库存查询。
3. **机端安全门禁与断点续传（`ota`）**：
   - `HTTP Range` 断点续传（`.part` + `.journal`）、Ed25519 Manifest 验签、防重放递增 `sequence`、强制校验 `:5054/health` 的 `!motor_output_enabled && cms_state=="grounded"` 与母线电压/电量门禁（非安全窗自动进入 `waiting_safe_window` 排队而非强杀）。
4. **运维 CLI（`tools/deploy/doso-ota-ops.py`）**：
   - 纯标准库 Python CLI，覆盖 `package`、`oss-upload`、`render-template`、`provision-device`、`render-robot-env`、`verify-manifest`、`rollout`、`job`、`fleet-status`、`android-publish`。
5. **真机实测验收（Dog3 `192.168.66.13`，2026-11-24 & 2026-11-26）**：
   - `2026-11-26` 已完成正式域名 `https://api.dosorobotics.com/ota` 切流与实包测试（Job `a8e1835d-dd45-4ab3-87a1-636a2a46bb72`，`sequence=202`，`activate=false`，`724,464 B` 全量校验通过，`phase=succeeded`，`motor_output_enabled=false`，`vbus_min_v=53.27V`，`doso-brainstem` 零重启）。
   - **尚未真机实测项**：真实 `brainstem_release`（Channel A）升级+故意坏包回滚实机验收、云端 `RolloutCampaign` 自动熔断实机演练、App 端 OTA 界面联调。

### 3.2 机器狗纯 Rust 主控与 SDK（`brainstem v0.0.3` + `DOSO-SDK`）

1. **纯 Rust 机载主控（`Kitjesen/brainstem` `d9617d6`, `v0.0.3`）**：
   - 单进程替代旧 Dart Brainstem，物理核硬隔离（`Core 4–5` `SCHED_FIFO 80` + `mlockall` + 预绑零分配 ONNX Runtime）。
   - RobStride RS04/RS02 驱动已对齐 2026-10-23 协议与 48 帧持久化 `set-zero`；并在 `DOSO-SDK` `47d92d7` 归档了**未做磁编码器校正导致飞车的硬件根因文档**。
   - 在 Dog3（ARM64 RDK S100P）完成满载压测（200Hz 数据零丢帧零撕裂、5 个 ONNX 策略与接口验证通过，冷启动由 `25.8s` 优化至 `20.5s`）。
2. **统一 SDK 与多语言绑定（`Kitjesen/DOSO-SDK` `f1f1d89`）**：
   - Rust `doso-sdk`、C ABI (`ffi`)、Python、C++17 单头文件、Android JNI、`doso-shm-ipc` 无锁共享内存与 `doso-rl-runner` 外部 200Hz RL 运行器已完成整理与回归测试。

### 3.3 443 WSS 反向运维通道与状态链路修复（2026-11-26）

- 在 SWAS `47.103.89.137` 与机器人端上线基于 `frp v0.72.0` 的 443 WSS 反向隧道（`doso-frps.service` + `doso-ops-tunnel.service`），不改安全组、不暴露公网 SSH、独立于 `doso-cloud-link` 客户开关。
- **3 号机（`22003`）**：实测 SSH 登录执行（465ms）、64KB SCP 双向校验（2166ms）、`frpc` `kill -9` 自愈（6.26s）、云端 `frps` 重启无感重连（3.55s）；**研发机 1（`22001`）** 于 `21:41` 现场激活并在阿里云 SSH 登录验通。
- 修复 `tools/deploy/apply-device-registration.sh` 公钥属组 bug（`mosquitto` → `doso-cloud`），并将 `shared/src/lib.rs` 默认 MQTT 心跳由 `15s` 放宽至 `45s`（支持 `DOSO_MQTT_KEEP_ALIVE_SECS` 15–120s 配置）。详情见 [OPS_TUNNEL_ACCEPTANCE_20261126.md](OPS_TUNNEL_ACCEPTANCE_20261126.md)。

---

## 四、现役两台样机现状对照表（截至 2026-11-26）

| 维度 | 3 号机（Dog3） | 研发机 1（`THUNDER-DEV-001`） |
| --- | --- | --- |
| **设备 UUID** | `13131313-1313-4131-8131-131313131313`（测试期按 LAN `.13` 手工拼写；**后续正式客户机及重登记建议改用随机 UUID v4**） | `d82a831a-2e86-4b46-975b-3d42473583b7`（正式库存 UUID） |
| **局域网 / 热点 IP** | `192.168.66.13`（RDK S100P ARM64） | 热点 `wlan1=192.168.50.1/28`，现场 Wi-Fi `wlan0=192.168.66.78/24`（历史 `.64`） |
| **运维隧道端口（SWAS `47.103.89.137`）** | `127.0.0.1:22003`（`thunder-003-ssh`，在线） | `127.0.0.1:22001`（`thunder-dev-001-ssh`，21:41 验通；21:49 现场下线，开机联网自动重连） |
| **主控运行时（Brainstem）** | **纯 Rust `doso-brainstem v0.0.3`**（`active`，`:5052/:5054/:13145/:13146/SHM`） | **旧版 Dart `brainstem.service`**（21:41 观察为 `inactive`，尚未迁移纯 Rust） |
| **机载 OTA (`doso-ota`)** | **已安装 `v0.1.0`**，走正式域名 `https://api.dosorobotics.com/ota`，已在云端 `devices.json` 登记（`canary` / `dog3`） | **未安装 `doso-ota`**，也尚未在 SWAS `/etc/doso/ota-release/devices.json` 登记 |
| **云状态上报 (`doso-cloud-link`)** | **未部署**（未生成 MQTT 设备证书与 ECS ACL） | **已安装并绑定账号**（`active`，`GET /status/latest` HTTP `200` 已恢复） |
| **硬件与外设现状** | 四路 CAN 正常；当前**无 5G 模组、无摄像头** | 带全套样机硬件（历史上曾枚举 RM500U-CNV 5G 模组，待确认开机在线状态） |

---

## 五、后续待办与推荐推进顺序

### P0 — 近期必做（把现有两台机和 OTA 闭环跑严实）

1. **研发机 1 补装 `doso-ota` 与迁移规划**：
   - 待研发机 1 开机连网（SWAS 上出现 `127.0.0.1:22001`）后，通过 `ssh -p 22001` 参照 3 号机布局安装 `doso-ota`（`/opt/doso/ota/bin/doso-ota`、`/etc/doso/ota.env`、`/etc/doso/ota-release-signing.pub.pem`），并用 `doso-ota-ops.py provision-device` 注册到 SWAS `/etc/doso/ota-release/devices.json`。
   - **重要前置**：研发机 1 当前仍为旧 Dart Brainstem，不具备 `:5054/health` 与 `/v1/policies/push`。若需对研发机 1 跑通 OTA，需同步规划将其主控升级迁移到纯 Rust `doso-brainstem v0.0.3`（注意先做好电机零位与 CAN 映射备份核对）。
2. **补齐 3 号机 OTA 与隧道的未测验收项**：
   - **Channel A 底座升级 + 自动回滚实机验收**：通过 `https://api.dosorobotics.com/ota` 对 3 号机真实下发一次 `brainstem_release` 升级，并单独执行一次故意构造健康检查失败的坏包演练，确认自动切回 `last-known-good` 且不丢失配置/标定。
   - **灰度熔断演练**：用 `doso-ota-ops.py rollout create` 创建 `RolloutCampaign`，验证单机失败即刻触发 `paused_circuit_open` 冻结放量。
   - **隧道断电重启实测**：对 3 号机执行整机断电/重启，验证 `doso-ops-tunnel.service` 开机自启并在 1 分钟内自动恢复 `127.0.0.1:22003`。
3. **3 号机接入云端状态上报**：
   - 决定是否趁正式登记把 `13131313-...` 替换为随机 UUID v4；
   - 生成设备 TLS 私钥/CSR/证书及 Ed25519 状态签名密钥，通过修正后的 `apply-device-registration.sh` 注册到 ECS `8.153.169.177` MQTT ACL 与 Gateway `devices.json`，并在 3 号机部署 `doso-cloud-link` 与 `doso-robot-authorizer`。
4. **5G 蜂窝网络真实长稳验收**：
   - 在配备 RM500U-CNV 模组的样机上打通主机 `usb0` 蜂窝拨号（不能仅靠 Wi-Fi 出口），实测 5G CGNAT 弱网切网下的 MQTT 保活（45s keep-alive）、OTA 断点续传与 443 WSS 隧道自愈。

### P1 — 云端远控与 App 正式上线

1. **升级线上旧版 Gateway 并打通远控 Claim/Hold/Release**：
   - 交叉编译最新 `doso-cloud-gateway`（替换 `8.153.169.177` 的 11-04 版与 `47.103.89.137` 的 11-08 版）；
   - 配置 Nginx HTTP/2 `/doso.remote.v1.RemoteControl/` 代理（见 [REMOTE_CONTROL_DELIVERY.md](REMOTE_CONTROL_DELIVERY.md)）并部署 `remote-control` Edge Function；
   - 在真机验收 `Claim / Hold / Release` 持权、断链自动释放与本地遥控器最高抢占。
2. **扩展远程运动控制协议**：
   - 当前 `shared/proto/control.proto` 仅支持 `Claim / Hold / Release`，下一步补齐离散动作（起立、趴下、阻尼、清故障、切换平地/爬楼策略）与 20–50Hz 速度遥操作（保留机端输入窗口、超时停机与本地遥控器硬件优先权）。
3. **App 端 OTA 与公网视频接入**：
   - 将 `doso-app` 接入 `/ota/v1/apps/android/latest` 与设备端 OTA 状态/进度查询；
   - 在真机配备摄像头后，打通 `go2rtc` → 云端 `doso-cloud-media` / MediaMTX 的公网 WHEP 观看与鉴权。

### P2 — 量产固化与平台完善

1. **新机器狗出厂固化 SOP**：每台强制生成随机 UUID v4、独立 TLS 与签名密钥、预置 `/etc/doso/ota-release-signing.pub.pem`、递增分配 `doso-ops-tunnel` 端口（`22004–22999`）并登记台账。
2. **OTA 进阶能力（按需）**：实现 `zstd_patch` Delta 差分升级（复用 `/ota/cache/` 与 `HTTP Range`）；仅当需要远程升级 Linux 内核/驱动时再规划物理 rootfs A/B + RAUC。
3. **遥测与管理后台独立云端部署**：将 `cloud/telemetry`（ThingsBoard）与 `cloud/console` 从开发机隔离 Docker + SSH 通道迁至独立云环境。
4. **仓库与凭据收口**：建议评估将 `Kitjesen/robot-cloud` 设为 Private 仓库；交接完成后按需轮换测试期 Token。

---

## 六、日常操作速查

### 6.1 通过阿里云隧道连接在线机器狗

**方式 A — 直接在阿里云 SWAS `47.103.89.137` 终端操作**：

```bash
# 查看当前在线隧道映射端口
ss -tlnp | grep -E ':22[0-9]{3}\b'

# 连入 3 号机 / 研发机 1
ssh -p 22003 sunrise@127.0.0.1
ssh -p 22001 sunrise@127.0.0.1
```

**方式 B — 本机 Windows/Linux `~/.ssh/config` 一步跳板直连**（需在 SWAS 防火墙放行当前办公 IP 的 `22` 端口）：

```sshconfig
Host doso-swas
  HostName 47.103.89.137
  User admin
  IdentityFile ~/.ssh/inovxio_deploy

Host dog1
  HostName 127.0.0.1
  Port 22001
  User sunrise
  ProxyJump doso-swas

Host dog3
  HostName 127.0.0.1
  Port 22003
  User sunrise
  ProxyJump doso-swas
```

### 6.2 标准 OTA 打包、上传 OSS、发版与灰度命令

所有运维脚本默认走正式域名 `https://api.dosorobotics.com/ota`，详细步骤见 [OTA_PRODUCTION_RUNBOOK.md](OTA_PRODUCTION_RUNBOOK.md)：

```bash
# 0. 本地注入凭据（切勿写入仓库）
export DOSO_OSS_ACCESS_KEY_ID="..."
export DOSO_OSS_ACCESS_KEY_SECRET="..."
export DOSO_RELEASE_API_TOKEN="..."

# 1. 打包不可变 .tar.gz（brainstem_release / component_release / policy_package）
python tools/deploy/doso-ota-ops.py package \
  --type policy_package --source-dir ./staging/flat_w6 --output ./dist/flat_w6.tar.gz

# 2. 上传到私有上海 OSS
python tools/deploy/doso-ota-ops.py oss-upload \
  --file ./dist/flat_w6.tar.gz --object-key policies/qa/flat_w6/flat_w6.tar.gz

# 3. 渲染发布模板（随后传至 SWAS /var/lib/doso-ota-release/releases/<releaseId>/manifest.template.json）
python tools/deploy/doso-ota-ops.py render-template \
  --release-id rel-flat-w6 --version 1.0.5 --sequence 203 --channel qa \
  --type policy_package --policy-slot flat --no-activate \
  --object-key policies/qa/flat_w6/flat_w6.tar.gz --artifact ./dist/flat_w6.tar.gz \
  --output ./dist/rel-flat-w6.manifest.template.json

# 4. 单机验收或创建灰度 Rollout
python tools/deploy/doso-ota-ops.py job start \
  --device-id 13131313-1313-4131-8131-131313131313 \
  --release-id rel-flat-w6 --idempotency-key accept-dog3-203
python tools/deploy/doso-ota-ops.py fleet-status
```

### 6.3 机载本地调试与测试命令

```bash
# robot-cloud 本地测试与静态检查（D:\DOSO\robot-cloud）
cargo fmt --all -- --check
cargo test -p robot-cloud-common -p doso-ota -p doso-ota-release
cargo clippy -p robot-cloud-common -p doso-ota -p doso-ota-release --all-targets -- -D warnings
python -m unittest discover -s tools/deploy -p "test_*.py" -v

# brainstem 交叉编译 ARM64（需 zig + cargo-zigbuild）
cargo-zigbuild build --release --locked --target aarch64-unknown-linux-gnu.2.35 -p doso-ota
```

局域网直推 RL 策略到 `:5054`（来自 `Kitjesen/brainstem`）：

```bash
python tools/export_doso_policy.py deploy ./new_model.onnx \
  --preset flat_53d --joint-order thunder_leg_major \
  --admin-url http://192.168.66.13:5054 --slot flat --shadow-frames 50 --activate
```

---

## 七、线下私密交接清单（严禁入 Git）

请通过加密渠道单独向接手同事移交以下线下资产与权限：

1. **阿里云账号 / RAM 子账号与控制台权限**：覆盖 SWAS `47.103.89.137`、ECS `8.153.169.177`、ESA/DNS 解析（`dosorobotics.com`）与私有 OSS（`doso-ota-prod-shanghai`）。
2. **SSH 登录私钥**：开发机 `~/.ssh/inovxio_deploy`（及对应公钥授权范围）与 SWAS/ECS 登录方式。
3. **云端 OTA 服务凭据**：SWAS `/etc/doso/ota-release/release.env` 内的 `DOSO_RELEASE_API_TOKEN` 与 OSS RAM `AccessKeyId`/`AccessKeySecret`。
4. **Ed25519 固件签名私钥**：SWAS `/etc/doso/ota-release/release-signing.key.pem` 保管责任与备份（不可分发给机器人或 App）。
5. **443 WSS 运维隧道令牌与现场离线包**：`doso-ops-tunnel.token` 与完整离线激活包（本机归档 `D:\DOSO\artifacts\ops-tunnel\thunder-dev-001-field-kit.zip` 等）。
6. **MQTT / Gateway 根 CA 与设备身份归档**：本机受限目录 `D:\DOSO\private\robot-cloud\cloud-stage\` 内的 CA/签名/服务凭据，以及 SWAS Supabase 服务密钥。
7. **机器狗本机登录凭据 & 私有 GitHub 仓库权限**：样机本地账号密码，以及 `Kitjesen/brainstem`、`Kitjesen/doso-voice`、`Kitjesen/doso-bms`、`Kitjesen/brainstem_api` 等私有仓库协作者权限。
