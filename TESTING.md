# 接入测试

更新：2026-11-09。本页汇总实际验证和可复现入口；每节按其当次源码、部署与硬件范围解释。当前功能状态及产品顺序见 [PROGRESS.md](PROGRESS.md)。

## 最新验证范围

| 验证层 | 最近记录 | 覆盖与边界 |
| --- | --- | --- |
| 目录调整后的Rust workspace | 2026-11-07 | 离线锁定构建、61项测试与格式检查通过；这是当次源码快照的结果 |
| 本机通信链路 | 2026-11-07 `lab check` | 真实Gateway、MQTT、cloud-link；Brainstem为模拟。覆盖许可、查询、上报、关闭停采和本地选择关闭退出 |
| 实际Web App与真实业务数据库 | 2026-11-07 [模拟设备流程](docs/SIMULATED_FLOW.md) | 正常账号和已登记模拟设备；绑定API、开启、退出页面持续更新、隧道断网恢复、关闭停采已验证 |
| GNSS扩展与地球地图 | 2026-11-08 [GNSS接入](docs/GNSS.md) | Rust56项、App32项、Deno26项、隔离PG36组通过；真实云库接收两坐标；实际Web App渲染地球/街道且标记移动；Linux原子快照通过；无真机或手机实测 |
| 遥测历史与ThingsBoard | 2026-11-08 | Rust9项、Deno31项；真实PG37组加实际平台4组=41组通过。实际SQL、生产handler、Rust和TB执行，源为模拟设备，PostgREST传输替换为白名单psql适配；无生产部署或真机验收 |
| GPS跟随与日期轨迹 | 2026-11-09 [TRACKS.md](docs/TRACKS.md) | 测试ECS实际SQL/HTTPS接口、ECS Rust投递、本机真实TB和实际Web App分段验证；关闭停采、后台更新与恢复均有实测，详见下节；不是独立常驻云平台或真机验收 |
| DOSO管理后台 | 2026-11-09 | 本轮 MiSans 与登录文案精简生产编译、补丁与指纹检查通过；本机18081镜像更新、HTTP/API/WebSocket 33项及191个资源通过，浏览器登录、总览、设备页和退出通过；数据源为已有模拟设备，视口663×2364，多断点和高级页面待验收 |
| 完全断云与本地重新开启 | 待实现及验证 | 上报停止不等于MQTT断开；可信本地写入、服务唤起与远程持久化关闭仍缺接线 |
| 真实机器狗和正式手机/G20 | 待验收 | 目标机安装、真实状态、扫码、独立公网MQTT与5G不在上述模拟证据内 |

本地联调由Rust工具运行。云连接服务、Gateway与Mosquitto使用真实实现，Brainstem使用只读模拟服务。每条证据按其实际业务后端、网络和客户端组成解释，模拟返回不能作为真机验收证据。

## GPS跟随与日期轨迹（2026-11-09）

源码验证通过：App地图脚本7项、轨迹统计6项、摘要组件最终10项、历史分页服务6项及原远程状态回归；Expo `tsc --noEmit`通过。历史接口Deno9项，连同相邻回归39项通过；隔离PG41组验证原样发布SQL、权限、换主/epoch与重复apply拒绝。部署边界4项通过。Rust telemetry9项及模拟圆环1项通过，x86_64 Linux遥测程序锁定离线zigbuild通过。

测试业务ECS已备份并应用遥测/历史两份迁移，新增两个HTTPS函数且原21个Functions文件保持原字节；主私有环境与其他核心服务保持。三个接口 `remote-history`、`telemetry-export`、原 `remote-state` 的无凭据请求均返回401。数据库备份为 `/opt/doso-backend-shadow/env/remote-history-database-34qyge0l`，运行时备份为 `remote-history-runtime-qzx44bt0`。发布包与本地验证见 [remote-history-20261109](<C:/Users/99563/Documents/ChatGPT/New project/doso-app/deploy/backend/remote-history-20261109/local-verification.json>)。

真实浏览器使用正常账号和模拟UUID `78ec077d-0cde-441b-bddc-022cfc850676`，验证了实际HTTPS历史读取、新轨迹增长、拖动释放跟随、全览起末点、刷新保留、离开App后台更新、关闭停采与保留摘要。关闭稳定后模拟源读取次数保持2050，历史保持276条；重新开启后的独立App检查读到373条以及更新的时间。各步骤汇总见 [acceptance.json](D:/DOSO/artifacts/robot-cloud/20261109-track-flow/acceptance.json)。后续采样仍会增长，数字是验收时快照。

