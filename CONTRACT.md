# Robot cloud read-only v1

Documentation reviewed on 2026-11-08. This document defines wire behavior and
explicitly marked product targets. Current delivery status is maintained in
[PROGRESS.md](PROGRESS.md); test and deployment evidence is indexed in
[TESTING.md](TESTING.md). A review date does not establish a new deployment.

## 客户数据与告警责任

### 设备、云端与本地 SDK 的权限边界（2026-11-12 确认）

设备凭据仅用于该设备获准的上报、请求应答与执行确认，不授予云端设备归属、用户授权、告警规则、平台配置或其他设备的管理权。云端服务凭据、管理 Bearer、平台映射及投递队列元数据不进入设备程序或客户公开响应。

云端禁止运动控制：不得通过云状态链、通用 RPC、代理或隧道转发站坐、速度、电机使能、关节目标及控制器切换。已有云状态请求仅允许严格定义的 `status.read`，机端固定调用 `GetControlStatus`；媒体观看沿用独立许可，不能附带运动方法。合法云端签名或服务凭据也不改变该范围。本地 Rust SDK 保留运动、导航与外部控制器接口，继续经过机端控制权、就绪和安全链。

普通客户获得设备身份、业务状态、已授权位置/历史和自身操作结果；不公开平台设备映射、归属版本、许可版本、投递队列计数、内部端点与运行配置。分页一致性、请求结果核查及机端并发切换所需的不透明标识是协议用途，不作为客户设置或界面信息。删除技术元数据时必须保留实际并发校验、断段、取消及未知提交语义。

机器人连接 IP、GNSS 位置、连接/活动异常的产品数据源与实施顺序见
[机器人云端管理](docs/MONITORING.md)。IP 指接入端观察到的网络出口，不推断地理位置；
账号登录 IP 与机器人连接 IP 分别处理。该说明不扩大现有采集许可或人员可见范围。

### 账号登录 IP 保存与本人查看（2026-11-12 补充）

用户明确本地 App 登录的账号也可查看自身登录 IP。登录 IP 是账号安全信息，不应一律归为客户不可见的内部数据。云端保存可信认证来源给出的登录 IP、登录时间和必要终端关联，允许已认证账号只读查询自己的记录；普通用户不能提交、修改 IP 或读取他人记录。

登录来源由认证服务及已核验代理链确认，客户端自报 IP 或未清洗的转发头不作为事实。来源未知时保持未记录，不填写猜测地址或地点。首次登录、令牌续期和最近活动分别表达；退出或撤销终端不应抹掉已经保存的登录安全记录，保留和账号删除遵循现有账号数据规则。

账号登录 IP、机器人接入云端的出口 IP、平台内部地址是不同事实，分别命名和授权。查看本人登录 IP 不授予云管理、远程运动控制或机器人采集许可，不自动开启机器人云连接；本功能不新增安全邮件或第三方 IP 定位。

2026-11-08补充产品边界。以下是用户提出的数据责任要求及建议的实施原则，**不代表现有代码已经实现字段授权、员工隔离或售后限时访问**。下面的协议章节继续描述实际wire行为。

DOSO负责产品能力、硬件本地保护和托管平台运行；客户决定自身业务规则、数据去向、通知对象和成员权限。提供告警引擎或模板，不赋予DOSO人员查看客户业务告警的默认权限。具体设备的低电量、故障、离线时间也可能反映客户业务，不能默认视为厂商运维数据。

### 首版建议

首版推荐允许客户选择DOSO托管服务，但将云服务按客户授权自动处理与DOSO人员查看分开。客户保留本地使用方式；不因购买、绑定或打开App自动开启云。客户启用某项云功能时说明用途、数据范围和是否保存历史，按功能授权，不让客户逐条配置协议字段。位置、视频、地图及售后分享在使用相应功能时另行明确范围。

告警引擎及通用模板由DOSO提供，客户选择启用规则、业务阈值和接收人；不默认向DOSO发送告警副本。机端硬件保护持续在本地执行。DOSO运维主要处理平台自身的错误、延迟、容量和积压，不能凭技术管理员身份把客户告警作为日常运营数据。

售后首版优先使用客户主动导出、预览并分享的诊断包，限定设备、时段与必要字段；位置、视频、地图和任务正文不默认包含。暂不为此建立常驻远程查看通道。后续确有远程售后需求，再实现客户授予的限时只读访问、撤销和审计。

对要求厂商技术上也不可读的客户，选择客户控制的本地/私有云处理方式，并取消DOSO常驻管理访问；不把托管服务的权限隔离宣传成这一能力。该交付方式尚未实现，不因本建议声称已支持现成的客户私有云安装包。

