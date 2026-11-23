# GNSS 位置与地球地图

2026-11-08 已将模拟位置通过正式 Rust 通信程序、隔离云 Gateway、真实业务数据库接入实际 DOSO Web App，并验证地球与街道渲染、两个坐标间的标记移动。真实 GNSS 驱动快照已实现并通过 Linux 行为检查，尚未安装到机器狗。

## 2026-11-17 SDK 与云端接入

```text
机器狗 GNSS 接收机 → 唯一串口驱动 → 原子位置快照
                                  ├→ LingTu Localization gRPC → Rust SDK
                                  └→ cloud-link → MQTT TLS → 云 Gateway → 业务库 → App
Brainstem GetControlStatus ───────────→ cloud-link
```

本地 SDK 提供 `client.localization().location().await?` 单次读取，以及 `watch_location().await?` 的 1Hz 服务端流。消息定义位于 `D:\inovxio\brain\DOSO-SDK\proto\messages\localization.proto`，调用实现在 `src/localization/location.rs`，运行示例为 `examples/watch_location.rs`。本地 gRPC 消费已有快照，串口读取由 GNSS 驱动负责。驱动与 Gateway 使用 `LINGTU_GNSS_STATUS_FILE`，cloud-link 使用 `DOSO_GNSS_STATUS`，两者默认均为 `/dev/shm/lingtu/gnss_status.json`。

| 数据 | SDK gRPC 字段 | 云状态 `location` 字段 | 含义 |
| --- | --- | --- | --- |
| 纬度、经度 | `latitude`、`longitude` | `lat`、`lon` | WGS84，单位度 |
| 定位质量 | `fix_type` | `fixType` | 原始 NMEA fix 代码 1–8 |
| 接收时间 | `observed_at` | `observedAt` | 驱动接收该 fix 时记录的 UTC 时间 |
| 水平精度 | 可选 `accuracy_m` | 可空 `accuracyM` | 实测米数，未知精度保持空 |
| 数据可用性 | `state`、可选 `age_s` | 当前报告省略无效的 `location` | SDK 区分 ready、unavailable、no_fix、stale、invalid |

本轮将 cloud-link 的有效期对齐本地 SDK，fix 年龄最多3秒、允许时钟超前最多1秒。上报周期为1秒，保留原始定位时间。失锁、快照过期或缺失时，新状态报告省略位置，云端读取当前报告时清除当前位置展示。

位置随获许可的状态报告签名上传。当前 `robot/src/grpc.rs` 先成功读取 Brainstem `127.0.0.1:13146`，再附加位置，Brainstem 读取失败时整条报告返回 `upstream_unavailable`。这一依赖仍存在，GPS 独立于 Brainstem 故障的上报尚待实现。

### 设备侧接入契约

GPS/RTK 驱动向同一快照写入以下结构。该示例沿用本地测试坐标。

```json
{
  "location": {
    "lat": 31.2304,
    "lon": 121.4737,
    "fix_type": 1,
    "ts": 1794847200.125,
    "accuracy_m": null
  }
}
```

`ts` 为该 fix 接收时间的 Unix 秒数。驱动完整写入同目录临时文件后原子替换快照，诊断更新保留 fix 的原始时间。启动、失锁、串口超时和读取错误时写入 `{"location":null}`。SDK 服务与 cloud-link 按各自读取时刻核对新鲜度，进程意外退出留下的旧快照会过期。

SDK 的 `GeographicLocation`、`GeographicFix`、`GeographicState` 已直接导出。应用读取 `state` 决定当前坐标是否可用，只有 `Ready` 带 `fix`。云端使用现有 `Status.location`，无效定位在新报告中省略该字段。具体接收机的 NMEA 或设备协议由驱动适配。

### 云端向机器人下发的内容

| 通道 | 消息 | 用途与时效 |
| --- | --- | --- |
| MQTT `status/requests` | `status.read` | 按需请求一次只读状态，最长10秒有效 |
| MQTT `reporting/control` | `reporting.control` | 开启、续期或关闭状态上报，许可最长30秒 |
| MQTT `video/control` | `video.control` | 按观看需求开启、续期或停止本地视频源，许可最长15秒 |

三类 MQTT 消息均经 TLS 和 Gateway 签名，QoS 1、retain=false，并校验设备、连接及期限。状态许可关闭后，机端保存关闭选择并断开云连接。GPS 随 `status.report` 从机器人上传。视频媒体另走媒体链路。

新增远控 gRPC 源码另有设备挑战、短期 grant、Claim/Hold/Release 输入与关闭会话消息，目前处于本地联调阶段。远程 OTA 的设备任务领取与执行回传仍待接通。各自交付状态见 [PROGRESS.md](../PROGRESS.md)。

### 真机数据源

2026-11-17 02:11 CST 只读检查中，`192.168.66.64` 可达，GNSS 进程和 `/dev/shm/lingtu/gnss_status.json` 均缺失。`ttyUSB0`、`ttyUSB1` 均由当前 Brainstem PID 2665 持有。正式 SDK `5052` 和 Brainstem 本机状态 `13146` 均未监听。