完整自动化脚本在最后恢复步骤前曾切到首页，最终等待超时；该失败原件保存在 `browser-before-resume.json`，恢复步骤另一次实际App运行通过，不能把两次结果描述为单次不间断全通过。源码检查发现同账号会话重新准备可能触发回首页，但该次Auth事件未记录，具体触发尚未确认；未为此改动账号或导航代码。

本次联调还实测到开发机Docker历史平台中断、SSH临时通道断开和本机worker请求云端超时。恢复既有卷后旧历史可读；已把Rust worker放到ECS，使用同机回环业务入口减少往返，原3秒超时保留。错误日志现保留失败阶段与HTTP状态，不输出密钥或原始报告。ThingsBoard仍在本机经SSH私有通道接入，当前不是常驻生产部署。没有发送运动指令，未验证真实GNSS、原生手机或5G。

### 本次文档检查（2026-11-08）

已核对云连接工程全部16份Markdown，以及App/SDK的19份相关说明，共35份。292处本地文件链接可解析；组件说明中的3个Cargo包名、7种lab命令模式和9个部署/检查脚本入口与现有文件及参数匹配。更新了目录、当前状态、历史边界和产品推进顺序，未改生产代码、协议字段、依赖或部署配置，也未重跑功能测试。以上是文档静态检查，不能累加为新的运行测试或部署证据。

## DOSO管理后台（2026-11-09）

### 字体与登录文案调整

复用 App 的 MiSans Regular、Medium、Demibold、Bold 原始 OTF，400/500/600/700 字重统一用于主题和正文。原许可 PDF 与 NOTICE 署名随发行物保留。字体文件及许可与 App 来源的 SHA256 一致，局部 Sass 编译与字体断言通过。登录页删除重复的「管理后台」「登录DOSO管理后台」可见文案，保留表单可访问名称。

正式 Angular 生产编译通过，构建阶段245.695秒。[本轮发行清单](cloud/console/.build/20261109-100457-b0ad94c5/manifest.json) 的补丁 SHA256 为 `8db3a2e5b365270c5e2f630edf8358b7e30f647b7edebe7b5c5d8663acf4d061`，Node 与锁文件沿用下述基线。打包阶段 Docker Engine 曾退出，原因未确定。处理 socket 启动阻塞并恢复后，已用该发行目录完成镜像构建和 `up -d --force-recreate`，容器内 `nginx -t` 通过。镜像 ID 为 `sha256:c36fce30d6fa886ad2af019f5ab738948cb416001c0762bde626128f3edd932e`。

本轮 HTTP/API/WebSocket 检查为33项全部通过、191个静态资源全部可读取，运行于2026-11-09 10:32:23–10:33:05 UTC。覆盖初版27项，并增加四个 OTF、原始许可 PDF 的 HTTP/发行目录/App 原件三方 SHA256 与文件标识检查，以及 MiSans NOTICE 署名和许可路径检查。[HTTP检查报告](D:/DOSO/artifacts/robot-cloud/console-20261109/login-font-http.json)

真实浏览器使用现有账号完成登录→总览→设备页→账号菜单退出→登录页。两处重复文案已从可见文字删除，输入框、按钮与设备表头的计算字体为 MiSans。CDP 确认用户名标签实际使用 MiSans-Regular 渲染10个字形，`customFont=true`，图标计算字体仍为 Material Icons。视口为663×2364，多断点和高级页面未覆盖。[浏览器报告](D:/DOSO/artifacts/robot-cloud/console-20261109/browser-font.json)、[登录页截图](D:/DOSO/artifacts/robot-cloud/console-20261109/login-misans.jpg)

### 初版部署与接口验证

以下27项、187个资源及构建指纹属于字体与登录文案调整前的版本。

定制基于ThingsBoard CE 4.3.1.8，固定上游提交 `ebbfc6789b81a4e04cae2711208130fb2841b31f`。正式生产前端编译通过，构建阶段367.226秒。实际构建使用Node v24.11.1、Yarn 1.22.22，设置单构建worker和关闭并行TypeScript编译。源改动包括DOSO品牌、中文默认、导航、设备总览与列表样式。

