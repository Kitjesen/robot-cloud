# 云端遥测投递

本目录的 Rust 程序运行在云服务器，接收 DOSO 业务数据库已接受的状态记录，投递到 ThingsBoard。客户仍使用 DOSO App 和 Rust SDK；设备归属、用户许可及当前状态继续由现有业务后端管理。源码与数据库改动的整体范围见 [迁移方案](../../docs/MIGRATION.md)。

客户数据与告警责任见 [产品合同](../../CONTRACT.md#客户数据与告警责任)。当前完整`sourceReport`可能含位置，单一实验租户和技术管理员只完成模拟接入；数据类别许可、DOSO人员默认禁读及售后限时访问尚未实现。真实客户接入需先落实所选隐私边界；现有总开关与运维出口开关不代表这些细分授权。

`deploy/` 提供独立开发用 ThingsBoard 4.3.1.8 和 PostgreSQL 18.1 配置。两个镜像摘要于 2026-11-08 直接从 Docker Registry 核对并固定；仅 HTTP 映射到 `127.0.0.1:18080`，数据库不映射宿主端口，MQTT、CoAP 等传输关闭。该配置不连接现有 DOSO 数据库。

单设备试验限制 ThingsBoard 容器 1 GiB、JVM 堆 256–512 MiB，PostgreSQL 容器 256 MiB；`JAVA_TOOL_OPTIONS` 由 JVM 读取，安装进程与服务进程都适用。该限制用于低负载验证，不是生产容量承诺；启动、安装与 Rust 大编译分开运行，出现内存不足时保留日志并增加可用内存。

## Rust 服务配置

复制 [env.example](env.example) 到私有目录。`DOSO_TELEMETRY_URL` 为业务 `telemetry-export` 完整地址，`DOSO_TELEMETRY_KEY` 是独立业务服务 key；`DOSO_THINGSBOARD_URL` 为 TB 根地址，`DOSO_THINGSBOARD_KEY` 是其专用技术账号 API key。两组凭据不互用，均不进入 App 或机器狗。HTTPS 之外仅允许 literal loopback HTTP；响应不接受重定向。

在工程根目录构建 `cargo build --locked -p doso-cloud-telemetry`，由服务管理器把私有环境变量注入 `doso-cloud-telemetry` 后运行。程序不会自行开启用户云连接；业务侧设备投递开关默认关闭。当前轮领取一条记录，请求失败或队列空闲时等待两秒，退出中断由数据库领取租约恢复。

## Linux 云服务器部署

[doso-cloud-telemetry.service](deploy/doso-cloud-telemetry.service) 运行非 root 的 `doso-telemetry` 用户，沿现有项目使用 `/opt/doso/bin/` 和 `/etc/doso/robot-cloud/`。服务没有控制 HTTP 监听；`SIGTERM` 触发 Rust 已有退出处理，未确认投递由数据库领取租约恢复。异常退出五秒后重试，一分钟内最多启动五次；配置错误须修正后再启动。

发布顺序：

1. 备份并核对业务数据库，按最新迁移链应用 [遥测增量 SQL](<C:/Users/99563/Documents/ChatGPT/New project/doso-app/supabase/migrations/20261108020000_add_telemetry_export.sql>)。设备投递默认关闭，保留原状态接口与用户许可。
2. 发布业务 `telemetry-export` Function，配置独立命名服务 key、Functions 路由白名单及该入口的 JWT 验证策略；函数内继续验证专用 `apikey`，不能把关闭平台 JWT 验证解释为匿名可用。入口与 RPC 的具体合同见 [后端说明](<C:/Users/99563/Documents/ChatGPT/New project/doso-app/supabase/functions/telemetry-export/README.md>)。先验未授权请求被拒。
3. 准备已验证的 Linux 构建制品、私有环境文件与服务单元，按下面路径安装并启动 worker；出口关闭时应为空闲领取。
4. 使用可信服务角色仅为本批指定设备执行 `configure_telemetry_export(..., true)`，再核对投递、TB 历史和原许可续期。该运维开关不授予用户云连接许可。

下面是目标 Linux 主机上的手工安装步骤，当前终端不会自动执行。`/private/release/` 代表本次已校验的发布资料目录；环境文件必须已填好独立服务 key 和真实 HTTPS/本机平台地址。

```sh
id doso-telemetry >/dev/null 2>&1 || useradd --system --home-dir /opt/doso --shell /usr/sbin/nologin doso-telemetry
install -d -m 755 /opt/doso/bin
install -o root -g root -m 755 /private/release/doso-cloud-telemetry /opt/doso/bin/doso-cloud-telemetry
install -d -o root -g root -m 700 /etc/doso/robot-cloud/telemetry
install -o root -g root -m 600 /private/release/telemetry.env /etc/doso/robot-cloud/telemetry/telemetry.env
install -o root -g root -m 644 /private/release/doso-cloud-telemetry.service /etc/systemd/system/doso-cloud-telemetry.service
systemd-analyze verify /etc/systemd/system/doso-cloud-telemetry.service
systemctl daemon-reload
systemctl enable --now doso-cloud-telemetry
systemctl is-active doso-cloud-telemetry
journalctl -u doso-cloud-telemetry -n 30 --no-pager
```

环境文件由 systemd 系统管理器读取，配置仅注入本服务进程，不要求非 root 用户直接读取私有文件。不要使用全局 `systemctl set-environment` 注入这些 key。

指定设备的出口由可信服务角色配置，UUID 使用已登记的原编号：

```sql
select public.configure_telemetry_export('设备UUID'::uuid,true);
```

回退先对本批设备执行同一函数的 `false`，取消新领取与待发送记录，再 `systemctl stop doso-cloud-telemetry`；需要保持停用时执行 `systemctl disable doso-cloud-telemetry`。平台已有历史和原 DOSO 实时状态保留。已经开始的外部 HTTP 可能完成，不把停止 worker 等同于原子撤销在途请求。二进制回滚使用事先保存的上版制品，不重跑已发布 SQL 或删除业务表。

## 隔离平台启动

需要 Docker Compose 与 Linux Engine。把 [部署环境样例](deploy/env.example) 复制到源码外的私有目录，设置独立、随机且至少 32 字符的数据库密码。后续命令始终使用同一份私有文件；不要把填好密码的文件提交到源码。

在工程根目录的 PowerShell 中设置本轮私有环境文件路径：

```powershell
$privateEnv = 'D:\DOSO\private\telemetry-lab\platform.env'
$compose = 'D:\DOSO\robot-cloud\cloud\telemetry\deploy\compose.yml'
docker compose --env-file $privateEnv -f $compose config --quiet
docker compose --env-file $privateEnv -f $compose up -d --wait postgres
```

**只有全新数据库第一次安装时**执行初始化。先确认本项目卷是本次新建的隔离卷；已有 ThingsBoard 数据库不要重跑安装，也不要把升级操作当成初始化。

```powershell
docker compose --env-file $privateEnv -f $compose run --rm -e INSTALL_TB=true thingsboard
docker compose --env-file $privateEnv -f $compose up -d thingsboard
docker compose --env-file $privateEnv -f $compose logs --tail 100 thingsboard
```

这里采用官方 clean install，不加载 demo 租户或设备。官方安装器仍会创建默认系统管理员。在全新隔离环境运行本目录初始化脚本，`StateFile` 必须位于源码外的专用私有目录：

```powershell
& 'D:\DOSO\robot-cloud\cloud\telemetry\deploy\bootstrap.ps1' -StateFile 'D:\DOSO\private\telemetry-lab\bootstrap.json'
```

脚本先把随机新密码写入当前用户独占 ACL 的私有状态文件，再更改默认系统管理员密码；随后创建独立试验租户、技术账号和七天有效的 API key。创建与激活账号均显式禁止发送邮件。相同状态文件重跑会恢复本次资源，不重置其他用户密码。完整私有状态保存到指定路径，裸 API key 与根地址另存同目录 `thingsboard.env`；控制台仅输出非敏感的资源 ID 和完成结果。不要删除私有状态后对已有安装重新执行首次引导。

脚本仅用于本机隔离平台；正式平台账号生命周期按运维流程管理。投递程序使用技术账号 API key，不使用默认管理员口令或每设备 Access Token。API key 继承用户权限，不等于天然只写遥测的窄权限凭据。

本次开发机实例地址为 `http://127.0.0.1:18080`，引导状态位于源码外 `D:\DOSO\private\robot-cloud\telemetry-lab-20261108\bootstrap.json`；其中 `userEmail/userPassword` 用于该隔离技术账号登录。平台数据库私有环境为同目录 `compose.env`。这些文件包含凭据，只供本机操作，不复制到发布包。

容器启动不表示数据接口已经可用；等待日志出现 `Started ThingsboardServerApplication` 并完成下面的读回检查。正式环境跨主机接入须有 HTTPS、合适的账号权限及独立运维配置，不直接把本地映射改成公网监听。

## 单设备 API 检查

在当前 PowerShell 的私有环境中设置 `DOSO_THINGSBOARD_URL=http://127.0.0.1:18080` 和 `DOSO_THINGSBOARD_KEY`，然后运行：

```powershell
& 'D:\DOSO\robot-cloud\cloud\telemetry\deploy\smoke.ps1'
```

脚本只允许 literal loopback HTTP，不接受 `localhost`、远程主机或重定向。它创建一台名称以 `doso-smoke:` 开头的独立模拟设备，设置模拟来源属性，重复写同一条记录，再写一条较旧记录，查询并核对历史只有两点且 latest 未倒退。输出仅含测试设备 ID、时间与结果。该设备会留在隔离租户作为验证证据，不绑定任何 DOSO 用户，也不触发机器狗采样；可在平台界面按名称清理。

脚本本身的本地行为检查：

```powershell
& 'D:\DOSO\robot-cloud\cloud\telemetry\deploy\smoke.test.ps1'
& 'D:\DOSO\robot-cloud\cloud\telemetry\deploy\bootstrap.test.ps1'
```

这两项检查使用 HTTP 函数夹具，覆盖 URL 边界、读回流程和引导的持久化/幂等行为，不能替代对真实 ThingsBoard 的运行。

## 停止与回退

先在业务侧关闭本批设备的遥测投递，再停止 Rust 投递程序。隔离平台可独立停止：

```powershell
docker compose --env-file $privateEnv -f $compose down
```

此命令保留 `doso-telemetry-lab_postgres-data` 卷；不加 `--volumes`，也不执行数据库删除。重新启动沿用原卷和原密码，直接 `up -d`，不再运行 `INSTALL_TB`。现有 DOSO Gateway、Broker、App 和机器人服务不属于该 Compose 项目。

## 验证与许可范围

配置与初始化方式来自 [官方 CE Docker 文档](https://thingsboard.io/docs/installation/docker/) 和 [4.3.1.8 启动源码](https://github.com/thingsboard/thingsboard/blob/v4.3.1.8/msa/tb-node/docker/start-tb-node.sh)。版本 4.3.1.8 为 Apache-2.0，官方安全支持至 2027-07-20；此处是隔离验证基线，生产升级路径另定。4.4+ 许可与采用边界见 [复用调研](../../docs/REUSE.md)。

本目录提供实际部署配置和检查脚本；仅当父级测试记录明确给出运行结果时，才能宣称容器、真实 API、数据迁移或真机接入通过。REST 遥测保存不等于原生设备在线状态或告警规则已经接通。

2026-11-08 已在本机真实 ThingsBoard 4.3.1.8 与独立 PostgreSQL 18.1 完成 clean install、无邮件账号引导和真实 API 读回。明确模拟的 `doso-smoke:` 设备重复写入后历史为两点，较旧记录未倒退 latest；结果见 [thingsboard-smoke.json](D:/DOSO/artifacts/robot-cloud/telemetry-20261108/thingsboard-smoke.json)。这验证平台安装和 REST 存储行为，不代表 DOSO 业务出队、云服务器 Linux 安装或真实机器狗已验收。

Linux 单元仅完成本地文件检查，未在 Linux 执行安装或 systemd 运行验证；目标机仍须核对服务用户、制品架构、证书信任、业务入口和停止行为。
