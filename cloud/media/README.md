# 视频信令适配

文档核对：2026-11-08；本次核对未重新部署或验证媒体服务。

`doso-cloud-media` 将现有 App 的 WHEP 接收接口转到 MediaMTX。Rust 管理授权与观看会话，视频媒体由 MediaMTX/WebRTC 传输，不进入 MQTT 或 Rust HTTP 响应。

**媒体适配与部署材料仍按候选管理。** 业务观看凭据签发与校验已有2026-11-06部署记录；这不表示媒体服务器、真实摄像头、G20或异地观看已验收。默认没有匿名模式或内置观看凭据；缺少业务授权端点时请求失败。程序固定监听回环，只有确认业务授权、上游会话回收和HTTPS接入后才能配置外部反向代理。部署候选和当前证据见[媒体部署说明](deploy/README.md)及[工程进度](../../PROGRESS.md)。

## 运行

```powershell
cd D:\DOSO\robot-cloud
cargo build --locked -p doso-cloud-media
cargo test --locked -p doso-cloud-media
```

启动已核对版本的 MediaMTX，使用本目录 `mediamtx.local.yml`。在源码外复制 `config.example.json`，替换业务授权 URL，再运行 `doso-cloud-media <配置绝对路径>`。MediaMTX 信令、RTSP、管理 API 和 ICE 都仅绑定本机；这个配置不支持异地观看。管理 API 仅供适配器检查流是否就绪，不对 App 暴露。

发布流路径必须为 `doso_<真实设备UUID>`。配置 `sourceGatewayUrl` 和私有 `sourceGatewayKey` 后，已授权的首次 status 为该设备预热最多十秒，WHEP 会话建立后每五秒通过 Gateway 续一次十五秒源租期。最后一个有效观看会话结束时请求停止，未继续观看的预热自动结束。两项均省略时沿用已有推流源，适合原本机媒体验证。

机器狗在私有 `DOSO_VIDEO_CONFIG` 文件中配置固定编码程序和参数，样例见 [video.example.json](../../robot/video.example.json)。按真实摄像头、编码能力、加密媒体地址和推流凭据调整并限制文件权限。程序必须直接持有编码进程，shell 启动脚本须使用 `exec`。云消息只有设备、连接、序号、期限和开关，不能携带命令或地址。实际设备、编码配置和公网媒体 TLS 尚待验收。

## 业务授权契约

App 现有 `viewerToken` 携带业务颁发的单设备短期观看凭据。适配器不生成用户身份、不解析或自行签发令牌；每次 status、WHEP POST、DELETE 都把 `Authorization: Bearer …` 发送到配置的授权 URL，body 为 `{"action":"video.view"}`。

业务端必须核对观看凭据、客户身份、设备归属、远程连接与观看许可，并在允许时返回 HTTP 200：

```json
{
  "viewerId": "91de0e56-2b3f-4ae1-9575-e91541d49d33",
  "deviceId": "d5619c98-0882-472a-b59c-d0f280f2253d",
  "expiresAt": 1793851200000
}
```

UUID 必须是合法设备/账号编号，`expiresAt` 是 UTC Unix 毫秒，必须晚于当前时间且最多五分钟。示例时间是格式示范，不是可用许可。拒绝返回 401/403，授权源失效返回不可用，绝不默认为允许。普通账号 access token 只有在业务端明确绑定目标设备并检查上述条件后才能作为观看凭据使用。

## App 接口

- `GET /api/v1/webrtc/status`：经授权后读取 MediaMTX 本机 API 的设备流状态，返回 `source_ready`、旋转角和 ICE 配置。管理 API 不可用时返回 503，不能把信令监听成功当作摄像头就绪；App 仍须确认实际解码帧增长。
- `POST /api/v1/webrtc/whep?src=cam`：接收 `application/sdp`，根据业务端返回的设备 ID 访问 MediaMTX；返回 201、SDP 及本服务的会话 Location。
- `POST /api/v1/webrtc/sessions/{id}/renew`：使用新的观看凭据续期同账号、同设备的现有会话，不重新连接画面；会话已过期或正在关闭时不能续期。
- `DELETE /api/v1/webrtc/sessions/{id}`：核对同一账号及设备，关闭实际 MediaMTX 会话。不存在的本地会话返回 204，上游删除失败返回 503并保留待清理项。

同一账号最多四个正在创建或已建立的观看会话。收到的 MediaMTX Location 必须仍指向配置的回环源与当前设备路径，客户端不能指定上游地址。许可到期会删除真实 WHEP 资源，失败则重试；关闭适配器时也尝试回收会话。

## 验证边界与上线缺口

2026-11-07目录重组后的workspace验证包含媒体包：整套构建、测试与格式检查通过，准确范围见[TESTING.md](../../TESTING.md)。这里的测试结果不代替目标服务器的安装、启动与观看验收。

自动测试启动真实 Rust HTTP 授权替身和 WHEP 上游替身，验证缺少/过期/拒绝授权、四路上限、账号隔离、资源 Location 改写、到期删除和恶意上游地址拒绝。替身返回的 SDP 明确标记模拟，不表示媒体解码或真实视频通过。

2026-11-05 本地另外验证了 MediaMTX v1.21.3（发布压缩包 SHA256 与官方清单一致）：回环配置加载成功，FFmpeg 模拟 H264 源可以经 RTSP 解码三帧，Rust 适配后的真实 WHEP 返回 201 和 SDP，关闭会话时 MediaMTX 返回 204。

同日本机独立 headless Edge 验收通过：模拟 H264 → MediaMTX → Rust WHEP 适配 → 浏览器真实 WebRTC 解码，`connectionState=connected`、视频尺寸 `640×360`，连续每秒采样的 `framesDecoded` 为 `8 → 18 → 28 → 38`，DELETE 返回 204。结果与画面保存在 `D:\DOSO\artifacts\robot-cloud\20261105-webrtc-smoke\result.json`、`decoded.png`。授权端和验收页面为明确的本机替身，实际媒体和 Rust 适配均非替身；所有本次启动进程已清理。未验证手机 App 的接收器、真实摄像头、四路媒体解码或异地网络。

业务观看凭据签发与验证已有部署记录，App四路入口与真实观看仍待端到端验收。设备推流许可已通过MQTT模拟启停联调；媒体服务的实际部署状态、HTTPS/CORS、公网ICE/TURN与真机验收需按[部署说明](deploy/README.md)逐项核对。候选部署单元提供专用MediaMTX随适配器停止/重启的生命周期，但目标机正常退出、崩溃和重启后的会话清理仍未验收；活跃会话到期前的业务撤销也尚未自动推送。客户端按期获取新凭据并调用会话续期；不得靠无限续期绕过授权。

媒体回环配置不录像，不接触运动、CAN 或 Brainstem。真实摄像头、5G、延迟和并发容量需另行实测。
