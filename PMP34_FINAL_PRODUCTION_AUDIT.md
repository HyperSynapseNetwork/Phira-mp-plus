# PMP `(34)` 最终生产审计报告

## 1. 审计定位

本报告继续沿用已经确定的 PMP 项目愿景，聚焦 PMP33 遗留的 P0 问题与生产化缺口：

> PMP 是以 Phira+ 为第一使用场景、全面依赖 PostgreSQL、保留完整插件 API 和原始多人游戏数据的稳定、高性能、高扩展 Phira 多人游戏服务端与运行时。

本轮审计接受项目方已确认的事实：
- 当前版本已经通过 `cargo check`
- 当前版本已经完成 build
- 当前版本已经通过现有 Clippy 门禁

本轮审计聚焦**运行时应正确性** 和 **数据完整性**，而非编译通过性。

---

## 2. 总体结论

PMP33 完成了 WAL v1→v2 升级、DLQ replay 顺序修正、Flush fence、Recovery 阶段拆分等关键改进，但仍存在 4 个 P0 级别未完成项和 2 个中长期重构项。

最准确的当前定位：

> **0.5 快速迭代开发版：WAL 基础路径已通，但 durable 语义缺乏显式状态机，playtime crash 保护缺失，scanner in_flight 存在竞态窗口，部分模块边界模糊。**

---

## 3. 当前评分

| 领域 | 评分 | 结论 |
|---|---|---|
| WAL 基础功能 | 7.5/10 | admit/ack/compact 完整，checksum 验证，v2 格式 |
| Durable 语义 | 4/10 | 仅有运行时 boolean，无显式状态机，crash 后可能重复或丢失 |
| Scanner in_flight | 5.5/10 | 基础登记/移除已实现，存在竞态窗口 |
| Playtime crash 保护 | 3/10 | 仅有粗暴的 startup 全关闭，缺 server_instance_id |
| Plugin TCP 并发控制 | 6.5/10 | 配额常量已定义，event channel worker 刚修复 |
| Persistent room 恢复 | 7/10 | snapshot 恢复已实现，但仍缺部分状态 |
| 模块拆分 | 5/10 | WAL/worker/HF 在同一 crate 层级，边界松耦合 |
| 数据完整性 | 6/10 | WAL fail-closed 正确，但 ack/durable 时序有缺口 |

---

## 4. PMP33 遗留待办详细分析

### P0-B: 严格 durable sequence state machine (未完成)

**当前实现分析：**
`src/persistence/wal.rs` — WAL 提供 admit() / ack() / replay() / compact() 四个基本操作。
`src/persistence/worker.rs:process_event_through_pipeline()` — 使用局部 `durable: bool` 跟踪事件是否到达持久化终点。

```rust
// worker.rs:217-341
let mut durable = false;
// ... 经过 pipeline 处理后 ...
if needs_wal_ack {
    if durable {
        worker_wal.ack(wal_id).await?;    // 标记 WAL 已确认
        in_flight.lock().await.remove(&wal_id);
    } else {
        // 不 ACK → 重启时 re-replay
        warn!("WAL entry not ACKed (non-durable outcome)");
    }
}
```

**问题：**
1. **时序缺口**：DB commit + ack 之间 crash，重启后 WAL 还没 ACKed → 重新处理已持久化的数据（幂等性问题）
2. **无显式状态机**：没有一个 `DurableState` enum 来跟踪每个 WAL entry 的完整生命周期
3. **无状态持久化**：WAL 只记录 admit/ack，不记录中间状态（如 DatabaseCommitted、DeadLetterStored）
4. **sequence fence 依赖局部变量**：Flush/Shutdown 的 target_wal_sequence 依赖 worker loop 的 next_expected_sequence 推进，与实际 durable 状态解耦

**预期修复：**
- 引入 `#[derive(Debug, Clone, Serialize, Deserialize)] enum DurableState { Pending, DatabaseCommitted, DeadLetterStored, Acknowledged }`
- 在 WAL 中持久化状态变更（可选：状态变更事件，或 admission 记录的 `state` 字段升级）
- Flush/Shutdown fence 应等待所有 sequence ≤ target 的事件到达 `Acknowledged` 而非仅 `durable = true`
- 修复 ack 失败后的重试路径：当前 `pending_acks` 队列与 `in_flight` 在重试期间不一致

---

### P0-E: scanner in_flight 登记修复 (部分完成)

**当前实现分析：**
`src/persistence/worker.rs` — `in_flight: Arc<Mutex<HashSet<uuid::Uuid>>>` (line 66)

