# 云端网关

2026-11-17 新增独立的用户授权远控入口，配置 `DOSO_CONTROL_PORT` 后提供 DeviceLink/Operate 双向 gRPC 中继与 HTTP 会话签发、撤销。当前支持 Claim、Hold、Release，源码位于 `src/control/`；配置与部署范围见 [远控交付](../../docs/REMOTE_CONTROL_DELIVERY.md)。下文记录既有状态链行为。

文档核对：2026-11-08；依据当前源码与已有部署、验收记录，本次未重新检查线上服务。

正式程序 `doso-cloud-gateway` 运行在云服务器，源码位于 `cloud/gateway/`。既有ECS安装位置为 `/opt/doso/bin/`，服务监听回环 `127.0.0.1:8130`。按需查询 `POST /api/v1/robots/{deviceId}/status` 的body为空，Gateway通过MQTT查询设备并核验签名响应；最新状态、上报许可和视频源控制也由同一服务处理，完整HTTP合同见 [CONTRACT.md](../../CONTRACT.md)。

## 配置与启动

配置参考 [env.example](env.example)，设备登记参考 [devices.example.json](devices.example.json)。配置、设备公钥与网关私钥放在 `/etc/doso/robot-cloud/gateway/`，限制为运行用户可读。服务凭据只交给可信后端，App 通过现有账号与设备后端接入。

在工程根目录构建目标 Linux 二进制，步骤见 [BUILD.md](../../BUILD.md)，由 `doso-robot-cloud-gateway.service` 常驻启动。ECS Nginx 提供 HTTPS 入口，Gateway 的 8130 端口保留为回环访问。首次部署资料见 [tools/deploy/README.md](../../tools/deploy/README.md)。

## 查询生命周期

Gateway 使用 rumqttc 的 MQTT 5 客户端与 mTLS，订阅回复主题并收到成功 SUBACK 后才接受查询。一次查询最多等待八秒，同时最多十六个请求。

发起查询和收到回复前，均重读设备登记、公钥与 `enabled`。断线时结束等待并清理会话，HTTP 请求取消时清理待完成项，重连后重新订阅。登记文件修改在下一次检查时生效。

`enabled` 为部署接入开关，客户远程状态选择由业务后端保存。用户选择与设备执行确认分开，旧命令 worker 保持关闭。

## 后台自动同步

配置 `DOSO_REMOTE_SYNC_URL` 与 `DOSO_REMOTE_SYNC_KEY` 后，Gateway 每两秒向业务 `remote-state-sync` 提交连接与最新报告，最多 32 台一批，串行执行；业务端决定许可和会话，Gateway 复用现有许可检查并发送给设备。已确认的完整同值许可不重复发送。

业务源失败时不自行延长授权；单台设备许可到期不阻断同批其他设备的续期或关闭。同步任务随 Gateway 退出取消，不依赖手机 App 保持运行。服务凭据只配置在服务器，用户不填写这些参数。配置与字段见 [CONTRACT.md](../../CONTRACT.md)。

2026-11-04已有新版Gateway、后台自动同步配置和报告主题ACL的ECS部署记录。2026-11-07另用已登记模拟设备跑通实际Web App、业务后端、PostgreSQL、隔离云Gateway及正式cloud-link，验证开启、退页面续期、断网恢复和关闭停采，见[模拟设备链路记录](../../docs/SIMULATED_FLOW.md)。该链路通过SSH隧道连接本机MQTT，Brainstem和设备状态为模拟；真实机器狗、5G和独立公网MQTT仍待验收。旧发布包与当前源码的能力应按各自构建记录核对。

用户本地可信开启、断云后的本地重新开启、远程关闭后持久化关闭并断云已在本地 Rust cloud-link + Gateway 模拟 mTLS 链路实现并回归；真机安装、真实设备 ACL、业务许可和 App 端到端仍需按 [PROGRESS.md](../../PROGRESS.md) 验收。普通许可到期仍不会改写本地用户选择。

## 开发入口

- [src/lib.rs](src/lib.rs) 管理 MQTT 会话、查询、登记和签名回复。
- [src/http.rs](src/http.rs) 处理服务鉴权、请求格式和错误响应。
- [src/sync.rs](src/sync.rs) 自动与业务后端同步许可、设备确认和最新状态。
- [examples/query.rs](examples/query.rs) 从独立私有配置读取地址、受信 CA 和服务 Bearer，执行 HTTPS 查询。

本地联调使用workspace的Rust `lab`，具体命令见 [TESTING.md](../../TESTING.md)。公网查询历史使用模拟Brainstem；实际Web App已参与模拟设备演练，真实机器狗与正式手机/G20仍待验收。
