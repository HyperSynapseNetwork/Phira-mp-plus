# PMP `(15)` 生产环境符合性继续审计报告

> 审计基线：`Phira-mp-plus-main (1) (15).zip`  
> 对照基线：上一轮 `(14)` 生产化复审问题清单  
> 审计类型：静态源码、配置、文档、部署与发布链符合性复审  
> 审计结论：**部分符合，生产发布仍为 NO-GO**

---

## 1. 执行摘要

`(15)` 相比 `(14)` 确实完成了若干实质性修复，不是单纯修改文档：

- dead-letter 现在复用稳定 `wal_id`，并向调用方返回写入成功或失败；
- 普通持久化事件的 WAL ACK 失败后进入内存重试队列；
- Worker 主循环已接入自动 WAL compaction；
- WAL 与 dead-letter 等价路径冲突校验已经接入配置验证；
- WAL replay 失败后 Worker 会进入拒绝事件的 degraded loop；
- 数据库显式配置失败时，默认会拒绝启动，只有显式允许 degraded 才继续；
- CI 已恢复 `cargo check/test/clippy -D warnings`；
- Docker、systemd、SECURITY、CHANGELOG、产品与运维文档继续增加。

但是，项目仍未形成可验证的端到端生产保证。当前最关键的问题是：

1. **Telemetry 仍未把 WAL admission ID 带到数据库提交层。**
2. **ACK 重试超过次数后直接放弃，Flush/Shutdown 仍可能返回成功。**
3. **WAL 对完整但缺少最后换行的有效记录会直接丢弃。**
4. **数据库全局桥接安装晚于 PersistenceWorker 启动，启动 replay 存在竞态。**
5. **WAL 文件被删除或清零被测试定义为可正常继续，属于恢复 fail-open。**
6. **Room 仍是多锁共享状态，尚未实现完整 Actor 单写者。**
7. **HTTP 没有真实 health/readiness/metrics，也没有 PPB→PMP 服务认证。**
8. **RBAC 仍主要是模型，没有接入管理入口。**
9. **SQL migration 文件没有成为唯一 schema 来源。**
10. **备份不包含 PostgreSQL，也没有恢复实现或恢复演练。**
11. **供应链、安全审计与 SBOM 仍可被跳过。**
12. **文档继续存在未实现承诺、失效链接与配置 Schema 缺项。**

因此当前版本更准确的工程定位是：

> **具备较多生产化构件的预生产加固候选版，而不是 production-ready 产品。**

---

## 2. 综合成熟度评分

| 领域 | 当前评分 | `(14)→(15)` 趋势 | 结论 |
|---|---:|---:|---|
| 数据持久化与恢复 | 5.5/10 | ↑ | 普通事件改善明显，Telemetry 终态仍未闭环 |
| Actor 与状态一致性 | 5/10 | → | 命令面扩大，但完整状态所有权未完成 |
| 插件安全与隔离 | 6/10 | → | 适合可信插件，不适合不可信第三方插件 |
| 网络接入与会话 | 7/10 | → | 接入、背压和慢客户端处理已有明显加固 |
| 内部 HTTP/PPB 边界 | 3.5/10 | → | 仍缺认证、健康检查、就绪和指标 |
| 数据库演进 | 4/10 | → | 有 migration 文件但无真实 runner，存在双真理源 |
| 安全与权限 | 4.5/10 | → | secrets 支持改善，RBAC 未接生产路径 |
| 可观测性 | 3.5/10 | → | 有日志和诊断，但没有标准指标/SLO/告警闭环 |
| CI/CD 与供应链 | 6/10 | ↑ | check/test/clippy 加强，但 audit/deny/SBOM 非硬门禁 |
| 部署与灾难恢复 | 4.5/10 | ↑ | 有部署骨架，但健康检查、备份恢复仍不合格 |
| 产品化与文档 | 5.5/10 | ↑ | 文档增多，但真相一致性和正式承诺管理不足 |
| 生产就绪度 | **NO-GO** | — | 仍存在数据终态和启动恢复级阻断项 |

---

# 3. 与 `(14)` 审计问题的逐项对照

## 3.1 已基本修复

### A. dead-letter 使用稳定身份并返回写入结果

`preserve_failed_event()` 已接收 `wal_id`，并同时写入：

```json
{
  "dead_letter_id": "<wal_id>",
  "wal_id": "<wal_id>"
}
```

写入成功返回 `true`，失败返回 `false`。调用方只有在 dead-letter 真正写入成功时才把事件视为 durable。

证据：

- `phira-mp-plus-server/src/persistence/worker.rs:80-138`
- 各失败路径根据返回值设置 `durable`：`worker.rs:283-405`

