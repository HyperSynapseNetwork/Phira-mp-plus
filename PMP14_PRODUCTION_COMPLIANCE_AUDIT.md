# PMP `(14)` 生产化审计要求符合性复审

> 审计对象：`Phira-mp-plus-main (1) (14).zip`  
> 对照基线：上一轮《PMP 生产化与产品化全面审计报告》及生产就绪清单  
> 审计性质：源码、配置、CI/CD、部署资产与文档的静态证据审计  
> 结论：**部分符合；当前仍为 NO-GO，不应标记为正式生产稳定版。**

---

## 1. 执行摘要

`(14)` 相比 `(13)` 有实质性进步，不是单纯补文档。新增或改进了：

- 带版本和 SHA-256 校验的本地 WAL 帧；
- WAL admission、ACK、replay、compact 共用 I/O 闸门；
- replay 失败后的降级入口；
- WAL 文件权限、父目录同步和磁盘空间检查；
- 更丰富的 Room Actor 命令面与 UUID/generation 防护；
- 固定 Rust 工具链、Clippy `-D warnings`、Dockerfile、systemd、SECURITY、CHANGELOG、deny 配置；
- secrets 环境变量/文件注入；
- RBAC 模型、数据库 migration 文件、备份命令；
- 产品、兼容性、容量、升级回滚、保证边界等文档。

但这些改进尚未形成端到端生产保证。最关键的问题是：

1. **Telemetry 没有把 WAL ID 带到数据库批次提交点，提交后无法 ACK；事件会长期重放。**
2. **dead-letter 成功没有反馈为 durable，原 WAL admission 不会 ACK，并且每次重放生成新的 dead-letter ID。**
3. **普通事件 WAL ACK 写入失败没有重试队列，Flush/Shutdown 不能证明 ACK 已收敛。**
4. **自动压缩判断函数存在但没有生产调用点。**
5. **Room 全数据面仍由多个锁拥有，Actor 尚未成为完整状态唯一写者。**
6. **HTTP bind 失败、readiness、内部认证和 metrics 尚未闭环，文档却宣称已有健康端点。**
7. **RBAC、migration、backup 等多项产品化构件停留在模型或局部实现，未接入实际生产路径。**
8. **CI 供应链门禁仍可被 advisory 路径绕过，发布包没有强制 SBOM、校验值、签名和 provenance。**

因此，当前版本可称为：

> **pre-production hardening candidate（预生产加固候选版）**

不能称为：

> **production-ready / production-stable / high-availability / zero-data-loss**

---

## 2. 总体符合性矩阵

以下百分比是基于验收项的定性完成度估算，不是性能测量值。

| 审计领域 | 判定 | 定性完成度 | 结论 |
|---|---|---:|---|
| WAL 与持久化终态 | 部分符合 | 45% | WAL 内核增强明显，但数据库提交、dead-letter、ACK 重试和自动压缩没有闭环 |
| Room/Session Actor | 部分符合 | 50% | 命令面扩大、代次隔离增强，但 Room 数据面仍多锁多源 |
| 插件安全与隔离 | 部分符合 | 55% | capability、fuel、Store limits、quarantine 存在；仍是进程内可信插件模型 |
| PMP/PPB 内部 HTTP | 不符合生产要求 | 30% | bind 未形成启动确认；无真实 ready/metrics；认证仍是 TODO |
| 身份、RBAC 与审计 | 部分符合 | 35% | RBAC 模型已写，但没有接入管理命令和 API 授权点 |
| 数据库迁移与备份 | 部分符合 | 35% | 有 migration/backup 外形，但无 migration runner，备份不包含 PostgreSQL 权威数据 |
| CI/CD 与供应链 | 部分符合 | 60% | check/test/clippy 改善；audit/deny/SBOM 可跳过，缺签名/provenance/哈希发布 |
| 部署与运维 | 部分符合 | 50% | 有 Docker/systemd；健康检查、secrets、恢复演练和 Wasmtime 沙箱兼容性未证明 |
| 可观测性与 SLO | 不符合生产要求 | 25% | 有结构化日志基础，无标准 metrics、追踪、告警规则和 runbook |
| 性能与故障证据 | 不符合生产要求 | 25% | 有容量文档，但没有可复现的负载、故障和 soak 验收结果 |
| 产品文本与文档 | 部分符合 | 55% | 文档体系显著扩展，但存在断链、过度承诺和实现不一致 |