可重复构建入口为 [build.ps1](cloud/console/build.ps1)，补丁归档为 [doso.patch](cloud/console/patches/doso.patch)。脚本检查源码改动与补丁完全匹配，成功编译后写入 `doso-build.json`。`-SkipCompile`在缺少编译记录时拒绝复用的行为已实际检查。生成的 [构建清单](cloud/console/.build/20261109-090556-c50e83e5/manifest.json) 记录上游提交、补丁与锁文件SHA256、Node版本和固定Nginx镜像。

本轮补丁SHA256为 `fafa8e6294d5072226234dfa59b620f99d257757df49b5d6c77febcd4ce090ea`，上游Yarn锁文件SHA256为 `45240db20dcd80e71a733f528c95295494f67d45b2ed9377a2113dc94fc84486`。

最终源码补全了3处DOSO修改声明后再次编译。本机独立前端已部署，打包使用 [发行清单](cloud/console/.build/20261109-091358-9bde3eb3/manifest.json)。`docker compose config --quiet`、`up -d --force-recreate` 和容器内 `nginx -t` 通过。容器 `doso-console-console-1` 为running，端口映射为 `127.0.0.1:18081 → 8080`，镜像ID为 `sha256:239b5bf8ea9d76122d34956c9801838800500158c262480be071e1e35a627978`。已核对进程用户为 `nginx`、根文件系统只读及 `cap_drop ALL`。

部署期间宿主重启后Docker Engine未自动恢复，经恢复Engine后完成最终镜像部署。既有ThingsBoard的18080首页返回200，未授权API返回401，PostgreSQL健康且沿用同一数据库卷。

最终镜像的 [HTTP/API/WebSocket检查报告](D:/DOSO/artifacts/robot-cloud/console-20261109/http-smoke.json) 为27项全部通过，187个静态资源全部可读取。实际检查覆盖下列内容。

- 首页、登录页、设备列表和详情的直接路由返回200，标题为DOSO管理后台，首页使用 `no-store`；缺失JS返回404，未授权API保留401及JSON响应。
- 现有账号通过真实登录API取得会话，读取租户管理员身份、设备列表、命中和空搜索、设备详情、模拟来源属性、两点历史及latest。看板、客户、告警列表API也返回200，当前数据为空。
- 新旧入口返回同一设备数据；`/api/ws`建立连接并收到一次设备属性订阅响应，读回模拟标记。
- 服务端发行清单与编译记录的提交、补丁、锁文件和Node版本一致；中英文locale资源都含DOSO修改声明。

数据来自既有模拟设备和历史点，本轮仅创建登录会话。按用户决定，本轮验收止于部署、HTTP/API及上述WebSocket属性订阅，浏览器显示、登录界面、搜索操作和退出交互暂缓。生产OAuth、公网部署、客户数据隔离和真实机器狗仍待各自验收。

## 遥测投递与实际平台（2026-11-08）

新增 `doso-cloud-telemetry` 的9项定向测试、Clippy和格式检查通过，Windows可执行程序构建通过。业务Edge入口及受影响鉴权/同步共31项Deno测试和类型检查通过。Compose配置、bootstrap/smoke脚本行为夹具通过；workspace metadata正确解析6个成员。

修正版Linux x86_64 release交叉构建成功，实际Linux容器验证ELF加载与缺少配置时安全退出；这是加载/启动边界检查，不等于目标云主机systemd验收。候选与锁文件/二进制校验见 [BUILD.md](BUILD.md) 和 [linux-smoke.json](D:/DOSO/artifacts/robot-cloud/telemetry-20261108/linux-smoke.json)。

本机运行固定摘要的ThingsBoard 4.3.1.8与独立PostgreSQL 18.1，完成初始化、私有凭据保存、默认密码更换、无邮件技术账号引导。实际 [API读回证据](D:/DOSO/artifacts/robot-cloud/telemetry-20261108/thingsboard-smoke.json) 核验两点历史、同点重试及较旧数据不倒退latest。平台只映射 `127.0.0.1:18080`，正式云端未安装本次组件。

[完整链路证据](D:/DOSO/artifacts/robot-cloud/telemetry-20261108/telemetry-chain.json) 使用实际业务SQL接受一个明确模拟设备的报告，经生产Edge handler、Rust程序写入真实TB，验证：原观察时间和完整location保留；重复sync不重复入队；读取失败只保存错误，不在该时刻生成正常状态；关闭许可后取消未发送记录。专用key缺失的401也在该链路断言中验证。PostgREST传输被白名单psql适配替换，未运行用户Auth登录、真实机器狗或手机App。