**判定：基本符合。**

剩余问题：dead-letter 文件没有像 WAL 一样显式设置 Unix `0600`，也没有在首次创建或目录项变化后同步父目录；尚无正式重放、处置和归档工具。

---

### B. 自动 WAL compaction 已接入生产调用点

Worker 每处理完事件后会检查：

```rust
if worker_wal.should_compact() {
    worker_wal.compact().await
}
```

证据：`persistence/worker.rs:463-469`。

WAL 的 admission、ACK、replay 和 compact 共用 `io_gate`，压缩写入临时文件、`sync_all`、原子 rename，并同步父目录。

证据：`persistence/wal.rs:430-525`。

**判定：符合基础要求。**

仍需增加：压缩耗时、失败次数、WAL oldest admission age、压缩前后字节数等生产指标。

---

### C. WAL/dead-letter 路径冲突校验已接入

配置加载过程中已调用路径冲突检查，不再只是存在一个未使用的辅助函数。

**判定：基本符合。**

仍应补充：符号链接解析、挂载点、跨文件系统 rename 可用性和目标目录写权限的启动检查。

---

### D. replay 失败后的 Worker 进入 fail-closed 数据入口

当 WAL replay 失败时，Worker 会进入 degraded loop，只允许 Shutdown，普通事件被拒绝。

证据：`persistence/worker.rs:496-510`。

**判定：局部符合。**

但“WAL 被删除或清零”仍被 replay 定义为正常空日志，详见 P0-5，因此整个恢复协议还不是严格 fail-closed。

---

## 3.2 部分修复但未闭环

### E. 普通事件 ACK 重试

新增 `pending_acks` 队列，ACK 写入失败会入队并在后续循环重试。

证据：`persistence/worker.rs:197-234, 448-455`。

然而实现会在尝试次数超过 10 后：

```text
ACK retry exhausted; giving up
```

并直接从内存队列删除。Flush/Shutdown 调用 `drain_pending_acks()` 后，也不检查是否仍有未确认 ACK，而是仅返回 Telemetry flush/shutdown 的结果。

证据：

- 放弃 ACK：`worker.rs:217-220`
- drain 超限后删除：`worker.rs:480-490`
- Flush 忽略 pending ACK 状态：`worker.rs:240-248`
- Shutdown 忽略 pending ACK 状态：`worker.rs:250-260`

**判定：部分符合，仍为生产阻断项。**

---

### F. TelemetryItem 新增 wal_id 字段

`TelemetryItem` 已定义：

```rust
pub wal_id: Option<uuid::Uuid>
```

证据：`telemetry.rs:301-315`。

但实际 Touch/Judge 构造器仍写：

```rust
wal_id: None
```

证据：

- Touch：`persistence/pipeline.rs:172-201`
- Judge：`persistence/pipeline.rs:202-230`

数据库批处理提交成功后也没有调用 WAL ACK。

证据：`telemetry.rs:664-708`。

**判定：接口预留已完成，生产语义未完成。**

---

# 4. P0：生产发布阻断项

## P0-1 Telemetry 数据库提交后无法 ACK WAL

### 当前路径

```text
PersistenceWorker WAL admission
→ stage_production_telemetry_if_needed
→ TelemetryItem(wal_id=None)
→ TelemetryBatcher
→ PostgreSQL transaction success
→ 无 WAL ACK
```

Worker 在 `ProductionTelemetryStage::Staged` 分支不会把事件设置为 durable：

```rust
ProductionTelemetryStage::Staged => {
    record_production_telemetry_staged(...)
}
```

随后非 durable admission 被保留在 WAL。

证据：

- `persistence/worker.rs:347-350`
- `persistence/worker.rs:448-460`

### 直接后果

- 数据库已成功写入，但 WAL 永久保留；
- 重启后重复 replay；
- 即使数据库依靠 `event_id` 去重，WAL 也不能收敛；
- compaction 无法删除这些 admission；
- 长时间运行将持续产生重复恢复工作和磁盘增长；
- 运维无法确定“已提交”和“待处理”的真实边界。

### 必须修复

1. `stage_production_telemetry_if_needed` 必须接收 admission `wal_id`；
2. `TelemetryItem.wal_id` 对权威 Worker 事件必须为 `Some(id)`；
3. 一次数据库事务提交成功后返回本批次全部 admission IDs；
4. TelemetryBatcher 批量 ACK；
5. ACK 失败进入可靠重试队列；
6. mirror 与 authoritative item 必须有不同终态语义；
7. dry-run 不得 ACK authoritative admission；
8. 测试必须覆盖“事务成功、ACK 失败、重启重复 replay、最终 ACK 收敛”。