按 USB 身份找到 RM500U-CNV AT 接口 `/dev/ttyUSB4` 后，`AT+QGPS=?`、`AT+QGPS?`、`AT+QGPSLOC?` 均返回 `+CME ERROR: 4`。用户已确认使用独立 GPS/RTK 接收机，本轮按用户要求先完成接口约定，具体型号与接口待确认。本次没有取得 GNSS fix。已有 WTRTK-980 后端的真机安装应使用已确认的端口。驱动 `auto` 会枚举并配置多个串口，现场部署需要明确串口归属。

## 体验入口

本机 `http://localhost:8081` → 正常登录 → 我的 → 远程状态 → 查看位置。
“地球视角”回到地球总览，“定位设备”缩放至设备所在地区。地图可拖动、缩放、旋转。底图加载失败时，面板中的坐标和采集时间仍可查看。

本轮设备为 `DOSO-SIM-20261107-001`，UUID `78ec077d-0cde-441b-bddc-022cfc850676`。两个模拟点为 `(31.2304, 121.4737)` 与 `(31.2309, 121.4742)`，约相距73米，位于上海人民广场附近；它们不是机器狗实际所在位置。模拟身份由库存定义，不能由定位消息改写。

## 数据怎么进入 App

```text
既有 GNSS 串口进程 → 原子位置快照 ─┐
                                ├→ Rust cloud-link → MQTT → Gateway → 业务库 → App
Brainstem 只读运行状态 ───────────┘
```

- 本地 GNSS 进程继续唯一读取串口与发布 DDS。现有 `/dev/shm/lingtu/gnss_status.json` 增加可选位置，完整写入同目录临时文件再替换，避免读取半份 JSON。
- `robot/src/gnss.rs` 在已授权的状态读取中读取一次快照。缺文件、无定位、格式错误或超过3秒时省略位置，运行状态仍正常上传。`DOSO_GNSS_STATUS` 可指定实际快照路径。
- `shared/src/status.rs` 唯一定义 `Location`。Status 的可选 `location` 包含 WGS84 `lat/lon`、NMEA `fixType`、独立 `observedAt` 和可空 `accuracyM`；旧状态消息继续兼容。
- 位置随原消息一起签名、校验、按设备归属与用户许可入库，不增加账号系统、运动接口或轨迹数据库。
- App 按位置自己的时间判断有效性。失锁、过期、无定位分别呈现，运行状态新并不代表位置新；未知精度为null，不能用名义精度冒充测量。

本地驱动另修复了RMC无效标志仍保留旧fix的情况。实现位于 `D:\inovxio\brain\lingtu\src\drivers\real\gnss`；这部分针对既有WTRTK-980接收机，不代表RM500U-CNV的GNSS已验证。

## 地图效果如何引入

当前采用 **MapLibre GL JS 6.17.0 + OpenFreeMap矢量底图**。MapLibre负责地球投影、缩放、交互和设备标记；底图服务负责道路、地区、建筑等地图数据；我们的服务器只接收和授权读取设备坐标，不生成地图瓦片。

Web使用隔离iframe，Android/iOS候选使用Expo57兼容的 `react-native-webview 13.16.1`。两者共用 `locationMapHtml.ts`，只传坐标和显示状态，不传账号token。`EXPO_PUBLIC_MAP_STYLE_URL` 可替换为兼容的地图样式服务，版权信息始终保留。

- [MapLibre地球示例](https://maplibre.org/maplibre-gl-js/docs/examples/display-a-globe-with-a-vector-map/)
- [OpenFreeMap接入说明](https://openfreemap.org/quick_start/)
- [Expo57 WebView说明](https://docs.expo.dev/versions/v57.0.0/sdk/webview/)

这提供地球至街道的矢量地图体验，不含卫星影像、街景、路线导航或摄影测量城市。需要这些效果时，应引入相应数据服务；无需改写GNSS上传协议。当前底图与JS模块来自联网服务，未承诺中国各地网络质量或正式服务SLA。

## 验证与部署

证据目录：`D:\DOSO\artifacts\robot-cloud\20261108-gnss-flow`。

- `two-locations.json`、`first-api.json`、`first-db.json`：真实云API和数据库保留两组坐标、原始fix时间和未知精度。
- `browser.json`：实际App正常登录后接收两坐标，地球和街道均无显示错误，marker位置变化；`globe.png`、`street.png`、`app-location.png`为实际页面截图。首轮出现过加载超时，后续实际App与独立浏览器均成功；未据此承诺弱网加载时间。
- Rust相关测试56项通过，1项既有ignored；Linux Gateway构建成功并已更新本轮隔离实例。
- App32项定向测试、类型检查和Web导出通过；定位质量中文文案随后另跑17项面板测试通过。
- Deno26项、隔离PostgreSQL36组检查通过。`device-location-20261108`增量发布只替换两个SQL校验/投影函数和一个Functions handler，保留旧消息与其他业务服务。
- `gnss-linux-test.log`：Linux实际编译并验证位置、失锁、超时、未知精度、200次并发原子替换及写失败保留旧文件。

本轮MQTT仍为本机Broker加SSH隧道；模拟器独立隐藏运行，启动方式见 `tools/lab/run.ps1`。真实机器狗、RM500U-CNV GNSS、完整DDS构建和原生手机地图仍未验收。