[数据库及集成总报告](D:/DOSO/artifacts/robot-cloud/telemetry-20261108/database-chain.json) 为41组通过（现有/扩展数据库37组、实际平台4组），临时PG、Deno和Rust进程均确认关闭，完整链路临时TB设备已删除。独立API smoke设备保留在本机隔离租户，标记为模拟。这些计数不能与历史基线简单相加当成不同测试数量。

首次完整链路发现TB查询范围为 `[startTs,endTs)`，原精确毫秒查询漏掉结束点；修正为 `[ts,ts+1)`，HTTP夹具按真实区间过滤，回归及实际链路均通过。首次失败证据保留于App `artifacts/multi-device/2026-11-08T05-47-26-015Z`，不能用后续成功抹去当时失败。高并发构建曾内存不足，单任务 `-j 1` 构建恢复成功。

可重复入口：先按 [telemetry README](cloud/telemetry/README.md) 准备隔离平台与私有环境；在App工程设置 `DOSO_THINGSBOARD_URL/KEY` 和 `DOSO_TELEMETRY_INTEGRATION_HOOK` 指向 [verify-telemetry-live.mjs](<C:/Users/99563/Documents/ChatGPT/New project/doso-app/scripts/verify-telemetry-live.mjs>)，再运行既有 `node scripts/verify-multi-device-ownership.mjs`。此hook仅允许本机平台，使用本机已有Deno和PostgreSQL工具链，不发布生产SQL或访问机器人。未设置hook时，原runner只做数据库验证。

运行代码检查：

```powershell
cargo test -p doso-cloud-telemetry --locked --offline -j 1
cargo clippy -p doso-cloud-telemetry --all-targets --locked --offline -- -D warnings
cargo fmt -p doso-cloud-telemetry -- --check
cargo build -p doso-cloud-telemetry --locked --offline -j 1
```

## 运行位置目录重组验证（2026-11-07）

源码迁入 `robot/`、`cloud/`、`shared/`、`tools/` 后，沿用原有测试完成以下检查：

- `cargo metadata --locked --offline --no-deps` 正确解析5个workspace成员及其新路径。
- `cargo build --workspace --locked --offline` 成功；`cargo test --workspace --locked --offline` 为61项通过，1个既有子进程夹具测试保持ignored。
- `cargo fmt --all -- --check` 通过。当次构建提示lab中的三个既有辅助方法未使用；该目录整理未调整其行为，后续源码变化需单独核对。
- `cargo run --locked --offline -p lab -- check` 通过，覆盖许可、查询、状态变化、读取失败、关闭、到期、重连、模拟视频源启停和本地关闭持久化。真实Gateway、MQTT和云连接进程使用回环网络，Brainstem为模拟源，模拟读取8次；退出后本轮13146监听已释放。
- 74个本地Markdown链接和7个主要源码入口有效。与迁移前87个源文件的快照对照，无文件遗漏；非文档内容只调整Cargo路径及lab目录定位、模板路径和格式。

未运行安装脚本、重新交叉编译Linux发布包或部署云端/机器狗。迁移前源码快照位于 `D:\DOSO\archive\robot-cloud\20261107-layout\source-before-225159.zip`；历史发布包继续按其原目录使用。

## 本机准备

在 `D:\DOSO\robot-cloud` 根目录使用 PowerShell。Rust 依赖由 `Cargo.lock` 锁定，外部工具需要 Mosquitto 2.1.x、对应 ACL 插件和 OpenSSL。

```powershell
$env:MOSQUITTO_BIN = 'D:\DOSO\toolchains\mosquitto-2.1.2\mosquitto.exe'
$env:MOSQUITTO_ACL_PLUGIN = 'D:\DOSO\toolchains\mosquitto-2.1.2\mosquitto_acl_file.dll'
$env:OPENSSL_BIN = 'C:\Users\99563\miniconda3\Library\bin\openssl.exe'
cargo build --locked -p doso-cloud-link -p doso-cloud-gateway
```

Linux 使用本机安装的工具路径设置相同环境变量，ACL 插件通常为 `mosquitto_acl_file.so`。未设置时，工具从 PATH 查找 Mosquitto / OpenSSL，从 Mosquitto 所在目录查找 ACL 插件。生产程序默认读取本工作区 `target/debug`，设置 `CARGO_TARGET_DIR` 时使用该目录的 `debug` 输出。