---

## P0-2 ACK 重试会被主动放弃，Flush/Shutdown 可能假成功

ACK 是“数据库或 dead-letter 已达到终态”的本地确认。如果数据库已经提交，但 ACK 失败，系统不能通过简单限制尝试次数后丢弃 ACK 意图。

当前实现最多重试约 10 次，然后删除队列项。之后：

- WAL admission 仍存在；
- 数据库可能已经提交；
- Flush 可能返回成功；
- Shutdown 可能返回成功；
- 下次启动重复 replay。

### 正确语义

可选方案：

- ACK 重试不设逻辑次数上限，只受控制操作 deadline 限制；
- 或把 ACK intent 写入独立 durable journal；
- 或将 WAL 状态模型改为可原地/分段提交的 durable index；
- Flush/Shutdown 如果 deadline 内不能收敛，必须返回失败；
- readiness 必须降级；
- 指标必须包含 `pending_ack_count`、最老 ACK 年龄和 ACK 错误次数。

---

## P0-3 WAL 会丢弃“完整但缺最后换行”的有效帧

`replay()` 只要发现文件最后一个字节不是 `\n`，就直接 `pop()` 丢弃最后一段，而不会先尝试解析、校验版本和 checksum。

证据：`persistence/wal.rs:314-323`。

这会混淆两种情况：

1. 真正的半条 JSON 断写；
2. JSON 和 checksum 已完整写入，仅最后换行未写入。

第二种情况可能包含一条完整 Admission 或 ACK：

- 丢 Admission：真实数据丢失；
- 丢 ACK：数据库已提交的数据重复 replay。

现有测试只覆盖不完整 JSON 尾部，没有覆盖“完整有效帧但缺换行”。

证据：`wal.rs:661-687, 742-757`。

### 必须修复

对最后一段执行：

```text
若可解析 + 版本合法 + checksum 正确
    接受该帧并补写换行/修复文件
否则
    仅当确认是尾部断写时截断
```

并增加 Admission 和 ACK 两类无换行测试。

---

## P0-4 数据库桥接安装晚于 PersistenceWorker replay

启动顺序目前是：

1. 创建 `DbManager`；
2. 立即 spawn `PersistenceWorker`；
3. Worker 启动并 replay WAL；
4. 加载插件；
5. 最后在 `init_internal_hooks()` 中把 DB 放进全局 `OnceLock`。

证据：

- Worker 启动：`server/orig.rs:197-250`
- DB 全局安装：`server/orig.rs:438-445`
- `DB.set`：`internal_hooks.rs:12-25`

而 Telemetry 和一部分持久化路径读取的是：

```rust
crate::internal_hooks::DB.get()
```

例如 `telemetry.rs:669`。

### 风险

WAL replay 可能在全局 DB 尚不可见时运行，将数据库事件判断为 NoDatabase。普通事件可能只保留在 WAL，直到下次重启又遇到同一竞态；Telemetry 是否成功则依赖时序。

### 必须修复

最佳方案是彻底删除持久化对全局 `OnceLock` 的依赖：

```text
DbManager
→ 显式注入 PersistenceWorker
→ 显式注入 TelemetryBatcher
→ replay 使用同一实例
```

`OnceLock<DbManager>` 还会妨碍同进程多实例、集成测试和有序重启，应一并淘汰。

---

## P0-5 WAL 被删除或清零时 fail-open

当前测试明确把以下情况定义为成功：

- 已有 admission 后 WAL 文件被删除，重新 replay 返回空且健康；
- 已有 admission 后 WAL 文件被写成空文件，服务接受新事件。

证据：

- `fault_wal_deleted_during_replay`：`wal.rs:813-825`
- `fault_zeroed_wal_recovers_empty`：`wal.rs:851-866`

对于首次启动，文件不存在当然可以视为新日志；但对于已经运行过的实例，日志突然消失或归零可能表示：

- 磁盘/挂载错误；
- 运维误删；
- 文件系统损坏；
- 容器卷未挂载；
- 数据目录切换。

### 必须修复

引入 WAL identity/epoch 元数据，例如：

- 单独 metadata 文件；
- instance ID；
- generation；
- clean-shutdown marker；
- durable high-water mark。

只有显式 bootstrap/reset 命令才能接受一个已存在实例的空 WAL。异常丢失必须使 readiness 失败，并要求运维确认。

---

## P0-6 WAL admission 后等待队列发送存在取消窗口

`enqueue()` 当前顺序：

