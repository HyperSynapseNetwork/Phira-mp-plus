# PMP 功能总览

> 以下内容基于代码逐项梳理 PMP（Phira-mp-plus）在官方 [Phira-mp](https://github.com/TeamFlos/phira-mp) 基础上新增的**所有功能与行为**。
> 官方 Phira-mp 是基础多人游戏服务器（TCP 协议 + 房间 + 游玩轮次）。以下所有条目均为 PMP 新增或增强。

---

## 一、协议命令（`phira-mp-common/src/command.rs`）

### 1.1 ClientCommand（客户端 → 服务器）
- **Ping**：心跳，服务器回 `Pong`
- **Authenticate { token: Varchar<32> }**：Phira 认证 token 登录（`/me` 校验 + WAL 持久化 + 认证屏障）
- **Chat { message: Varchar<200> }**：房间聊天，200 字符上限
- **Touches { frames }**：高频触控帧（fire-and-forget，不回复），坐标 f16 压缩
- **Judges { judges }**：判定事件流（Perfect/Good/Bad/Miss/HoldPerfect/HoldGood）
- **CreateRoom { id: RoomId }**：建房（20 字符，字母数字 + `-` `_`）
- **JoinRoom { id, monitor }**：加入房间；`monitor=true` 需专属 monitor 认证
- **LeaveRoom / LockRoom / CycleRoom**：离开 / 锁定 / 循环（后两者仅房主）
- **SelectChart { id }**：选谱（仅房主，需 SelectChart 状态）
- **RequestStart**：房主请求开赛（进入 WaitForReady）
- **Ready / CancelReady**：准备 / 取消准备
- **Played { id }**：提交游玩成绩（服务器按 record id 从 Phira API 拉取真实成绩并校验 player）
- **Abort**：对局弃权
- **ConsoleAuthenticate**：控制台客户端认证（权限受限）
- **RoomMonitorAuthenticate { key }**：房间 monitor 认证（派生密钥）
- **QueryRoomInfo**：查询全服房间列表（monitor/console 专用）
- **GameMonitorAuthenticate**：游戏 monitor 认证（负 user id 标识，可旁观看）

### 1.2 ServerCommand（服务器 → 客户端）
- `Pong`、`Authenticate`、`Chat`、`Touches`、`Judges`、`Message`、`ChangeState`、`ChangeHost`
- `CreateRoom/JoinRoom/OnJoinRoom/LeaveRoom/LockRoom/CycleRoom/SelectChart/RequestStart/Ready/CancelReady/Played/Abort`
- `RoomResponse`（房间 monitor 快照）、`RoomEvent`、`UserVisit`（通知 monitor 新用户上线）

### 1.3 Message（房间广播语义事件）
- `Chat`、`CreateRoom`、`JoinRoom`、`LeaveRoom`、`NewHost`、`SelectChart`、`GameStart`、`Ready`、`CancelReady`、`CancelGame`、`StartPlaying`、`Played`、`GameEnd`、`Abort`、`LockRoom`、`CycleRoom`

### 1.4 RoomState / RoomEvent
- `RoomState`：SelectChart / WaitingForReady / Playing
- `RoomEvent`：CreateRoom / UpdateRoom / JoinRoom / LeaveRoom / **PlayerScore / StartRound**（对应 SSE 事件名）

---

## 二、房间 / 游玩语义

- **建房**：`max_rooms` 上限、`room_creation_enabled` 动态开关、ID 占用校验；房间 UUID；建房后异步扩展工作（事件、历史、插件）
- **加入**：会话类别区分（Normal/Console/Monitor，防绕过）；锁定/黑名单/满员/游戏状态校验
- **Playing 状态下加入**：首次提示「进行中加入确认」，确认后 `late_join` 加入进行中对局（异步 abort 旧对局）
- **游玩进度通知**：加入游玩中房间后立即推送一次 `游玩进程:[进度条]XX%    还剩X.X分钟`（进度条 20 格 + 百分比 + 剩余分钟，按谱面时长 + 游玩开始时间计算），之后每 30 秒推送一次，直到轮次结束或用户离开房间
- **加入后**：广播 OnJoinRoom + JoinRoom → JoinRoom(Ok)（完整快照+live）→ 回放聊天历史 → ChangeHost 补偿
- **离开**：房主离开随机转移；广播 LeaveRoom
- **锁定/循环**：仅房主，全员广播
- **选谱**：仅房主；从 Phira API 拉谱面元数据（谱面名/谱师/曲师/难度/评分/更新日期，失败回退 `#id`）；HTTP Range 下载 zip 内音频文件，lofty 探针解析谱面时长（MP3/FLAC/WAV/OGG）——**谱面时时可能更新，不长期缓存**：每次选谱解析、写入房间级 `chart_duration`，结算后释放，下轮重新解析；该时长用于**对局超时**计算（见下），未解析到时回退 10000s（≈2.8h，避免误导）；**选谱后广播谱面信息**（`谱师:... 曲师:... 难度:AT Lv.15 评分:0.918 谱面更新:...`）
- **开赛**：`RequestStart`（房主）、管理员强开、`CancelStart`；全员 Ready / 强开 → Playing
- **准备倒计时**（默认 60s）：超时自动开赛，未 Ready 标记 aborted
- **对局超时**（默认 `playing_timeout_offset_secs`=60）：基础 deadline = **谱面时长 + offset**（时长来自选谱缓存，未命中回退 10000s，避免提前强制放弃）；开赛后首个完成者将 deadline **顺延一个 offset**（给其余玩家追赶）；无论是否有人完成，到达 deadline 即 `force_end_playing`：**所有未完成且未 abort 的玩家标记 aborted → 结算**——即无人完成时到点全体 aborted 结算；该 deadline 由正常开赛（`set_playing_deadline`）与倒计时强开（`force_start_playing`）两条路径各自计算一次（双冗余）
- **结算**：全员完成/abort → WAL 持久化 RoundCompleted → 广播 StartRound + **本地化结算排行**（成绩/准确率/std/FC/判定/弃权）→ GameEnd → 回 SelectChart
- **循环模式**：整局结束房主按用户列表轮转
- **房主管理**：`SetHost`、首个非 monitor 加入者成为房主、系统房主（host=-1）
- **监视者**：房间 monitor（接收 RoomEvent/UserVisit）、游戏 monitor（实时 Touches/Judges）；绕过锁定/人数/状态门槛
- **隐藏房间**：`-` 前缀默认隐藏、`SetHidden` 动态切换；不出现在公开列表
- **持久空房间**：`create_empty_room`、`set_persistent_empty`；快照 debounce 500ms 持久化；启动恢复
- **赛事模式**（房间级配置 `tournament`，非全局）：开启后禁用 PMP 默认交互行为——准备倒计时自动开赛、每轮结算广播、房主自动转移、cycle 自动轮换、Playing 期 late-join 确认、聊天，全部交由 PPB 编排（PPB 经 OpenUDS `room.set_tournament` 设置）
- **force-move**：绕过限制强移用户（RemoveUser→AddUser→广播→JoinRoom(Ok) critical flush→房主指派），失败回滚
- **踢人/关房**：`KickUser`、`CloseRoom`
- **degraded 标志**：Join 补偿失败置 true，阻塞新加入，CLI 清空
- **显示名**：后台从 Phira `/me` 刷新（并发闸门 8）
- **聊天历史**：内存保留最近 50 条，新成员回放
- **Ghost member 防护**：Join 中断派发补偿 remove_user；原子 BindAndSnapshot + room_seq cutover

---

## 三、会话 / 认证

- **认证状态机**：Authenticating → WAL 入队 → DurableAccepted → Auth(Ok) flush → ResponseFlushed → Gate 激活 → Active；每步失败回滚（清绑定/移除用户/补发补偿/关传输）
- **认证预算**（默认 5000ms）：覆盖 API + 重试 + 退避 + WAL + 最低响应时延 + flush
- **token 认证缓存**：SHA256(token) 缓存（上限 4096，LRU，重启恢复）；缓存命中跳过 `/me`
- **IP 封禁检查**：认证路径发 `Authenticate(Err(reason))`（不静默断连）
- **重连**：旧 Session + 代际捕获，新会话 Active 后才关旧连接（两阶段交接）；失败原子恢复旧绑定
- **代际**：`SessionBinding{generation, session}` 原子；`CommandOrigin` 绑定所有响应到发起会话，重连后旧命令不投递
- **断线**：`dangle_grace_secs` 宽限恢复；Playing 独立 `playing_reconnect_grace_secs`；代际守卫防竞态
- **每会话出站队列 + 认证屏障**：认证帧 flush 前缓冲（有界 256 事件/1MiB/8s）；cutover 剔除快照已含事件但不丢 Chat/语义
- **每会话业务命令邮箱**：`run_or_deadline` 区分 commit/response deadline；不确定结果 close_uncertain
- **命令权限**：Normal/Console/Monitor 权限区分，拒绝映射官方错误响应（绝不静默）
- **速率限制**：Chat 10/3s、RoomOp 20/6s、Api 12/3s
- **会话容量**：`max_sessions`（4096）+ `max_pending_auth`（256）
- **欢迎语**：本地化欢迎消息（en-US/zh-CN/zh-TW **三语键集一致**，按用户语言渲染；**单文件配置** = server_config.yml 的 `welcome` 段，缺省回落内置国际化且随版本更新）
- **登录统计**：`mp_user_visits`（幂等）、playtime 累计、login_count 对账

---

## 四、游戏内 CLI 命令

- **`_` 房间名快捷方式**：管理员在房间名输入框创建名为 `_<command>` 的房间执行 CLI：`_`→空格（`_room_info`→`room info`）、`__` 转义字面量；结果以 `[CLI]` 前缀 Chat 消息回显
- **多行续行**：命令 `--` 结尾暂存，下一条 `--` 开头续接
- 支持全部管理 CLI 命令

---

## 五、管理 CLI（`cli/`）

**核心/生命周期**：`exit/quit`、`help`、`check-config`（含活跃会话/房间数，吸收 status/doctor）、`config reload`
**用户**：`users`、`kick`、`admin-id list/add/remove/set`
**封禁**：`ban [reason]`、`ban ip`、`unban`、`banlist`、`ip-history`
**广播**：`broadcast all|room|user`
**房间**：`rooms`、`room create-empty`、`room info`、`room start/ready/cancel/kick`、`room force-move`、`room close`、`room set <field>`（lock/cycle/hidden/persistent/degraded/host/chart/api_endpoint/tournament/live）、`room history/rounds/round/uuid`、`room ban/unban/banlist`
**插件**：`plugin list/enable/disable/remove/reload/info/call`（WASM 插件可动态注册 CLI 命令）
**扩展**：`extension list/get`
**杂项**：`roomcreation on|off`、`approve openuds`、`welcome-config`、`player-count`
> `roomcreation` / `update auto` / `connections` 是**运行时开关**（config reload 不重置，YAML 对应项仅启动时生效）——语义统一：运行时开关 = reload 免疫。
**基准**：`benchmark run <fixed|ramp>`（进程内内部调用，复用当前实例、虚拟会话/房间隔离、结束全清理，不依赖独立数据库；详见 cli.md）
**运行时**：`runtime`（一次打印 registry/phira/events/schema/persistence/latency 全部分区）
**WAL/死信**：`wal inspect`、`dead-letter list/replay`

---

## 六、HTTP / SSE / WebSocket（`plugin_http.rs`）

监听 `http_port`（默认 12347）；可选 `trusted_forwarded_http_port`
- **GET /health/live**：存活探针
- **GET /health/ready**：就绪探针（supervisor degraded → 503）
- **GET /api/events**：SSE 主事件流（15s keep-alive；ready + 房间事件）
- **GET /api/ws**：WebSocket（二进制帧承载 SseEvent）
- **ANY /{path} catch-all**：插件动态注册路由（路径参数、JSON body 转发）
- **插件 SSE 流**：`sse.register_stream` + `sse:translate` 回调

---

## 七、插件 / WASM

- **PluginEvent（11 种）**：UserConnect/UserDisconnect/RoomCreate/RoomJoin/RoomLeave/RoomModify/GameStart/GameEnd/PlayerTouches/PlayerJudges/RoundComplete（可靠投递有界队列）
- **WIT 世界 `phira-plugin-v3`**：导出 init/get-info/cleanup/on-event/on-api
- **宿主 API（按能力门控）**：
  - `phira-host`：log、generate_uuid、api_call、send_chat、http_request（SSRF 防护）
  - `phira-query`：get_user/get_room/list_rooms/在线用户等
  - `phira-room-mgmt`：建空房/踢人/转移房主/锁/隐藏/关房
  - `phira-user-mgmt`：踢用户/封禁/解封
  - `phira-messaging`：send-to-user/room/all
  - `phira-persistence`：query-events/snapshots/touches/judges/playtime
  - `phira-admin`、`phira-config`、`phira-crypto`、`phira-timer`、`phira-tcp`（连接/监听/收发）、`phira-room-state`、`phira-handler`、`phira-runtime`
- **通用 api_call / ServerStateQuery**：约 60+ 方法名（http.register_route、sse.register_stream、rooms.history、rooms.chat_history 等）
- **能力清单**：`.capabilities.json`（14 个可声明能力）；未知能力拒绝加载
- **WasmRuntimeConfig**：内存 64MB、fuel、栈 2MB、HTTP 超时、并发 8、队列 2048、调用超时 2s、init 超时 10s
- **插件状态**：Loaded/Enabled/Disabled/Error(quarantined)

---

## 八、持久化（`persistence/`）

- **PostgreSQL 18 张表**：playtime、mp_users、mp_room_snapshots、mp_events、mp_user_room_history、mp_rounds、mp_round_player_data（合并触控/判定，UUID 主键 + 嵌套批）、mp_round_results、mp_runtime_telemetry_*、mp_runtime_persistence_meta、mp_runtime_retention_policies、mp_runtime_benchmark_reports、mp_settings、user_ip_history、mp_user_visits、mp_server_instances、_pmp_schema_version
- **WAL**：`persistence-worker.wal.jsonl`（SHA-256 校验，fsync 后入队）；marker 检测意外删除；启动重放未 ack；自动压缩（pending<25% 且 >256KiB）
- **死信**：`persistence-dead-letter.jsonl`（DB 重试耗尽后写入）；启动重放 + 畸形隔离
- **持久化 worker**：有界队列（2048）+ WAL 先写；queue 满 100ms 返回 WalOnly（不丢）；5s 恢复扫描器
- **高频写入**：Touch/Judge 绕过 WAL 批量（256 条/500ms）COPY 入库
- **轮次持久化**：RoundStore 事务；RoundCompleted 原子事务（幂等）
- **保留策略**：`persistence_retention_days`(30)、`touch_judge_retention_days`
- **启动恢复（fail-closed）**：Schema 校验 → WAL 健康（30s）→ DLQ 重放 → 未完成轮次 abort → 持久房间恢复 → playtime 清理
- **后台任务**：WAL 扫描 5s、playtime 刷新 60s、login 对账 1h、保留清理 1h、实例心跳 30s、认证缓存持久化 60s

---

## 九、网络（PROXY 协议等）

- **PROXY 协议 v1/v2**：v1 文本、v2 二进制（12 字节签名）；`TcpStream::peek` 非消费读取（非 PROXY 直连零消耗）；超时 3s
- **可信 CIDR（proxy_allow_cidr）**：可信代理解析真实 IP；真实 IP 独立限流 + 用于认证/封禁
- **转发头 HTTP 监听**：`trusted_forwarded_http_port` 信任 `X-Forwarded-For`
- **TCP accept**：零协议读（慢/恶意连接不阻塞）；session_gate(4096) + pre_auth_gate(256)；Active 发布屏障
- **断连处理**：banned 用户先发本地化封禁原因再断连；管理员踢人串行化 + 补偿

---

## 十、配置项（`server/config.rs`）

- **profile**：Development/Staging/Production（Production 更严校验）
- **网络**：port(12346)、http_port(12347)、trusted_forwarded_http_port、proxy_allow_cidr、连接限流
- **容量**：max_rooms、max_users_per_room(100)、max_sessions(4096)、max_pending_auth(256)、play_history_cache_size(100，每房内存保留最近游玩轮次上限)
- **游玩**：ready_countdown_secs(60)、playing_timeout_offset_secs(60)、room_creation_enabled、chat_enabled
- **Phira 上游**：phira_api_endpoint、HTTP 重试/退避/熔断
- **WASM**：wasm_runtime（见插件节）
- **运行时**：persistence_queue_capacity、WAL/DLQ 路径、persistent_rooms_required、startup_recovery_timeout(30s)
- **保留**：`table_retention`（每表策略：`{max_rows?, days?, time_col?}`，支持任意表；max_rows 超限清 80%、days 超期删）——取代旧全局 retention_days
- **断线**：heartbeat_timeout(15s)、auth_timeout(15s)、dangle_grace(10s)、playing_reconnect_grace(15s)
- **其他**：monitors、admin_phira_ids、sentry_dsn、plugins_dir、cli_enabled、openuds、graceful_shutdown_timeout
- **覆盖顺序**：YAML < 环境变量（PM_DATABASE_URL）< CLI 参数

---

## 十一、可观测性

- **ProtocolTrace 全局计数器**：请求/响应时序、认证屏障、慢路径；**生产必须为 0**：silent_response_paths/late_commit/commit_without_response/compat_queue_drop/stale_commit_prevented/gate_control_overflow/critical_compat_drop
- **延迟直方图**：9 桶（1/5/10/50/100/500/1000/5000ms）
- **`runtime latency` CLI**：在管理控制台打印两个直方图——响应延迟（命令收到→响应）与**握手延迟**（收到认证→AuthOK 发出），以 `█` 条形图渲染各桶计数与百分比（`< 1ms` / `1–5ms` / … / `≥ 5000ms`）
- **EventBus / RoomCommandGateway / PersistenceWorker 统计**：队列/延迟/计数
- **日志**：每小时滚动 + stdout + TUI + OpenUDS；JSON 结构化；脱敏（token/password）
- **Sentry**：`sentry_dsn` + `sentry` feature（Release 默认关）
- **supervisor actor**：后台任务健康检查；critical 任务退出 → degraded；有序关机

---

## 十二、本地化（`l10n.rs`、`locales/`）

- **语言**：en-US / zh-CN / zh-TW（三语键集一致）
- Fluent `.ftl`，`set_use_isolating(false)` 去双向隔离字符
- **key 分类**：房间管理/会话认证/CLI/服务器/系统广播
- **语言来源**：Phira `/me` 的 language 字段；task-local `LANGUAGE` 作用域

---

## 十三、性能 / 其它

- **高频遥测**：Touch/Judge 独立通道批量写 PG；慢 monitor 有界 broadcast 不阻塞热路径
- **基准测试**：进程内纯内部调用（两模式：fixed 维持会话/游玩房间上限、ramp 加压直到 CPU/RAM 触顶）；虚拟会话（负数 id）+ `bench-` 房间隔离、结束全清理；不依赖独立数据库；实时状态（TUI 状态矩形 + 进度条 + x 键结束）；报告指标（会话/房间/CPU/RSS/速率）
- **OpenUDS（Unix Domain Socket API）**：`/var/run/pmp-openuds.sock`；命令（room/player/server/broadcast/plugin/runtime）+ 事件流（user.online/offline、room.*、round.*、touches/judges/logs）；**仅 Unix 平台——Windows 版本不编译/不支持 UDS**
- **备份/恢复**（pmp-admin）：backup create/verify（config + WAL + dead-letter + extensions + SHA-256 manifest）
- **ServerStats / Web 快照**：房间/用户富快照（状态、ready/finished/aborted、当前轮、历史）

---

## 十四、自动更新

- **自动更新**（默认关闭）：`update auto on|off` 开关；启动时与按 `check_interval_secs` 间隔检查 GitHub Release，发现新版本且无在线玩家满 `min_idle_minutes` 后自动下载替换并重启
- **命令**：`update check`（检查新版）/ `update apply`（立即更新，检查在线与空闲）/ `update force`（跳过检查强制更新）/ `update schedule`（预约更新）/ `update cancel`（取消预约）/ `update auto [on|off]`（开关）
- **预约更新**：`update schedule` 与自动更新统一走 `pending_update` 预约流程（幂等，不重复预约），后台执行器在下线满 `min_idle_minutes` 后执行；重启成功后一次性提示「更新完成：已更新到 vX」
- **重启接管**：替换二进制后 spawn 新进程（stdin 重开 `/dev/tty`，管理控制台续用；`PMP_RESTARTED` 跳过交互提示），旧进程以非零退出码退出触发 systemd `Restart=on-failure` / Docker 重启；新进程绑定端口有 3 秒重试窗口
- **产物**：Linux 统一发布静态 musl（`linux-musl` / `linux-arm64-musl`，原生 arm runner 构建，无 glibc 依赖，兼容任意 Linux 版本），不再发布 glibc 构建
- **谱面时长**：选谱时用 HTTP Range 只下 zip 内的音频文件，lofty 探针解析 MP3/FLAC/WAV/OGG 真实时长（弃用 info.txt `EditTime`——非时长字段），用于对局超时计算
- **低兼容终端提示**：首次选 y 写 `data/low-compat-ack` 持久化，之后不再提示；自动更新重启进程自动跳过

---

## 十五、数据存储总览（房间 / 用户 / 持久化）

### 房间数据（内存状态，非持久化）

房间是 **Actor 模型**：权威状态在 `RoomActorState`（每房间独占），外部只读走快照缓存。

| 数据 | 说明 |
|---|---|
| `id` / `uuid` | 房间名（玩家可见）/ 唯一标识 |
| `creator_id` | 建房者（房主回退标识） |
| `lifecycle` | `SelectChart` / `WaitForReady{started, admin_started}` / `Playing{results, aborted}` |
| `control` | host_id、locked、cycle、hidden、persistent_empty、system_host、phira_api_endpoint、max_users、generation |
| `members` | users（玩家）+ monitors（观战），弱引用 |
| `chart` / `chart_name` | 当前谱面 ID 与名称 |
| `round` | round_id / round_uuid（当前轮） |
| 计时 | `ready_countdown_started_at`、`playing_timeout_deadline`、`chart_duration`（秒）、`playing_started_at` |
| `progress_subscribers` | 进度通知订阅者（user_id → 上次通知时间） |
| `live` / `degraded` | 活跃标志 / 降级（Join 补偿失败阻塞） |
| `play_history` | 历史游玩记录（**不持久化**，房间解散即清除） |
| `room_event_seq` | 权威状态事件序号（快照 cutover 用） |

### 用户数据（内存 + 持久化）

| 数据 | 内存/持久化 | 说明 |
|---|---|---|
| `id`（Phira ID）| 内存 | 唯一标识 |
| `name` | 内存 + 持久化（`mp_users`、事件）| 用户名 |
| `language` | 内存 + 持久化 | 语言（l10n） |
| `auth_token` | **仅内存** | Phira token，不下盘；认证缓存只存其 SHA256 hash |
| `binding`（session generation）| 内存 | 会话代际（重连/代际失效判定） |
| `room` | 内存 | 当前所在房间（弱引用） |
| `monitor` | 内存 | 观战者标志 |
| `dangle_mark` / `admin_cli_pending` / `join_pending_game` | 内存 | 断开宽限 / CLI 续行 / 进行中加入确认 |
| 游玩时长 | 持久化（`playtime`）| 房间内时间（进房计时、离开/断开累加） |
| 在线/访问记录 | 持久化（`mp_users`、`mp_user_visits`）| |
| 房间历史 | 持久化（`mp_user_room_history`）| |
| IP 历史 | 持久化（`user_ip_history`，明文）| 供 `ban_ip` / 审核 |
| 轮次结果 | 持久化（`mp_round_results`）| 按轮次 |
| 认证缓存 | 持久化（`data/extensions.json` auth_cache）| token SHA256 → {user_id, name, language, cached_at}，LRU 上限 |

### 持久化数据（非配置类）

**`data/` 文件：**

| 文件 | 内容 | 说明 |
|---|---|---|
| `data/persistence-worker.wal.jsonl` | WAL：认证 / 轮次结果 / 房间事件 | 先落盘再回包，崩溃重放；落库后截断 |
| `data/persistence-dead-letter.jsonl` | 死信队列 | 落库失败重试 / 轮转 |
| `data/extensions.json` | 扩展数据存储 | user_data、room_data（含黑/白名单）、auth_cache |
| `data/update/` | 更新下载的二进制 + `updated-version` 标记 | 更新完成后清理 |
| `data/plugins/` | WASM 插件文件 | 运行时加载 |
| `data/admin-phira-ids.json` | 管理员 Phira ID | 运维管理（配置类） |
| `welcome`（server_config.yml） | 欢迎语每语言配置 | 运维配置 |
| `data/low-compat-ack` | 低兼容终端确认 | 一次性确认标记 |

**PostgreSQL 表：**

| 表 | 内容 |
|---|---|
| `mp_users` | 用户记录（认证、在线/离线） |
| `playtime` | 游玩时长（total_secs，房间内时间） |
| `mp_user_visits` | 用户访问记录 |
| `mp_user_room_history` | 用户房间历史 |
| `user_ip_history` | 用户 IP 历史（明文） |
| `mp_rounds` | 轮次记录 |
| `mp_round_results` | 轮次玩家结果 |
| `mp_round_player_data` | 轮次玩家数据：聚合 touches/judges + 嵌套原始批（touch_batches/judge_batches），data_uuid 主键 |
| `mp_room_snapshots` | 持久房间快照 |
| `mp_events` | 领域事件日志（room.join / round.completed / chat.message 等，带全局序列，PPB 事件溯源数据源） |
| `mp_server_instances` | 服务器实例跟踪（心跳） |
| `mp_settings` | 设置 |
| `mp_runtime_telemetry_batches` | **高频运行时遥测批量**：Touch/Judge 遥测项由 HighFrequencyWriter 批量（256 条/500ms）COPY 入库（pipeline=`runtime.telemetry_batcher`），可观测性/审计用；**不影响** `mp_round_player_data` 的回放数据 |
| `mp_runtime_telemetry_items` | 同上遥测的逐条明细 |
| `mp_runtime_benchmark_reports` | benchmark 报告 |
| `mp_runtime_retention_policies` | 保留策略元数据 |
| `mp_runtime_persistence_meta` | 持久化运行元数据 |

---

## 部署差异（相对官方）

| 项 | 官方 | PMP |
|---|---|---|
| 数据库 | 无 | PostgreSQL 必填（`database_url`/`PM_DATABASE_URL`） |
| 持久化文件 | 无 | `data/`（WAL、快照、死信、回放） |
| 插件 | 无 | `plugins/`（WASM） |
| 端口 | 游戏端口 | 游戏 + HTTP 双端口 + OpenUDS |
| 依赖 | 仅 Rust | PostgreSQL + 可选 Sentry |
