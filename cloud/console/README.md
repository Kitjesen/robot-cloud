# DOSO 管理后台

本目录将 ThingsBoard CE **4.3.1.8** 的定制 Angular 前端独立部署为 DOSO 管理后台。界面由上游真实组件构建，设备、历史、告警和账号请求继续交给现有 ThingsBoard 服务。

本机新入口为 `http://127.0.0.1:18081`，原版入口 `http://127.0.0.1:18080` 保留供对照和回退。两个入口使用同一个后台和数据库，原设备与账号可直接登录；浏览器存储按端口隔离，因此第一次打开新入口需要登录。本目录没有创建第二套客户账号，也没有扩大员工访问权限。

## 部署位置与使用

当前管理前端、ThingsBoard 服务及其数据库运行在开发机，业务 ECS 服务正常运行。两者之间的原私有通道已恢复，云端通过既有凭据只读访问 ThingsBoard 返回 200，与云业务库、本机读取的历史模拟记录一致。本次没有新的待投递记录，新增数据投递仍待验证。开发机、Docker 和临时 SSH 通道需要持续运行，重启后需恢复通道。具体记录统一见 [PROGRESS.md](../../PROGRESS.md) 和 [TESTING.md](../../TESTING.md)。管理后台本机可访问与设备数据已经经过云端链路属于不同验收项。

启动现有 ThingsBoard 与本目录前端后，在本机浏览器打开 `http://127.0.0.1:18081` 并使用既有租户账号登录。总览显示当前账号可访问的登记设备，设备卡进入详情，设备列表提供搜索、分页与原有操作，告警通过导航进入。账号获取方法见下方「运行与回退」。

## 内部工作台

2026-11-11 本轮源码已完成通知入口、会话提示与高级管理分组调整。总览使用现有接口的真实登记设备总数和最近五台设备，设备卡显示标签、必要的连接名称、来源分类、平台活跃状态与创建时间。总数卡、逐设备卡和逐快捷入口中的重复装饰图标已删除，保留数据范围提示、活动状态点与详情箭头。快捷入口采用紧凑文字行，设备卡按内容自然排布。

设备、告警等实体列表统一使用原有 `EntitiesTableComponent` 的表头、行、分页及操作区，避免分别维护同类表格。搜索、排序、选择、权限和详情行为沿用各页面配置。主要导航与侧栏操作使用八份 Lucide 原始 SVG，复用现有 `MatIconRegistry` 注册，主导航与面包屑图标20 px，原展开箭头16 px。正文与 Material 组件继续使用 MiSans。桌面使用双列设备卡和快捷面板，窄屏调整为单列与可滚动表格，保留键盘焦点及减少动态效果规则。

工作区与内容表面采用纯白，侧栏采用近白色。总览卡片、快捷面板、空态和共享表格采用无外框、无阴影布局，通过紧凑排版与中性灰悬停区分。侧栏右侧、顶部工具栏、品牌区与总数区底部使用极浅分界，表格内部保留极浅行分隔。“查看全部设备”采用无框 `mat-button`，设备与快捷链接覆盖上游默认下线规则，键盘焦点保留可见轮廓。主要间距采用4/8/12/16/24 px。

桌面侧栏默认宽240 px，可拖动右侧边缘在200–360 px之间调节，也可聚焦分隔条后用左右键每次调整8 px。宽度由 `HomeComponent` 在当前页面会话内保存，刷新恢复240 px。手机沿用覆盖式侧栏和原有宽度规则，调节手柄仅在桌面侧栏显示。拖动时内容边距随侧栏同步更新，拖动中切换到手机布局会取消调节状态。

铃铛合计平台通知与本地未读提示。本地列表保存当前登录会话最近30条提示，按消息和类型合并重复内容，支持单条或全部已读、清除。刷新、退出或换账号时清空会话列表，账号变化时立即销毁旧通知弹层。平台通知继续使用既有 WebSocket 订阅与服务端已读状态。

本地列表将已有消息的 `<br>` 换行标记转换为普通换行，并按归一后的文本去重，原 toast 的消息内容继续传递。平台通知在铃铛中预览三行正文，可通过「查看全部」及原有操作进入完整内容。

