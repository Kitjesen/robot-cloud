# 正式 SDK Gateway `5052` 部署门（2026-11-15）

## 结论

机器狗当前的 `5050` 是只读 `gateway.video_probe`，只提供相机/视频 HTTP 路由，不是 DOSO Rust SDK Gateway。正式 SDK Gateway 由 LingTu `lt-host.service` 启动：HTTP 观察端口由真实 RunPlan 配置为 `5053`，gRPC SDK 端口固定为 `5052`。

当前现场只读事实：

- `lt-video-gateway.service` active，`5050` 可读；
- `lt-host.service` 未安装，`5052` 未监听；
- `/run/lingtu/session.env` 不存在，所以 ProductControl 尚未选择并启动 Host；
- `13145/13146` Brainstem 及只读云连接保持运行；本轮没有启动 Host。

## 必须准备的发布物

1. Linux ARM64 LingTu native release，包含 `bin/`、`lib/`、`etc/`、`share/` 和 Python runtime 依赖；使用 `scripts/deploy/package_native_release.sh` 生成，不在 Windows 直接执行。
2. `/opt/lingtu/current` 双布局发布链接，以及 `lt-host.service`、对应 Product RunPlan 和 `/run/lingtu/session.env`。
3. gRPC 认证配置：`LINGTU_GRPC_API_KEY`；如启用 TLS，还必须成对配置 `LINGTU_GRPC_TLS_CERT_FILE` 与 `LINGTU_GRPC_TLS_KEY_FILE`。只配置现有 HTTP `LINGTU_API_KEY` 不能证明 gRPC 已鉴权。
4. 与发布物匹配的 native 库：`liblingtu_nav_client.so`、巡检库、DDS probe、相机/SLAM/地图依赖，以及 Python `grpcio`/`protobuf`/`uvicorn` 等锁定运行环境。

当前发布前置仍未齐：目标机现有 `camera_dds`，本轮已在目标机源码 build tree 生成 `gnss_dds`、`dds_probe` 和 `livox_sdk2_stream`；`doso` driver 构建因目标机缺少 `BrainstemClientConfig.cmake`（0.6）停止。导航、SLAM、maps 等 native 组件和标准 `install/` 树仍未形成；必须在 Linux ARM64 构建机补齐 BrainstemClient 及其余组件后再生成 release。Windows 上的 `bash`/WSL 没有可用发行版和 `rsync`，因此本地 `package_native_release.sh --self-test` 不能作为通过证据。

另外，现有凭据脚本写入的是 HTTP/MCP 使用的 `LINGTU_API_KEY`；gRPC 服务读取的是独立的 `LINGTU_GRPC_API_KEY` 及 TLS 文件。部署时必须显式生成、安装并验证 gRPC 凭据，不能把 HTTP API key 的存在当成 `5052` 已受保护。

## 安装顺序（维护窗口执行）

1. 只读核对 Brainstem 状态、CAN、当前控制 owner 和电机输出；保存快照。
2. 校验 release manifest、架构和 SHA256；将 release 放入 `/opt/lingtu/releases/<version>`，原子更新 `current`，不覆盖旧 release。
3. 安装 catalog unit：

   ```text
   bash scripts/deploy/thunder/install_services.sh field-cpp
   ```

   该命令安装 `lt-host.service`，但不会把 Host 设置为开机自动启动；ProductControl 负责选择产品并生成 session。

4. 写入并校验 gRPC API key/TLS 文件，权限最小化；不得把密钥写入 RunPlan、App、日志或验收截图。
5. 先在产品停止、输出关闭、没有控制租约的条件下启动 Host；只做 `System.Handshake`、`Observations.CameraStatus`、单帧 JPEG 和短连续流检查。
6. 通过正式 `5052` 后，才允许继续 App 真机播放和七域真机矩阵；云端仍不获得运动控制路径。

## 回滚与停止

- 任何握手、TLS、依赖、DDS 或硬件健康检查失败，停止本次 Host，恢复旧 `current` 链接和旧 RunPlan；不重启 Brainstem、不改 CAN、不发送运动命令。
- Host 的 gRPC 线程注册了 `System/Motion/Localization/Maps/Navigation/Observations/Tasks`，因此即使本次只测相机，也必须把它视为完整 SDK 服务安装，不能把 `5050` video probe 当作替代品。
- 只有 `5052` 使用 TLS/API key、握手身份正确、云端远程运控拒绝回归通过后，才可将 Gate 5 从 `PASS_PARTIAL` 提升为正式通过。

本文件是部署门，不代表已经部署。当前真实视频源和局域网 WHEP 已可读，但正式 `5052`、App 真机播放和公网 WHEP 仍未验收。