```text
持有 send_gate
→ WAL admit + fsync
→ await mpsc::send
```

证据：`persistence/worker.rs:651-690`。

如果调用 future 在 WAL admission 已成功、但队列发送尚未完成时被取消，事件会：

- 留在 WAL；
- 不在当前进程的实时队列；
- 只能等下一次进程重启才 replay。

如果队列已关闭，调用方收到原事件并可能走直写 fallback，而 WAL 中仍保留 admission，可能造成重复处理。

### 推荐方案

```text
reserve queue permit
→ 写 WAL admission 并 fsync
→ 使用已保留 permit 非取消地提交 WorkerMessage
```

或建立独立 admission coordinator，让调用者取消不影响已经开始的 durable admission 事务。

---

## P0-7 必需 HTTP 子系统未纳入启动就绪

HTTP listener 在 `PlusServer::new()` 后半段以 Supervisor 后台任务启动。主服务可以继续启动，HTTP bind 随后失败才报告 degraded。

证据：`server/orig.rs:447-456`。

如果 PPB 依赖 PMP 内部 HTTP，HTTP 就是必需子系统，必须在 server ready 之前完成 bind。

此外源码注释称 health routes 已先注册，但实际 Router 只有：

```text
/api/events
/api/ws
/{*path}
```

证据：`plugin_http.rs:105-111`。

没有真实：

- `/health/live`
- `/health/ready`
- `/metrics`

Docker HEALTHCHECK 也只是运行二进制 `--version`，无法判断已运行服务是否健康。

### 必须修复

- listener 先 bind，再发布 ready；
- readiness 汇总 DB、WAL replay、Supervisor、插件事件队列、持久化 pending ACK；
- liveness 只判断事件循环是否仍运行；
- Docker/systemd 使用真实 readiness；
- HTTP 不是必需时必须在配置和产品文档中明确 optional/degraded 行为。

---

# 5. P1：生产架构与安全问题

## P1-1 Room 仍未实现完整 Actor-owned state

`Room` 仍包含多个独立状态源：

- `control: StdRwLock<RoomControlState>`
- `state: RwLock<InternalRoomState>`
- `display_names`
- `users`
- `monitors`
- `chart`
- `current_round_id`
- `player_data`
- `live: AtomicBool`

证据：`room.rs:170-201`。

即使命令经过 mailbox，Actor 内部仍是在操作一个可被其他路径读取或修改的共享对象，无法获得：

- 完整单写者；
- 原子业务状态转换；
- 一致快照；
- 清晰 reducer/state machine；
- 可重放 domain event。

### 目标结构

```rust
struct RoomActorState {
    control: RoomControlState,
    lifecycle: InternalRoomState,
    members: MembersState,
    chart: Option<Chart>,
    round: RoundState,
    live_data: PlayerDataState,
}
```

所有写操作只在 Actor task 中执行，外部只能发送命令或获取版本化快照。

---

## P1-2 PPB→PMP 仍没有服务身份认证

HTTP 当前使用：

```rust
CorsLayer::permissive()
```

并公开 SSE、WebSocket 和插件 catch-all 路由。

证据：`plugin_http.rs:105-111`。

`admin_token` 和 RBAC 模型并未形成统一 HTTP middleware。`docs/api/ppb-contract.md` 仍把认证列为待完成项。

即使 PMP 只在 PPB 后方，也必须防止：

- 同网段未授权访问；
- 错误端口暴露；
- SSRF 访问内部管理面；
- PPB 凭据泄露后的无限权限；
- SSE/WS 被匿名订阅。

### 最小要求

- mTLS，或短期轮换 service token；
- token audience/issuer/expiry；
- route scope；
- replay 防护；
- 失败审计；
- 默认 loopback/private bind；
- 未配置内部认证时生产 profile 拒绝启动。

---

## P1-3 RBAC 仍主要停留在模型层

项目存在角色、权限和 `AdminAction` 类型，但生产管理路径仍主要依赖 `admin_phira_ids`。未发现 RBAC 被统一接入：

- 房间管理；
- 用户踢出；
- 插件启停；
- 配置变更；
- HTTP 管理路由；
- WIT 管理能力。

因此目前不能对外声称“已启用完整 RBAC”。

### 必须增加

统一授权函数：

```text
authorize(subject, action, resource, context)
```

以及不可变审计记录：

```text
actor / role / action / resource / outcome / reason / correlation_id / timestamp
```

---

## P1-4 动态 HTTP Router 不区分 Method，冲突静默覆盖

`DynamicRouter::resolve()` 接收 `_method` 但不使用。

证据：`plugin_http/router.rs:36-40`。