| 内容 | DOSO负责 | 客户决定 | 产品默认的人工作业可见范围（待实现约束） |
| --- | --- | --- | --- |
| 机器人本地安全保护 | 必要硬件保护、故障码和本地警示、固件维护 | 产品允许范围内的使用与配置 | 客户能在本地/App看到；保护不依赖云，不因此自动给DOSO上传现场内容 |
| DOSO平台运行告警 | 服务错误率、请求延迟、消息积压、存储/证书等运行维护 | 是否使用托管服务 | DOSO运维可看维护所需指标；日志不默认记录原报告、位置、任务或告警正文 |
| 客户业务告警 | 引擎、可选模板、配置界面与投递能力 | 是否启用、阈值、适用设备、接收人和处置流程 | 客户授权成员可见；不默认抄送DOSO售后或管理后台 |
| 状态、位置、任务、地图、视频与历史 | 按约定范围执行采集、传输、保存和查询 | 哪些数据进入哪个环境、保留时间、谁可访问与导出/删除安排 | 服务自动处理与员工查看分别授权；DOSO员工不作为客户默认成员 |
| 售后诊断 | 处理客户发起的故障请求，使用被授权的必要资料 | 分享哪些设备/字段/时间段；是否授予限时查看 | 优先客户主动导出经检查的诊断包；需要远程查看时，限定人员、范围和期限并留痕 |
| 账号、设备登记与服务关系 | 认证、设备归属核对及必要的服务管理 | 客户成员/设备的授权操作 | 业务人员仅按职责访问必要登记信息；机器人ID、机器人版本、固件版本不自动附带现场轨迹 |

实施时保留三个清楚的概念：客户允许服务自动处理的数据与用途、客户成员的访问权限、客户授予售后的临时访问。允许云服务保存历史，不自动等于允许员工浏览；开启云连接，不自动授予视频、位置、原始日志、告警副本或研发训练用途。

告警的例子：电机过热由本地保护处理并提示机主；是否把诊断资料交给DOSO另行授权。巡检任务失败由客户规则通知客户指定的人。DOSO云数据库容量不足由平台运维处理，其通知不需要包含客户任务正文或现场图像。

### 托管服务与技术不可读的区别

若客户允许DOSO云自动处理所选数据，服务器需要读取参与规则计算的字段，但DOSO员工默认不得浏览业务内容；这一目标需要真实的人员身份、访问控制、审计和授权流程来落实。

若客户要求连DOSO主机/数据库管理员也不能读取，应让业务数据与规则在客户本地或客户控制的服务器执行，且DOSO没有常驻管理访问；客户独占密钥的加密存储也是可选设计，但普通云规则无法直接计算其不可解密的字段。仅使用TLS、磁盘加密、独立租户或托管在DOSO账号下的专属实例，都不能单独证明厂商技术不可读。首版采用上述托管权限分离建议，不引入密文云计算框架，也不宣称已实现客户持钥加密。

### 当前代码差距与实施顺序

现有状态/投递路径核对机主、epoch、许可版本、开关与账号有效性，限制普通客户跨设备访问和撤权后的投递。`Permission`仍是整套状态许可，没有位置/历史等数据类别范围。业务库保存可读的完整`Received`，worker写入完整`sourceReport`，其中可能含`location`；这不是仅隐藏看板字段就能改变的数据处理事实。

当前ThingsBoard的单一实验租户、`TENANT_ADMIN`技术账号和全局API key只完成模拟接入验证，不是生产客户/人员隔离体系。现有代码未实现DOSO员工默认禁读、售后临时授权与查看审计，也没有客户独占解密钥匙。

下一批真实客户数据接入前，先明确上传类别、用途和存放环境，并在机端采集/上传与服务端接收边界执行范围检查；历史库只能保存授权范围内的内容，不能无条件复制完整报告。随后补客户成员访问和最小售后分享路径；如选严格不可读，先调整数据与告警执行位置。当前试验制品不作为这些隐私目标已验收的证据，不因新增文档扩大任何人的访问权限。

## Cloud telemetry export

The separate `cloud/telemetry/` Rust process exports business-accepted snapshots
to ThingsBoard. It does not receive raw device MQTT, issue robot commands, change
user consent, or replace the existing App status API. The robot/Gateway wire
contract below is unchanged. Gateway's two-second latest projection yields
snapshot history, not every one-second source report.

The business migration `20261108020000_add_telemetry_export.sql` adds private
device/authorization-epoch mappings and a transactional outbox. An update trigger
records the complete accepted `Received` JSON only when latest and seq advance.
User disable or authority reset cancels pending records. Export is separately
enabled per enrolled device by a service-only configuration RPC and defaults off;
it never substitutes for the user's existing cloud consent.

