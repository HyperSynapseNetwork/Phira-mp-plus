# PMP 架构加固第二阶段报告

## 1. 本阶段目标

本阶段以第一阶段加固版本为基线，继续消除“表面 Actor 化、实际仍存在双执行路径”和“持久化调用完成但无法证明数据落地”的问题。PP 架构边界保持不变：PMP 是 PPB 后方的游戏服务端，PMP HTTP/SSE/WebSocket 继续作为受控内部兼容与诊断接口，不扩展为公共 Web API。

## 2. 已完成修改

### 2.1 Session 与 Room mailbox 强制化

- Session 有序业务命令不再保留 mailbox 缺失、关闭或超时后的直接处理器 fallback。
- Room 管理命令不再在 mailbox 缺失、关闭、拥塞或 reply 不确定时切换为 inline 执行。
- Room gateway 已删除未使用的 inline closure 参数，Actor 内部执行器统一命名为 `*_in_actor`，源码层不再保留可恢复双执行路径的兼容入口。
- 已入队命令的结果未知时显式失败；Session 路径关闭连接并要求客户端重连同步权威状态。
- 运行时不再根据局部故障临时改变锁顺序和执行模型。
- 旧的 fallback/retry 统计字段仅为诊断结构兼容保留，不再代表可执行路径。
- per-room mailbox 与快照注册表绑定房间 UUID；同名房间删除后重建时，旧 Actor 不能复用、覆盖或清理新房间代次。
- Room Actor 每秒核对权威房间注册表；房间已删除或 UUID 已替换时自动退出并清理匹配代次，避免注册表 sender 使无主 Actor 永久存活。Actor 快照显式携带 `room_uuid`。

### 2.2 Room 控制面一致性

房主、系统房主、lock、cycle、hidden、房间级 Phira endpoint、持久空房、管理员开局标志和容量已收敛到同一个 `RoomControlState` 锁，并通过 `generation` 标识变更。控制面读取不再由多个原子量拼接出可能不存在的状态组合。

成员列表、monitor、谱面、当前轮次、实时玩家数据和完整游戏状态仍未全部收敛为 Actor-owned `RoomState`，因此本阶段只声明“控制面一致快照”，不声明“Room 全状态单写者”。

### 2.3 遥测权威写入模式

新增并完成三种明确模式：

- `direct_only`：RoundStore/数据库直写为唯一权威路径；
- `worker_preferred`：先尝试直写；直写成功时 Worker 只接收迁移镜像，直写失败但 Worker 成功接收时，该批次由 Worker 作为权威补偿路径；
- `worker_authoritative`：Worker/TelemetryBatcher 为正常运行时单写者，只有 Worker 在入队前明确拒绝时才允许直写回退。

`worker_authoritative` 必须同时满足：

- `database_url` 非空；
- PostgreSQL 实际初始化成功；
- TelemetryBatcher 启用；
- `dry_run=false`。

静态条件不满足时配置校验拒绝启动；数据库运行时初始化失败时也拒绝启动，不再静默降级到另一种写入模式。

### 2.4 遥测批处理可靠性与幂等性

- TelemetryBatcher 队列满时施加有界背压，不再把权威批次当成可静默丢弃的镜像。
- 数据库写失败时保留当前批次并重试；Flush/Shutdown 返回真实失败。
- Touch/Judge 事件包含稳定 `event_id`，数据库头记录和明细记录使用唯一键去重。
- Worker 权威写入同时更新原有轮次主表和 Runtime v2 明细表，现有查询路径不会因 cutover 读不到数据。
- 修复生产遥测成功计数重复累加造成 readiness 虚高的问题。
- 新增 `direct_failed`、`worker_canonical_fallback` 与 `unaccepted` 诊断：分别表示直写已尝试但失败、`worker_preferred` 由 Worker 接管该批次，以及直写和 Worker 队列均未接受该批次；最后一种情况会触发节流后的 Supervisor degraded 报告。
- Worker 入队只定义为“持久化管线已接收”，不是 PostgreSQL commit ACK；最终数据库成功、重试、dead-letter 与失败状态由 Worker/Batcher 统计单独报告。

### 2.5 普通持久化事件失败留痕

- 普通生产、Simulation、Benchmark 和遥测 staging 事件在数据库重试预算耗尽后写入本地 JSONL dead-letter。
- 默认路径：`data/persistence-dead-letter.jsonl`。
- 每条记录包含 schema version、dead-letter ID、失败时间、阶段、事件类型、错误和原始事件负载。
- 写入执行 `flush + sync_data`，并计入 `runtime persistence` 诊断。
- 若 dead-letter 被禁用或写入也失败，Supervisor 将记录一次关键持久化故障并进入 degraded。

该机制是失败事件保全，不是入队前 WAL：进程在事件写入数据库或 dead-letter 之前被 `kill -9`，仍可能丢失内存队列中的事件。

### 2.6 PostgreSQL 契约修复

修复了多处会导致真实 SQL 失败或数据错误的契约问题：

