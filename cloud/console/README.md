# DOSO 管理后台

本目录将 ThingsBoard CE **4.3.1.8** 的定制 Angular 前端独立部署为 DOSO 管理后台。界面由上游真实组件构建，设备、历史、告警和账号请求继续交给现有 ThingsBoard 服务。

本机新入口为 `http://127.0.0.1:18081`，原版入口 `http://127.0.0.1:18080` 保留供对照和回退。两个入口使用同一个后台和数据库，原设备与账号可直接登录；浏览器存储按端口隔离，因此第一次打开新入口需要登录。本目录没有创建第二套客户账号，也没有扩大员工访问权限。

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

## 验证范围

部署前检查 Compose、Nginx 配置与构建脚本。HTTP/API检查覆盖登录会话、设备列表、详情、搜索、直接路由和未授权API；WebSocket已验证一次设备属性订阅。其余页面交互、生产OAuth、正式云端和客户数据隔离继续按实际环境分别验收。

2026-11-09 本轮 MiSans 与登录页精简的生产编译通过，构建阶段245.695秒，发行目录为 `.build/20261109-100457-b0ad94c5`，已部署到本机18081。最终镜像 HTTP/API/WebSocket 定向33项和191个静态资源检查通过。浏览器已完成登录、总览、设备页及退出，确认输入框、按钮和设备表头使用 MiSans，用户名标签实际渲染 MiSans-Regular，图标仍使用 Material Icons；登录页两处重复文案已删除。本次视口为663×2364，多断点和高级页面仍待验收。构建指纹、报告及历史版本记录统一见 [TESTING.md](../../TESTING.md#doso管理后台2026-11-09)。