`POST /functions/v1/telemetry-export` requires a dedicated named service key in
`apikey`. The default name in business `SUPABASE_SECRET_KEYS` is
`telemetry-export`; `DOSO_TELEMETRY_EXPORT_SECRET_NAME` can select the deployed name.
No user bearer, Gateway management key or device key is an export credential.
The JSON body is at most 4 KiB, has `v:1`, and rejects unknown fields/query strings.

| action | Request fields besides v/action | Response |
| --- | --- | --- |
| claim | none | `{v:1,items:[...]}`, zero or one item |
| check | id, leaseToken, tbDeviceId (UUID or null) | `{v:1,allowed:boolean}` |
| finish | id, leaseToken, outcome (delivered/retry/conflict) | `{v:1,accepted:boolean}` |

An item contains `id`, `leaseToken`, `deviceId`, `ownerId`, `authorizationEpoch`,
`consentVersion`, `tbDeviceId` (nullable), database-derived `simulated`, and
`received:{report,receivedAt}`. The claim lasts 120 seconds. Current owner, epoch,
consent version, enabled choice and account validity are rechecked before writing;
the original acquisition lease need not remain unexpired for accepted history.
Non-null tbDeviceId in check saves the mapping idempotently. Claim tokens fence
expired workers; one device/epoch has at most one outstanding delivery.

The pending queue is bounded to 10,000 records per device. Overflow increments
an observable dropped count; 20 failed attempts or a one-day backlog becomes
failed. Completed records do not consume pending capacity and are cleaned after
one day during subsequent ingestion. These bounds do not promise complete history
during an unbounded outage. A started third-party HTTP request can finish after a
concurrent close; stopping new acquisition is not retroactive deletion.

Worker configuration uses `DOSO_TELEMETRY_URL/KEY` and `DOSO_THINGSBOARD_URL/KEY`.
Keys stay server-side. HTTPS is required except literal loopback HTTP; redirects
are disabled, HTTP timeouts are three seconds and bodies are bounded to 128 KiB.
ThingsBoard device names are `doso:<device UUID>:<authorization epoch>`. The worker
uses its server API, preserves report observedAt as telemetry ts and the original
report as sourceReport, marks read failures explicitly, and rejects a conflicting
identity already present at the same timestamp instead of inventing a new time.
REST storage success is not evidence of robot connectivity or rule-engine alarms.

Implementation and deployment: [telemetry README](cloud/telemetry/README.md).
Business RPC/security contract: [telemetry-export README](<C:/Users/99563/Documents/ChatGPT/New project/doso-app/supabase/functions/telemetry-export/README.md>).

## On-demand video source

The trusted media service uses the existing Gateway service Bearer to call
`PUT /api/v1/robots/{deviceId}/video` with `{"enabled":true}` to start or renew
a source, and `{"enabled":false}` to stop it. No other fields are accepted.
The media service must first validate a live viewer grant. It renews every five
seconds while a viewer or a short initial warm-up is active, then sends stop
when the final viewer leaves. A warm-up must expire if no WHEP session follows.

The Gateway checks registered/enabled device identity, MQTT availability and a
recent device connection, then signs a `video.control` message on
`doso/v1/devices/{deviceId}/video/control`. A control contains `v`, `kind`,
`deviceId`, `connectionId`, `seq`, `enabled` and `expiresAt`. Its lifetime is at
most fifteen seconds. The device accepts only its current connection, a newer
sequence, a valid Gateway signature and an unexpired control. Video permission
is independent of status-reporting permission.

Success is HTTP 202 with `{"accepted":true,"enabled":true,"expiresAt":"..."}`
(or `enabled:false`). It confirms queuing only. Source readiness is confirmed
separately through MediaMTX `path.ready`, and playback through decoded frames.
Missing service authorization returns 401, disabled/unregistered devices 403,
invalid body 400, and unavailable MQTT/device connections 503. The Gateway never
accepts an executable, arguments, stream URL or viewer credential in this API.

The source program and arguments come from the protected local JSON file named
by `DOSO_VIDEO_CONFIG`. The program path must be absolute and it must directly
own the encoder process. The device starts at most one source per MQTT session,
renews its local monotonic deadline, and kills/reaps it on stop, lease expiry,
local cloud close, MQTT disconnect or process shutdown. Reconnection needs a new
valid control and does not resume old video. No video frames are sent over MQTT.
Source termination is polled without blocking status sampling. A failed stop is
logged, retains its process handle for retry and prevents another source from
starting. Session-exit cleanup waits at most one second; a cleanup error does
not convert the status connection into a permanent configuration failure.

Gateway, Broker and HTTPS have deployment records. On 2026-11-07 an isolated
Gateway, real business database and actual DOSO Web App exercised registered
simulated-device binding, reporting, background renewal and stop. The device
source was simulated and MQTT used a local broker plus SSH tunnel. Robot
deployment and real robot acceptance remain pending. This contract reads
Brainstem `GetControlStatus`; it cannot issue motion or acquire control.

