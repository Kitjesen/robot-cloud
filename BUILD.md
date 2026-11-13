# Linux 构建

更新：2026-11-14。当前源码目录为 `robot/`、`cloud/`、`shared/`、`tools/`；包名和目标二进制名沿用原合同。研发机已安装 ARM64 `doso-cloud-link` 候选、本机 authorizer 和独立身份材料；云端设备登记已启用，但目标机当前不可达，不能把安装记录写成真实 MQTT 上线。其他候选包、ABI 和安装记录仍属于各自标明的历史构建，当前源码与线上制品须核对具体制品。

## 当前 ARM64 云连接候选（2026-11-12）

### 2026-11-13 远程关闭持久化候选

本轮源码已重新交叉编译为 AArch64 Linux 候选，加入远程 `enabled=false` 持久化本地 `choice=false`、停止采样并断开 cloud-link，以及本地重新开启后的新许可流程。候选包位于
`D:\DOSO\artifacts\robot-cloud\releases\20261113-cloud-link-aarch64-remote-close\link-aarch64.tar.gz`。

| 项目 | 值 |
| --- | --- |
| ARM64 ELF | 4,487,048 bytes，AArch64 |
| 二进制 SHA256 | `15a1603fcd3af3feba71d3481e91f481011bfce0b80f141df6015517dd7bea76` |
| 包 SHA256 | `46016e2bf0092e44230347d94d6ff5135db085d278feee98820d900841730ac1` |
| Cargo.lock SHA256 | `86daa7df13755cf8df3db69420eee5e9a44c0f3297b2dbe9df0664c0c048d547` |
| 关闭闭环测试 | 远程关闭、重启保持关闭、本地重新开启、许可到期不改 choice 均通过 |
| 真机状态 | 未安装；13146/G1 尚未验收 |

该候选没有设备私钥，默认保持 `DOSO_CLOUD_ON=false`。它只证明新源码可生成可交付 ARM64 包；在现场安装前仍必须通过 CAN/Brainstem 前置检查、G1 维护窗口、G3 云端登记和证书验收。

已用当前工作区源码重新构建 `doso-cloud-link`，目标为 `aarch64-unknown-linux-gnu.2.35`。候选包位于
`D:\DOSO\artifacts\robot-cloud\releases\20261112-cloud-link-aarch64\link-aarch64.tar.gz`，包内默认保持
`DOSO_CLOUD_ON=false`，不包含设备私钥。

| 项目 | 值 |
| --- | --- |
| ARM64 ELF | 4,473,832 bytes，AArch64 |
| 最高 GLIBC | `GLIBC_2.34` |
| 二进制 SHA256 | `d9fe0504b0c4fab27236e31a6171a6947cf85eece8125a69730e2e602b826d85` |
| 包 SHA256 | `b3c4fd2cfa796f5233942a84c392ab723bd6cc00286eaa2de8cee0f78699ea36` |
| Cargo.lock SHA256 | `86daa7df13755cf8df3db69420eee5e9a44c0f3297b2dbe9df0664c0c048d547` |
| 狗端测试 | 24 passed + 2 startup passed，1 ignored |
| 真机状态 | 未安装；目标机当前不可达 |

该候选只证明当前源码可以生成可交付的 ARM64 包，不证明目标机 Brainstem `127.0.0.1:13146` 已发布、SSH 可达、证书/UUID/ACL 已配置或真实云状态已接通。安装前仍必须先核对 Brainstem 只读入口，再按 [LINK.md](tools/deploy/LINK.md) 保持云连接关闭完成安装。

## 遥测投递候选（2026-11-08）

`doso-cloud-telemetry` 已用锁定依赖和单任务交叉构建生成 x86_64 Linux ELF，构建目标 `x86_64-unknown-linux-gnu.2.35`。二进制3,952,040字节，SHA256为 `bf33aad933e3d20f8e54091edbe99aff8363ecbe10bd2473d80b2f1fb16d2b7c`。真实Linux容器中ldd解析成功、缺失配置时以1退出并报固定错误，未运行目标机systemd或生产投递。[Linux检查](D:/DOSO/artifacts/robot-cloud/telemetry-20261108/linux-smoke.json)

