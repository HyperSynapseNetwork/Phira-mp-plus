# Phira-mp+ Project Audit Report

> Generated: 2026-07-02
> Scope: Full source tree review (157 .rs files, ~75k lines total)
>
> ⚠️ **Historical audit snapshot** — This report may reference deprecated
> commands, modes, or configuration. Current authoritative docs:
> README.md, docs/cli.md, docs/configuration.md, docs/simulation.md,
> docs/benchmark-real.md, docs/wit-abi.md.

---

## A. 当前做得好的地方 (Strengths)

### 1. 架构设计意识
- **Runtime v2 渐进式路线已确立**：`actor_runtime.rs`, `command_registry.rs`, `event_bus.rs`, `simulation.rs`, `persistence_worker.rs` 等模块清晰展示了一个完整的分阶段重构蓝图，而非一次性大爆炸重写。
- **Actor 蓝图明确**：`actor_runtime.rs` 定义的七个 Actor Boundary（server-supervisor, session-actor, room-actor, persistence-actor, simulation-actor, plugin-actor, cli-actor）覆盖了系统的每个关键部分，且 migration rule ("mirror → route reads → route writes → delete") 很合理。
- **Simulation 隔离设计**：`SimulationManager` 使用 shadow world 隔离虚拟数据，不污染真实 rooms/users，且设计了多种 preset/scenario 组合和 suite 顺序执行。

### 2. 持久化层
- **PostgreSQL 统一持久化**：`db.rs` 表结构设计完整 — `mp_events`, `mp_rooms`, `mp_rounds`, `mp_round_touch_batches`, `mp_round_judge_batches`, `mp_users`, `mp_runtime_telemetry_batches` 等表覆盖了所有结构化数据，且都有 `created_at`/`updated_at`/`sequence` 时间戳和序列号。
- **独立 retention policy**：每个数据域（telemetry, events, simulation）都有自己的保留时间和清理策略。
- **双写入路径**：TelemetryBatcher Cutover 设计（direct_only/dual_write/worker_only/fallback_only）考虑了平滑迁移。

### 3. 协议兼容与扩展
- **Phira API 统一客户端**：`phira_client.rs` 集中了 retry、backoff、circuit breaker 逻辑，避免分散在各处。
- **多认证模式**：Normal / Console / RoomMonitor / GameMonitor 四种 SessionCategory 覆盖了所有连接类型。
- **房间级 `phira_api_endpoint` 覆盖**：支持精细的 API 端点配置。

### 4. CLI / TUI
- **命令注册表**：`command_registry.rs` 实现了结构化的命令元数据，支持按组查看、补全、格式化帮助。
- **TUI Dirty Render**：`cli_tui.rs` 中 `last_wrap_width`/`scroll_from_bottom` 等设计减少了不必要的全屏重绘。
- **续行命令 `--` 语法**：`collect_cli_continuation` 解决了游戏内 `_command` 输入框长度限制问题。

### 5. 代码质量
- **文档注释**：大部分核心模块有清晰的模块级文档（`//!`），说明设计意图和当前阶段。
- **测试覆盖率**：`simulation.rs`, `event_bus.rs`, `phira_client.rs`, `command_registry.rs`, `session_telemetry.rs`, `runtime_v2_contracts.rs` 等模块有单元测试。

---

## B. 必须马上修的问题 (Critical Issues)

### B1. 全局 static DB (OnceLock) 反模式

**文件**：`internal_hooks.rs:16`
```rust
pub static DB: OnceLock<super::db::DbManager> = OnceLock::new();
```
这是典型的全局状态反模式。`PlusServerState` 已经是一个中央状态对象，DB 连接应该存在里面。当前写法导致：
- 测试无法 mock/替换 DB
- 生命周期不明确（OnceLock 永不释放）
- 任何地方都能通过 `DB.get()` 访问状态，破坏了模块边界
- `db.rs:1639` 的两千行紧耦合 SQL 逻辑无法独立测试

**修复方案**：将 `db_manager` 移到 `PlusServerState` 中，通过参数传递而不是全局访问。

### B2. cli.rs room set chart-id 跳过统一 Phira HTTP Client

**文件**：`cli.rs:864-879`
```rust
let chart = match reqwest::get(format!("{}/chart/{cid}", endpoint.trim_end_matches('/'))).await {
```
直接用裸 `reqwest::get()` 而不是走 `phira_client` 的统一 retry/backoff/circuit-breaker。且没有错误处理 — 502 直接被静默忽略。

**修复方案**：改用 `state.phira_client.get_json::<Chart>()`。

### B3. session.rs 认证路径有两条（`phira_client` 和 `extensions` 各一条）

**文件**：`session.rs:344-371`
auth 路径先从 `extensions.get_auth_cache()` 查缓存，不命中才走 `phira_client.get_json()`。但 auth cache 的持久化在 `extensions.rs` 中，而 token 哈希作为 key（SHA-256 前 8 字节 = u64），存在 hash collision 风险。