## Programs and authority

- `robot/`: robot Linux cloud-link process; outbound MQTT, local gRPC read, signed reply.
- `cloud/gateway/`: ECS process; authenticated internal HTTP to MQTT request/reply.
- `cloud/broker/`: ECS Mosquitto templates; mTLS identities and exact topic ACLs.
- `shared/`: wire validation and client connection configuration used by both.
- `cloud/media/`: cloud-side video signaling and viewer-session candidate; deployment and acceptance scope are documented in its README.
- Cloud link `src/grpc.rs`: read-only protobuf projection of the existing Brainstem
  API, calling only `GetControlStatus`. The deployed binary needs neither the
  Node bridge nor a runtime proto directory.
- `tools/lab/`: Rust-only local checks and public simulation; never deployed to a robot or ECS.
- `tools/deploy/`: deployment preparation, installation scripts and historical records; scripts operate on their documented release-package layouts.

The App must call a DOSO backend endpoint using its existing user session. That
backend must check device ownership and remote-management consent before calling
the internal gateway. That integration is implemented in the separate App/business
project, with simulated-device Web evidence in [SIMULATED_FLOW.md](docs/SIMULATED_FLOW.md).
Never distribute the gateway service credential to an App or robot.

The cloud link defaults to disabled and reads no robot data until explicitly enabled
locally. `DOSO_CLOUD_ON` is the startup setting; when it is absent, the legacy
`DOSO_REMOTE_ENABLED` setting is accepted. If both exist, the new setting wins,
including when its value is invalid. Only the exact value `true` enables startup.
The existing `/etc/doso/robot-cloud/agent/agent.env` path and `DOSO_DEVICE_ID`
remain unchanged because LingTu also reads them. Gateway registration also defaults
to disabled. These installation
switches are not the customer consent UI. Periodic reads additionally require
the short reporting permission below. There is no offline status backlog or inferred battery reading.

## Product target: local cloud connection switch (partially implemented)

The user selected complete disconnection. The cloud connection is per device
and off by default. While off,
the device must not connect to MQTT, send hello messages, or upload status. A
locally authorized user may enable it; the choice must survive a device restart.
An absent, unreadable or invalid local choice must fail closed. Binding or
opening the App must not enable it implicitly.

The link now additionally requires `DOSO_CLOUD_CHOICE`, defaulting to
`/var/lib/doso-cloud/choice`, to identify a protected regular file containing
exactly `true` or `true` followed by LF. The installation environment gate alone
cannot enable a connection. Missing, unreadable, symlinked or invalid files mean
off; Linux also rejects a group- or world-writable file. Its parent directory
must only permit the trusted local authorization component to replace the file.
The saved choice is read again on startup; while running, off is detected within
250 ms and drops the active read, MQTT session and reconnect loop. Network errors
and expired reporting permissions do not rewrite this local choice. Local lab
fixtures create their own explicitly simulated choice file.

This file is the local persistence input, not proof of account ownership. The
research robot has a local owner-authorized writer (`doso-robot-authorizer` on
`127.0.0.1:13148`) that validates a protected bearer token, writes the choice
atomically, and starts or stops only the fixed cloud-link service. The product
App wiring and real-device enable/telemetry acceptance are still pending. Do
not present a manually written `true` value as a completed customer flow.

Remote status upload additionally requires the existing account/device consent
and a confirmed reporting permission. A network outage while enabled must not
change the user's saved choice; reconnection may resume only under the still-valid
local choice and backend permission.

Single status queries must be limited by the user's active permission;
possession of a gateway service credential alone must not imply user consent.
The gateway accepts a single query only while the device has confirmed a current
unexpired reporting permission. The device also requires its current permission,
binds the read to that session, and cancels it if permission changes or expires.
The gateway checks again when the reply arrives. This rule is implemented and
verified locally; installation of a particular server version must be checked
against its deployment record.

On remote disable, the business backend immediately rejects new data. The product
target requires a device that receives the disable to stop reads, persist its
local off choice and disconnect. This remote persistence/disconnect is not yet
implemented: the current `reporting.control` message only describes reporting
permission and does not distinguish an explicit user cloud disconnect from a
default disabled permission or revocation. Do not latch these into permanent off
without a distinct authenticated intent. Device stop acknowledgement and observed disconnect are distinct:
an acknowledgement sent over MQTT cannot by itself prove a later disconnect. If
the device is unreachable, the App must show the device action as unconfirmed.