[增量候选包](D:/DOSO/artifacts/robot-cloud/releases/telemetry-20261108-x86_64.tar.gz) 包含Linux程序、环境样例、systemd单元、业务新增SQL/Function、固定镜像的隔离部署材料和验收证据；附带SHA256SUMS及manifest，不含任何私钥/密码/API key。它依赖现有业务工程的共享函数模块，不是空服务器全量安装包。[安装与回退](D:/DOSO/artifacts/robot-cloud/releases/20261108-telemetry/INSTALL.md)

```powershell
$env:PATH = 'D:\DOSO\toolchains\robot-cloud\zigbuild\bin;D:\DOSO\toolchains\robot-cloud\zig\ziglang;' + $env:PATH
cargo zigbuild --release --locked --offline -p doso-cloud-telemetry --target x86_64-unknown-linux-gnu.2.35 -j 1
```

## 原云连接与Gateway构建

在 `D:\DOSO\robot-cloud` 根目录构建。正式交付程序为 `doso-cloud-link` 和 `doso-cloud-gateway`，依赖由当前 workspace 的 `Cargo.lock` 锁定。`lab` 仅用于开发。

2026-11-04历史候选：源码收紧单次状态查询的用户许可后，ARM64狗端与x86-64 Gateway可执行候选保存于 `D:\DOSO\artifacts\robot-cloud\releases\20261104-status-consent`，当时已核对架构和GLIBC要求，未在目标机运行或安装、未生成安装包。该记录不能推断后续线上或隔离Gateway的制品版本。

当前源码已包含持续状态、短期采集许可和业务自动同步，并将狗端程序改名为云连接服务 `doso-cloud-link`。新版 Gateway 已于 2026-11-04 部署 ECS；当天生成的旧名 Agent 候选包读取本机只读 `13146`，尚未安装到机器狗。

新名称狗端输出为 `target/aarch64-unknown-linux-gnu/release/doso-cloud-link`，云端输出为 `target/x86_64-unknown-linux-gnu/release/doso-cloud-gateway`。新名离线候选包为 `D:\DOSO\artifacts\robot-cloud\releases\20261104-cloud-link-naming\link-aarch64.tar.gz`；已核验 AArch64、最高 GLIBC_2.34、默认关闭和包内文件，目标机启动及部署尚未验证。使用步骤见 [tools/deploy/LINK.md](tools/deploy/LINK.md)。旧名候选包记录见下节。

## Windows 交叉构建

现有构建工具为 Rust 1.94.0、cargo-zigbuild 0.24.0、Zig 0.15.2，工具已独立存放于 `D:\DOSO\toolchains\robot-cloud`。需安装 Rust targets `aarch64-unknown-linux-gnu` 和 `x86_64-unknown-linux-gnu`。

```powershell
$env:PATH = 'D:\DOSO\toolchains\robot-cloud\zigbuild\bin;D:\DOSO\toolchains\robot-cloud\zig\ziglang;' + $env:PATH
cargo zigbuild --release --locked -p doso-cloud-link --target aarch64-unknown-linux-gnu.2.35
cargo zigbuild --release --locked -p doso-cloud-gateway --target x86_64-unknown-linux-gnu.2.35
```

在已有 Rust 的 Linux 构建机，也可安装相应 Zig / cargo-zigbuild 后使用相同的两条 `cargo zigbuild` 命令。工具安装路径按构建机配置。

`.2.35` 是 cargo-zigbuild 的 glibc 兼容参数，rustup target 与输出目录使用无后缀的目标名。构建应清除已有的 `RUSTFLAGS` 或 `CARGO_ENCODED_RUSTFLAGS` linker 覆盖，沿用 Zig 链接。

