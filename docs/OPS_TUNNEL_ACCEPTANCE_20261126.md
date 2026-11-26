# DOSO 机器狗 443 WSS 反向运维通道部署、验收与交付手册（2026-11-26）

本文档记录 2026-11-26 完成的两项线上交付与真机验收：
1. **研发机 1（`THUNDER-DEV-001`）云端状态读回链路修复**及同类根因源码修复；
2. **全量客户机统一 443 WSS 反向运维通道（`doso-ops-tunnel`）**云端部署、**3 号机真机端到端实测验收**、研发机 1 现场离线激活包及后续客户机出厂固化 SOP。

---

## 一、研发机 1 云端状态恢复与根因修复记录

### 1.1 现网问题与根因
- **故障现象**：ECS `8.153.169.177` 上 `doso-robot-cloud-gateway` 每 2 秒打印 `robot_cloud_sync_permission_failed`，`GET /api/v1/robots/d82a831a-2e86-4b46-975b-3d42473583b7/status/latest` 恒返回 `HTTP 503 unavailable`。
- **直接根因**：`/etc/doso/robot-cloud/gateway/device-d82a831a-2e86-4b46-975b-3d42473583b7-signing.pub` 属组被登记脚本误设为 `root:mosquitto`（`0640`），运行用户 `doso-cloud` 无权读取。
- **并发风险规避**：排查确认研发机 1 在业务库处于未绑定状态（`owner_id = NULL`，`enabled = false`），同步接口每 2 秒返回 `enabled: false`。若单改文件属组会触发网关向机端下发 `enabled: false`，使机端持久化写死本地云开关而彻底断云。

### 1.2 实际执行顺序与结果（零重启、未改安全组）
1. **SWAS `47.103.89.137`（Supabase 事务内原子执行）**：将 `d82a831a-2e86-4b46-975b-3d42473583b7`（`THUNDER-DEV-001`）绑定至账号 `e934cee0-2b14-45f9-bcf7-2679cb9ef372`，并开启状态上报（`authorization_epoch = 2`、`revision = 2`、`enabled = true`、`stop_pending = false`）；等待 5 秒跨同步周期确认保持 `enabled = true`。
2. **ECS `8.153.169.177`**：执行 `chgrp doso-cloud /etc/doso/robot-cloud/gateway/device-d82a831a-2e86-4b46-975b-3d42473583b7-signing.pub`，验证 `doso-cloud` 可读。
3. **验证结果**：`robot_cloud_sync_permission_failed` 报错完全停止；`GET /api/v1/robots/d82a831a-2e86-4b46-975b-3d42473583b7/status/latest` 恢复为 **`HTTP 200`**。

### 1.3 源码根因修复
- [apply-device-registration.sh](../tools/deploy/apply-device-registration.sh)：修复第 127 行公钥属组误设 `-g mosquitto`，改为 `-g doso-cloud`。
- [shared/src/lib.rs](../shared/src/lib.rs)：MQTT 心跳由硬编码 `15s` 调整为默认 **`45s`**，支持环境变量 `DOSO_MQTT_KEEP_ALIVE_SECS`（`15..=120` 秒），`robot-cloud-common` 单测 11/11 通过。

---

## 二、443 WSS 反向运维通道架构与安全边界

面向所有 5G CGNAT（无固定公网 IP）环境下的交付客户机与研发机，采用自建 `frp v0.72.0` 走现有 HTTPS 443 WebSocket 穿透。

```text
机器狗 (5G CGNAT / Wi-Fi / 任意内网)
  └─ doso-ops-tunnel.service (frpc v0.72.0, ARM64, loginFailExit=false)
        │  wss://api-test.dosorobotics.com:443/~!frp
        ▼
阿里云 SWAS 47.103.89.137 (api-test.dosorobotics.com)
  ├─ Nginx (:443 TLS 1.2/1.3) ──▶ location = /~!frp ──▶ http://127.0.0.1:7000
  └─ doso-frps.service (frps v0.72.0, bindAddr=127.0.0.1:7000, proxyBindAddr=127.0.0.1)
        ├─ 127.0.0.1:22001 ──▶ THUNDER 研发机 001 (d82a831a-...) :22
        ├─ 127.0.0.1:22003 ──▶ 3 号机 (13131313-...) :22
        └─ 127.0.0.1:22004..22999 ──▶ 后续客户机 :22
```