同时 `session.rs:1276-1278` 过长的认证处理代码（~200 行）直接内联在 stream handler 中，很难独立测试和审查。

### B4. 硬编码 `reqwest::Client` 与 `PhiraRetryClient` 共存重复

多处（`server.rs:462-477`, `cli.rs:864`）仍然使用独立的 `reqwest::Client` 或裸 `reqwest::get`，而不是 `PhiraRetryClient`。统一目标应该是所有 Phira HTTP 都走 `phira_client`。

### B5. `run_benchmark_sync` 使用 spin-lock 宏在 tokio 上下文中做阻塞 I/O

**文件**：`server.rs:1400-1527`
`sync_read!`/`sync_write!` 宏（`server.rs:85-118`）用 busy-loop `loop { match lock.try_read() { ... yield_now() } }` 来获取 tokio RwLock。这在 async 上下文中是 CPU 浪费。而且整个 `run_benchmark_sync` 在同步上下文中直接篡改内存，不应该保留 — 应该用 simulation 或 real benchmark 替代。

---

## C. 可以以后优化的问题 (Improvements)

### C1. `telemetry.rs` / `telemetry_batcher.rs` 两个文件是同一模块的分拆

两个文件 `telemetry.rs`（~736 行）和 `telemetry_batcher.rs`（facade）与 `session_telemetry.rs`（~319 行）存在职责重叠。Cutover mode 的四向决策（DualWrite/DirectOnly/WorkerOnly/FallbackOnly）增加了不必要的复杂度，特别是考虑到项目还未上线。

建议简化：只保留 direct + worker_only 两种模式，去掉 telemetry cutover 仪表板代码。

### C2. `server.rs` 中的 `server_state_query_inner` 过于庞大

该函数（约 700 行，`server.rs:2268-2985`）是巨大的 match 表达式，处理约 40 种不同的 state query。每个 case 都用 tokio::spawn + mpsc::channel 来桥接同步→异步。应该按 domain 拆分为多个模块。

### C3. WASM ABI 仍是 JSON Memory V1

`plugin_abi.rs` 明确记录当前 ABI 是 JSON-memory bridge，目标是 WIT typed V2。风险包括：schema drift、编解码开销、无编译器检查。WIT 定义已规划但未开始。

### C4. 日志分散且一致性不足
- 部分用 `info!`/`warn!`/`error!`（tracing），部分用 `println!`
- 日志格式不一致：有的带 `?err`，有的带 `%error`
- `cli.rs` 的 ANSI 颜色函数 (`c::red`, `c::green` 等) 与 TUI 颜色系统重复

### C5. `round_store.rs` 的 JSONL 文件持久化和 PG 持久化双重路径

Touches/Judges 既写 JSONL 文件又写 PostgreSQL（`round_store.rs:154-161, 185-192`）。上线后应该只保留 PG 路径。

### C6. `message.rs` 序列化问题（使用 `#[serde(untagged)]` 导致性能开销）

`phira-mp-common` 的 `ClientCommand` 和 `ServerCommand` 使用 `#[serde(untagged)]` 枚举，反序列化时会尝试每个变体直到匹配，有 O(n) 性能开销。

### C7. `cli.rs` 内联命令实现在新命令模式下重复

许多命令既在 `command_registry.rs` 注册了元数据，又在 `cli.rs` 中直接实现。目标是拆分 handler 到独立文件（已有 `cli/commands/` 目录），但 `dispatch_command` 中还是 `self.dispatch_*` 模式。

### C8. TUI 缺少关键面板
目前只有 log/output + input 区域，缺少：
- Rooms 面板（实时房间列表）
- Users 面板（在线用户列表）
- Simulation/Benchmark 面板
- Database 状态
- CPU/RAM 监控

### C9. `internal_hooks.rs` 使用 `once_cell::sync::Lazy` 的全局 Mutable 状态
`WELCOME`, `PLAYERS`, `PLAYTIME_DATA` 三个 static Lazy 是全局可变状态，与 DB OnceLock 同一类问题。

### C10. 文档与代码部分脱节
- `docs/runtime-v2.md`（42k 字节）可能包含过时的路线图
- `docs/configuration.md` 中 `runtime_v2` 段落的注释与实际配置可能不同步
- 插件 WIT ABI 文档无实际 WIT 文件对应

---

## D. Runtime v2 推荐实施路线

### Phase 0 (当前 — 清理技术债)
1. 移除 `internal_hooks::DB` 全局 OnceLock，将 db 放入 `PlusServerState`
2. 修复 `cli.rs` 中裸 `reqwest::get` 调用
3. 移除 `sync_read!`/`sync_write!` 宏和旧 sync benchmark 代码
4. 简化 `session_telemetry.rs` cutover 逻辑为 direct 或 worker_only

### Phase 1 (核心重构)
5. **CLI 命令执行从 `cli.rs` 迁移到独立 handler 文件**（`cli/commands/` 各子模块）
   - 每个子命令一个文件，通过 registry 注册 handler
   - CommandRegistry 增加 `execute()` 方法
   - 统一 help 格式（已完成 metadata 层，只差执行层）