| 程序输出 | 目标系统基线 |
| --- | --- |
| `target/aarch64-unknown-linux-gnu/release/doso-cloud-link` | 机器狗 Ubuntu 22.04.5 / aarch64 / glibc 2.35 |
| `target/x86_64-unknown-linux-gnu/release/doso-cloud-gateway` | ECS Ubuntu 24.04.5 / x86_64 / glibc 2.39 |

设置 `CARGO_TARGET_DIR` 时输出移至指定目录。Windows 原生 `cargo build` 产生 Windows 二进制，Linux 部署需使用对应 Linux ELF。产物使用 GNU glibc 动态链接，部署目标须满足 ABI 要求。

## 已生成的旧名 Agent 候选包

`D:\DOSO\artifacts\robot-cloud\releases\20261104-local-status` 保存此次 ARM64 包、展开目录和 `package-check.json`。二进制为 4,279,344 字节，ELF64 / AArch64，最高所需 GLIBC_2.34；归档内容、文件校验和与默认禁用配置已验证。沿用现有安装脚本、systemd 单元及公有信任材料，真实设备身份与私钥由登记流程单独提供。

该次旧名Agent构建记录为14项单元测试通过、目标机SSH重连超时。它描述当时的验证条件；ARM64实际启动、本机状态端口和真实公网读取仍无已完成验收记录，不能据旧超时推断今天的网络状态。

## 历史发布包

现有包位于 `D:\DOSO\artifacts\robot-cloud\linux`，包含 `agent-aarch64.tar.gz`、`gateway-x86_64.tar.gz` 及对应展开目录。包内有二进制、配置样例、systemd 单元和 `manifest.json`，Gateway 包还带 Broker 与 HTTPS 模板。私钥和服务凭据独立保管。

2026-11-03 狗端包补齐 `agent.sh`、`AGENT.md`、公开 CA、网关验签公钥和 `SHA256SUMS`，顶层目录为 `agent/`。安装后保持远程连接关闭，其包内安装入口仍为旧名；新名称安装步骤见 [tools/deploy/LINK.md](tools/deploy/LINK.md)。此次只补安装材料，沿用下表已核验的 ARM64 二进制，未重新编译或部署；包 SHA256 位于同目录 `agent-aarch64.tar.gz.sha256`。

这两份包来自迁移前 2026-11-03 的生产构建。本次迁移保留同一份生产二进制，新增 Rust `lab` 工作区成员后的锁文件不能用于反向说明旧包的依赖来源。

| manifest 字段 | 迁移前构建记录 |
| --- | --- |
| `cargo_lock_sha256` | `5614cb2cc9195c1ef0b3c56b4f6bd9a9f2d4d82b3fb59887cf6a3d721c339bca` |
| Agent 二进制 SHA256 | `3dda448ee1f661a16c2b47cf0c9f9cab684e49328d1709907c78877a17d56ab6` |
| Gateway 二进制 SHA256 | `89b67516a2cc28469f2381ebaabaf1ef6b3f5f9d35c9a992adec5e459e146495` |
| 最高所需 glibc 符号 | 两份均为 `GLIBC_2.34`，为迁移前静态检查记录 |

以后从本工程重建发布包时，重新记录当前二进制、锁文件哈希和 ABI 检查结果。每台狗使用独立证书、身份与配置，同架构和系统基线的设备可共用二进制。

## 验证边界

迁移前x86_64 Gateway曾在ECS启动并完成公网模拟状态查询，历史结果见 [tools/deploy/RESULT.md](tools/deploy/RESULT.md)。11月4日有新版Gateway部署记录，11月7日另有隔离Gateway参与模拟设备Web流程的记录，见 [PROGRESS.md](PROGRESS.md)。这些记录不证明当前正式环境运行全部最新源码。机器狗端候选与真实Brainstem公网状态仍待目标机交付验收；历史Linux目录保留原包。

本地 Rust 检查与联调入口见 [TESTING.md](TESTING.md)。