### 安全与稳定性设计
- **零安全组变更**：复用既有 `api-test.dosorobotics.com:443` HTTPS 入口，不开放新公网端口。
- **公网零暴露 SSH**：`frps` 控制口（`127.0.0.1:7000`）与全部映射端口（`proxyBindAddr = "127.0.0.1"`，`allowPorts = 22001..22999`）仅监听云端回环地址。
- **独立于客户状态开关**：`doso-ops-tunnel.service` 为独立低权限 `doso-tunnel` 系统服务，不受 `doso-cloud-link` 或客户状态开关影响。
- **二进制与配置完整性**：
  - `frp_0.72.0_linux_amd64.tar.gz` SHA-256：`daf719b32ca2778770bb92955e231a07414d754378b9af328aa2642862479804`
  - `frp_0.72.0_linux_arm64.tar.gz` SHA-256：`02180b857480302fd934e9ddbd50fe2f73229f99062f88756abc6de9df5e95cf`
  - 服务端配置权限：`/etc/doso/ops-tunnel/frps.toml` (`0640 root:doso-tunnel`)；机端配置权限：`/etc/doso/ops-tunnel/frpc.toml` (`0640 root:doso-tunnel`)。
  - 认证令牌不入 Git（由 [tools/ops-tunnel/.gitignore](../tools/ops-tunnel/.gitignore) 屏蔽 `*.token`）。
- **抗弱网保活参数**：`loginFailExit = false`、`heartbeatInterval = 20`、`heartbeatTimeout = 90`、`dialServerKeepalive = 30`、`Restart=always`、`RestartSec=5`、`StartLimitIntervalSec=0`。

---

## 三、3 号机真机端到端验收记录（2026-11-26）

### 3.1 被测对象与环境
- **云端节点**：阿里云 SWAS `e86b553bc0de4181ac17f0ecc0fce86a`（`47.103.89.137`，`x86_64`，Ubuntu 22.04，Nginx `1.18.0`，`frps 0.72.0`）。
- **机器人节点**：3 号机（`192.168.66.13`，`aarch64`，内核 `6.1.158-rt58-DR-4.0.5-2603031328-g9f678e-g6caa4d`，`frpc 0.72.0`，proxy `thunder-003-ssh` → `127.0.0.1:22003`）。
- **硬件/网络说明**：验收时 3 号机通过 `wlan0`（`192.168.66.13/24`）访问公网；经 `lsusb` 核查该机当前未插 5G 蜂窝模组。全程未启动电机，未修改 `brainstem`、`doso-ota` 或 CAN 配置。

### 3.2 验收用例与实测结果

| 编号 | 验收项目 | 执行位置与方式 | 实测结果 | 耗时/关键证据 |
| --- | --- | --- | --- | --- |
| T0 | 云端 `frps` 与 Nginx 443 WSS 上线校验 | `47.103.89.137`（`t-sh0749rp6pilkow`） | **PASS** | `frps verify` 通过；仅监听 `127.0.0.1:7000`；`nginx -t` 通过并热重载；`/` 返回 `404`、`/~!frp` 无升级头返回 `400` |
| T1 | 3 号机 `frpc` 443 WSS 建连与回环握手 | `192.168.66.13` + `47.103.89.137`（`t-sh0749sqd4yh69s`） | **PASS** | 3 号机 240ms 内 `start proxy success`；云端 `127.0.0.1:22003` 回环监听，直连读到 `SSH-2.0-OpenSSH_8.9p1 Ubuntu-3ubuntu0.13` |
| T2 | 云端经隧道真实 SSH 登录并执行命令 | `47.103.89.137` → `127.0.0.1:22003`（`t-sh074a0soxr0y68`） | **PASS** | **465 ms**；真实读回 `USER=sunrise`、`HOST=ubuntu`、`ARCH=aarch64`、`wlan0=192.168.66.13/24`、`OTA=active`、`TUNNEL=active` |
| T3 | 云端经隧道 64 KB 文件往返传输与哈希校验 | `47.103.89.137` ↔ `127.0.0.1:22003` SCP（`t-sh074a0soxr0y68`） | **PASS** | **2166 ms**；上传、远端计算、回传三方 SHA-256 完全一致（`11faa37f...ea19c6d`），远端临时文件清理确认 `REMOTE_CLEANUP_OK` |
| T4 | 机端 `frpc` 异常崩溃（`kill -9`）自动恢复 | `192.168.66.13`（`PID 4750` → `PID 22287`） | **PASS** | **6.26 s**（含 `RestartSec=5s`，拉起后 ~0.53s 恢复 `start proxy success`）；服务确认 `enabled` |
| T5 | 云端 `doso-frps.service` 重启后机端无感自动重连 | `47.103.89.137` 重启 `frps`（`t-sh074a0soxr0y68`） | **PASS** | **3550 ms**；未触碰 3 号机，3.55s 内自动重连并通过 `POST_RESTART_SSH_OK` 登录验证；systemd 零告警 |