登记点：
- `spawn_with_journals` (line 930): replay 事件加入 in_flight
- `process_event_through_pipeline` (line 333): ACK 后移除
- `wal_recovery_scanner` (line 846): try_send 成功后加入

**问题：**
1. **竞态窗口 A**：当主路径 `enqueue()` 成功但事件尚未进入 worker 循环时，scanner 看到 WAL pending 条目不在 in_flight 中 → 重复 enqueue。虽然 sequence gating 会处理重复，但 wasting channel capacity。
2. **竞态窗口 B**：`drain_pending_acks` 在 ACK 成功后从 in_flight 移除（line 706），但此时事件可能还在 buffer 中尚未被 process_worker_loop 处理。scanner 看到 in_flight 无该 ID + WAL 有 pending → 再次 enqueue。
3. **scanner 注册时序**：scanner 在 try_send 成功后登记（line 846），但 try_send 成功不代表 worker 已收到。若 worker 在处理前 crash，in_flight 保留死条目 → scanner 下次扫描时跳过该条目 → 条目永远不被处理。

**预期修复：**
- 修复竞态窗口 B：`drain_pending_acks` 不应从 in_flight 移除，应让 `process_event_through_pipeline` 的正常 ACK 路径负责移除
- 修复竞态窗口 A：enqueue 时直接在 send_gate 内登记 in_flight
- scanner 应处理 in_flight 中 "卡住" 的条目（超时机制）

---

### User/playtime server_instance_id & crash downtime 保护

**当前实现分析：**
恢复路径（`recovery.rs:357-367`）：
```rust
const MAX_RECOVERY_SECS: i64 = 3600;
async fn close_all_stale_playtime_sessions(db: &DbManager) -> Result<()> {
    let affected = db.close_all_stale_sessions(MAX_RECOVERY_SECS).await?;
    // 粗暴关闭所有开放 session，cap 1h
}
```

SQL（`users.rs:332-335`）：
```sql
UPDATE playtime SET total_secs = total_secs + LEAST(GREATEST(0, ($1 - session_start) / 1000), $2),
    session_start = NULL WHERE session_start IS NOT NULL
```

**问题：**
1. **无 server_instance_id**：完全缺失。无法区分"在线用户被 crash 断开"和"session 早已过期"
2. **1h cap 掩盖问题**：若 crash 持续 30 分钟，所有用户的 playtime 增加 30 分钟（实际未被在线）。 
3. **短 crash 被错误累加**：server 重启耗时 2 秒，但所有在线用户增加 2 秒 playtime
4. **clean shutdown 与 crash 无区别**：正常 shutdown 和 crash 都走同一条 cleanup 路径
5. **无主动 session 保护**：server 启动后，如果某个用户在恢复过程中立即重连，旧 session 可能被误 clean

**预期修复：**
- 新增 `server_instance_id`：在 `PlusServerState` 中存储一个 UUID，服务器启动时生成
- `playtime` 表增加 `server_instance_id` 字段（可选 NULL）
- `close_all_stale_sessions` 改为只关闭 `server_instance_id != current_instance_id` 的 session
- 正常 shutdown 时主动关闭所有 session（计入准确 playtime），而不是留到 startup 处理
- 对 crash 恢复的 playtime，使用更保守的 cap（建议 300s = 5min）或记录 server_down_at 时间戳来精确计算

---

### Plugin TCP 真正的有界并发 task

**当前实现分析：**
`quota.rs` 定义了常量限制：
- MAX_CONNECTIONS_PER_PLUGIN: 32
- MAX_LISTENERS_PER_PLUGIN: 8
- MAX_READ_BUF_PER_CONNECTION: 1 MB
- MAX_PENDING_EVENTS_PER_PLUGIN: 64

`actor.rs` 使用 `PluginEventChannel`：
- 每个 plugin 一个 bounded channel（MAX_PENDING_EVENTS_PER_PLUGIN = 64）
- Recent fix (090a37f): event channel worker 改为 await callback 而不是 spawn

**问题：**
1. event channel worker 虽然 await，但没有设置超时。若 callback 阻塞，worker 卡住
2. `pending_connects: HashMap<u64, (String, SyncReply<u64>)>` (line 63) 在插件卸载时清理，但缺少超时保护
3. 连接级别的并发控制（MAX_CONNECTIONS_PER_PLUGIN）通过计数器实现，但 listen/accept 路径的并发 task 数量没有限制
4. event callback 使用 `Arc<dyn Fn(...) + Send + Sync>` — 无法取消或超时