**综合判定：部分符合，NO-GO。**

---

## 3. 已满足或基本满足的改进要求

### 3.1 WAL 文件协议明显增强

`phira-mp-plus-server/src/persistence/wal.rs` 已实现：

- schema/version 字段；
- SHA-256 checksum；
- admission 与 ACK 日志帧；
- replay 校验；
- 截断尾部恢复测试；
- 单一 `io_gate` 串行化 append/replay/compact；
- compact 临时文件、rename 和父目录同步；
- Unix 文件权限收紧；
- 磁盘空间预检查；
- 未知 ACK、重复 ACK、版本不兼容等测试。

这基本符合上一轮对“不能只使用普通 JSONL 追加”的改进方向。

### 3.2 Actor 管理命令范围扩大

Room mailbox 已从少量控制命令扩展到约 16 类，包括用户加入/移除、谱面、ready、结果提交等；同名房间采用 UUID/generation 防止旧 Actor 覆盖新代次。这是实质性改进。

### 3.3 插件默认权限和运行预算有所收紧

现有实现具备：

- capability 默认拒绝和逐插件身份；
- fuel 必须大于零；
- Store memory/table/instance 限制；
- 单插件执行闸门；
- timeout 后 quarantine；
- 插件事件队列和并发预算。

在“只加载受信任插件”的产品边界内，安全性明显优于早期版本。

### 3.4 CI 基础门禁改善

Build/Release 已包含：

- `cargo check --locked --workspace --all-targets --all-features`；
- `cargo test --locked --workspace --all-features`；
- `cargo clippy ... -- -D warnings`；
- 工作区版本一致性；
- release tag 与 Cargo 版本一致性；
- release 二进制 `--version` / `--help` smoke test；
- 固定 `rust-toolchain.toml`。

此前全局关闭 Clippy 的问题已消除。

### 3.5 产品化资产从“几乎没有”提升为“已有骨架”

新增 Dockerfile、systemd、SECURITY、CHANGELOG、deny 配置及多层产品/运维/开发文档。配置文件支持数据库 URL 和管理 token 通过环境变量或文件注入，避免强制明文写入 YAML。

---

## 4. P0：仍然阻断生产发布的问题

### P0-1：Telemetry 的 WAL ACK 链没有完成

`TelemetryItem` 结构没有 `wal_id`，`stage_production_telemetry_if_needed()` 只把业务内容送入 TelemetryBatcher。Worker 收到 `ProductionTelemetryStage::Staged` 后仅记录 staged 统计，不会 ACK；TelemetryBatcher 在 PostgreSQL commit 后也没有 WAL 句柄或 admission ID 可用于 ACK。

后果：

- 数据库已经成功提交，WAL admission 仍保留；
- 每次重启重复 replay；
- 数据可能被重复追加或依赖数据库幂等键去重；
- WAL 持续增长；
- 无法准确区分“提交成功但 ACK 丢失”和“尚未提交”。

必须改为：

```text
WAL admission ID
→ TelemetryItem/BatchItem
→ DB transaction commit
→ 按批次 ACK 对应 WAL IDs
→ ACK 失败进入 durable retry queue
```

数据库写入必须有稳定幂等键，以覆盖“事务已提交但 ACK 失败”的重放窗口。

### P0-2：dead-letter 不是正确终态

`preserve_failed_event()`：

- 返回 `()`，调用方不知道写入是否成功；
- 生成新的随机 `dead_letter_id`；
- 没有保留原始 `wal_id`；
- dead-letter 成功后没有把 `durable` 设为 true。

结果是：即使 dead-letter 已经成功同步，原 WAL admission 也不 ACK；下次启动再次重放并生成新的 dead-letter 记录。

必须改为：

```rust
async fn preserve_failed_event(
    wal_id: Uuid,
    ...
) -> Result<DeadLetterReceipt, DeadLetterError>
```

其中：

