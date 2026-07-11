# PMP 架构加固说明

## 1. 边界与目标

PMP 在 PP 架构中是受 PPB 控制的游戏服务端。PMP 负责 TCP 游戏协议、认证后的会话、房间运行时、WASM 插件、事件分发和持久化。PPB 负责公网 Web API、统一认证、边缘限流、网关、TLS 与外部接口治理。

因此，本轮没有把 PMP 内部 HTTP/SSE/WebSocket 重构为独立公网网关。现有 HTTP 能力保留，用于 PPB 后方的内部兼容、诊断和插件集成。部署时不得把这些端口直接暴露到不可信公网。

本轮加固目标是修复 PMP 内部会直接影响稳定性、吞吐、故障隔离与扩展语义的问题。

## 2. 已完成的架构修改

### 2.1 连接接入与认证

- TCP accept 与认证任务解耦。
- 新增 `max_sessions`，以 permit 覆盖认证前预留和 Session 生命周期，避免先认证后超配。
- 新增 `max_pending_auth`，限制同时执行的认证握手数量。
- 慢认证连接不再串行阻塞监听循环。
- 会话上限不再依赖竞争性的 `RwLock::try_read()` 快照。

### 2.2 会话 I/O 与慢消费者隔离

- 会话发送增加非阻塞有界入队路径。
- 房间广播不再因为单个发送队列已满的客户端无限等待。
- 网络读取与业务命令通过有界队列解耦，避免单个慢业务处理直接停止 socket 读取。
- 发送队列关闭、积压和慢消费者被转化为明确失败，而不是把压力传播到整个房间。

### 2.3 Room Actor 命令语义

- 房间管理命令继续通过 per-room mailbox 串行化。
- 命令成功入队但 reply 超时时，不再自动内联重放带副作用的操作。
- Session 与 Room 有序命令都取消运行时 direct/inline fallback；mailbox 缺失、关闭、拥塞或结果不确定时显式失败。
- Room gateway 的兼容 inline closure 已从函数签名删除；`*_in_actor` 仅由 mailbox handler 调用。
- host、system-host、lock、cycle、hidden、endpoint、persistent-empty、管理员开局标志和容量收敛到同一个 `RoomControlState`，并提供 generation 一致快照。
- per-room mailbox 与 Actor 快照绑定房间 UUID；同名房间被删除并重建后，旧 Actor 的迟到快照和退出清理不能覆盖或删除新代次。
- Room Actor 定期核对权威房间注册表；房间删除或 UUID 替换后自动退出，避免 registry sender 让无主 Actor 永久存活。快照包含 `room_uuid` 供诊断识别代次。
- Room 成员、monitor、chart、round 和完整游戏状态仍未完全收敛到单一 Actor-owned state；这是保留的迁移项。

### 2.4 插件隔离与 capability

- capability 检查移动到真实生产 host 调用边界，不再仅存在于测试包装层。
- capability 与具体插件实例绑定，未知 capability 默认拒绝。
- Wasmtime fuel 计量启用，每次 guest 调用重新设置预算。
- Store 接入线性内存、实例、表等资源限制。
- 插件 init、event 与 API 调用接入观测期限；超时后隔离插件并阻止后续调用。
- 插件事件进入有界队列，并受并发上限约束。
- 每插件增加单执行闸门；超时调用未真正退出前不会继续堆积新的 blocking 调用。
- 出站 HTTP 改为受限读取，避免先完整载入响应再截断。

注意：当前启用了 fuel 和 Store resource limiter，但没有使用 epoch interruption。墙钟超时用于检测、隔离和阻止后续调用，无法对已经进入任意阻塞宿主函数的线程提供操作系统级强杀保证。需要运行完全不可信插件时，应把插件执行迁移到独立进程。

### 2.5 事件语义

- 用户断开统一经 canonical event pipeline 发布。
- 移除同一断开事件的直接插件触发与 EventBus 二次触发并存问题。
- 关闭流程按用户去重断开事件和离线持久化。

### 2.6 持久化

- 持久化队列从静默 `try_send` 丢弃改为有界背压/明确错误。
- Flush 和 Shutdown 增加 acknowledgment。
- Worker 增加有限重试和关闭前 flush。
- Touch/Judge、普通事件、Simulation 与 Benchmark 写入增加稳定幂等键。
- 数据库重试耗尽后的原始事件写入 `data/persistence-dead-letter.jsonl`；dead-letter 同时失败会把 Supervisor 标记为 degraded。
- Idle 状态不再跳过权威持久化。
- 关机路径显式执行 persistence flush 与 shutdown。

Dead-letter 是失败事件保全，不是入队前 WAL。它降低数据库长期失败导致的不可恢复丢失，但仍不承诺 `kill -9`、进程崩溃或主机掉电时内存队列零丢失。

### 2.7 Supervisor 与关闭流程

- Supervisor 现在跟踪具名后台任务，并周期性回收已退出或 panic 的 JoinHandle。
- 注册队列满时不再静默丢失任务句柄。
- 关闭时取消并等待所有受监督任务。
- 主关闭流程使用一个共享 deadline，避免每个子系统分别消耗完整超时时间。
- 会话从权威 map 摘除后再关闭 transport，降低重复生命周期副作用。
- 关闭开始后停止接受新连接；已接受但尚未注册完成的认证任务会在提交前再次检查关闭状态。
- Supervisor 句柄带 generation；有序关闭后允许同一进程重新初始化，旧 generation 不能清除新 sender。

Supervisor 不会无条件自动重启任务。自动重启要求任务副作用可幂等，必须由具体调用点显式定义策略。


