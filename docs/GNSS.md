# GNSS 位置与地球地图

2026-11-08 已将模拟位置通过正式 Rust 通信程序、隔离云 Gateway、真实业务数据库接入实际 DOSO Web App，并验证地球与街道渲染、两个坐标间的标记移动。真实 GNSS 驱动快照已实现并通过 Linux 行为检查，尚未安装到机器狗。

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
- `robot/src/gnss.rs` 在已授权的状态读取中读取一次快照。缺文件、无定位、格式错误或超过5秒时省略位置，运行状态仍正常上传。`DOSO_GNSS_STATUS` 可指定实际快照路径。
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