### 3.3 已知验证边界（后续补测项）
- **整机断电重启自启实测**：已确认 `doso-ops-tunnel.service` 为 `enabled` 且配置 `After=network-online.target` 与 `loginFailExit=false`，但本次未对 3 号机执行整机 `reboot`。
- **真实 5G 蜂窝弱网/换 IP 长稳实测**：3 号机无 5G 模组，本次在 Wi-Fi 公网出口完成全链路验收；真实 5G CGNAT 切换重连将在配备 RM500U 模组的样机或研发机 1 激活后补验。

---

## 四、端口台账与日常运维操作

### 4.1 映射端口分配（`127.0.0.1:22001–22999`）

| 设备 | Proxy 名称 | 云端回环端口 | 当前状态 |
| --- | --- | --- | --- |
| THUNDER 研发机 001 (`d82a831a-...`) | `thunder-dev-001-ssh` | `127.0.0.1:22001` | **2026-11-26 21:41 现场激活上线并 SSH 验通**（`wlan1 192.168.50.1/28`、`wlan0 192.168.66.78/24`，`t-sh074a5ase81fcw`） |
| 3 号机 (`13131313-...`) | `thunder-003-ssh` | `127.0.0.1:22003` | **在线并验收通过** |
| 后续出厂客户机 | `thunder-<sn>-ssh` | `127.0.0.1:22004–22999` | 出厂逐台递增分配并登记 |

### 4.2 从云端连入在线机器狗
在阿里云 `47.103.89.137` 上查看当前在线机器端口并直接连接：

```bash
# 查看所有在线机器的映射端口
ss -tlnp | grep -E ':22[0-9]{3}\b'

# 登录指定机器（示例：3 号机 22003 / 研发机 1 为 22001）
ssh -p 22003 sunrise@127.0.0.1

# 向指定机器下发文件
scp -P 22003 local-file sunrise@127.0.0.1:/tmp/
```

---

## 五、新客户机出厂固化 & 研发机 1 现场激活 SOP

### 5.1 新客户机出厂固化（每台出厂前执行 1 次）
仓库脚本位于 [tools/ops-tunnel/](../tools/ops-tunnel/)：
1. 为该台机器分配唯一名称（如 `thunder-004`）和唯一回环端口（如 `22004`），记录进端口台账。
2. 将离线 `frpc`（ARM64 `v0.72.0`）、[install-robot-tunnel.sh](../tools/ops-tunnel/install-robot-tunnel.sh) 和 `doso-ops-tunnel.token` 传到机器 `/tmp/`。
3. 在机器上执行并清理临时文件：
   ```bash
   sudo bash /tmp/install-robot-tunnel.sh \
     --name thunder-004 \
     --remote-port 22004 \
     --server api-test.dosorobotics.com \
     --server-port 443 \
     --token-file /tmp/doso-ops-tunnel.token \
     --frpc-bin /tmp/frpc
   rm -f /tmp/doso-ops-tunnel.token /tmp/frpc /tmp/install-robot-tunnel.sh
   ```
4. 在 `47.103.89.137` 验证 `ss -tlnp | grep 127.0.0.1:22004` 与 `ssh -p 22004 sunrise@127.0.0.1 "hostname"` 通过即可出厂。

### 5.2 研发机 1 现场一键激活（无需现场人员懂技术）
- 离线激活包位置（本机产物目录，含认证令牌，请勿上传公共仓库）：
  `D:\DOSO\artifacts\ops-tunnel\thunder-dev-001-field-kit.zip`
- 操作步骤：
  1. 将压缩包发给现场接触研发机 1 的人员并解压；
  2. 笔记本连上研发机 1 的 Wi-Fi 或网线；
  3. 双击 `field-provision-thunder-dev-001.bat`（自动扫描机器 IP，按提示回车使用默认凭据，约 30 秒完成安装并自动删除机端临时令牌）；
  4. 看到绿色 `DONE` 后，即可在 `47.103.89.137` 通过 `ssh -p 22001 sunrise@127.0.0.1` 永久接管研发机 1。

---

## 六、回滚方案
- **单机下线**：在目标机执行 `sudo systemctl disable --now doso-ops-tunnel.service`。
- **云端整套回滚**：
  ```bash
  sudo systemctl disable --now doso-frps.service
  sudo cp -a /root/doso-api.nginx.bak.<timestamp> /etc/nginx/sites-available/doso-api
  sudo nginx -t && sudo systemctl reload nginx
  ```