本地模拟服务监听回环，`13146`被占用时失败；不要为重跑模拟而停止正在运行的他人服务。每轮临时证书、日志和配置写入本工程 `.runtime/lab-*`，其中包含本轮开发凭据，目录限制为当前用户访问，结束时关闭所拥有的子进程并清理本轮目录。`public`和`uplink`可参与外部联调，按对应章节明确配置。

## 自动检查本地链路

设备证书和主题权限可单独检查：

```powershell
cargo run --locked -p lab -- auth-check
```

该检查在回环地址启动独立的真实 Mosquitto，生成临时 CA 和设备证书，检查合法证书可向自己的主题发布、无证书和不受信任证书被拒绝、合法设备向其他设备主题发布被拒绝。它使用同目录的 `mosquitto_pub` 和 MQTT 5 QoS 1 回执，不连接真实设备或公网，不能代替证书签发、吊销或真机验收。

历史环境说明：2026-11-06运行 `auth-check` 时曾因锁文件与当时清单不一致，在独立副本中完成验证。随后11月7日原工作区已完成 `--locked --offline` 构建、测试和 `lab check`；旧锁文件问题不再作为当前操作指引。本次文档核对未重跑 `auth-check`。

```powershell
cargo test --workspace --locked
cargo run --locked -p lab -- check
```

`check` 创建临时证书、签名密钥和设备登记，启动真实 Rust Gateway、云连接服务与 Mosquitto，验证以下行为后退出并关闭本轮服务。

- 用户许可缺失时返回 HTTP 403，模拟 gRPC 不发生读取。
- 用户许可经设备确认后，HTTP 200 经 `Gateway → MQTT/mTLS → 云连接服务 → 模拟 gRPC → 签名回复 → Gateway` 返回模拟状态。
- 未带服务凭据返回 HTTP 401。
- 设备登记禁用后返回 HTTP 403，模拟 gRPC 读取次数保持不变。

随后同一 `check` 流程恢复测试设备登记，验证新的持续状态路径：

- 没有采集许可时不会自动读取；有效许可被设备确认后，从模拟 GROUNDED 变化到 STANDING，报告序号推进。
- gRPC 读取失败单独报告错误，最后成功状态保留原始时间，`fresh=false` 与 `reachable=true` 可以同时成立。
- 关闭收到设备确认后停采，旧许可返回 409，已关闭设备的按需查询返回 403。
- 短期许可到期停采，过期会话不能续租；云连接服务重启产生新连接编号，旧连接许可不能重新开启。
- 本地选择文件关闭后，正在上报的云连接进程退出，读取次数停止；保留同一份关闭文件重启时输出 `cloud_off`，不连接 MQTT。

模拟状态为 `GROUNDED`、控制方 `NONE`、电机输出关闭、无关键电机故障。开发工具输出模拟标识，生产协议沿用 [CONTRACT.md](CONTRACT.md)。

## 手工请求本地环境

第一个终端保持运行。

```powershell
cargo run --locked -p lab -- local
```

复制输出的 `client.json` 绝对路径，在第二个 PowerShell 终端执行下列命令，将路径替换为本轮输出。

```powershell
$client = Get-Content '.runtime\lab-本轮UUID\client.json' -Raw | ConvertFrom-Json
Invoke-RestMethod -Method Post -Uri $client.url -Headers @{ Authorization = "Bearer $($client.apiKey)" }
```

请求 body 为空。`local` 默认没有用户许可，因此本次查询应返回 HTTP 403；要验证设备确认许可后的 HTTP 200，使用上面的 `lab check`，或运行 `lab demo` 并在模拟业务确认开启后使用它输出的 `client.json`。`simulated` 标识说明使用模拟 Brainstem，`url` 和 `apiKey` 仅供本轮本地请求。返回第一个终端按 `Ctrl+C` 结束服务。

## 后台自动同步联调

使用同样的本机工具配置和构建步骤，运行：

```powershell
cargo run --locked -p lab -- auto-check 'D:\DOSO\artifacts\robot-cloud\auto-sync-wire.json'
```

此入口运行真实 Gateway 自动同步任务、MQTT 和云连接服务；业务 API 使用 Rust 内存模拟，Brainstem 也为模拟源，不接真实数据库或账号服务。模拟源采用 8 秒许可和 4 秒续期阈值加速检查，生产合同仍为 30 秒／15 秒。