### 2.8 CI 与 Release 质量门禁

- Build 工作流对本次变更的 Rust 文件执行 rustfmt 检查，并执行 workspace/all-targets check、feature 边界 check、workspace tests 和 Clippy。
- Linux/Windows 构建使用锁文件，产物缺失直接失败。
- Release 工作流包含独立 `quality-gate`，构建和 SDK 打包均依赖该门禁，避免标签发布绕过测试。
- Server、SDK 与 Cargo.lock 版本由 `scripts/sync-workspace-version.py` 统一校验/更新；标签与 Cargo 版本不一致时 Release 直接失败。

## 3. 配置新增与约束

```yaml
max_sessions: 4096
max_pending_auth: 256
graceful_shutdown_timeout_secs: 15

runtime_v2:
  persistence_queue_capacity: 4096
  persistence_dead_letter_path: data/persistence-dead-letter.jsonl
  telemetry_cutover_mode: direct_only  # direct_only | worker_preferred | worker_authoritative
  telemetry_batcher:
    enabled: true
    dry_run: false
    queue_capacity: 8192
    max_items_per_batch: 256
    flush_interval_ms: 1000

wasm_runtime:
  max_memory_mb: 64
  fuel_per_call: 10000000
  max_stack_bytes: 2097152
  max_event_concurrency: 8
  event_queue_capacity: 2048
  call_timeout_ms: 2000
```

约束：

- `max_sessions > 0`
- `max_pending_auth > 0`
- `max_pending_auth <= max_sessions`
- `graceful_shutdown_timeout_secs > 0`
- `fuel_per_call > 0`
- `max_memory_mb > 0`
- `max_stack_bytes >= 65536`
- `max_event_concurrency > 0`
- `event_queue_capacity > 0`
- `call_timeout_ms > 0`
- `persistence_queue_capacity >= 16`
- `persistence_dead_letter_path` 为非空路径或显式 `null`
- `telemetry_batcher.max_items_per_batch <= telemetry_batcher.queue_capacity`
- `worker_authoritative` 要求有效 `database_url`、启用 TelemetryBatcher 且 `dry_run=false`；PostgreSQL 实际初始化失败时拒绝启动

## 4. 关键不变量

本轮代码按以下不变量设计：

1. 一个慢认证连接不能阻止监听器接受其他连接。
2. 在线 Session 数与认证中预留总数不能超过 `max_sessions`。
3. 已入队但结果未知的非幂等命令不能通过 fallback 自动执行第二次。
4. 一个慢客户端不能无限阻塞同房间其他客户端的广播。
5. 插件只能通过其已授予 capability 的 host 方法。
6. 每次 guest 调用都有 fuel；墙钟期限用于超时观测与 quarantine，而不是进程内硬终止。
7. 正常关闭必须尝试 drain 插件事件、flush 持久化并等待后台任务。
8. Idle 不能改变权威数据的持久化语义。
9. 同一用户生命周期事件只进入一次 canonical pipeline。
10. 生产 Touch/Judge 的权威写入模式必须显式可判定；`worker_preferred` 只在直写成功后把 Worker 事件视为镜像，直写失败时允许 Worker 以同一稳定事件 ID 接管该批次；`worker_authoritative` 不得在数据库或 batcher 不可用时静默降级。
11. 数据库最终失败的普通事件要么进入 dead-letter，要么把运行时标记为 degraded；不得仅记录日志后假定成功。
12. 同名房间的新 UUID 代次不得复用旧 mailbox；旧 Actor 不得清理或覆盖新代次的 mailbox/快照。
12a. 已从权威房间表删除或被新 UUID 替换的 Room Actor 必须在有限时间内自动退出。
13. Supervisor 旧 generation 退出不得清除新 generation 的进程句柄。
14. Worker 队列接收只代表持久化管线接纳，不得在诊断或文档中等同于数据库 commit；两条入口均未接收的遥测必须计数并使运行时进入 degraded。

## 5. 保留限制

### 5.1 Room 全状态 Actor 化尚未完成

Room 控制面已经收敛到单一 `RoomControlState`，但成员、monitor、chart、round、实时数据与 `InternalRoomState` 仍由多个锁保存。下一阶段应把这些字段及其状态转换收敛到 Actor-owned `RoomState`，并建立并发状态机测试。

### 5.2 没有崩溃一致性 WAL

数据库最终失败事件现会写入本地 dead-letter，但进程崩溃和断电仍可能丢失尚未进入数据库尝试或 dead-letter 的内存队列。若业务要求审计级零丢失，需要入队前 append-only WAL 或数据库 transactional outbox，并实现启动 replay 与安全 compaction。

### 5.3 插件不是进程级隔离

资源 limiter 降低插件拖垮主进程的概率，但不能替代独立进程、seccomp/cgroup 或容器边界。

### 5.4 PMP HTTP 仍是内部接口

PMP HTTP 没有被扩展为完整公网安全面。必须由 PPB 或可信反向代理隔离；不得把 `http_port`、`proxy_protocol_port` 直接暴露给不可信客户端。

## 6. 建议验收

发布前至少执行：

- 认证慢连接与认证洪泛测试；
- 1,000/5,000 并发连接测试；
- 单热点房间慢消费者测试；
- 插件无限循环、内存增长、越权调用测试；
- DB 中断、恢复、关机 flush 测试；
- 并发 join/leave/start property test；
- SIGTERM 与强制超时退出测试；
- 6 小时以上 soak test，记录 p50/p95/p99、RSS、CPU、队列深度和丢弃率。
