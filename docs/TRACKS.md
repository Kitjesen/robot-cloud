# GPS 跟随与轨迹

更新：2026-11-09。DOSO App 在“我的 → 设备 → 查看位置”显示地图和一张摘要卡。运行中更新位置与轨迹；停止上报后保留记录。没有导航任务真源时，不显示到达、剩余路程、ETA 或转向指令。

## 用户看到什么

- 默认街道跟随视角；拖动、旋转或缩放后自由查看，点击“跟随”恢复。
- “跟随/全览/地球”位于地图上方独立工具栏，不覆盖绘图区；起点和末次记录用小点标识，靠近时避免重叠。
- 下方摘要支持上拉展开、下拉收起和点击/键盘切换。收起时保留GPS估算里程、有效记录时长和重要提示；展开滚动查看首次/末次定位和完整说明。模拟数据始终标识为模拟。
- 定位失效时暂停自动跟随。轨迹中断、异常跳点和许可切换处不连线；历史日期不叠加今日实时位置。
- 默认今日，可查看前一天、后一天。跨午夜时仅“今日”自动切日，手动选择的历史日保持。
- 暂时未同步或记录不完整以用户文案提示，不要求用户理解队列、映射或服务密钥。

11月9日UI改版参考Inspora上Divan Raj的 [sleep-tracking dashboard UI](https://www.inspora.design/posts/sleep-tracking-dashboard-ui)：白色单卡、紧凑日期、并排核心数字；保留DOSO天蓝色与深色主题。按后续截图反馈，摘要改为滑动卡片，移除起末点常驻文字，工具栏移出地图；自有控件统一使用AppText/MiSans，里程和时长统一字号、行高和字重。底图地名字体由地图服务提供。初版参考记录见 [20261109-ui](D:/DOSO/artifacts/robot-cloud/20261109-ui)，滑动版记录见 [20261109-track-sheet](D:/DOSO/artifacts/robot-cloud/20261109-track-sheet)。正式安装包另行发布。

## 一份历史，一个归属

```text
Rust cloud-link → MQTT → Rust Gateway → 业务 PostgreSQL 接受状态
                                      ↓ 同事务短期 outbox
                              Rust doso-cloud-telemetry
                                      ↓ sourceReport
                               ThingsBoard 历史库
                                      ↑ 归属/会话/epoch 校验
DOSO App ← HTTPS remote-history ← 业务历史查询接口
```

PostgreSQL 的 outbox 仅负责可靠投递；长期轨迹从 ThingsBoard 原始 `sourceReport` 读取，不另建轨迹事实表。用户关闭上报后，当前机主仍可读取自己的已有历史。换主与授权 epoch 变化不会把旧机主历史交给新机主。

地图使用 MapLibre GL 6.17.0 + OpenFreeMap。轨迹保持 WGS84，不混入高德 GCJ-02 坐标。首版没有改用高德 SDK，也未将街道地图当成机器狗可通行路线或运动控制输入。

## 代码入口

| 职责 | 文件 |
| --- | --- |
| 地图相机、轨迹线与标记 | [locationMapHtml.ts](<C:/Users/99563/Documents/ChatGPT/New project/doso-app/src/components/locationMapHtml.ts>) |
| 日期、地图和摘要卡 | [TrackPanel.tsx](<C:/Users/99563/Documents/ChatGPT/New project/doso-app/src/components/TrackPanel.tsx>) |
| 摘要高度、拖动和吸附 | [TrackSheet.tsx](<C:/Users/99563/Documents/ChatGPT/New project/doso-app/src/components/TrackSheet.tsx>) |
| 地图外部按钮与状态 | [MapBar.tsx](<C:/Users/99563/Documents/ChatGPT/New project/doso-app/src/components/MapBar.tsx>) |
| 去重、断段和估算 | [track.ts](<C:/Users/99563/Documents/ChatGPT/New project/doso-app/src/features/location/track.ts>) |
| App 历史请求与分页 | [remoteHistory.ts](<C:/Users/99563/Documents/ChatGPT/New project/doso-app/src/services/remoteHistory.ts>) |
| 业务历史接口与合同 | [handler.ts](<C:/Users/99563/Documents/ChatGPT/New project/doso-app/supabase/functions/remote-history/handler.ts>)、[API 合同](<C:/Users/99563/Documents/ChatGPT/New project/doso-app/docs/production/DOSO_REMOTE_HISTORY_API.md>) |
| Rust 历史投递 | [cloud/telemetry](../cloud/telemetry/README.md) |
| 连续模拟路线 | [uplink.rs](../tools/lab/src/uplink.rs) |

App 只在前台展开地图时每 10 秒读取历史，最新状态仍沿用原查询。每页500条，按时间升序读取，上限2000条；卡片以实际“记录时段”标明里程和时长范围，不显示截断条数文案。该上限仍存在，当前不能把已读时段称为全天或最新历史；长时间全天轨迹还需服务端降采样或分时段查询。

里程按有效点的距离估算，位移门槛取 3 米与已知精度的较大值；有效时长只累计连续定位覆盖。重复 fix 不重复累计，15 秒以上间隔和超过 15 m/s 的跳点断段。这些是当前低速机器人展示规则，不能替代轮式里程计、任务运行时间或真实 GNSS 标定。

## 本轮部署范围

测试业务 ECS 已安装 `remote-history`、`telemetry-export` 及两份数据库增量，原业务数据已备份；只有已登记模拟设备 `78ec077d-0cde-441b-bddc-022cfc850676` 开启本轮历史投递。发布入口见 [remote-history-20261109](<C:/Users/99563/Documents/ChatGPT/New project/doso-app/deploy/backend/remote-history-20261109/README.md>)。

Rust 投递程序在 ECS 临时服务运行。ThingsBoard 和其 PostgreSQL 仍使用开发机原有隔离实例，通过 SSH 私有回环通道供云端查询；公网不开放 ThingsBoard 端口。模拟器、本机 MQTT 和 DOSO Web App 也在开发机运行。这是实际 HTTPS 业务联调，尚不是独立于开发机的常驻历史产品部署。

开发机 Docker 平台重启后沿用原卷和账号；原记录保留，待发记录重试。不能把这种临时部署的可用性作为生产承诺。真机 GNSS、原生手机、真实 5G 和国内底图长期可达性另验；本轮不发送运动指令。

截图与接口证据保存在 [20261109-track-flow](D:/DOSO/artifacts/robot-cloud/20261109-track-flow)。具体结果以 `browser.json` 的实际步骤和 [TESTING.md](../TESTING.md) 为准。
