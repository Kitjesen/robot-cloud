# DOSO 机器狗 443 WSS 反向运维通道 (`doso-ops-tunnel`)

用于所有交付给客户的机器狗及研发机，在 5G CGNAT 无固定公网 IP 环境下长期稳定反连回阿里云，支持随时 SSH 维护、传文件与升级，**不改阿里云安全组、不暴露公网 SSH、独立于客户状态上报开关**。

## 架构

```text
机器狗 (5G CGNAT)
  └─ doso-ops-tunnel.service (frpc, WSS :443, loginFailExit=false)
        │  HTTPS/WSS https://api-test.dosorobotics.com/~!frp
        ▼
阿里云 SWAS 47.103.89.137
  ├─ Nginx (:443 TLS) ──▶ location = /~!frp ──▶ http://127.0.0.1:7000
  └─ doso-frps.service (frps, bindAddr=127.0.0.1:7000, proxyBindAddr=127.0.0.1)
        ├─ 127.0.0.1:22001 ──▶ THUNDER 研发机 001 (d82a831a-...) :22
        ├─ 127.0.0.1:22003 ──▶ 3 号机 (13131313-...) :22
        └─ 127.0.0.1:220xx ──▶ 后续客户机 :22
```

## 端口分配台账

| 机器 | Proxy 名称 | 云端本机映射端口 | 状态 |
| --- | --- | --- | --- |
| THUNDER 研发机 001 | `thunder-dev-001-ssh` | `127.0.0.1:22001` | **2026-11-26 21:41 远端热点激活上线，云端真实 SSH 登录验通** |
| 3 号机 (`.13`) | `thunder-003-ssh` | `127.0.0.1:22003` | **2026-11-26 端到端实测通过**（SSH 465ms / 64KB SCP 校验通过 / 崩溃 6.26s 自愈 / 服务端重启 3.55s 自动重连） |
| 后续出厂客户机 | `thunder-<sn>-ssh` | `127.0.0.1:22004–22999` | 出厂刷机统一分配 |

完整部署证据、安全边界与出厂/现场激活 SOP 见 [OPS_TUNNEL_ACCEPTANCE_20261126.md](../../docs/OPS_TUNNEL_ACCEPTANCE_20261126.md)。

## 云端连接机器狗

在阿里云 `47.103.89.137` 上直接连接对应映射端口（无需知道狗的 5G IP）：

```bash
ssh -p 22003 sunrise@127.0.0.1   # 3 号机
ssh -p 22001 sunrise@127.0.0.1   # 研发机 1（激活后）
```

## 现场一键激活研发机 1（或新客户机）

1. 将离线包目录（包含 `frpc` ARM64 二进制、`install-robot-tunnel.sh`、`field-provision-windows.ps1`、`一键接入研发机1.bat` 以及云端颁发的 token 文件）发给现场人员。
2. 现场电脑连上机器狗局域网/热点后双击 `一键接入研发机1.bat`，脚本自动发现机器狗 IP、上传离线 `frpc` 并启用开机自启服务。