Once disconnected, the same cloud connection cannot remotely enable itself.
Re-enabling requires an authorized local channel, such as a paired G20. That
channel, its authorization and product service integration do not yet
exist; only the local durable-choice input and stop behavior above exist. Do not expose an unauthenticated LAN switch or treat the startup
environment variable as a user-facing control. The current implementation
below can still send hello while remote status reporting is off.

Local robot control authority alone does not establish cloud ownership: a
borrower may legitimately use the robot locally. The local switch must verify
the owner's or an explicitly delegated manager's authority independently.
The LAN development peer allowlist and development session label are not proof
of ownership and must not authorize production cloud-management changes.

## Transport

### GNSS location extension (2026-11-08)

Successful `status` may additionally contain `location`. Older senders omit it;
receivers must continue accepting the original three status fields. When present,
location has exactly `lat`, `lon`, `fixType`, `observedAt`, and `accuracyM`.
Coordinates are WGS84 degrees (`lat` -90..90, `lon` -180..180), never SLAM X/Y.
`fixType` is the existing NMEA/LingTu code 1..8; 0 means no usable fix and must
not be sent as a location. `accuracyM` is a finite nonnegative measured horizontal
uncertainty in metres (at most 100000) or null when unknown; do not turn HDOP or a nominal fix-type
accuracy into a claimed measurement. `observedAt` is the original fix timestamp
in the existing UTC timestamp format, independent of control-state read time.
The inventory's immutable simulated flag remains the authority for simulation.

The existing GNSS serial owner atomically writes its status snapshot with an
optional `location` object containing `lat`, `lon`, `fix_type`, `ts` (Unix
seconds), and `accuracy_m`. Invalid fixes, serial timeouts and errors clear the
location; refreshing diagnostics must not refresh the fix timestamp. The default
file is `/dev/shm/lingtu/gnss_status.json`, overridden by `DOSO_GNSS_STATUS`.
Cloud link attaches only a valid location at most five seconds old (allowing the
existing two-second future clock tolerance). Missing, malformed, stale or
unavailable GNSS omits location and does not suppress the control-state report.
Location reads happen only within the existing authorized status read. No new
permission channel, location history or motion behavior is introduced.

Business validation and storage preserve this optional object. App evaluates
location freshness using its own `observedAt`, labels simulated inventory,
and shows unavailable/expired location explicitly. New control-state reports
must not make an old fix appear current. Map rendering receives coordinates and
freshness only; it receives no Auth token or Gateway credential.

The cloud link reads Brainstem through the dedicated local status endpoint at
`http://127.0.0.1:13146`, using only
`/brainstem.api.v1.RobotControl/GetControlStatus`. Brainstem enables this
loopback-only listener with `THUNDER_LOCAL_STATUS_ENABLED=true`; it registers
only the status method. Other RPCs return `UNIMPLEMENTED`. The existing LAN or
mTLS control listener remains a separate endpoint.

This endpoint alignment is a deployment candidate. The corresponding Brainstem
listener and updated cloud link must be installed together before robot acceptance.

MQTT 5 over verified mTLS, clean sessions, QoS 1 and `retain=false`. A device's
certificate identity is its immutable UUID; ACLs bind it to its own topics:

```
doso/v1/devices/{deviceId}/status/requests   gateway writes; device reads
doso/v1/devices/{deviceId}/status/replies    device writes; gateway reads
```

Each message is an Ed25519-signed envelope:
`{"payload":"<base64url UTF-8 JSON>","signature":"<base64url signature>"}`.
Verify the original decoded bytes before parsing. Gateway and devices use distinct
signing identities in addition to their distinct TLS credentials. Maximum envelope
size is 8192 bytes. Payloads are limited to the fields below.

Request:
`{v:1,kind:"status.read",deviceId,requestId,issuedAt,expiresAt}`.
IDs are UUIDs, timestamps are ISO UTC, and lifetime is at most 10 seconds. The
cloud link rejects expired requests and duplicate request IDs during their lifetime within the current process,
and rechecks expiry before sending. Environment configuration is read at startup;
the durable local choice is also monitored while connecting and running.
Local off cancels the active read and exits the link; re-enabling requires the
planned authorized local writer and service-start integration.
Each request authorizes only one read under the current confirmed reporting
permission; it does not establish or renew user consent. No commands are retained
or queued deliberately.

Success reply:
`{v:1,kind:"status.result",deviceId,requestId,observedAt,status}`.
`status` contains exactly:

```
state: {kind, transition}
control: {owner, motorOutputEnabled, criticalMotorFault}
transport: "local-insecure" | "mtls"
```

`transport` describes the cloud link's local gRPC hop, not the encrypted MQTT hop.
For a failed local read, replace `status` with `error:"upstream_unavailable"`.
There is no battery, sensor-health, motion permission or invented online flag.
Gateway verifies the registered device's signature and the outstanding request ID;
it accepts each response once and never returns a cached reply as a fresh read.
Timeout or disconnect means unavailable, not proof of physical robot shutdown.