相同 path 的后注册 handler 会覆盖旧 handler，没有插件所有权、冲突拒绝和卸载清理语义。

证据：`plugin_http/router.rs:19-32`。

需要改为：

```text
(method, normalized path, plugin owner, API version)
```

组成唯一键，冲突默认拒绝，并支持插件卸载时原子注销。

---

## P1-5 Proxy compatibility listener 生命周期未纳入 Supervisor

主 HTTP task 内部用裸 `tokio::spawn` 启动 trusted-forwarded-header listener。

证据：`plugin_http.rs:129-148`。

该任务：

- 没有单独注册 Supervisor；
- 退出后只记录日志；
- 没有明确 shutdown/join；
- 主 direct HTTP 状态不能代表 proxy port 状态。

应和 direct listener 一样返回启动确认并受统一 cancellation/join 管理。

---

## P1-6 插件仍只能视为可信代码

现有 fuel、Store limits、capability 和 quarantine 是有效改善，但 wall-clock timeout 不能硬终止已经进入阻塞宿主函数或不可中断路径的调用。

产品保证必须明确：

> 当前 PMP 只支持经过审核的可信插件，不提供恶意第三方插件的操作系统级隔离。

第三方插件平台需独立进程 worker，并结合：

- cgroup；
- seccomp；
- user namespace；
- 网络 namespace/egress proxy；
- 文件系统沙箱；
- IPC 超时和强制 kill；
- 插件签名和 provenance。

---

## P1-7 数据库 migration 没有成为唯一 schema 真理源

仓库有 `migrations/20260720000001_initial_schema.sql`，但没有发现 `sqlx::migrate!()` 或等价 runner。

运行时仍通过 `db.rs::init_tables()` 手写建表、索引和 `_pmp_schema_version`。

证据：

- `db.rs:60-115`
- `db.rs:620-650`

这形成两套 schema 来源：

```text
migrations/*.sql
+
Rust init_tables DDL
```

升级文档宣称自动 migration，与实现不一致。

### 必须修复

- migration 文件为唯一 schema 来源；
- 使用数据库 advisory lock；
- 迁移在 readiness 之前完成；
- 不兼容版本拒绝启动；
- irreversible migration 必须有备份与回滚策略；
- CI 在空库和前一版本库上执行 migration 测试。

---

## P1-8 应用程序不应在生产中自动 CREATE DATABASE

`DbManager::new()` 会解析 URL，连接失败后尝试连接 `postgres` 并执行 `CREATE DATABASE`。

证据：`db.rs:61-109`。

问题包括：

- 生产服务需要过高数据库权限；
- URL 字符串拆分脆弱；
- provisioning 和 runtime migration 职责混合；
- 容易掩盖错误数据库名或部署配置。

生产推荐：数据库由 IaC/DBA 创建，PMP 账号只有目标 schema 所需权限。

---

## P1-9 no-database 模式会使数据库事件长期滞留 WAL

生产 profile 要求数据库，这是合理的。但开发或 degraded 模式下，`SkippedNoDatabase` 不会使事件 durable，因此 admission 长期保留，反复 replay，WAL 持续增长。

必须定义明确策略：

- `reject_db_required_events`；
- `durable_file_backend`；
- 或显式 `ack_and_drop`（只允许开发，带强警告）。

不能让模式语义依赖无限重放。

---

# 6. 备份、恢复与灾难恢复审计

## 6.1 当前备份并非完整生产备份

`create_backup()` 只复制：

- 配置；
- 固定路径 WAL；
- 固定路径 dead-letter；
- extensions；
- SHA-256 manifest。

证据：`backup.rs:19-81`。

缺失：

- PostgreSQL dump/snapshot；
- 自定义配置路径中的 WAL/dead-letter；
- 插件包及 manifest；
- schema/version metadata；
- 恢复函数；
- 一致性 pause/flush；
- 加密和密钥管理；
- 文件权限收紧；
- 定期恢复演练。

此外复制整个配置可能把数据库 URL 或 token 写入普通备份目录。

### 正确生产流程

```text
进入 backup barrier
→ persistence flush + ACK 收敛
→ WAL compact/checkpoint
→ pg_dump 或数据库快照
→ 复制配置的脱敏版本、插件 manifest 与本地 journals
→ 生成带版本和实例 ID 的 manifest
→ 加密、签名、上传对象存储
→ 退出 barrier
```

并提供真实 `restore --dry-run`、`restore` 和恢复验收测试。

---

# 7. CI/CD 与供应链审计

## 7.1 已有改进

Build 和 Release 已强制：

