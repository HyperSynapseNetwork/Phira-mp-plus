# PMP 功能总览

## 是什么 / 不是什么

**是什么**：Phira-mp+（PMP）是 Phira+ 架构中的实时多人游戏运行时。它负责 TCP 游戏协议、会话管理、房间状态机、游戏轮次、可信 WASM 插件执行，以及可靠的事件持久化。

```
PMP (game runtime) → PostgreSQL
                                        ↑
                                    WASM plugins
```

**不是什么**：PMP **不是**面向公网的 Web 网关。以下职责**不属于** PMP：

- 公共用户账号与 OAuth
- Web API 网关
- TLS 终结与限速
- 管理面板前端
- CDN 与 WAF

PMP 运行在可信内网，并内置 HTTP/SSE/WS 接口用于兼容、诊断与内部集成。

**目标读者**：自托管 Phira 服务端运维者、Phira+ 服务部署者、插件开发者（可信生态）。

**当前状态**：生产前加固候选（v0.4.x），适用于受控预发与内部灰度测试，不适用于公开生产发布。

---

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
- **加入后**：广播 OnJoinRoom + JoinRoom → JoinRoom(Ok)（完整快照+live）→ 回放聊天历史 → ChangeHost 补偿
- **离开**：房主离开随机转移；广播 LeaveRoom
- **锁定/循环**：仅房主，全员广播
- **选谱**：仅房主；从 Phira API 拉谱面元数据（谱面名/谱师/曲师/难度/评分/更新日期，失败回退 `#id`）；HTTP Range 下载 `info.txt` 解析谱面时长并缓存；**选谱后广播谱面信息**（`谱师:... 曲师:... 难度:AT Lv.15 评分:0.918 谱面更新:...`）
- **开赛**：`RequestStart`（房主）、管理员强开、`CancelStart`；全员 Ready / 强开 → Playing
- **准备倒计时**（默认 60s）：超时自动开赛，未 Ready 标记 aborted
- **对局超时**（默认 +60s）：首个完成者延长一个偏移；超时未完成者标记 aborted 并结算
- **结算**：全员完成/abort → WAL 持久化 RoundCompleted → 广播 StartRound + **本地化结算排行**（成绩/准确率/std/FC/判定/弃权）→ GameEnd → 回 SelectChart
- **循环模式**：整局结束房主按用户列表轮转
- **房主管理**：`SetHost`、首个非 monitor 加入者成为房主、系统房主（host=-1）
- **监视者**：房间 monitor（接收 RoomEvent/UserVisit）、游戏 monitor（实时 Touches/Judges）；绕过锁定/人数/状态门槛
- **隐藏房间**：`-` 前缀默认隐藏、`SetHidden` 动态切换；不出现在公开列表
- **持久空房间**：`create_empty_room`、`set_persistent_empty`；快照 debounce 500ms 持久化；启动恢复
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
- **欢迎语**：本地化欢迎消息（`welcome-config.json` 可配占位符）
- **登录统计**：`mp_user_visits`（幂等）、playtime 累计、login_count 对账

---

## 四、游戏内 CLI 命令

- **`_` 房间名快捷方式**：管理员在房间名输入框创建名为 `_<command>` 的房间执行 CLI：`_`→空格（`_room_list`→`room list`）、`__` 转义字面量；结果以 `[CLI]` 前缀 Chat 消息回显
- **多行续行**：命令 `--` 结尾暂存，下一条 `--` 开头续接
- 支持全部管理 CLI 命令

---

## 五、管理 CLI（`cli/`）

**核心/生命周期**：`exit/quit`、`help`、`status`、`check-config`、`doctor`、`config reload`
**用户**：`users`、`kick`、`admin-id list/add/remove/set`
**封禁**：`ban [reason]`、`ban ip`、`unban`、`banlist`、`ip-history`
**广播**：`broadcast all|room|user`
**房间**：`rooms/room list`、`room create-empty`、`room info`、`room start/ready/cancel/kick/host`、`room force-move`、`room hide/unhide`、`room close`、`room lock/cycle`、`room set <field>`（lock/cycle/hidden/persistent/degraded/host/chart/api_endpoint）、`room history/rounds/round/uuid`、`room ban/unban/banlist`、`force-start`
**插件**：`plugin list/enable/disable/remove/reload/info/call`（WASM 插件可动态注册 CLI 命令）
**扩展**：`extension list/get`
**杂项**：`roomcreation on|off`、`approve openuds`、`welcome-config`、`player-count`、`round-last`
**基准**：`benchmark list/run/suite/compare/cleanup`
**运行时**：`runtime status/phira/commands/events/rooms/actors/schema/persistence/latency`
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
- **通用 api_call / ServerStateQuery**：约 60+ 方法名（http.register_route、sse.register_stream、rooms.history 等）
- **能力清单**：`.capabilities.json`（14 个可声明能力）；未知能力拒绝加载
- **WasmRuntimeConfig**：内存 64MB、fuel、栈 2MB、HTTP 超时、并发 8、队列 2048、调用超时 2s、init 超时 10s
- **插件状态**：Loaded/Enabled/Disabled/Error(quarantined)

---

## 八、持久化（`persistence/`）