## Internal HTTP

`POST /api/v1/robots/{deviceId}/status`, empty body, service Bearer credential.
The gateway listens only on `127.0.0.1:8130`; Nginx exposes its authenticated query over HTTPS.
Success is the snapshot read by the cloud link with deviceId,
requestId and observedAt. Errors are fixed codes, not raw upstream messages.

## Public HTTPS

The trusted-backend endpoint is `POST https://8.153.169.177/api/v1/robots/{deviceId}/status`.
Nginx terminates TLS on 443 and forwards the same empty-body request to the loopback Gateway;
the service Bearer remains required. Clients verify the server using the provided CA certificate.
This endpoint is for backend services and developer tools, not a replacement for App user/owner/consent checks.
The service credential must never be embedded in the customer App. See `cloud/gateway/examples/query.rs`
for the Rust caller and `tools/deploy/https.sh` for installation.

The existing `DOSO_ROBOT_COMMAND_DELIVERY_ENABLED` remains false. App/backend
consent, robot deployment and real robot status acceptance remain separate work.
Public MQTT transport acceptance is recorded in tools/deploy/RESULT.md; it does not establish those business outcomes.

## Reporting extension

Status and the existing POST status response stay unchanged. Business Auth/DB is
the owner of device ownership, robot_authorization_epoch and consentVersion.
Gateway keeps only a persisted permission fence and an in-memory latest-state
projection. The customer backend must recheck ownership, epoch, consent and
session in the transaction that commits a report. A committed user disable must
reject old reports there even if its notification to Gateway fails.

New device topics (same signed envelope, no retain):

| Suffix under doso/v1/devices/{deviceId}/ | Direction |
| --- | --- |
| reporting/hello | Cloud Link -> Gateway |
| reporting/control | Gateway -> Cloud Link |
| reporting/acks | Cloud Link -> Gateway |
| status/reports | Cloud Link -> Gateway |

All new messages use v=1. Positive epoch/version/seq values are JSON-safe integers.
The cloud link creates a fresh connectionId UUID on each MQTT connection and announces
it every 5 seconds with a signed reporting.hello. Hello has deviceId,
connectionId and observedAt; it contains no Brainstem state. It supports control
delivery and recent-connection visibility while periodic collection is disabled.
Both the DOSO_CLOUD_ON installation gate (or its legacy fallback) and the durable
local choice are required for the cloud link to run. A reporting stop alone does
not currently persist local off or disconnect MQTT.

PUT /api/v1/robots/{deviceId}/reporting accepts a JSON Permission, using the
same trusted service Bearer as POST/status. Body limit is 2048 bytes:

```json
{"authorizationEpoch":1,"consentVersion":1,"enabled":true,"connectionId":"22222222-2222-4222-8222-222222222222","reportingSessionId":"33333333-3333-4333-8333-333333333333","expiresAt":"2026-11-03T12:00:30.000Z"}
```

The business backend allocates reportingSessionId and reads connectionId from
the latest endpoint. expiresAt must be in the future and at most 30 seconds away.
The backend renews explicitly while user consent remains valid. Equal versions
may renew the same enabled decision; they cannot change enabled. Versions compare
lexicographically by (authorizationEpoch, consentVersion). A new session is needed
after cloud link reconnection or expired permission; sequence numbers never reset on
renewal of an active session. Each session binds the exact epoch, consent version
and connection. Changing that binding requires a new session. An active session
cannot be replaced under the same permission version; expired sessions cannot be renewed.

Disable increments the consent version and sets enabled=false. Its connectionId
and reportingSessionId may be null, so it works when the dog is unreachable:

```json
{"authorizationEpoch":1,"consentVersion":2,"enabled":false,"connectionId":null,"reportingSessionId":null,"expiresAt":"2026-11-03T12:00:30.000Z"}
```

Gateway durably stores (epoch, version, enabled) before applying a change; old or
conflicting decisions return 409 stale_permission. A failed write does not accept
the decision. New grants need a recent matching hello; unavailable connections
return 503. A disable immediately rejects new reports and pending status results,
then sends stop when the current connection is known. HTTP 202 means accepted,
not device-confirmed. The response uses the same projection as GET/latest.

The persisted fence file is DOSO_REPORTING_STATE (default reporting.json beside
the device registry). It contains neither user accounts nor live leases. The
service configuration reserves a writable state directory for this persisted
permission fence. It does not persist a live lease across restart.