检查默认关闭零采集、用户只开启一次后的自动续期与最新状态保存、云连接服务重启后自动恢复、业务源不可用时到期停采，以及用户关闭后拒收旧状态和停采。命令不会用管理 PUT 冒充自动续期，也没有 App 保活定时器。

可选输出文件只保存真实 Gateway 发出的同步请求/响应 body 供业务侧验证格式，不保存 apikey、证书或私钥。这里的模拟保存不能替代真实数据库事务、托管鉴权或 App 端到端验收。

## 公网模拟联调

本节是独立公网模拟的操作入口；2026-11-07的实际Web App流程使用本机MQTT与SSH隧道，单独记录在 [SIMULATED_FLOW.md](docs/SIMULATED_FLOW.md)。二者不能互相替代。

历史链路为 `HTTPS → ECS Nginx → Rust Gateway → 公网 MQTT/mTLS → 本机云连接服务 → 模拟 Brainstem`。

公网运行前，需要运维重新准备短期设备证书、签名密钥、设备登记及主题 ACL。历史测试身份已清理，旧 UUID 与配置不能直接作为在线设备复用。私有准备目录位于 `D:\DOSO\private\robot-cloud\cloud-stage`，接入边界见 [tools/deploy/README.md](tools/deploy/README.md)。

`link.json` 是字符串值的 `DOSO_*` 配置对象，字段对应 [robot/env.example](robot/env.example)。其中 `DOSO_CLOUD_ON` 必须为字符串 `"true"`，证书、设备签名私钥及网关公钥使用实际路径。旧配置缺少新变量时可继续使用 `DOSO_REMOTE_ENABLED`，新变量优先。工具拒绝包含 `DOSO_GATEWAY_API_KEY` 或 `DOSO_REMOTE_SYNC_KEY` 的云连接服务配置；查询凭据独立放在查询配置中，同步凭据仅供 Gateway 使用。

还须指定 `DOSO_CLOUD_CHOICE`，指向本轮私有模拟配置目录内的选择文件，内容仅为 `true` 或 `true` 加 LF。该持久化输入不能替代正式用户身份与本地授权；真实设备的可信写入入口尚未实现。启动开关即使为 `true`，缺失或无效的本地选择仍会阻止联网。

完成准备并核对目标Gateway制品后，在两个终端分别运行，将示例路径和UUID替换为本轮私有配置。当前源码查询需要业务侧有效许可并由设备确认；旧公网成功记录不能证明目标服务器已运行同一版本。

```powershell
cargo run --locked -p lab -- public 'D:\DOSO\private\robot-cloud\cloud-stage\link.json'
cargo run --locked -p doso-cloud-gateway --example query -- 'D:\DOSO\private\robot-cloud\cloud-stage\query.json' '本轮设备UUID'
```

第一个终端保持模拟服务和云连接服务运行，第二个终端使用真实 Rust HTTPS 客户端。查询配置包含 `baseUrl`、`apiKey`、`caFile`，相对 CA 路径按配置文件所在目录解析。查询结束后对第一个终端按 `Ctrl+C`，随后由运维清理云端临时设备登记、公钥及主题 ACL。

此流程会连接已配置的公网服务。目录迁移本身只调整本地工程，ECS 保留现有 Mosquitto、Rust Gateway、Nginx 部署。

2026-11-04 新版 Gateway、业务同步和报告主题 ACL 已部署到 ECS。云连接服务与 Brainstem 本机只读入口仍待真机发布；Linux 包的具体版本见 [BUILD.md](BUILD.md)，历史模拟结果不代替真实上云验收。

## 2026-11-03历史验证记录

以下保留各次运行当时的源码、环境和结论。诸如“待接线”“未部署”描述该历史时点；当前完成程度见本文顶部及 [PROGRESS.md](PROGRESS.md)。

2026-11-03 后台自动同步验证：Gateway 原 17 项定向测试及新增“单设备许可过期不阻断另一台关闭”回归通过，模拟业务模型边界测试通过，工作区构建/格式/Clippy 通过。`lab auto-check` 实际验证了一次开启后自动跨首份许可到期、状态保存、Agent 重启自动恢复、业务源 503 后到期停采及恢复，以及一次关闭后拒旧报告和停采。