进入会话列表的提示保留短暂浮层反馈，未指定时长时默认6秒，错误和警告默认8秒，调用者显式设置的时长含0沿用原语义。提示正文13 px，采用紧凑半透明面板。铃铛浮层宽360 px、高度受窗口限制，空态使用简短文字。高级管理通过轻分界与留白分组。

本轮最终版已部署本机18081，生产编译414.538秒、9项服务/组件自检、45项接口与202资源、桌面和手机10项页面检查通过。构建指纹、截图与未覆盖范围见 [TESTING.md](../../TESTING.md#通知收纳与紧凑浮层2026-11-11)。完成状态统一维护在 [PROGRESS.md](../../PROGRESS.md)，具体实现值见 [design.md](design.md)。

设计参考采用 Ant Design 官方 GitHub 规范与 token 快照，参考重点为减少元素、规律间距、中性色层级与协调密度。DOSO 色值和尺寸由本项目选择，页面继续使用 Angular/Material。固定提交、原始文件与来源校验见 [本地参考说明](D:/DOSO/references/console-design/README.md) 和 [参考清单](D:/DOSO/references/console-design/manifest.json)。

白色方向参考了 ChatGPT 宿主窗口中可见的 Codex 工作区，当次截图像素采样包括主区 `#FFFFFF`、侧栏 `#FDFDFD` 与选中区 `#F1F1F1`。采样来源为桌面窗口画面观察，DOSO 应用色值由本项目样式维护。[颜色采样记录](D:/DOSO/artifacts/robot-cloud/console-20261110/workspace-color-samples.json)

11月11日新增 shadcn-ui、shadcn-admin 和 Lucide 三项目参考，共17份原始文件。侧栏结构、轻分界和独立尺寸调节组件用于理解交互，Lucide 原始 SVG 用于导航。shadcn 的 SidebarRail 点击用于折叠，DOSO 拖宽由现有 Angular Material 侧栏实现。[导航与图标参考](D:/DOSO/references/console-design/navigation-20261111/README.md) · [来源校验清单](D:/DOSO/references/console-design/navigation-20261111/manifest.json)

## 源码与构建

上游代码位于 `D:\DOSO\vendor\thingsboard-4.3.1.8`，固定提交 `ebbfc6789b81a4e04cae2711208130fb2841b31f`。DOSO 改动归档为 `patches/doso.patch`；界面说明见 [design.md](design.md)。修改后的源文件保留原 Apache 版权头与 DOSO 修改说明。

需要 Node、Yarn 1.22.22 和 Docker。上游 Maven 固定 Node 22.23.4；生产构建的 Node 堆上限为 4096 MiB，另需系统与构建进程空间。构建时只停止本任务拥有的试验容器以释放资源，不能关闭其他用户程序。`build.ps1` 使用单构建 worker，失败即停止，不修改上游锁文件。

从精确 tag 获取源代码后运行：

```powershell
git clone --depth 1 --branch v4.3.1.8 --filter=blob:none --sparse https://github.com/thingsboard/thingsboard.git D:\DOSO\vendor\thingsboard-4.3.1.8
git -C D:\DOSO\vendor\thingsboard-4.3.1.8 sparse-checkout add ui-ngx
```

源码已存在时不重复 clone。构建脚本检查固定提交，并核对源码改动与归档补丁完全一致。补丁尚未应用时，仅在干净源树应用。新增资源也须包含在二进制补丁中。默认Nginx镜像为本次已拉取核验的官方`nginx:1.30.6-alpine@sha256:583b7f2fce1cdb8da5ad5c39ebd3c33b98b87d493190e548bdee1fa8c619c311`，更新时另核验摘要。

```powershell
& 'D:\DOSO\robot-cloud\cloud\console\build.ps1'
```

`-SkipInstall` 复用已安装依赖，`-SkipImage` 只准备发行目录。成功编译后脚本将上游提交、补丁、锁文件和Node版本写入产物`doso-build.json`。`-SkipCompile`仅复用指纹与当前输入一致的生产输出，首次直接调用Angular CLI产生的文件需要通过本脚本重新编译后才能复用。发行物保存在独立 `.build/<时间与编号>/`。

Angular构建设置`NG_BUILD_MAX_WORKERS=1`和`NG_BUILD_PARALLEL_TS=0`，结束后恢复调用环境。发行manifest记录上游提交、补丁与锁文件哈希、实际Node版本和Nginx摘要，随镜像放在`/licenses/build.json`。

`Dockerfile` 使用构建时指定的 Nginx 镜像，以 `nginx` 非 root 用户监听容器 8080；镜像包含 Apache 许可证、DOSO 修改说明和 Angular 生成的第三方声明，位于 `/licenses/`。界面复用 App 的 MiSans Regular、Medium、Demibold、Bold 原始 OTF，字重为 400/500/600/700；原许可位于 `/assets/fonts/MiSans-License.pdf`，可从用户菜单打开，软件署名保存在 NOTICE。此处的品牌定制仅适用于固定版本许可，升级到 4.4+ 时重新核对白标授权。

Lucide SVG 按原字节随资源打包，完整 ISC 许可与文件内 Feather MIT 归属保存在 `/assets/icons/lucide/LICENSE`，软件署名记录在本目录 `NOTICE`。

## 运行与回退

先确认现有 `doso-telemetry-lab` ThingsBoard 已运行，并存在外部 Docker 网络 `doso-telemetry-lab_default`，然后运行：

```powershell
docker compose -f D:\DOSO\robot-cloud\cloud\console\compose.yml config --quiet
docker compose -f D:\DOSO\robot-cloud\cloud\console\compose.yml up -d
```

本机试验账号沿用既有 ThingsBoard 租户账号。需要登录时，在本机 PowerShell 读取私有引导文件中的两个字段。

```powershell
$bootstrap = Get-Content -LiteralPath 'D:\DOSO\private\robot-cloud\telemetry-lab-20261108\bootstrap.json' -Raw | ConvertFrom-Json
$bootstrap | Select-Object userEmail, userPassword
```

凭据只在本机查看，仓库文档和验收证据仅记录获取方法。

该 Compose 只管理新前端，不安装或初始化数据库，不调用账号引导脚本。只监听宿主 `127.0.0.1:18081`；跨主机生产部署需要单独配置 HTTPS 和后台网络。

Nginx 代理 `/api`、WebSocket、规则节点和 widget 静态资源，以及 OAuth 登录回调。API 的 401/403/404 原样返回，不会被前端首页吞掉。普通前端路由可直接刷新，缺失的 JS/CSS 等文件返回 404；首页不缓存，防止升级后继续引用旧资源。

访问日志仅记录方法、去掉query的路径、状态码和响应大小；不记录原始请求、授权头或query中的WebSocket凭据。API/OAuth等代理路由的运行错误日志写入`/dev/null`，其失败通过脱敏访问状态查看；静态资源与全局错误仍输出到标准错误，配置语法在部署前通过`nginx -t`验证。排错不临时启用可能暴露凭据的完整请求日志。

停止新界面并回到原版入口：

```powershell
docker compose -f D:\DOSO\robot-cloud\cloud\console\compose.yml down
```

此命令只停止新前端，不删除后台、设备、账号或数据库卷。修改菜单只改变入口，不替代后端权限检查。当前独立实例用于模拟设备验证；客户业务与 DOSO 人员访问边界以 [产品合同](../../CONTRACT.md#客户数据与告警责任) 为准。

> **OTA 边界**：`cloud/console`（ThingsBoard CE 4.3.1.8）仅用于设备遥测、告警与看板展示，**未接入** `doso-ota-release`（`:8140` / `/ota/`）接口，Nginx 也仅代理 ThingsBoard 原生路径。侧栏高级菜单中保留的上游 `/features/otaUpdates` 指向 ThingsBoard 原生包接口，不应用于 `doso-ota` 固件或策略发布；正式 OTA 发布、灰度与熔断统一使用 `tools/deploy/doso-ota-ops.py` 与 `docs/OTA_PRODUCTION_RUNBOOK.md`。

## 验证范围

部署前检查 Compose、Nginx 配置与构建脚本。HTTP/API检查覆盖登录会话、设备列表、详情、搜索、直接路由和未授权API；WebSocket已验证一次设备属性订阅。其余页面交互、生产OAuth、正式云端和客户数据隔离继续按实际环境分别验收。

2026-11-11 此前侧栏与图标版已部署本机18081，最终45项接口与202个静态资源、Chromium IAB三种尺寸21项检查通过，两次前置接口检查的失败原件保留。该记录属于本轮通知入口与会话提示调整前的版本，证据见 [侧栏调宽与图标历史记录](../../TESTING.md#侧栏调宽与图标2026-11-11)。

2026-11-10 首版内部工作台曾完成生产构建并部署本机18081，HTTP/API/WebSocket 35项、193个静态资源及Chromium IAB四种尺寸22项检查通过，登录页桌面与手机左卡基线回归通过。该记录属于紧凑调整前的版本，保留在 [内部工作台历史记录](../../TESTING.md#内部工作台重构2026-11-10)。

同日紧凑版曾部署本机18081，HTTP/API/WebSocket 35项及193个资源、Chromium IAB四种尺寸21项检查通过。该记录属于白底无框调整前的版本，构建指纹、截图与验收范围保留在 [紧凑工作台历史记录](../../TESTING.md#紧凑工作台与设计参考2026-11-10)。白底无框版的构建、部署和浏览器结果单独记录。

同日白底无框版曾部署本机18081，HTTP/API/WebSocket 35项及193个静态资源、Chromium IAB四种尺寸18项检查通过。该记录属于11月11日侧栏宽度、Lucide 图标和极浅分界调整前的版本，原证据保留在 [白底无框工作台历史记录](../../TESTING.md#白底无框工作台2026-11-10)。

2026-11-09 前一版白底蓝色流光与毛玻璃登录页已部署到本机18081，生产编译313.978秒，发行目录为 `.build/20261109-145800-bcdd484b`。最终 HTTP/API/WebSocket 35项及193个静态资源通过。Chromium 浏览器19项检查通过，覆盖1440×900、390×844、320×568、900×500、流光变化、玻璃效果、低高度滚动、错误提示、键盘登录和退出。reduced-motion 与不支持背景模糊的运行时回退仅检查源规则。景观为生成的虚构素材，愿景文字仍为试排，尚未用户确认定稿。构建指纹、报告、截图和此前版本历史统一见 [TESTING.md](../../TESTING.md#doso管理后台2026-11-09)。

2026-11-10 左右等大布局已部署到本机18081。桌面两列等宽、拉伸同高，右侧内容组最大宽380 px并整体居中，959 px及以下为单列自然高度。最终生产编译208.136秒，HTTP/API/WebSocket 35项与193个资源通过，Edge浏览器六种尺寸17项检查通过，覆盖等大对齐、内容居中、移动排列、低高度滚动及键盘登录和退出。其他浏览器、reduced-motion与背景模糊回退运行时尚未实测。完整指纹、报告及截图见 [TESTING.md](../../TESTING.md#登录页左右等大布局2026-11-10)。

同日连续场景方案曾部署到本机18081，生产编译212.083秒，HTTP/API/WebSocket 35项与193个资源通过，Chromium IAB六种尺寸22项检查通过。用户随后明确要求恢复原左侧景观，该轮技术证据保留为历史。完整指纹、报告及截图见 [TESTING.md](../../TESTING.md#登录页连续场景2026-11-10)。

当前登录页版本已恢复等大版左侧图源、尺寸、构图、圆角和文字，右侧玻璃向左填满原间距并藏入左图后28 px，移动布局向上藏入24 px。右侧为0.4白底与20 px模糊，保留15 px辅助标题和0.75白底填充输入框。最终生产编译188.048秒，本机18081已部署，HTTP/API/WebSocket 35项及193个资源通过，Chromium IAB六种尺寸25项检查通过，四种尺寸左图边界与基线差异小于0.1 px。开发服务已停止。以上为登录页技术验证，用户视觉反馈另记。愿景文字仍为试排，其他浏览器与两种兼容模式的运行时效果尚未实测。[完整记录](../../TESTING.md#原左卡与延伸玻璃2026-11-10)