- workspace version consistency；
- `cargo check --locked --all-targets --all-features`；
- `cargo test --locked --all-features`；
- Clippy `-D warnings`；
- release build；
- 产物缺失即失败。

这是符合生产化方向的真实进步。

---

## 7.2 Audit 和 deny 仍不是硬门禁

Build workflow 使用：

```bash
cargo audit ... || echo "audit warnings found"
cargo deny ... || echo "deny warnings found"
```

证据：`.github/workflows/build.yml:78-81`。

漏洞或许可证策略失败不会阻止合并/发布。

---

## 7.3 SBOM 可跳过且未随 Release 发布

Release 中 SBOM 安装/生成失败只输出提示，`files:` 列表也没有包含 `.cdx.json`。

证据：`.github/workflows/release.yml:145-160`。

因此当前不能宣称每个正式制品都有 SBOM。

---

## 7.4 版本 bump workflow 未同步 Cargo.lock

`bump-version.yml` 只修改两个 `Cargo.toml` 并提交、打 tag，没有调用现有版本同步脚本，也没有提交 `Cargo.lock`。

证据：`.github/workflows/bump-version.yml:55-68`。

这可能生成一个版本元数据不一致的标签，随后 Release quality gate 才失败。

---

## 7.5 自动版本提交发生在质量门禁之前

Build 的 `auto-patch` 会直接向 main push `[skip ci]` commit，然后才运行 check/test。

证据：`.github/workflows/build.yml:17-52`。

生产仓库不应让未验证的自动修改直接进入 main。推荐 bot PR 或先在临时 ref 验证，再原子推送。

---

## 7.6 缺少供应链证明

仍缺：

- SHA-256 清单；
- minisign/cosign 签名；
- GitHub artifact attestations/SLSA provenance；
- 容器镜像构建及漏洞扫描；
- Actions 固定到 commit SHA；
- 可复现构建比较；
- 发布物运行真实 server readiness smoke test。

当前 smoke test 只执行 `--version` 和 `--help`，不能证明服务可启动。

---

# 8. Docker 与 systemd 审计

当前已有多阶段 Dockerfile 和非 root 运行方向，是进步。但仍需修复：

1. Docker HEALTHCHECK 只运行二进制 `--version`，不检查运行中的服务；
2. 没有 production config 示例随镜像或明确挂载失败提示；
3. 没有容器级只读根文件系统、cap drop、seccomp 示例；
4. systemd `Documentation=` 仍包含占位组织地址；
5. `MemoryDenyWriteExecute=true` 与 Wasmtime JIT 的兼容性没有实测证据；
6. 没有 systemd watchdog/readiness integration；
7. 没有正式 secret file/environment file 模板；
8. 没有数据库 migration 或 restore 前置步骤。

在验证 Wasmtime 与 systemd 沙箱组合之前，不应把该 unit 标记为已验证生产模板。

---

# 9. 可观测性与 SRE 审计

项目已经有 tracing、Supervisor 状态和部分 CLI diagnostics，但缺少产品级可观测闭环：

- 无 Prometheus/OpenTelemetry metrics endpoint；
- 无 readiness endpoint；
- 无 SLO/error budget；
- 无告警规则；
- 无统一 correlation ID；
- 无持久化 backlog oldest age；
- 无 pending ACK count；
- 无 dead-letter oldest age；
- 无 WAL replay/compaction 时延指标；
- 无插件调用 queue wait/timeout/quarantine 指标；
- 无 PPB auth failure 指标；
- 无数据库 pool saturation 指标。

`PersistenceWorker::is_healthy()` 当前只判断：

```rust
wal.replay_succeeded() && !closed
```

证据：`worker.rs:856-858`。

它没有考虑：

- pending ACK；
- DB 必需但不可用；
- dead-letter 失败；
- telemetry backlog；
- Supervisor degraded；
- disk free threshold。

不能直接作为 readiness 定义。

---

# 10. 产品化、文本与文档审计

## 10.1 文档体系有所改善

当前已有：

- SECURITY；
- CHANGELOG；
- guarantees；
- capacity planning；
- PPB contract；
- upgrade/rollback；
- Docker/systemd；
- 产品概述与架构文档。

这说明工程已经开始从“源码项目”向“可交付产品”转变。

---

## 10.2 仍存在实现—文档冲突

主要冲突包括：

1. CHANGELOG 声称存在 health endpoints，源码没有注册；
2. 产品概述称 PMP 只接收已认证的内部请求，HTTP 没有认证 middleware；
3. 升级文档称 migration 自动运行，实际没有 runner；
4. 部分文档仍保留“无 WAL”的历史描述；
5. 部分文档称 Telemetry 在数据库提交后 ACK，实际 `wal_id=None`；
6. capacity planning 中的并发、延迟和内存数字缺少实测证据标记；
7. README 链接的生产审计文件不在仓库；
8. SECURITY 链接的 `docs/security/data-privacy.md` 不存在；
9. archive 文档存在错误相对链接。