- `dead_letter_id` 应稳定复用 `wal_id` 或包含它；
- 文件必须有唯一约束/去重规则；
- `append + flush + sync_all + parent dir fsync` 成功后才返回 receipt；
- receipt 成功才允许 WAL ACK。

### P0-3：ACK 写入失败没有可靠重试

普通事件进入 durable 状态后，如果 `worker_wal.ack(wal_id)` 失败，当前逻辑只记录 critical failure，没有：

- ACK retry queue；
- 有界退避；
- Flush/Shutdown 等待 ACK 收敛；
- 未完成 ACK 数量指标；
- ACK journal fallback。

这会造成成功数据库提交的事件无限重放。

必须建立：

- `pending_wal_acks: HashMap<Uuid, AckRetryState>`；
- 指数退避；
- Flush/Shutdown 必须 drain；
- 超时返回失败；
- readiness 在 pending ACK 超阈值时失败；
- 迟到重复 ACK 必须幂等。

### P0-4：自动 compaction 只是未接线函数

`PersistenceWal::should_compact()` 有实现和单元测试，但生产代码没有调用点。当前主要在显式 Flush/Shutdown 时 compact。

长期运行服务可能持续膨胀。必须按以下任一条件触发：

- 文件大小超过配置阈值；
- ACK/admission 比率超过阈值；
- 未确认数量低但历史帧数高；
- 周期性维护任务。

自动 compaction 失败不能影响当前有效 WAL，也必须暴露指标和告警。

### P0-5：WAL 与 dead-letter 路径冲突校验未真正接入

`validate_paths_not_equal()` 存在，但没有发现生产调用点；同时单纯 `canonicalize()` 对尚不存在的路径不可靠。

必须在配置加载阶段使用规范化的绝对路径比较，覆盖：

- `data/a/../x`；
- 相对/绝对等价路径；
- symlink；
- 大小写不敏感平台；
- WAL、dead-letter、backup 输出互相冲突。

### P0-6：HTTP 子系统没有形成启动就绪闭环

HTTP `start()` 能返回 bind 错误，但它仍在 Supervisor 后台任务中启动。主进程可能先打印 server started，之后 HTTP task 才 bind 失败。

另外源码中没有实际 `/health/live`、`/health/ready`、`/metrics` 路由，但 CHANGELOG 和 Dockerfile 注释宣称存在健康端点。

必须改为：

```text
构造 listener 并成功 bind
→ 返回 PreparedHttpServer
→ 主程序确认所有必需 listener/DB/WAL 已准备
→ readiness=true
→ 再启动 accept loops
```

Readiness 至少检查：

- shutdown 未开始；
- WAL replay 成功；
- PersistenceWorker 可接收；
- 关键 Supervisor task 未 degraded；
- 显式配置 PostgreSQL 时数据库可用；
- 必需 TCP/HTTP listener 已绑定。

### P0-7：PPB→PMP 内部认证未实现

`admin_token` 和 RBAC 已有配置/模型，但内部 HTTP 没有统一认证中间件；`docs/api/ppb-contract.md` 也承认鉴权仍是 TODO。

即使 PMP 不直接暴露公网，受控网络也不能等同于可信。至少需要：

- PPB 服务身份 token 或 mTLS；
- token rotation；
- audience/service name；
- timestamp/nonce 或短有效期；
- 所有管理路由统一强制校验；
- SSE/WS 握手同样受控；
- 审计日志记录调用主体。

### P0-8：显式数据库配置失败时的语义必须统一

需要保证：

- 未配置 `database_url`：允许明确的无数据库模式；
- 显式配置了 URL：初始化失败默认拒绝启动；
- 只有明确 `allow_database_degraded_mode=true` 才允许降级；
- degraded 模式下 readiness、数据保证和禁用功能必须清晰。

不能让“配置错误或数据库故障”静默变成“用户选择了无数据库部署”。

---

## 5. P1：生产前必须完成的架构改进

### 5.1 Room Actor 必须真正拥有完整 RoomState

`room.rs` 仍有独立的：

- control state；
- internal state；
- users；
- monitors；
- chart；
- current round；
- player data；
- live/其他原子字段。

当前 Actor 只是扩大了串行化命令面，但命令内部仍访问共享 `Room` 多锁对象。

目标应是：