- `mp_round_player_data` 写入使用 `sequence`，但旧建表缺列；
- `mp_round_results` 写入列名与建表定义不一致；
- `mp_users` 写入使用不存在的 `first_seen/last_seen`；
- runtime metadata 写入使用不存在的 `created_at`；
- 在线时长把毫秒时间戳直接当秒相减；
- 无数据库部署中 `DbManager::None` 被误判为可用数据库，导致本地 RoundStore fallback 失效；
- RoomSnapshot 事件只写审计事件而未更新快照表；
- 用户进房历史重试可能重复。

进一步增加：

- `mp_events.event_id` 与 `mp_sim_events.event_id` 唯一幂等键；
- BenchmarkReport `report_id` 唯一幂等键；
- 用户进房历史自然唯一键；
- 房间快照、轮次打开/关闭和遥测批次事务化；
- `set_online` 在覆盖旧 `session_start` 前结算异常遗留在线时长，避免重启或异常重连导致时长丢失。

### 2.7 Supervisor

Supervisor 现在区分关键与普通任务，能够记录：

- 提前正常退出；
- panic；
- join error；
- 子系统主动报告的关键故障。

PersistenceWorker、TelemetryBatcher、插件事件分发、EventBus 订阅和断线处理属于关键任务。Supervisor 不盲目自动重启具有副作用的任务；恢复策略必须在具备幂等与状态重建协议后单独实现。

Supervisor 全局句柄采用 generation 保护：有序关闭后可在同一进程重新初始化，旧 generation 退出时不能清除新 Supervisor 的 sender。这修复了集成测试、嵌入式生命周期或同进程重启继承永久关闭通道的问题。

### 2.8 配置校验

启动前增加以下边界校验：

- Session、pre-auth、Room 容量关系；
- 关闭期限；
- WASM 内存、栈、fuel、事件并发、队列和调用期限；
- PersistenceWorker 和 TelemetryBatcher 容量、批量大小及刷新周期；
- dead-letter 路径空字符串；
- `worker_authoritative` 前置条件。

非法值不再由运行时悄悄 `max()`、夹取或降级来掩盖。


### 2.9 CI 与发布门禁

- 主分支/PR 检查增加“本次变更 Rust 文件”的 rustfmt 检查、workspace all-targets check、三组 feature 边界 check、workspace tests 和 Clippy。
- Linux/Windows 发布构建统一使用 `--locked`，并在缺失产物时直接失败。
- 修复工作区 TOML 已为 `0.4.169`、`Cargo.lock` 仍停留在 `0.4.0` 的版本漂移；新增 `scripts/sync-workspace-version.py`，自动补丁提交会原子更新 Server、SDK 与锁文件。
- 标签发布强制校验 `GITHUB_REF_NAME == v<Cargo version>`，避免错误标签生成另一版本的 Release。
- Release 工作流新增独立 `quality-gate`；标签或手动发布必须先通过变更文件格式、类型、feature、测试和 Clippy，不能绕过主构建工作流直接发布。
- 工作流增加 concurrency 约束，PR/分支的新运行会取消旧构建，Release 则禁止相互取消。

## 3. 当前仍未完成的结构性问题

### 3.1 Room 全状态单写者

控制面已经一致化，但成员、monitor、chart、round、`InternalRoomState`、实时数据和历史缓存仍分散在多个锁中。严格一致快照和完整 Actor ownership 仍需专门迁移，并必须伴随并发状态机测试。

### 3.2 入队前崩溃一致性 WAL

Dead-letter 只能保全“已经尝试写数据库且最终失败”的事件，不能恢复尚在内存队列中的事件。审计级零丢失需要：

1. enqueue 前 append-only journal；
2. 稳定事件 ID；
3. 数据库幂等消费；
4. ACK 与安全 compaction；
5. 启动时 replay。

本阶段已经完成稳定 ID 与多数数据库幂等基础，但没有把 dead-letter 伪装成 WAL。

### 3.3 插件硬隔离

WASM guest 已有 fuel、Store limiter、单执行闸门和 quarantine；进程内任意阻塞宿主调用仍不能被操作系统级强杀。完全不可信插件仍应迁移到独立进程/容器。

### 3.4 动态与性能验证

本环境已组装 Rust 1.88 的 `rustc`、`cargo`、`rustfmt` 和 `clippy`，并完成变更 Rust 文件的直接 rustfmt 校验；但 `index.crates.io` DNS 解析失败，依赖无法获取，因此 workspace 类型检查、测试和 Clippy 仍未实际完成。静态修改不能替代：

- `cargo check/test/clippy`（以及全仓既有格式债清理后的全量 fmt 门禁）；
- PostgreSQL 真实迁移与事务故障注入；
- Session/Room mailbox 关闭和 reply 丢失测试；
- 1,000/5,000 连接、热点房间和慢消费者测试；
- 6–24 小时 soak test。

## 4. 当前结论

第二阶段已经把 PMP 从“有界队列和 Actor 外形”推进到更严格的执行单入口、控制面一致性、显式遥测单写模式、数据库幂等与失败事件保全。其稳定性和可运维性比第一阶段进一步提高。

但在 Cargo/CI 和动态故障测试通过前，版本仍应标记为 **hardening candidate**，而不是 production verified。Room 全状态单写者、入队前 WAL 和插件进程级隔离仍是最终愿景的结构性剩余项。