静态链接检查发现 **6 个失效本地链接**。

---

## 10.3 配置 JSON Schema 不完整

`PlusConfig` 约有 34 个字段，Schema 只覆盖约 27 个属性。除 serde skip 字段外，仍缺少多个用户可配置字段，例如：

- `benchmark_phira_tokens`
- `extensions_file`
- `idle`
- `monitors`
- `rbac`

结果是 IDE 校验、文档生成和产品配置体验不完整。

---

## 10.4 产品文本需要建立“保证等级”

建议所有宣传和 README 文本使用三类词汇：

### 已验证保证

仅用于有自动化测试、故障测试和发布门禁证据的能力。

### 设计目标

例如“目标支持 5,000 连接”“目标 p99 < 100ms”，必须明确标注目标和测试环境。

### 已知边界

例如：

- 单节点本地 WAL；
- 不提供跨主机复制；
- 当前仅支持可信插件；
- PPB 负责公网边缘认证和 TLS；
- Room 全状态 Actor 化仍在迁移。

禁止使用：

- “零数据丢失”；
- “完全沙箱”；
- “高可用”；
- “企业级”；
- “无限扩展”；

除非对应验收证据已经纳入 Release artifact。

---

## 10.5 需要补齐的产品文档

### 产品层

- `PRODUCT_OVERVIEW.md`
- 支持范围与非目标；
- PMP/PPB/客户端职责矩阵；
- 版本和生命周期策略；
- 兼容性矩阵；
- 限制与容量假设。

### 运维层

- production deployment；
- production configuration reference；
- DB outage runbook；
- WAL corruption runbook；
- disk-full runbook；
- plugin quarantine runbook；
- backup/restore runbook；
- upgrade/rollback；
- incident response；
- metrics and alerting。

### 安全层

- threat model；
- data privacy；
- secrets management；
- PPB service authentication；
- RBAC matrix；
- plugin trust model；
- vulnerability disclosure；
- audit log retention。

### 开发与插件层

- architecture invariants；
- canonical event semantics；
- Room Actor state machine；
- WIT conformance；
- capability reference；
- plugin lifecycle；
- HTTP/SSE/WS versioning；
- error-code catalog；
- SDK compatibility and deprecation policy。

---

# 11. 测试与验收证据缺口

仓库测试数量较多，静态统计约有：

- 326 个 `#[test]`；
- 31 个 `#[tokio::test]`。

但数量不能替代关键失败模式覆盖。仍缺：

1. 真实 PostgreSQL service-container 集成测试；
2. 事务已提交但客户端收到失败的 commit-unknown 测试；
3. ACK 文件写失败；
4. dead-letter 磁盘满/权限失败；
5. WAL 完整帧缺换行；
6. WAL 意外删除/归零应 fail-closed；
7. startup DB/WAL replay ordering；
8. HTTP bind 失败和 readiness；
9. PPB authentication/RBAC 端到端；
10. Room 并发 join/leave/start/cancel 状态不变量；
11. 慢客户端和断线清理队列压测；
12. 插件无限循环、阻塞宿主函数和内存上限；
13. packet parser fuzz；
14. property testing/loom；
15. 认证洪泛；
16. 热点房间 fan-out；
17. 24–72 小时 soak；
18. 备份恢复演练。

当前命名为 `fuzz_*` 的普通单元测试不等同于持续 fuzzing。

---

# 12. 建议整改路线

## 阶段 R0：数据安全阻断项

必须全部完成后才能进入正式预生产：

1. Telemetry commit-after-ACK 链；
2. ACK retry 不丢弃；
3. Flush/Shutdown 等待 ACK 收敛；
4. 修复无换行完整 WAL 帧；
5. 修复 DB bridge/replay 启动顺序；
6. 异常 WAL 删除/归零 fail-closed；
7. queue admission cancellation 语义；
8. dead-letter 权限、目录同步和处置工具；
9. 对上述路径执行故障注入测试。

## 阶段 R1：单实例生产运行

1. health/live/ready/metrics；
2. listener 启动确认；
3. PPB→PMP 认证；
4. RBAC 接入所有管理入口；
5. migration runner；
6. 数据库池参数与超时；
7. 完整备份/恢复；
8. 运维 runbooks；
9. 告警与 SLO；
10. 72 小时 soak。