```rust
struct RoomActorState {
    control: RoomControlState,
    lifecycle: InternalRoomState,
    users: ...,
    monitors: ...,
    chart: ...,
    round: ...,
    player_data: ...,
    generation: u64,
}
```

所有写操作只在 Actor loop 中发生；外部只能获得不可变 snapshot。命令 ID 需要配合幂等去重表，而不只是日志跟踪字段。

### 5.2 统一 canonical domain event pipeline

所有业务状态变化应采用：

```text
validated command
→ atomic state transition
→ canonical event with stable event_id
→ persistence/outbox
→ plugin subscriber
→ telemetry
→ client projection
```

不能继续存在业务函数分别广播、写 DB、调插件和写日志的部分完成路径。

### 5.3 插件必须明确分级

当前只能支持：

> PMP 进程内的受信任插件。

若产品要允许第三方插件，则必须增加独立 worker 进程：

- cgroup CPU/memory/pids；
- namespace/seccomp；
- 网络 allowlist；
- 文件系统只读/专属目录；
- IPC RPC deadline；
- 心跳和强制 kill；
- 插件包签名、manifest、版本和 capability 审批；
- 原子 reload 和失败回滚。

在完成前，产品文案必须禁止使用“安全运行不可信插件”。

### 5.4 动态 HTTP 路由仍需产品化

当前 router：

- 忽略 HTTP method；
- 线性首匹配；
- 重复路径可覆盖；
- 没有插件 ownership；
- unload/reload 清理原子性不足。

需要 method-aware route key、冲突拒绝、owner ID、version、原子注册/注销和路由数量上限。

SSE 翻译必须改为每插件/事件只转换一次，再广播给订阅者，不能随客户端数量重复调用 WASM。

### 5.5 数据库 schema 只能保留一个权威来源

项目新增了 `migrations/*.sql`，但未发现 `sqlx::migrate!()` 或等价 runner；`db.rs` 仍手工建表。当前形成“双 schema 真理源”。

必须：

- 使用版本化 migration runner；
- 删除运行时手写重复 DDL；
- migration table 记录版本/checksum；
- startup 支持 validate-only 和 migrate 模式；
- 发布前测试 N-1→N 升级；
- rollback 遵循 expand/contract，而不是文档口头承诺。

### 5.6 备份不等于生产备份

当前 backup 主要复制配置、WAL、dead-letter 和 extension 文件，没有 PostgreSQL dump，因此没有覆盖权威生产数据。

产品化备份必须包括：

- PostgreSQL `pg_dump` 或快照；
- migration/schema 版本；
- 插件包与 capability grants；
- 配置的脱敏副本；
- WAL/dead-letter；
- SHA-256 manifest；
- 可选加密；
- restore 到隔离环境的自动验证；
- 明确 RPO/RTO。

备份前必须与 PersistenceWorker 协调，不应无锁复制正在被改写的 WAL。

---

## 6. CI/CD 与供应链复审

### 已符合

- workspace check/test；
- Clippy `-D warnings`；
- lock file；
- tag/version 一致性；
- release 二进制基本 smoke；
- Rust 工具链固定；
- Docker/systemd 资产存在。

### 未符合

1. Build/Release 没有强制 `cargo fmt --check`。
2. 只跑 `--all-features`，缺少：
   - no-default；
   - PostgreSQL-only；
   - WIT-only；
   - jemalloc/目标平台组合。
3. `cargo audit` 和 `cargo deny` 使用 `|| echo`，失败不会阻断。
4. SBOM 生成允许失败，也没有加入 release files。
5. 没有 release SHA-256 文件。
6. 没有 Cosign/Sigstore 签名。
7. 没有 SLSA provenance/attestation。
8. 没有容器镜像扫描和依赖漏洞阻断策略。
9. 普通 main CI 仍包含自动改版本并 push 的 auto-patch，改变被验证 commit，增加审计与分支保护复杂度。
10. Release smoke 只跑 `--version/--help`，没有配置启动、listener bind、WAL replay、ready probe 和最小数据库 migration 测试。

建议将版本生成从普通 Build workflow 移至明确的 release preparation 流程，任何被发布制品都必须能够追溯到不可变 commit SHA。

---

## 7. 部署与运行复审