本轮使用 8 秒／4 秒的加速模拟许可，生产仍为 30 秒／15 秒。业务 API 与 Brainstem 为模拟，Gateway、MQTT 和 Agent 为真实进程；退出后进程和临时凭据已清理。证据：[自动运行结果](D:/DOSO/artifacts/robot-cloud/auto-sync-result.json)、[运行日志](D:/DOSO/artifacts/robot-cloud/auto-sync-check.log)、[五阶段真实同步报文](D:/DOSO/artifacts/robot-cloud/auto-sync-wire.json)。实际业务数据库、托管鉴权和 App 验收以业务侧独立记录为准，不能由模拟存储替代。

同日完成工程标准收敛：共享计数边界、会话身份比较和报告匹配只维护一份，HTTP/JSON 契约不变。受影响三个包的 31 项单元测试（Agent 14、Gateway 12、shared 5）、格式与 Clippy 通过；未重跑无变化的 Broker/公网基线。

2026-11-03 持续状态扩展已完成本地验证：

- 三个包定向单元测试合计 30 项通过：Agent 14、Gateway 12、shared 4。
- 工作区构建、格式检查、全目标 Clippy 通过。
- `lab check` 使用真实 Rust Gateway、MQTT/mTLS、Rust Agent 和模拟 Brainstem，通过旧查询兼容、缺少凭据、登记禁用、许可确认、状态变化、读取失败、关闭、到期和重连行为检查。
- 实际报告从 GROUNDED 更新为 STANDING，序号递增；读取失败不改变最后成功采集时间；关闭后旧许可 409、按需查询 403；到期和重连都不自动恢复采样。此次模拟总读取次数为 7。
- 退出后确认无本轮 Lab/Agent/Gateway/Broker 进程及临时 Lab 目录。可查看 [运行日志](D:/DOSO/artifacts/robot-cloud/reporting-check.log) 与 [报告样例](D:/DOSO/artifacts/robot-cloud/reporting-result.json)。

该次新增功能当时未安装或部署，未连接机器狗或启用5G。该记录尚不包含后来完成的业务许可事务、数据库写入与Web App演练，不能将通信层模拟本身当成这些业务能力的证据。

2026-11-03 迁移前的记录包括 14 项 Rust 单元测试，以及本地和公网模拟链路的 HTTP 200、401、403。公网独立记录保存在 [tools/deploy/RESULT.md](tools/deploy/RESULT.md) 与 [tools/deploy/query-result.json](tools/deploy/query-result.json)，它们描述迁移前运行结果。

2026-11-03 在新目录完成本次迁移验证：

- `cargo build --workspace --locked` 成功；`cargo test --workspace --locked` 的 14 项单元测试全部通过。
- `cargo run --locked -p lab -- check` 通过 HTTP 200、401、403 三项检查，模拟 `GetControlStatus` 总读取次数为 1。
- `lab local` 启动后按 Ctrl+C 退出，确认本轮子进程、私有临时目录和 13145 监听均已清理。Windows 终端因 Ctrl+C 返回 1，清理结果通过进程、目录和监听检查确认。
- `cargo fmt --all --check`、`cargo clippy -p lab --no-deps -- -D warnings` 通过。新增工具使用当前依赖，没有 Node.js 或旧实验目录运行依赖。

本次没有重新连接公网；`lab public` 已编译，公网成功记录仍属于迁移前那次模拟验证。

上述历史运行未覆盖真实机器狗、目标机Agent启动、5G、App账号/设备归属/客户许可及运行中的MQTT连接撤权。后续模拟设备Web演练补充了账号与业务流程证据，真机和完全断云仍待验收。迁移前Node脚本保存在 `D:\DOSO\archive\robot-cloud\20261103`，当前源码与测试入口由本独立工程提供。


## 本地版本管理基线（2026-11-09）

本轮在 `D:\DOSO\robot-cloud` 初始化独立本地 Git 仓库，并补充本地凭据、缓存和工具状态的忽略规则。忽略边界在隔离仓库及实际仓库分别验证；未配置远端或推送。

`cargo fmt --all -- --check` 通过。首次直接使用现有 `target/` 执行离线工作区测试时，正在运行的 `doso-cloud-link.exe` 占用文件，Windows 返回拒绝访问（os error 5）；本轮没有停止该进程。改用独立的 `D:\DOSO\build-cache\robot-cloud-git-baseline-20261109` 构建目录后，`cargo test --workspace --locked --offline -j 1 --quiet` 实际通过 77 项，1 项保持 ignored。

本次为本地代码和测试验证，不代表云端部署、真实机器狗或视频端到端验收。
