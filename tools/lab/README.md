# 本地状态 Demo

文档核对：2026-11-08。本目录运行在开发机；源码位于 `tools/lab/`，Cargo包名仍为 `lab`。命令均从workspace根目录执行，外部工具要求与最近验证结果见[TESTING.md](../../TESTING.md)。

2026-11-07已用本目录 `uplink` 入口、正常账号、真实业务数据库和实际Web App验证模拟设备的绑定、开启、后台持续更新、断网恢复与关闭停采，详情见[模拟设备链路](../../docs/SIMULATED_FLOW.md)。该记录不包含真机、手机扫码、5G或完整断云后本地重新授权。

## 模拟设备接入真实业务

先通过正常库存和绑定流程登记一台明确标记为模拟的设备，再使用它的永久 UUID：

```powershell
cargo run --locked -p lab -- uplink <已登记的模拟设备UUID>
```

此入口只启动本机模拟 Brainstem、真实 Rust cloud link 和独立 Mosquitto，不启动本地 Gateway，也不自行授予采集许可。终端输出受限临时目录中的 `gateway.json` 和 MQTT 端口；云端运维将该 Gateway 配置及其需要的证书复制到隔离目录，用 SSH 反向隧道连接本轮 MQTT，再配置真实业务同步接口。设备证书私钥和 CA 私钥留在本机，不能上传云端。

状态每六秒切换，`reads` 表示真实 Rust cloud link 调用模拟只读入口的次数。正常账号在 App 开启远程状态后，后端签发和续期许可，云端 Gateway 才允许设备采集；关闭后应停止采集与新的状态报告入库，连接最近可见时间等管理投影仍可更新。此入口保持运行，退出 App 不会结束模拟设备。按 `Ctrl+C` 停止本轮子进程并删除临时配置。

`uplink` 同时每两秒原子写入本轮 `gnss-status.json`，沿上海附近半径 32 米、64 个定位点的 WGS84 圆环行进，每次约 3.14 米，一圈约 201 米。模拟定位沿用已有 GNSS 快照格式、Rust 授权读取、签名上报和云端保存流程；`accuracyM=null` 表示未提供测量精度。定位有自己的原始时间，不复用控制状态采集时间；实际 App 根据库存模拟标记展示为模拟设备。

这里验证的是“模拟设备 → 本机 MQTT → SSH 隧道 → 云端 Gateway → 真实业务数据库 → App”。它不能作为真实机器狗、5G 网络或独立公网 MQTT 接入的验收证据。数据库与 App 必须通过库存模拟标记区分模拟画面和状态。

## 本机独立演示

在 `D:\DOSO\robot-cloud` 根目录运行。模拟机器狗每六秒在落地和站立之间切换，真实 Rust cloud link 经本地 MQTT/mTLS 持续上报，Gateway 自动续期并接收状态。业务 API 和 Brainstem 使用内存模拟。

Demo 沿用模拟业务的八秒许可和四秒续期阈值。`fresh` 按实际确认与采集时间返回，等待确认时可能短暂为 `false`。

```powershell
$env:MOSQUITTO_BIN = 'D:\DOSO\toolchains\mosquitto-2.1.2\mosquitto.exe'
$env:MOSQUITTO_ACL_PLUGIN = 'D:\DOSO\toolchains\mosquitto-2.1.2\mosquitto_acl_file.dll'
$env:OPENSSL_BIN = 'C:\Users\99563\miniconda3\Library\bin\openssl.exe'
cargo build --locked -p doso-cloud-link -p doso-cloud-gateway -p lab
cargo run --locked -p lab -- demo
```

终端每两秒从 Gateway 读取并打印 `simulated=true`、状态、采集时间和序号。复制输出的 `client.json` 完整路径，在另一个 PowerShell 中读取同一份最新状态。

```powershell
$client = Get-Content '本轮输出的client.json完整路径' -Raw | ConvertFrom-Json
$snapshot = Invoke-RestMethod -Uri $client.latestUrl -Headers @{ Authorization = "Bearer $($client.apiKey)" }
[pscustomobject]@{
    simulated = $client.simulated
    fresh = $snapshot.fresh
    seq = $snapshot.latest.report.seq
    observedAt = $snapshot.latest.report.observedAt
    state = $snapshot.latest.report.status.state.kind
}
```

所有服务只监听本机回环。`client.json` 含本轮开发凭据，保持在受限临时目录中。按 `Ctrl+C` 结束后，Demo 关闭子进程并删除本轮临时配置。

`lab public <link.json>` 沿用公网 MQTT 配置入口。配置文件中使用字符串值的 `DOSO_CLOUD_ON=true`，文件名可自行选择。已有的 `DOSO_REMOTE_ENABLED` 在新开关缺失时仍可使用。两个开关同时存在时以 `DOSO_CLOUD_ON` 为准，其值必须为 `"true"` 才会启动。

新版还需要 `DOSO_CLOUD_CHOICE` 指向本轮私有目录中的模拟本地选择文件，内容为 `true` 或 `true` 后加换行；本机 `check/local/demo` 会自行准备。缺失、无效或内容关闭时不连接云端，运行中关闭会停止连接。该文件只用于模拟持久化输入；正式用户的可信本地开启、断云后的本地重新开启，以及远程关闭后持久化关闭并断云仍待实现。App关闭上报许可后出现停采，不表示MQTT连接已断开。