### Dockerfile

优点：非 root 用户、multi-stage、固定工具链思想正确。

问题：

- HEALTHCHECK 仅执行二进制 `--version` 或注释声称不存在的 endpoint，不能证明服务可用；
- 默认启动没有强制 production profile；
- 没有只读根文件系统和明确 volume 设计；
- secrets 注入说明不足；
- 没有镜像 digest/signature/SBOM 的发布闭环。

### systemd

优点：存在沙箱化设置和重启策略。

问题：

- Documentation URL 仍是占位域名；
- `MemoryDenyWriteExecute=true` 可能与 Wasmtime JIT 冲突，必须在真实服务启动/插件执行中验证；
- 没有 `EnvironmentFile`/credential file 示例；
- 没有 ExecStartPre 配置验证；
- 没有 sd_notify/readiness；
- 没有 WAL、日志、配置和 backup 目录权限初始化说明。

---

## 8. 可观测性、性能和故障恢复

当前有结构化日志与部分统计字段，但尚不足以支撑生产 SLO。

必须补充：

### 指标

- accept/pre-auth 当前值、拒绝数和延迟；
- sessions、rooms、hot room queue depth；
- room/session mailbox depth、超时和 uncertain result；
- slow consumer disconnect；
- persistence queue、WAL bytes、unacked admissions、ACK retry；
- DB commit/retry/dead-letter；
- plugin duration、fuel、trap、quarantine、queue lag；
- supervisor degraded 和 task exits；
- HTTP/SSE/WS 活跃数和请求延迟。

### 追踪和关联

统一：

- connection/session ID；
- command ID；
- room UUID/generation；
- domain event ID；
- WAL ID；
- plugin ID；
- PPB request ID。

### 告警与 runbook

至少为以下事件提供阈值和处理手册：

- WAL replay failed；
- WAL size/磁盘空间；
- ACK retry backlog；
- DB unavailable/retry exhausted；
- dead-letter increased；
- critical task exit；
- plugin quarantined；
- mailbox saturation；
- slow consumer surge；
- readiness failed。

### 必须取得的动态证据

- 慢认证连接和认证洪泛；
- 单热点房间与多房间；
- 慢消费者；
- PostgreSQL 中断/恢复；
- 事务已提交但响应丢失；
- WAL ACK 失败；
- dead-letter 磁盘失败；
- 磁盘写满；
- WAL 尾部断写/完整帧损坏；
- SIGTERM、kill -9、重启 replay；
- 插件无限循环/内存超限/trap；
- 24–72 小时 soak；
- 备份恢复演练。

没有这些结果时不能宣称“稳定、高性能”。

---

## 9. 产品文本和文档符合性

### 已改善

文档已覆盖产品定位、架构、保证边界、配置、容量、升级回滚、插件生命周期、安全和 API 合同等主题，产品化意识明显增强。

### 仍然不符合“文档即产品”的要求

#### 9.1 实现与文档冲突

- CHANGELOG 宣称已提供 `/health/live`、`/health/ready`，源码没有对应路由；
- Dockerfile 注释引用健康端点，但实现缺失；
- `docs/guarantees.md` 表述 dead-letter 成功后终态 ACK，代码实际不设置 durable；
- 文档暗示 Telemetry 在数据库提交后 ACK，实际没有传递 `wal_id`；
- product overview 描述“authenticated internal requests”，内部 HTTP 认证尚未接入；
- upgrade/rollback 文档称 migration 自动执行，源码没有 migration runner。

所有承诺类文档必须由自动契约测试校验，不能手工保持一致。

#### 9.2 断链和缺文档

已发现 README 链接到包内不存在的生产审计报告，以及安全/归档文档中的相对链接错误。

仍缺少正式：

- `SUPPORT.md`；
- `CONTRIBUTING.md`；
- `ROADMAP.md`；
- backup/restore runbook；
- incident response runbook；
- data privacy/retention；
- OpenAPI/AsyncAPI；
- compatibility support policy；
- deprecation policy；
- release notes 模板；
- production acceptance report 模板。

#### 9.3 配置 schema 不完整

