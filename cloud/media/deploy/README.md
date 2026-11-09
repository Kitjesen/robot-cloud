# ECS 媒体部署候选

文档核对：2026-11-08。目标为MQTT ECS `8.153.169.177`，MediaMTX固定为1.21.3。本目录提供首次安装及激活候选；代码、配置和安装包存在不证明已部署，本次未访问目标服务器。业务视频授权有2026-11-06部署记录，媒体服务、公网入口和真实播放应各自提供执行结果，当前证据见[PROGRESS.md](../../../PROGRESS.md)。

部署前在 root 私有暂存目录准备 `bin/mediamtx`、`bin/doso-cloud-media`、本目录配置和单位文件，以及 `tls/media.crt`、`tls/media.key`。核对 MediaMTX 官方发布校验值和 Linux x86_64 架构，填入已有 Gateway 服务凭据，再生成覆盖所有交付输入的 `SHA256SUMS`。目录权限 700，含凭据文件权限 600。执行 `sh install.sh <暂存目录绝对路径>` 安装后服务保持停止。

媒体入口如下。

| 入口 | 作用 |
| --- | --- |
| TCP 8322 | 机器狗主动发布 RTSPS，服务证书必须匹配狗端使用的域名或 IP |
| UDP / TCP 8189 | MediaMTX 与 App 的 WebRTC ICE 媒体连接 |
| 回环 19889 | MediaMTX 原始 WHEP，由 Rust 会话服务访问 |
| 回环 19997 | MediaMTX 管理 API |
| 回环 19890 | Rust 视频会话服务，由已验证的 HTTPS 虚拟主机反向代理 |

`mediamtx.yml` 仅授予回环读取和管理权限，设备发布权限默认空。每台正式登记设备增加独立用户名与高强度发布凭据，并只授予 `publish` 到 `doso_<设备UUID>` 的精确 path。发布权限不授予 `read`、`api` 或其他设备路径。不得使用本地调试配置中的匿名发布规则。

当前 MQTT ECS 的 HTTPS 使用 IP 私有 CA，手机公网入口需要新的媒体域名与可信证书。域名候选为 `media-test.dosorobotics.com`，须核对 DNS 指向 MQTT ECS 并成功签发、验证可信证书后才能开放。业务域名位于另一台上海服务器，不能直接在 MQTT ECS 复用其虚拟主机。`nginx-location.conf` 必须置于实际核验的媒体 HTTPS server 内，再运行 `nginx -t` 后 reload。Nginx 只代理 Rust 路由，原始 WHEP 与管理 API 保持回环。浏览器跨域观看还需明确允许的 App Web origin，当前片段适合相同 origin 或原生 App。

唯一启动入口为 `systemctl start doso-cloud-media`。该单位在启动前重启本次专用的 `doso-mediamtx.service`，退出或崩溃后停止它，以关闭旧观看会话。两条固定的 `systemctl` 生命周期命令以 root 运行，主程序仍以受限用户运行。首次安装拒绝覆盖已有媒体单位与二进制，禁止把既有或共享 MediaMTX 实例接入这套重启流程。该机制须在目标机实测正常停止、`SIGKILL`、适配器自动重启及机器重启，当前尚未验证，不能据此宣布公网服务可开放。

两服务正常后验证回环端口归属、HTTPS 无凭据请求 401、未知凭据 403、管理 API 和原始 WHEP 公网不可达。使用具有精确 path 权限的临时测试发布者验证错误凭据拒绝和跨设备发布拒绝，测试身份撤销后再接真实机器狗。

完整验收使用真实摄像头和异地 G20，检查有效授权、帧数增长、ICE 实际选中路径、关闭停流、到期回收及断网恢复，并记录码率、延迟和流量。服务 active 与 HTTPS 成功不足以确认媒体已传输。

本地配置检查可设置 `MEDIAMTX_BIN` 和 `OPENSSL_BIN` 后运行 `python check.py`。2026-11-06 已用现有 MediaMTX 1.21.3 在随机回环端口验证部署配置可加载，确认 RTSPS strict、TCP ICE、默认无发布权限、禁用录像与发布者替换。测试临时证书和进程已清理。Linux 安装脚本及 systemd 生命周期仍待目标机验证。

当前 UDP/TCP ICE 可用于公网首轮验证。受限网络还需 TURN 回退和 App 的 ICE 配置下发，现有 Rust status 的 `ice_servers` 为空。当前候选还需补齐观看撤权对活跃会话的处理，并验证上述退出清理。量产开放前完成这些验证。

回退时先停止 `doso-cloud-media`，再停止 `doso-mediamtx`，移除本次引入的 Nginx include 后校验并 reload。保留服务器既有配置与凭据，按本轮安装清单逐项恢复。

## 2026-11-07 首次激活入口

本节说明2026-11-07固定暂存布局的候选入口，未记录激活成功。`activate.py` 固定用于 `/root/doso-media-20261107/media`，此路径是服务器暂存目录，不随源码迁入 `cloud/media/deploy/` 改写。需先完成 `media-test.dosorobotics.com` 的可信LE证书签发。HTTP ACME webroot为 `/var/www/doso-media-acme`，80端口虚拟主机由现场部署配置，激活脚本仅新增独立443虚拟主机，保留既有MQTT HTTPS配置。

以 root 审查后运行 `python3 activate.py`，脚本从已运行 Gateway 的进程环境读取服务凭据且不输出，将证书复制进私有暂存目录、补齐配置与校验清单、调用首次安装脚本，检查 Nginx 配置后加载媒体 HTTPS 入口并启动专用媒体服务。已有文件或单位会导致拒绝覆盖。成功检查包括匿名请求 401、无效观看凭据拒绝和两项服务 active。失败会停止本次媒体服务并撤销本次新增的 443 虚拟主机，保留已安装文件供排查，不把部分安装报告为成功。

激活脚本同时安装 `renew.py` 为 Certbot deploy hook。只有本媒体证书的 `RENEWED_LINEAGE` 才触发更新，先验证证书与私钥匹配，再以同目录临时文件替换专用媒体证书，重启专用媒体服务并校验、reload Nginx。续期会令当前视频连接短暂重连，失败返回非 0。首次安装后需现场验证该 hook 的实际执行。

该激活脚本不更新Gateway二进制。当前源码已有视频源控制接口；2026-11-07候选说明对应的旧线上Gateway尚未包含该接口，不能从当前源码推断线上已支持。媒体启用前须核对实际Gateway版本，并交付包含视频控制及所需查询许可校验的匹配版本，才能由观看请求自动启动机器狗推流。