## 阶段 R2：架构一致性

1. Room 完整 Actor-owned state；
2. canonical domain event pipeline；
3. 管理命令幂等 ID；
4. 一致快照；
5. 状态机 property tests；
6. 删除旧共享状态兼容入口。

## 阶段 R3：产品与供应链

1. audit/deny 成为硬门禁；
2. SBOM 必须生成并上传；
3. SHA-256 和签名；
4. provenance；
5. 容器扫描；
6. 真实 startup/readiness smoke；
7. 文档真相检查和链接检查；
8. production config schema 完整覆盖；
9. Release Notes、支持策略和弃用策略。

## 阶段 R4：第三方插件和高可用

1. 插件独立进程；
2. OS 级资源和网络隔离；
3. 插件签名；
4. 多实例房间归属、lease 和 fencing；
5. 跨实例事件系统；
6. 数据库 HA；
7. 滚动升级和连接迁移。

---

# 13. 正式生产发布硬门槛

## 数据可靠性

- 任一 WAL admission 最终只能处于：DB committed、durable dead-letter、pending；
- 不允许存在“已处理但无法证明终态”的第四状态；
- SIGKILL、磁盘写失败和数据库中断测试不产生静默丢失；
- Flush/Shutdown 对未收敛 ACK 返回失败；
- 备份恢复验证达到定义的 RPO/RTO。

## 稳定性

- 慢认证不影响其他 accept；
- 慢客户端不阻塞房间；
- 断线风暴不留下幽灵 Session；
- 关键后台任务异常使 readiness 失败；
- HTTP、DB、WAL 必需子系统启动失败时拒绝 ready；
- 72 小时 soak 无持续内存/WAL/任务增长。

## 性能

至少公布固定环境下：

- 1k/5k 连接；
- 单热点房间和多房间；
- p50/p95/p99；
- CPU/RSS；
- mailbox depth；
- slow-consumer 比例；
- 插件开/关/慢插件对照；
- DB 正常/抖动/中断；
- WAL fsync 和 compaction 开销。

## 安全

- PPB→PMP 鉴权；
- RBAC 全路径；
- 管理审计；
- secrets 不进入普通备份和日志；
- audit/deny 通过；
- SBOM、签名和 provenance；
- 插件信任边界清晰且文档不夸大。

## 产品文档

- 无失效链接；
- 无实现—承诺冲突；
- 配置 Schema 完整；
- 部署、升级、回滚、备份、恢复和故障 runbook 齐全；
- 每项性能数字标注“目标”或“实测”；
- 发布 artifact 附带验证报告和已知限制。

---

# 14. 最终判定

## 是否比 `(14)` 更符合要求？

**是。**

主要进步是 dead-letter 终态判断、普通 ACK 重试、自动 compaction 和 fail-closed Worker 入口已经实际写入代码。

## 是否已满足上一轮全部审计要求？

**没有。**

最关键的 Telemetry commit-after-ACK 仍未完成，ACK 重试仍会放弃，恢复协议还存在完整帧丢弃、启动竞态和 WAL 异常丢失 fail-open。

## 是否可以进入正式生产？

**不可以，NO-GO。**

当前可用于：

- 受控开发环境；
- 集成测试；
- 预生产故障验证；
- 可信插件联调。

当前不应：

- 宣称 production stable；
- 承诺零数据丢失；
- 公开暴露 PMP HTTP；
- 加载不可信第三方插件；
- 依赖现有备份作为完整灾难恢复方案；
- 在没有 R0/R1 验收证据时承载关键正式业务。

当前推荐产品标签：

> **PMP 0.4.x — Pre-production Hardening Preview**

---

# 15. 审计方法与限制

本次完成：

- 解包并检查 `(15)` 工程；
- 对照仓库内上一轮 `(14)` 审计报告；
- 关键 Rust 控制流与状态所有权审计；
- WAL/ACK/dead-letter/replay/compaction 语义审计；
- HTTP、RBAC、migration、backup、CI/CD、Docker/systemd 审计；
- 11 个 TOML、5 个 YAML、2 个 JSON 的静态解析检查；
- 配置结构与 JSON Schema 覆盖核查；
- 本地 Markdown 链接检查；
- 测试声明和文档承诺一致性检查。

当前环境未检测到可用 `cargo`/`rustc`，因此没有在本次会话实际运行：

```bash
cargo fmt --check
cargo check --locked --workspace --all-targets --all-features
cargo test --locked --workspace --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
```

即使这些命令全部通过，也不能消除本报告指出的端到端数据语义、启动顺序、恢复策略和产品承诺问题；它们需要实现修复和故障测试证据。