The reporting/control message contains deviceId and the complete Permission in
its permission field. Gateway binds the control to the current connectionId.
The cloud link accepts only the Gateway signature, its own device/connection, a current
deadline and a non-regressing version. It returns reporting.ack with deviceId,
permission (exact accepted value) and observedAt. An ack cannot restore a revoked
version. Saving a user preference, accepting the API request and receiving this
ack are separate states.

After enabling, the cloud link samples GetControlStatus at most once per second, sharing
one local read with on-demand queries. Busy ticks are skipped. Every status.report
contains deviceId, connectionId, reportingSessionId, authorizationEpoch,
consentVersion, increasing seq and observedAt, plus exactly one of status or
error=upstream_unavailable. It never stamps old state with a new collection time.
Reports use QoS 1 and Gateway rejects duplicate/out-of-order seq. The cloud link bounds
pending report work and drops the MQTT queue on reconnect; no offline backfill.
Revocation/expiry cancels periodic reads. Data already submitted to MQTT may be
in flight and must be rejected by Gateway/backend after disable.

GET /api/v1/robots/{deviceId}/status/latest is a cache read (no new gRPC request).
It returns deviceId, connectionId, lastSeenAt, reachable, permission, latest,
lastSuccess and fresh. permission is null before a decision; otherwise it has
authorizationEpoch, consentVersion, desiredEnabled, deviceConfirmed,
reportingSessionId and expiresAt. latest/lastSuccess are null or
{report: status.report, receivedAt}. Last successful state retains its original
times when a later read fails. fresh requires a current confirmed permission and
a successful latest read observed/received within 5 seconds. reachable means a
verified hello/report within 15 seconds while Gateway is connected; it does not
prove physical power state. Changing authorizationEpoch clears old-owner state.

On Gateway restart/disconnect no live permission is restored; the persisted
fence prevents an old enable from undoing a newer disable. Cloud link reconnect makes
a new connectionId, clears the active permission and asks for a new grant before
periodic reading. Disable awaiting device ack is still pending confirmation;
expiry bounds collection but is not an ack. POST/status requires a current,
confirmed, unexpired permission both before dispatch and when accepting a reply.
An expired periodic lease does not invent a new user decision, but it blocks
single reads until a new permission is confirmed.

The App/backend implementation, state database commit and user-facing consent
UI remain owned by the business-side project. This extension does not grant
motion, map upload/download or media permissions. Its simulated-device Web
validation does not establish robot deployment or complete cloud disconnection.

## 用户体验与自动职责（业务侧接线标准）

本节说明已有远程状态许可流程。App关闭后的停采已在模拟设备Web流程中验证；产品要求的持久化断云及本地重新开启仍按上文“Product target”推进，不能将两者都显示成已完成的云连接控制。

用户只需为每台设备选择是否开启远程状态，默认关闭。开启说明只描述实际上传的状态范围；视频、地图和媒体仍需各自操作。短期采集许可不是需要用户反复确认或付费的“续期”。

| 环节 | 用户能感知 | 系统自动完成 |
| --- | --- | --- |
| 选择开启 | 操作开关，显示开启中 | App 提交已有用户身份；后端核对归属并保存用户期望与版本 |
| 等待设备 | 显示等待设备确认 | 后端分配会话、发送短期许可；云连接服务验证并确认，202 只表示受理 |
| 持续运行 | 查看实际状态和更新时间 | 后端在用户许可仍有效时续期；云连接服务采样签名；Gateway 核验并更新缓存；后端事务提交最新状态 |
| 查看状态 | 数据可用、读取失败或已过期 | App 自动订阅/读取业务后端的最新状态，不直接持有 Gateway 服务凭据 |
| 选择关闭 | 显示关闭待设备确认，确认后显示已关闭 | 后端先保存关闭并立即拒旧数据，通知 Gateway 和云连接服务停采；未确认不能显示设备已执行 |

用户选择以业务数据库为准，Gateway 的 permission 是执行投影。授权版本、connectionId、reportingSessionId、seq、30 秒截止时间和重试策略都不作为用户设置。App 退出或切后台不等于用户关闭；在持续远程状态这一产品模式下，后端负责续期，用户使用开关停止。无法核实用户许可时不得续期。

界面分别表达“远程状态是否开启”和“这份数据是否可用”：

- 保存成功但设备未确认：开启中／等待设备确认。
- 设备已确认且许可有效：已开启；数据尚未到达时单独显示等待数据。
- latest.error 存在：状态读取失败；lastSuccess 如需展示，必须保留原始时间并标注历史数据。
- fresh=false 或连接暂不可用：数据已过期／连接暂不可用，不推断机器狗物理关机。
- 用户已关闭但未收到设备确认：云端已停收、等待设备确认。许可到期不冒充设备 ACK。