- **PostgreSQL 21 张表**：playtime、room_history、mp_users、mp_room_snapshots、mp_events、mp_user_room_history、mp_rounds、mp_round_touch/judge_batches、mp_round_player_data、mp_round_results、mp_runtime_telemetry_*、mp_runtime_persistence_meta、mp_runtime_retention_policies、mp_runtime_benchmark_reports、mp_settings、user_ip_history、mp_user_visits、mp_server_instances、_pmp_schema_version
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
- **容量**：max_rooms、max_users_per_room(100)、max_sessions(4096)、max_pending_auth(256)
- **游玩**：ready_countdown_secs(60)、playing_timeout_offset_secs(60)、room_creation_enabled、chat_enabled
- **Phira 上游**：phira_api_endpoint、HTTP 重试/退避/熔断
- **WASM**：wasm_runtime（见插件节）
- **运行时**：persistence_queue_capacity、WAL/DLQ 路径、persistent_rooms_required、startup_recovery_timeout(30s)
- **保留**：round_data_retention_days(7)、persistence_retention_days(30)、touch_judge_retention_days
- **断线**：heartbeat_timeout(15s)、auth_timeout(15s)、dangle_grace(10s)、playing_reconnect_grace(15s)
- **兼容性**：official_phira_client、minimum_response_latency_ms(0)、session_command_deadline_ms(4500)、commit_response_reserve_ms(1000)、auth_deadline_ms(5000)、gate 上限、protocol_hack_delay_ms
- **其他**：monitors、admin_phira_ids、sentry_dsn、plugins_dir、cli_enabled、openuds、graceful_shutdown_timeout
- **覆盖顺序**：YAML < 环境变量（PM_DATABASE_URL）< CLI 参数

---

## 十一、可观测性

- **ProtocolTrace 全局计数器**：请求/响应时序、认证屏障、慢路径；**生产必须为 0**：silent_response_paths/late_commit/commit_without_response/compat_queue_drop/stale_commit_prevented/gate_control_overflow/critical_compat_drop
- **延迟直方图**：9 桶（1/5/10/50/100/500/1000/5000ms）
- **`runtime latency` CLI**：在管理控制台打印响应延迟直方图（服务端命令处理，`命令收到→响应`），以 `█` 条形图渲染各桶计数与百分比（`< 1ms` / `1–5ms` / … / `≥ 5000ms`）
- **EventBus / RoomCommandGateway / PersistenceWorker 统计**：队列/延迟/计数
- **日志**：每小时滚动 + stdout + TUI + OpenUDS；JSON 结构化；脱敏（token/password）
- **Sentry**：`sentry_dsn` + `sentry` feature（Release 默认关）
- **supervisor actor**：后台任务健康检查；critical 任务退出 → degraded；有序关机

---

## 十二、本地化（`l10n.rs`、`locales/`）

- **语言**：en-US（57 key）、zh-CN（57 key）、zh-TW（30 key，缺失回退英文）
- Fluent `.ftl`，`set_use_isolating(false)` 去双向隔离字符
- **key 分类**：房间管理/会话认证/CLI/服务器/系统广播
- **语言来源**：Phira `/me` 的 language 字段；task-local `LANGUAGE` 作用域

---

## 十三、性能 / 其它

- **高频遥测**：Touch/Judge 独立通道批量写 PG；慢 monitor 有界 broadcast 不阻塞热路径
- **基准测试**：11 场景 × 4 预设；真实二进制协议客户端压测；Mock Phira HTTP（故障注入）；报告指标（延迟/CPU/RSS/DB rows/s）
- **OpenUDS（Unix Domain Socket API）**：`/var/run/pmp-openuds.sock`；命令（room/player/server/broadcast/plugin/runtime）+ 事件流（user.online/offline、room.*、round.*、touches/judges/logs）；**仅 Unix 平台——Windows 版本不编译/不支持 UDS**
- **备份/恢复**（pmp-admin）：backup create/verify（config + WAL + dead-letter + extensions + SHA-256 manifest）
- **ServerStats / Web 快照**：房间/用户富快照（状态、ready/finished/aborted、当前轮、历史）

---

## 部署差异（相对官方）

| 项 | 官方 | PMP |
|---|---|---|
| 数据库 | 无 | PostgreSQL 必填（`database_url`/`PM_DATABASE_URL`） |
| 持久化文件 | 无 | `data/`（WAL、快照、死信、回放） |
| 插件 | 无 | `plugins/`（WASM） |
| 端口 | 游戏端口 | 游戏 + HTTP 双端口 + OpenUDS |
| 依赖 | 仅 Rust | PostgreSQL + 可选 Sentry |

---

## 兼容矩阵

> 最后更新：2026-07-19

### 服务端版本

PMP 使用 SemVer（`major.minor.patch`）。当前：`0.4.x`（pre-production）。

| 组件 | 版本方案 | 当前 |
|-----------|---------------|---------|
| 服务端 | SemVer | `0.4.x` |
| 游戏协议 | Integer | `1` |
| WIT ABI | Integer | `2` |
| 配置 Schema | Integer | `1` |
| DB Schema | Integer | `1` |
| 事件 Schema | Integer | `1` |

### 升级规则

| 升级类型 | 可滚动升级 | 说明 |
|-------------|-----------------|-------|
| Patch（0.4.1 → 0.4.2） | ✅ | 仅修复 bug，无 schema 变更 |
| Minor（0.4 → 0.5） | ⚠️ | 查看 changelog 是否有破坏性变更 |
| Major（0.x → 1.0） | ❌ | 需要完整迁移 |

### 数据库兼容性

- DB schema 在同一 minor 版本内向前兼容
- Schema 迁移采用 expand/contract 模式
- 回滚：旧服务端版本必须能读取旧列
- 降级可能需要手动逆向迁移

### 协议兼容性

- 游戏协议版本 `1` 稳定
- 客户端必须在连接时协商协议版本
- 服务端拒绝协议版本不支持的客户端

### 插件兼容性

- WIT ABI `v2` 是唯一受支持的 ABI
- ABI 版本变更需要插件重新编译
- 服务端在加载时校验插件 WIT ABI
- 破坏性 ABI 变更会递增 WIT ABI 版本
