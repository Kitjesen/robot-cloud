# 设备身份与首次开通

更新：2026-11-12。设备登记、密钥/证书准备和实际开通分别以本页及[当前进度](../../PROGRESS.md)的证据为准；身份准备不等于客户认领或已启用云。

业务库存中的 `devices.id` 是机器狗的设备 ID。先核对实体的出厂序列号和型号，确认库存是否已有记录；不能使用 IP、MAC 或模拟设备 ID 替代。客户认领改变设备归属，不改变设备 ID。

现有系统有三种不同用途的身份，不能默认共用密钥：

| 用途 | 设备侧 | 云端 |
| --- | --- | --- |
| 出厂身份与客户认领证明 | 出厂身份私钥 | 业务服务保存制造公钥，验证认领挑战 |
| MQTT 连接 | 独立客户端私钥和证书 | Broker 验证证书，证书 CN 与设备 ID 一致 |
| 状态消息签名 | 独立 Ed25519 私钥 | Gateway 按设备 ID 登记公钥，验证消息 |

首次开通时，在设备上生成 MQTT 私钥和证书签名请求（CSR），以及状态签名密钥；只将 CSR 和状态签名公钥送往可信开通端。签发端核对库存 ID 与实体设备，为 CSR 签发仅用于客户端认证的证书，并记录证书序列号、指纹和有效期。设备安装证书、Broker CA 公钥证书与 Gateway 验签公钥；私钥不随程序分发，也不交给 App。

[device-keys.sh](device-keys.sh) 可在 Linux 设备上为已核实的 UUID v4 生成这些材料，目标必须是可信存储中的新目录：

```sh
sh device-keys.sh <真实设备UUID> <新建的绝对目录>
```

工具只输出 CSR 和签名公钥的路径，不签发证书、不连接云端，也不启用设备。[device-keys-check.sh](device-keys-check.sh) 用临时身份验证生成与拒绝覆盖行为。

Gateway 的登记、公钥和 Broker 六条设备主题权限使用同一个真实设备 ID；参见 [ACL 样例](../../cloud/broker/acl.conf.example)。开通设备连接和客户同意状态采集是两件事：用户没有开启远程状态时，不能自动采集上报。

业务设备登记、扫码认领及制造身份相关服务已有代码，但首次近场设备证明、原子认领与MQTT证书签发尚未形成完整自动流程。2026-11-07已登记并绑定明确标记的模拟设备，见[模拟设备链路](../../docs/SIMULATED_FLOW.md)；该结果不证明首台真实设备已完成开通。

Broker尚无完整证书吊销和已连接设备断开流程；仅禁用Gateway或移除ACL不等于证书已撤销。生产使用前须完成证书台账、续期、吊销及现有连接处理，并验证这些操作。机器狗的可信本地云连接授权与远程关闭后持久化断云仍缺完整实现，凭据安装不能替代用户授权。真实设备身份与App端到端上云结果以 [PROGRESS.md](../../PROGRESS.md) 为准。

## 首台研发机身份准备（2026-11-12）

用户明确选择研发机库存序列号 `THUNDER-DEV-001`，业务库存已创建真实UUID `d82a831a-2e86-4b46-975b-3d42473583b7`，型号代码 `THUNDER 01`、显示名「THUNDER 研发机 001」，非demo、非模拟、owner为空。该事实只证明真实库存登记，不能记为客户已认领、近场证明或设备已上云。[库存记录](D:/DOSO/artifacts/robot-cloud/sdk-stability-20261112/inventory-created.json)

云端库存再次读回同一UUID及序列号，仍为非demo/非模拟、未认领，没有为重复核对重新分配身份。[云端复核](D:/DOSO/artifacts/robot-cloud/sdk-stability-20261112/inventory-verified.json)

机器狗 `192.168.66.64` 的 `/etc/doso/robot-cloud/agent/agent.env` 已原子写入该UUID，替换旧样例 `00000000-0000-4000-8000-000000000001`，旧文件保留备份 `agent.env.before-identity-20261112T110124Z`。配置权限为root:doso-cloud 0640，`DOSO_CLOUD_ON=false`保持，未重启服务；不能将写入配置解释为正在运行的Host已经加载身份。[配置写入](D:/DOSO/artifacts/robot-cloud/sdk-stability-20261112/identity-110125.json)

再次执行身份配置返回changed=false，前后均为同一真实UUID，没有生成新身份或备份，云开关仍false、没有服务重启。[重复执行](D:/DOSO/artifacts/robot-cloud/sdk-stability-20261112/identity-111345.json) 电脑只读开发桥已配置该UUID，用于SDK握手字段对照；它不是机端Host，字段一致不能替代机端完整SDK服务部署验收。

三次Rust双流10秒短重连均完成，电脑桥robot_id均为该UUID，源时间在重连之间继续前进；robot_version和firmware_version仍为None，真实机端release接线尚未完成。[重连原件及范围](../../TESTING.md#真实身份准备与-sdk-稳定性复测2026-11-12) 库存的 `model_code=THUNDER 01` 只描述库存型号，不证明SDK型号字段已设置；不得用Brainstem flowfix8构建名或SDK版本补齐固件版本。

复用现有 `device-keys.sh` 在机端生成TLS私钥/CSR以及独立Ed25519状态签名密钥，CSR CN与库存UUID一致，公私钥一致性与权限检查通过。`device.key`、`device-signing.key`和`device-signing.pub`位于该机端agent目录，root:doso-cloud 0640；私钥没有下载，仅导出CSR与状态签名公钥。[准备结果](D:/DOSO/artifacts/robot-cloud/sdk-stability-20261112/device-keys.json)

尚未签发或安装 `device.crt`，云端Gateway状态验签公钥登记及Broker设备ACL尚未完成；尚未生成制造认领密钥，已有MQTT TLS密钥和状态签名key不能作为制造认领证明复用。真实库存UUID和已备份配置无需再次分配；下一步沿用同一UUID完成对应的证书/云侧登记，再分别验证开通与用户许可。云开关保持关闭，本节不代表客户认领、远程状态或运动验收通过。