**预期修复：**
- PluginEventChannel 增加 send_timeout（可选，用于阻塞式 callback）
- accept loop 使用 semaphore 限制并发 task 数
- pending_connects 增加超时淘汰
- 考虑将 callback 改为带 deadline 的 tokio::spawn 池

---

### Persistent room 完整 snapshot 恢复

**当前实现分析：**
`recovery.rs:249-308` 在 `restore_persistent_rooms` 中：
- 从 `mp_settings` 读取 persistent room ID 列表
- 调用 `create_empty_room()` 重建房间
- 从 `mp_room_snapshots` 加载最新 snapshot 并应用 lock/cycle/chart/hidden/host

最近修复（9d8faa2）：
- RoomSnapshot 类型有正确的 serde 字段
- host 状态被正确恢复
- lock/cycle/chart/hidden 被正确应用

**问题：**
1. **权限设置未恢复**：snapshot 中有 `user_roles` / `permissions` 字段吗？需要确认 RoomSnapshot 类型完整字段
2. **settings 恢复缺口**：room 级别的 settings（如 max_users, phira_api_endpoint_override）在 snapshot 恢复路径中未应用
3. **host 可能丢失**：如果 host user_id 在 snapshot 中是非 persistent 的（即随 session 存在），设置为 host 可能产生不一致
4. **无恢复验证**：没有检查恢复后的 room 状态是否与 snapshot 一致

**预期修复：**
- 核查 RoomSnapshot 完整字段（`room_actor/actor.rs`）
- 补充 settings 恢复路径
- 增加恢复后验证（可选：log snapshot vs actual diff）

---

### WAL/worker/HF 模块拆分

**当前结构：**
```
persistence/
├── mod.rs              # pub mod wal/worker/high_frequency
├── wal.rs              # WAL 逻辑
├── worker.rs           # 持久化 worker 循环
├── high_frequency/     # HF writer (Touch/Judge)
├── message.rs          # PersistenceEvent 枚举
├── pipeline.rs         # 持久化 pipeline
├── queries.rs          # 只读查询
├── users.rs            # 用户/playtime SQL
├── ...
```

**问题：**
1. WAL（`wal.rs`）的业务边界清晰，可与 worker 解耦
2. Worker（`worker.rs`）包含 scanner、degraded mode、flush/shutdown fencing——这些可以拆出子模块
3. HF writer 已在独立目录，但其写入路径与 worker 共享 `PersistenceEvent` 枚举——这个依赖可以清理
4. `pipeline.rs` 和 `message.rs` 的公共类型被 worker、wal、HF 共享——拆分成独立 crate 成本高但值得

**当前阶段建议：**
- 暂不拆分为独立 crate（成本高，收益不确定）
- 清理模块内部依赖：worker 不再直接引用 wal 的 `WalRecord` 内部类型
- 将 `process_event_through_pipeline` 拆为独立函数，减少 worker.rs 的大小

---

## 5. 本轮执行优先级

| 优先级 | ID | 领域 | 工作量 | 风险 |
|---|---|---|---|---|
| P0 | B | Durable state machine | 3d | 高 — 数据完整性核心 |
| P0 | E | Scanner in_flight 修复 | 1d | 高 — 恢复正确性 |
| P0 | - | Playtime server_instance_id | 2d | 中 — 用户信任数据 |
| P1 | - | Plugin TCP 有界并发 | 1d | 中 — 资源耗尽防护 |
| P1 | - | Persistent room 补全 | 0.5d | 低 — 边缘 case |
| P2 | - | 模块拆分 | 2d | 低 — 纯重构 |

---

## 6. 附录：关键文件索引

| 文件 | 行数 | 用途 |
|---|---|---|
| `src/persistence/wal.rs` | ~1328 | WAL 核心（admit/ack/replay/compact） |
| `src/persistence/worker.rs` | ~1000+ | Worker 循环、scanner、flush/shutdown |
| `src/persistence/users.rs` | ~370 | Playtime SQL、user CRUD |
| `src/server/recovery.rs` | ~700+ | 启动恢复（WAL drain/DLQ replay/room restore） |
| `src/server/init.rs` | ~200+ | 服务器初始化（PersistenceWorker spawn） |
| `src/plugin_tcp/actor.rs` | ~600+ | Plugin TCP actor |
| `src/plugin_tcp/quota.rs` | ~19 | 资源配额常量 |
| `src/server/snapshot.rs` | ~80 | RoomSnapshot 类型定义 |
| `src/server/state.rs` | ~130 | PlusServerState 定义 |