6. **EventBus 接入真实路径**
   - 目前 EventBus 只是 observe 层，没有模块通过它驱动 side-effect
   - 选中 1-2 条低风险路径（如 `ChatMessage`, `SimulationStarted/Stopped`）从 observe-only 升级为驱动路径

### Phase 2 (Simulation 真正可用)
7. **Simulation 接入真实 Room/User 操作**
   - 当前 shadow world 是纯内存计数器增减，不经过实际 room/session 代码
   - 增加 `RealisticSimulationRunner` 让仿真流量经过 Room actor 和 Session handler

8. **Simulation 数据隔离正式实现**
   - `mp_sim_*` 表或 `is_simulation` 字段
   - Web API 默认过滤 simulation 数据
   - Simulation 生命周期广播

### Phase 3 (Room Actor 拆分)
9. **Room 状态机迁移到 RoomActor**
   - `room_actor/` 已经有 mailbox/command/ops 骨架
   - 迁移 set_lock/set_cycle/set_host/close/kick
   - 最终迁移 state machine（select_chart/ready/playing/results）

### Phase 4 (WIT ABI v2)
10. **WIT 定义编写**
11. **Host 从 JSON memory 切换到 WIT component bindings**
12. **Guest SDK 更新**

---

## E. 按优先级排序的 TODO List

### P0 — 立即 (阻塞编译/正确性)
- [ ] P0 检查 `cargo check` 是否通过（当前首次编译可能需要下载大量依赖）
- [ ] P0 `internal_hooks::DB` 全局 OnceLock → `PlusServerState.db_manager`
- [ ] P0 `cli.rs` chart-id 改用 `phira_client.get_json`

### P1 — 高优先级 (架构风险)
- [ ] P1 移除 `sync_read!`/`sync_write!` spin-lock 宏
- [ ] P1 删除 `run_benchmark_sync` 旧同步压测代码
- [ ] P1 简化 telemetry cutover 为 direct/worker 两态
- [ ] P1 统一所有 Phira HTTP 请求走 `PhiraRetryClient`

### P2 — 中优先级 (维护性)
- [ ] P2 `server_state_query_inner` 按 domain 拆分为独立模块
- [ ] P2 `cli.rs` handler 迁移到 `cli/commands/` 各文件
- [ ] P2 `internal_hooks.rs` 全局 Lazy → `PlusServerState` 持有
- [ ] P2 TUI 增加 Rooms/Users/Simulation 面板

### P3 — 低优先级 (性能/优雅)
- [ ] P3 TUI Dirty Render 优化：只在变化时更新面板区域
- [ ] P3 广播使用 `FuturesUnordered` 减少 `clone` 和 `send` 开销
- [ ] P3 WASM WIT ABI v2 定义
- [ ] P3 热点 HashMap 替换为 DashMap

---

## F. 第一批最小可行补丁建议

### Patch 1 (当前一回合内完成): 修复关键 Bug + 清理旧代码

1. `cli.rs:864` — chart-id 裸 reqwest → `phira_client.get_json`
2. 移除 `sync_read!`/`sync_write!` 宏
3. 删除旧 sync benchmark path (`run_benchmark_sync`, `cleanup_benchmark_sync`, `run_benchmark_network` 中的直接内存操作)
4. 简化 `session_telemetry.rs` 的 cutover 决策
5. `cargo check` + `cargo test` 验证

### Patch 2: DB 重构
6. `PlusServerState` 中增加 `db_manager` 字段
7. 替换所有 `internal_hooks::DB.get()` 为 `state.db_manager`
8. 移除 `static DB: OnceLock`
9. 测试编译

### Patch 3: CLI Handler 迁移
10. 在 `command_registry.rs` 中增加 `execute()` + `handler` 字段
11. 将 `cli.rs` 中的命令实现逐个迁移到 `cli/commands/` 各文件
12. 统一 help 输出使用 registry.format_help()

---

## 文件统计摘要

| 类别 | 文件数 | 代码行数 |
|------|--------|----------|
| 核心逻辑 (`server.rs`, `room.rs`, `session.rs`) | 3 | ~4,800 |
| 持久化 (`db.rs`, `round_store.rs`, `persistence/`) | 10 | ~3,500 |
| CLI/TUI (`cli.rs`, `cli_tui.rs`, `command_registry.rs`, `cli/`) | 25 | ~4,500 |
| Runtime v2 (`actor_runtime.rs`, `simulation.rs`, `event_bus.rs`, `telemetry*.rs`, `persistence_worker.rs`, `runtime_*.rs`) | 11 | ~4,000 |
| 插件 (`plugin.rs`, `wasm_host.rs`, `plugin_abi.rs`, `plugin_http*.rs`) | 6 | ~2,500 |
| 其他 | 35 | ~2,500 |
| 依赖 (phira-mp, phira-web-monitor, phira-mp-plus-server-api) | ~67 | ~54,000 |
| **总计** | **~157** | **~75,000** |