`PlusConfig` 约有 34 个公开字段，JSON schema 仅覆盖约 27 个。缺失项包括 monitors、extensions_file、benchmark token、RBAC、idle 等业务字段。Schema 必须由 Rust 类型生成或通过契约测试强制同步。

### 推荐产品定位文本

当前可以使用：

> PMP 是 PP 平台中位于 PPB 后方的实时多人游戏服务端，负责连接、会话、房间运行时、受信任插件和可靠事件持久化。公共 Web API、用户入口认证、TLS 与边缘流量治理由 PPB 统一承担。

当前不能使用：

- “零数据丢失”；
- “可安全运行不可信第三方插件”；
- “已实现高可用”；
- “已验证支持任意规模并发”；
- “完整 RBAC 已启用”；
- “数据库迁移和恢复全自动”；
- “提供生产级健康检查和监控”。

---

## 10. 正式生产版验收门槛

只有以下全部满足，才建议把版本标为 Production Stable：

### 数据

- Telemetry 在 PostgreSQL commit/dead-letter sync 后 ACK；
- ACK retry queue 可 drain，Flush/Shutdown 返回真实结果；
- dead-letter 稳定 ID、幂等和安全 fsync；
- 自动 compaction 接入；
- WAL corruption fail-closed；
- 数据库 migration 具有单一权威来源；
- backup/restore 演练通过并达到 RPO/RTO。

### 状态与并发

- Room 完整状态 Actor-owned；
- 无 inline fallback；
- side-effect commands 有幂等键/去重；
- join/leave/start/close 并发 property/fault tests 通过；
- 慢客户端不能拖慢同房正常客户端。

### 安全

- PPB→PMP 服务认证；
- RBAC 接入全部管理入口；
- 管理审计；
- secrets rotation；
- 仅受信任插件，或完成进程级插件隔离；
- dependency/license/container 扫描为阻断门禁。

### 运维

- startup fail-fast；
- live/ready/metrics；
- JSON logs + correlation IDs；
- dashboards/alerts/runbooks；
- Docker/systemd smoke；
- SIGTERM 有序关闭；
- 24–72 小时 soak；
- 容量基线与 SLO 已发布。

### 发布与产品

- 全 feature matrix；
- fmt/check/test/clippy/audit/deny 全为硬门禁；
- release SHA-256、SBOM、签名、provenance；
- 不可变 commit→artifact 追踪；
- 文档无断链；
- guarantee 文档由测试证据支撑；
- 版本、兼容、弃用、升级、回滚、支持和安全策略齐全。

---

## 11. 最终判定

### 是否符合上一轮审计要求？

**部分符合。**

`(14)` 已完成若干重要基础工程，并明显好于 `(13)`，尤其是 WAL 帧协议、I/O 串行化、部署资产、工具链和文档骨架。

### 是否已经达到正式生产环境版本？

**没有。**

WAL/Telemetry 终态确认、dead-letter、ACK 重试、Room 状态所有权、内部认证、健康就绪、migration/backup、可观测性与真实故障性能证据仍未闭环。

### 是否已经完成产品化？

**完成了产品化骨架，但没有完成产品化闭环。**

主要差距是：实现与承诺不一致、配置 schema 不完整、部分功能仅有模型/文档而未接入生产路径、缺少支持/隐私/runbook/API 规范及可验证的发布供应链。

### 当前推荐标签

```text
0.4.x — Pre-production / Hardening Candidate
```

不建议使用：

```text
Production Stable
Enterprise Ready
High Availability
Zero Data Loss
Untrusted Plugin Safe
```

---

## 12. 审计限制

本次复审完成了：

- ZIP 解包和 `(13)→(14)` 差异审阅；
- Rust 关键控制流静态核查；
- 11 个 TOML、5 个 YAML、2 个 JSON 结构解析；
- CI、Docker、systemd、配置 schema 和文档链接检查；
- 实现—文档承诺交叉核验。

当前执行环境未检测到 Cargo/Rustc，因此本次没有实际执行：

```bash
cargo fmt --check
cargo check --locked --workspace --all-targets --all-features
cargo test --locked --workspace --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
```

因此本报告不能证明工程可编译，也不能替代负载、数据库、文件系统和进程故障注入。即使上述命令全部通过，也不会自动消除本报告指出的架构语义问题。