后台自动同步在本工程Gateway中实现，用户许可事务、状态入库和App展示在独立业务工程实现。两边按下述同一合同对接；11月7日已有真实账号、业务数据库及实际Web App配合模拟设备的演练。真实机器狗、正式手机/G20及完整云连接开关仍需各自验收。

## Automatic business sync v1

Gateway may be configured with DOSO_REMOTE_SYNC_URL and DOSO_REMOTE_SYNC_KEY.
The URL is the business POST /functions/v1/remote-state-sync endpoint. Requests
use the dedicated named service key in the apikey header, not a user session or
the Gateway management Bearer. HTTPS is required except literal loopback HTTP
for the local lab. Redirects are disabled. Without both settings no sync task is
started; partial configuration is an error.

A single task runs every 2 seconds with skipped overdue ticks, at most one
outstanding HTTP request, 32 devices per batch and a 128 KiB body limit. Registry
device IDs remain the enrollment source. Network, authentication, format or
business failures do not mint new permissions locally; existing leases expire.

Request:

```json
{"v":1,"observedAt":"2026-11-03T12:00:00.000Z","devices":[{"deviceId":"11111111-1111-4111-8111-111111111111","connectionId":null,"lastSeenAt":null,"reachable":false,"permission":null,"latest":null,"sessionReusable":false}]}
```

permission and latest have exactly the existing GET/latest projection shapes.
lastSuccess/fresh are not sent: the DB retains only successful state that it has
accepted and computes display freshness from the original timestamps.
sessionReusable reports whether Gateway still accepts renewal of its projected
session, independent of whether that permission has been acknowledged. This is
execution evidence, not a user setting or consent source.

Response:

```json
{"v":1,"permissions":[{"deviceId":"11111111-1111-4111-8111-111111111111","permission":null}]}
```

Every item refers to a requested device, without duplicates. null/missing means
no permission to apply, never an implicit enable. Non-null values use the existing
Permission type. Gateway reuses set_reporting and skips an already-confirmed,
identical complete permission; pending confirmation retries the same value.
One device's rejected grant does not prevent applying other items.

The business DB transaction checks current owner/account, authorization epoch,
user consent/version, reporting session, connection, increasing seq and report
times before saving latest state or issuing a permission. Batch observedAt must
be no more than 15 seconds old or 2 seconds in the future; a per-device watermark
rejects older batches so stale connections cannot overwrite newer observations.
Duplicate report seq must not refresh observedAt/receivedAt.

Renewal extends to 30 seconds only when the currently issued lease has at most
15 seconds left and the matching permission has deviceConfirmed=true with a still
valid confirmed deadline. Matching includes the issued expiresAt, not only the
session ID. If it has not been confirmed, the backend retries that same complete
permission without extending its deadline; failed ACKs cannot prolong it forever. A
matching sessionReusable=false freezes renewal; with the same connection the DB
waits until its maximum previously issued expiresAt before allocating a fresh
session. A changed connectionId permits a fresh session immediately. This avoids
both Gateway 409 loops and cloud link rejection of a replacement while its old lease
is still valid. Epoch/version remain business decisions, never Rust retry counters.

A user's desired=true with no current connection or while waiting for session
expiry produces null, not enabled=false under the same consent version. An actual
user disable or ownership reset produces the corresponding new disabled decision
and immediately prevents old report commits. Default-off, never-granted devices
display "未开启"; pending-close is reserved for a previously issued collection
permission whose stop has not been confirmed. The App never keeps this loop alive.

### 登录 IP API 的源码与发布边界（2026-11-12）

App 真源位于 `C:\Users\99563\Documents\ChatGPT\New project\doso-app`。登录 IP 已在源码中接入：迁移 `supabase/migrations/20261112020000_add_login_history.sql` 的 `capture_account_login` 从已验证的 `auth.sessions` 读取当前 `session_id` 对应的 `created_at` 与 `ip`，以幂等的 `account.login` 审计记录保存；`account-sessions` 的 `capture_login` 和 `list_login_history` 只允许当前认证账号使用。查询最多返回 50 条 `{id, loggedInAt, ipAddress, method}`，未知 IP 为 `null`，客户端不能提交 IP 或账号标识。完整字段与调用方式见 App [账号登录 IP 自查 API](<C:/Users/99563/Documents/ChatGPT/New project/doso-app/docs/production/DOSO_ACCOUNT_LOGIN_IP_API.md>)。

本段源码和本地回归已完成，但迁移、匹配的 Edge Function、可信代理修正和真实账号登录读回尚未部署/验收。认证 IP 只有在反向代理覆盖伪造的 `X-Forwarded-For`、限制绕过入口并由 Auth 会话写入后才可作为可信来源；来源不明保持 `null`。账号登录 IP 永不代替 Broker 看到的机器人 MQTT socket peer，二者不共用字段、权限或查询接口。
