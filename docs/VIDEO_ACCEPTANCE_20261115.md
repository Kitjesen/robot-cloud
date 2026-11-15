# 视频链路验收状态（2026-11-15）

这份记录把四个层次分开：Rust SDK JPEG、App 接收器、局域网 WebRTC 和公网 WHEP。端口可达、服务状态或合同测试都不能单独证明画面已交付。

## 当前结果

| 层次 | 结果 | 证据 | 结论 |
| --- | --- | --- | --- |
| Rust SDK 单帧 JPEG | `PASS_PARTIAL` | `sdk-camera-acceptance-20261115-go2rtc.json`、`camera-warm-readonly-20261115T013709Z.json` | 真实 relay 经临时只读桥返回有效 JPEG；预热后当前窗口 5/5 成功；正式 `5052` 尚未监听 |
| Rust SDK 连续 JPEG | `PASS_PARTIAL` | `sdk-camera-acceptance-20261115-go2rtc.json` | 收到 10/10 帧，序号 1–10、唯一帧 10/10；冷启动仍有 503/等待窗口 |
| App 视频显示 | `NOT_RUN_REAL_DEVICE` | App 视频合同回归 59/59；无本轮正式 App 真机回放证据 | 代码具备 WHEP 接收、解码帧 watchdog、停止/后台清理，但不能把合同测试当作手机画面通过 |
| 局域网 WebRTC/WHEP | `PASS`（查看器） | `D:/DOSO/artifacts/robot-cloud/real-connect-20261113/real-webrtc-acceptance-20261115.json` | 真实 LAN 查看器收到 640×480 帧且计数增长；不是公网或正式 App 验收 |
| 公网 WebRTC/WHEP | `NOT_DEPLOYED` | `D:/DOSO/artifacts/robot-cloud/real-connect-20261113/public-whep-probe-20261115.json` | `media-test.dosorobotics.com` TLS 可达，但 status/WHEP 路由为 404，未发送 viewer 凭据或 SDP |

重复只读请求仍能观察到 relay 冷启动/就绪抖动：历史窗口 5 次请求中 2 次返回 JPEG、3 次 5 秒零字节超时。[历史窗口证据](D:/DOSO/artifacts/robot-cloud/real-connect-20261113/relay-retry-window-20261115.json)

随后用真实 LAN WHEP 查看器预热后，连续 5 次 JPEG 请求全部 HTTP 200，约 26 KB/帧；WHEP 解码计数从 55 增长到 325，丢包为 0。[预热后窗口](D:/DOSO/artifacts/robot-cloud/real-connect-20261113/webrtc-warmup-jpeg-window-20261115.json) 这只能证明“有消费者后稳定”，尚未形成正式 Gateway 的 readiness 语义。

当前只读复核再次得到相同的预热后行为：相机状态 `200/streaming`、约 `29.6 FPS`，连续 5 次快照全部为有效且不同的 JPEG（25,277–25,600 bytes）。[当前窗口](D:/DOSO/artifacts/robot-cloud/real-connect-20261113/camera-warm-readonly-20261115T013709Z.json) 这不改变正式 5052、App 真机与公网 WHEP 的未验收状态。

## 本轮现场只读观察

当前窗口可通过 SSH 只读检查到：HTTP `5050`、RTSP `8554` 和 ICE `8555` 正在监听，go2rtc 管理 API 仅绑定机器人回环地址；`13146` 与正式 SDK Gateway `5052` 未监听。相机状态接口报告 `streaming`，LAN WHEP 预热后 `api/frame.jpeg?src=cam` 可返回 JPEG。这个结果只说明独立媒体源存在，不能替代正式 SDK Gateway 部署。

## 下一步顺序

1. 在机器人上部署与当前 LingTu/Brainstem 版本匹配的正式 `lt-host` SDK Gateway，确认 `5052` 的 TLS/API-key/只读方法边界；不启动运动控制路径。
2. 为 JPEG relay 增加可观测的 readiness/warm-up 结果，或在 Gateway 中把冷启动超时映射为明确的 `UNAVAILABLE`，再重复单帧和连续流验收。
3. 用当前真实 LAN WHEP 源安装隔离的 App Video Test，记录解码帧计数、停止、后台和恢复清理；通过后再接正式 App 控制页。
4. 部署公网媒体路由、HTTPS/WHEP 和 ICE，使用短期 viewer grant 完成 `status → WHEP → DELETE`，再把公网结果写入本矩阵。

本轮没有绑定账号、没有发送运动/电机/导航/External 命令，也没有重启机器狗服务。正式 Gateway、App 真机显示和公网媒体仍是交付门，不以模拟或临时 bridge 替代。
