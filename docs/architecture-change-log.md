# Architecture change log

## 2026-07-11 · Hardening phase 2

- Session/Room ordered commands are mailbox-only; runtime direct/inline fallback removed.
- Removed the unused Room gateway inline-closure compatibility API and renamed actor-local executors to `*_in_actor`.
- Room control fields consolidated into one coherent snapshot with generation.
- Room mailbox/snapshot registries are UUID-generation bound, preventing stale same-name room actors from corrupting replacement rooms.
- Orphaned Room Actors now self-retire when their room UUID is removed or replaced; snapshots expose `room_uuid`.
- Added explicit `worker_authoritative` telemetry cutover with fail-fast prerequisites.
- Tightened `worker_preferred`: Worker is a mirror only after direct acknowledgement; direct failure promotes the accepted Worker event to a canonical compensation path, while dual rejection is counted and reported as degraded.
- Separated direct persistence failures from policy-driven direct skips in cutover diagnostics.
- Added stable idempotency keys for telemetry, generic events, simulation events and benchmark reports.
- Added JSONL persistence dead-letter and Supervisor degradation when both DB and dead-letter fail.
- Fixed PostgreSQL schema/write mismatches, room snapshot/history transactions, no-DB file fallback and playtime millisecond accounting.
- Tightened configuration range and cross-field validation.
- Supervisor handle is restartable in-process and protected by generation-aware cleanup.
- Release now has a mandatory changed-file-format/check/feature/test/clippy quality gate; Linux/Windows artifacts use the lockfile and fail on missing outputs.
- Synchronized workspace TOML and Cargo.lock versions, added a checked version-sync helper, and rejected release tags that do not match the Cargo version.

## 本轮代码修改

- 连接接入：accept/认证解耦，增加全局 Session 与 pending-auth permit。
- 会话 I/O：有界命令队列、非阻塞发送、慢消费者隔离。
- Room Actor：取消不确定状态下的副作用 fallback 重放，统一命令结果。
- 插件：生产路径 capability enforcement、fuel、Store limiter、有界事件队列、每插件单执行闸门和超时 quarantine。
- 持久化：背压、有限重试、Flush/Shutdown acknowledgment、关闭 drain。
- 生命周期：canonical disconnect event，移除重复派发。
- Supervisor：任务退出/panic 观测、注册可靠化、统一取消和 join。
- 关闭流程：停止接入、共享 deadline、先摘除 Session、插件与持久化有序关闭；认证任务提交前复查 shutdown。
- 配置：增加 `max_sessions`、`max_pending_auth`、`graceful_shutdown_timeout_secs`、插件事件队列和调用期限。
- 文档：明确 PPB/PMP 边界，修正 PROXY 命名和 Idle/持久化语义。

## 明确保留

- PMP 内部 HTTP/SSE/WebSocket 不重构为公网网关。
- Room 全状态 Actor ownership 尚未完成。
- 当前有数据库最终失败 dead-letter，但无 enqueue-before WAL、启动 replay 与 compaction。
- 插件仍在 PMP 进程内执行。
## Phase 3 — Persistence admission WAL

- Added `runtime_v2.persistence_wal_path` (default `data/persistence-worker.wal.jsonl`).
- PersistenceWorker now fsyncs an admission record before accepting an event into its bounded queue.
- Startup replays admissions that do not have a matching ACK.
- Terminal processing appends an ACK; explicit Flush/Shutdown safely compact the WAL to outstanding admissions only.
- A malformed WAL line is treated as a critical durability failure rather than being silently skipped.
- Current boundary: ordinary PersistenceWorker events reach terminal DB/dead-letter handling before ACK. Touch/Judge events are ACKed after successful TelemetryBatcher staging; end-to-end batch commit ACK is still required before claiming crash-safe telemetry commits.
