# PMP `(16)` 生产环境符合性继续审计报告

- 审计对象：`Phira-mp-plus-main (1) (16).zip`
- 对照基线：`Phira-mp-plus-main (1) (15).zip`
- 审计日期：2026-07-21
- 审计类型：静态源码、配置、测试契约、CI/CD、部署资产与产品文档一致性审计
- 最终判定：**部分符合，Production Release NO-GO**

---

## 1. 执行摘要

`(16)` 相比 `(15)` 存在真实改进，尤其是：

1. 数据库全局句柄被提前安装到 PersistenceWorker 启动之前；
2. WAL 删除或清零后通过实例标记进行 fail-closed 检测；
3. TelemetryBatcher 新增 WAL ACK 通道骨架；
4. 普通事件 Flush 已能检测部分 pending ACK；
5. RoomActor 新增 `RoomActorState` 迁移目标结构；
6. Release 新增 build provenance；
7. 新增 CONTRIBUTING、ROADMAP、SUPPORT、事故处理、排障和测试文档；
8. 引入真实 TCP/TLS Federation 实现骨架。

但关键端到端语义依然没有闭环，并且新增 Federation 扩大了生产安全面。当前版本仍不能作为正式生产版发布。

推荐产品标签：

> **PMP 0.4.x — Pre-production Hardening Preview**

不应使用：

- Production Stable
- Enterprise Ready
- Zero Data Loss
- High Availability
- Secure Federation
- Untrusted Plugin Safe

### 1.1 综合评分

| 维度 | 分数 | 判定 |
|---|---:|---|
| 稳定性 | 6.0/10 | 基础生命周期改善，但仍有 shutdown 和队列语义缺陷 |
| 数据可靠性 | 5.0/10 | WAL 基础存在，Telemetry ACK 链仍无效 |
| 性能可扩展性 | 5.5/10 | 有界队列与 Actor 基础存在，但 Room 仍是共享状态，多项压测未证明 |
| 插件安全 | 5.0/10 | capability/fuel/limiter 改善，仍是进程内可信插件模型 |
| Federation 安全 | 2.5/10 | 配置项未生效、身份信息为空、允许跳过证书验证 |
| 可运维性 | 5.0/10 | 文档增加，但缺真实 readiness/metrics/恢复闭环 |
| CI/CD 与供应链 | 6.0/10 | check/test/clippy/provenance 存在，audit/deny/SBOM 仍可跳过 |
| 文档与产品化 | 5.5/10 | 信息架构改善，但存在 8 个断链和多项错误承诺 |
| 生产就绪度 | **NO-GO** | P0 数据终态和身份安全问题未关闭 |

---

## 2. 与 `(15)` 审计问题的逐项对照

| 上一版问题 | `(16)` 状态 | 结论 |
|---|---|---|
| Telemetry 数据库 commit 后 ACK | 增加通道和 `wal_id` 字段，但实际 item 仍为 `None` | **未修复** |
| ACK 失败重试 | 普通 Worker 路径有重试；Telemetry ACK 仅日志 | **部分修复** |
| Flush/Shutdown 等待 ACK 收敛 | Flush 部分检查；Shutdown 返回值不包含 pending ACK | **部分修复且存在错误成功** |
| WAL 完整无换行尾帧 | 仍直接丢弃最后分段 | **未修复** |
| DB 安装晚于 replay | DB 在 Worker 前注册 | **已修复** |
| WAL 删除/清零 fail-open | 新增 instance marker | **基本修复** |
| Room 全状态 Actor-owned | 新增镜像 `RoomActorState`，仍从共享 Room 重建 | **未完成** |
| health/live、health/ready、metrics | 文档继续声明，源码无路由 | **未修复** |
| RBAC 接入真实管理路径 | 仍仅模型和单测引用 | **未修复** |
| migration runner | 仍未发现 runner | **未修复** |
| PostgreSQL 完整备份/恢复 | 仅文件复制和 verify，文档提示手工 pg_dump | **未修复** |
| 供应链硬门禁 | 新增 provenance；audit/deny/SBOM 仍非硬门禁 | **部分修复** |
| 文档断链/过度承诺 | 删除旧报告但 README 仍引用 | **出现回归** |

---

# 3. P0：正式生产阻断项

## P0-1 Telemetry commit 后 ACK 通道在实际生产路径中无效

### 证据

`persistence/pipeline.rs:165-225` 构造 Touch/Judge `TelemetryItem` 时仍写入：

```rust
wal_id: None
```

具体位置：

- Touch：`persistence/pipeline.rs:185`
- Judge：`persistence/pipeline.rs:214`

`telemetry.rs:630-639` 虽然在 DB commit 后尝试发送 ACK，但只有 `item.wal_id` 为 `Some` 才执行。

因此真实流程仍是：

```text
WAL admission
→ Worker
→ TelemetryItem(wal_id=None)
→ PostgreSQL commit
→ 无 WAL ACK
→ 重启 replay
```

### 后果

- 已提交数据会被重复 replay；
- 同一 Touch/Judge 批次可能重复写入；
- WAL 无法收敛；
- 自动 compaction 仍会保留这些 admission；
- 文档中的“Telemetry batcher events survive crash”和“commit 后 ACK”没有实现证据。

### 必须整改

将 API 改为：

```rust
stage_production_telemetry_if_needed(wal_id, event, batcher)
```

并强制：

```rust
TelemetryItem { wal_id: Some(wal_id), ... }
```

必须增加集成测试：

1. admission；
2. TelemetryBatcher 数据库事务 commit；
3. 对应 WAL ID ACK；
4. replay 结果不包含该事件；
5. DB 失败时 WAL admission 保留。

---

## P0-2 Telemetry ACK 通道是 lossy 的

### 证据

`telemetry.rs:631-636` 使用：

```rust
let _ = tx.try_send(wal_id);
```

队列满或通道关闭时返回值被忽略。

`persistence/worker.rs:611-622` 的 ACK 接收任务：

- ACK 失败只记录日志；
- 没有重试队列；
- 没有 degraded 状态；
- 使用普通 `spawn_named`，不是关键任务；
- Flush/Shutdown 不等待 ACK 接收通道 drain。

### 后果

即便后续把 `wal_id` 正确传入，也可能出现：

```text
DB commit 成功
→ ACK channel 满
→ ACK 丢失
→ Shutdown 返回成功
→ 重启重复 replay
```

### 必须整改

不要使用 best-effort `try_send` 表达 durable ACK。应采用以下之一：

- TelemetryBatcher 在 commit 后直接调用持久 WAL ACK API；或
- 发送带批次确认的可靠消息，等待 Worker 返回 ACK 结果；或
- 使用共享 durable ACK retry queue。

Flush/Shutdown 必须等待：

```text
batch DB commit
+ ACK channel drained
+ WAL ACK retry queue empty
```

---

## P0-3 Shutdown 可在 pending ACK 存在时返回成功

### 证据

`persistence/worker.rs:258-273`：

- `should_stop` 会检查 `ack_pending == 0`；
- 但 reply 发送的是原始 `result`：

```rust
let _ = reply.send(result);
```

如果 Telemetry shutdown 成功而 pending ACK 仍存在：

- Worker 不退出；
- 调用方收到 `Ok(())`；
- 外部 `shutdown()` 随后执行 compaction 并返回成功。

### 后果

产品层可能报告“持久化已正常关闭”，但：

- Worker 仍在运行；
- ACK 未收敛；
- closed 标志已经设置；
- 后续事件入口被关闭；
- 调用方无法区分部分关闭状态。

### 必须整改

Shutdown reply 必须返回 combined result：

```rust
if ack_pending > 0 {
    Err(format!("{ack_pending} WAL ACKs remain"))
} else {
    telemetry_result
}
```

并明确状态机：

```text
Open → Draining → Stopped
              ↘ FailedDraining
```

---

## P0-4 普通 ACK 重试最终会被丢弃

### 证据

`persistence/worker.rs:215-221` 在重试超过 10 次后：

```rust
pending_acks.pop_front();
```

`drain_pending_acks()` 在超过阈值后同样不再保留该 ACK。

### 后果

- 数据库已 commit，但 WAL admission 永久不清除；
- 重启后重复执行；
- pending queue 变成 0，Shutdown 可能误判为已收敛；
- “retry until durable”文档承诺不成立。

### 必须整改

ACK 失败不能仅因达到内存重试预算而忘记。应：

- 无限保留到 operator repair，或
- 写入独立 durable ACK journal，或
- 将服务 readiness 标记为 false，并禁止成功 shutdown。

指数退避可以降低频率，但不能删除 ACK 意图。

---

## P0-5 队列满时返回成功，但事件不会在当前进程继续处理

### 证据

`persistence/worker.rs:706-729`：

1. 先 WAL admit；
2. `try_send()`；
3. 队列满时记录 warning；
4. 返回 `Ok(())`；
5. 注释明确说“will replay on restart”。

### 后果

这意味着正常运行中：

```text
调用方收到 accepted
→ 事件只在 WAL
→ 当前 Worker 不处理
→ 必须重启才继续
```

Flush 也无法 drain 这些未进入队列的 admission。

这是严重的 API 语义错误：accepted 不应等同于“等待下一次进程重启”。

### 必须整改

正确模式应为：

```text
reserve queue permit
→ WAL admit/fsync
→ 使用已预留 permit 提交队列
```

若 reserve 失败，应在 WAL admit 前返回 backpressure 错误。

不能在队列满后返回成功并依赖重启。

---

## P0-6 WAL 完整但缺末尾换行的有效帧仍会被丢弃

### 证据

`persistence/wal.rs:331-339`：只要最后一个字节不是 `\n`，最终 segment 就直接 `pop()` 丢弃，没有先尝试：

- JSON 解析；
- 版本检查；
- checksum 验证。

`compact()` 中也使用同样逻辑。

### 后果

完整 Admission 已经写入，但最后换行在崩溃前没有写入时，合法事件会被误判为半帧并永久丢弃。

### 必须整改

对尾段执行：

1. 尝试完整解析和 checksum；
2. 合法则保留，并在修复阶段补换行；
3. 只有无法解析或 checksum 不完整时才截断；
4. 完整但 checksum 不匹配应 fail-closed，而不是静默截断。

---

## P0-7 新 Federation/TLS 不能作为“安全联邦”启用

`(16)` 新增了约 500 行 Federation TCP/TLS 实现，但关键安全语义未接通。

### 配置存在但未生效

`FederationTlsOpts` 声明：

- `expected_ca_ids`
- `min_tls_version`
- `verify_peer`

其中 `expected_ca_ids` 和 `min_tls_version` 没有实际使用。

### 身份信息为空

Accepted 事件返回：

```json
"peer_pubkey": "",
"peer_ca_id": ""
```

因此无法进行真正的 peer identity、授权、审计或 fencing。

### 可完全跳过证书验证

`verify_peer=false` 时安装 `AcceptAllVerifier`，任何证书都被接受。

### 服务端信任模型错误或不完整

服务端 mTLS 使用公共 WebPKI root store，而不是 `expected_ca_ids` 或部署指定 CA。私有联邦证书体系无法形成稳定身份边界。

### 其他未闭环项

- `SetReadTimeout` 只打印“not yet implemented”；
- accept、connection 子任务使用裸 `tokio::spawn`，未纳入 Supervisor；
- 没有 handshake timeout；
- 没有最大连接数和每 peer 限额；
- 没有消息帧长度上限；
- IPv6 地址解析通过 `split(':')`，会失败；
- synchronous callback 在 I/O task 中调用，可能阻塞连接循环；
- 没有证书轮换、吊销、身份映射和审计日志协议。

### 结论

Federation 当前只能标记为：

> Experimental, disabled by default, trusted-network-only

正式产品不得宣传“安全 TLS 联邦已完成”。

---

# 4. P1：架构和运维阻断项

## P1-1 RoomActor 仍然只是状态镜像，不是状态所有者

`room_actor/actor.rs` 新增 `RoomActorState`，这是正向改进。

但实现明确说明：

```text
During migration, populated from Room
```

`from_room()` 每次仍分别读取：

- control；
- lifecycle；
- users；
- monitors；
- chart；
- round ID；
- live。

命令后再从共享 `Room` 重建镜像。

### 风险

- 读取仍不是单一原子快照；
- Actor state 和 Room state 是双份状态；
- 迁移期可能出现镜像滞后；
- 增加锁读取和 clone 开销；
- 新代码误以为 actor_state 是权威来源。

### 必须整改

RoomActor 必须直接拥有：

```rust
RoomState {
    control,
    lifecycle,
    members,
    monitors,
    chart,
    round,
    player_data,
}
```

外部只持有 `RoomHandle`，不能直接访问内部锁。

---

## P1-2 health/live、health/ready、metrics 仍不存在

`CHANGELOG.md:11` 宣称已实现：

- `/health/live`
- `/health/ready`

但源码没有对应 route。

Docker HEALTHCHECK 已从虚假的 health endpoint 改成：

```text
binary --version
```

这只证明镜像中二进制可以启动一个短命进程，不证明实际服务：

- 主进程仍活着；
- TCP listener 已绑定；
- HTTP listener 已绑定；
- DB 可用；
- WAL replay 成功；
- Supervisor 未 degraded；
- PersistenceWorker 正常；
- 关机未开始。

### 必须整改

实现：

- `/health/live`：进程事件循环仍活跃；
- `/health/ready`：关键依赖和状态全部可服务；
- `/metrics`：Prometheus/OpenMetrics；
- readiness 必须包含 WAL、DB、Supervisor、listener、插件事件分发状态。

---

## P1-3 RBAC 仍未接入生产入口

源码检索显示 `AdminAction` 和 `user_has_role` 的引用只存在于 `rbac.rs` 自身和测试。

未保护：

- admin kick；
- Room 管理；
- 插件安装/删除/reload；
- dead-letter replay；
- backup；
- 配置 reload；
- HTTP/WIT 管理接口。

因此 RBAC 当前是“数据模型”，不是“安全边界”。

---

## P1-4 PPB→PMP 内部接口仍无统一服务认证

配置文档明确说 `admin_token` 仍是预留字段。

同时内部 HTTP：

- permissive CORS；
- SSE/WS/动态插件路由没有统一认证；
- 依赖“受控网络”作为主要边界。

受控网络不是身份认证。正式生产至少应实现：

- mTLS 或短期 service JWT；
- audience、issuer、expiry、nonce；
- PPB service identity；
- 管理 API scope；
- 全链路 request ID 和审计。

---

## P1-5 Migration 仍是双 schema 真理源

没有发现：

```rust
sqlx::migrate!()
```

或等价 runner。

`db.rs` 仍持有运行时建表逻辑，同时文档要求使用 `migrations/`。

### 风险

- migration 文件和实际建表 SQL 漂移；
- 升级顺序无法验证；
- rollback 文档没有真实执行器；
- schema version 可能被手写插入，而非迁移框架控制。

正式生产必须以 migration 文件作为唯一 schema 来源。

---

## P1-6 Backup/restore 不包含完整权威数据

`backup.rs` 只复制固定路径：

- `data/persistence-worker.wal.jsonl`
- `data/persistence-dead-letter.jsonl`
- `data/extensions.json`
- 配置文件

问题：

- 忽略自定义 WAL/dead-letter 配置路径；
- 不包含 PostgreSQL；
- 不包含插件二进制和 capability sidecar；
- 不包含 instance marker；
- 没有一致性 freeze/flush；
- 没有 restore，只提供 verify；
- 没有加密；
- 没有恢复演练自动测试。

文档中的 `pg_dump` 只是人工命令，不是产品级备份事务。

---

## P1-7 插件 Manifest 无签名和来源证明

当前插件由：

```text
.wasm + .capabilities.json
```

组成。

没有：

- 插件包签名；
- 内容 hash pinning；
- 发布者身份；
- 版本约束；
- SDK/WIT compatibility 声明；
- 安装前安全审核；
- capability grant 审批记录；
- rollback package。

本机文件被替换后，capability sidecar 可随之被篡改。

正式第三方插件平台需要签名 manifest 和可信发布流程。

---

# 5. CI/CD 与供应链审计

## 已有进步

- workspace check；
- all-features tests；
- Clippy `-D warnings`；
- 锁文件构建；
- tag/version 一致性；
- Linux/Windows 制品；
- Linux build provenance。

## 仍不符合生产要求

### audit/deny 不是硬门禁

Build workflow 中：

```bash
cargo audit ... || echo
cargo deny ... || echo
```

严重漏洞或许可证违规不会阻止发布。

### SBOM 可被跳过且未附加到 Release

Release 中安装和生成失败都只 echo，且 release files 列表没有包含 `.cdx.json`。

### 缺少制品完整性和签名

未生成：

- SHA-256/SHA-512 清单；
- cosign/minisign 签名；
- 容器镜像签名；
- SDK 包签名。

### smoke test 不启动真实服务

仅执行：

```text
--version
--help
```

没有：

- 加载生产配置；
- 启动 listener；
- readiness；
- 建立 TCP session；
- WAL admission/replay；
- 插件加载；
- SIGTERM shutdown。

### auto-version 在质量检查前推送

自动版本提交可能发生在 check/test 之前，污染主分支历史。推荐由 release PR 或 release job 在质量门禁后更新版本。

### feature matrix 不完整

只跑 all-features，仍应单独覆盖：

- no-default-features；
- PostgreSQL only；
- WIT/Wasmtime only；
- jemalloc on/off；
- Linux glibc 与目标生产镜像组合。

---

# 6. 文档与产品化审计

## 6.1 文档断链

本地 Markdown 链接检查：

- 检查本地链接：56
- 失效链接：8

失效项包括：

1. `README.md → PMP_PRODUCTION_PRODUCTIZATION_AUDIT.md`（重复两处）
2. `README.md → HARDENING_REPORT.md`
3. `README.md → PHASE2_HARDENING_REPORT.md`
4. `README.md → docs/functional-reliability-audit.md`
5. `README.md → docs/architecture-hardening.md`
6. `README.md → docs/static-verification.md`
7. `SECURITY.md → docs/security/data-privacy.md`

`(16)` 删除了多份报告和 archive，但没有同步清理 README 链接，是明显产品文档回归。

## 6.2 文档承诺超过实现

### 健康端点

CHANGELOG 宣称 health endpoints 已实现，源码不存在。

### Telemetry durability

`docs/guarantees.md` 声称 Telemetry 未提交批次可通过 WAL 恢复，但实际 `wal_id=None`，提交后也无法完成 ACK 闭环。

### retry until durable

文档声称 retry until durable，但普通 ACK 超过预算后会从重试队列删除。

### 旧“没有 WAL”文本仍存在

`docs/configuration.md` 仍包含“没有 enqueue-before WAL”的旧阶段表述；`server/query.rs` 诊断说明也残留类似文本。

### 当前状态引用不存在审计报告

README 把不存在的 audit 文件作为当前生产状态真相源。

## 6.3 配置 Schema 不完整

JSON Schema 未完整覆盖 `PlusConfig` 和嵌套配置，例如：

- monitors；
- extensions_file；
- benchmark_phira_tokens；
- rbac；
- idle；
- runtime_v2.telemetry_batcher；
- runtime_v2.telemetry_cutover_mode；
- runtime_v2.phira_http；
- WasmRuntimeConfig 的完整字段。

由于 `PlusConfig` 使用 `deny_unknown_fields`，Schema 与真实解析器不一致会直接伤害产品配置体验。

---

# 7. 静态验证结果

本轮实际完成：

- `(15) → (16)` 全仓差异分析；
- 63 个差异文件核查；
- 关键持久化、WAL、Telemetry、RoomActor、Federation、HTTP、RBAC、Migration、Backup、CI/CD 控制流审计；
- 11 个 TOML 全部可解析；
- 5 个 YAML 全部可解析；
- 2 个 JSON 全部可解析；
- 131 个 Rust 文件清点；
- 39 个 Markdown 文件清点；
- 12 个集成测试文件清点；
- 56 个本地 Markdown 链接检查，8 个失效。

当前执行环境没有可用 `cargo` / `rustc`，因此未执行：

```bash
cargo check --locked --workspace --all-targets --all-features
cargo test --locked --workspace --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo build --locked --release
```

静态审计不会把未执行命令描述为通过。

---

# 8. 正式生产前的强制整改顺序

## 阶段 A：数据终态（P0）

1. 让 `wal_id` 从 PersistenceWorker 贯穿 TelemetryItem；
2. DB commit 后执行可靠批量 ACK；
3. 禁止 ACK `try_send` 丢弃；
4. Telemetry ACK 失败进入 durable retry；
5. Flush/Shutdown 等待所有 ACK 收敛；
6. Shutdown pending ACK 必须返回 Err；
7. ACK 达到重试预算后不得删除意图；
8. reserve queue permit 后再 WAL admit；
9. 队列满不得返回“已接受但等重启”；
10. 修复完整无换行尾帧；
11. 增加 kill -9、磁盘写满、ACK I/O 失败、队列满故障测试。

## 阶段 B：服务身份与运行状态（P0/P1）

1. 实现 live/ready/metrics；
2. listener bind 必须参与启动 barrier；
3. PPB→PMP mTLS 或 JWT 服务认证；
4. RBAC 接入全部管理入口；
5. 审计日志包含 actor、action、target、request ID、result；
6. Federation 默认关闭；
7. Federation CA/identity/min TLS/timeout/连接限额真实接线。

## 阶段 C：架构边界（P1）

1. Room 全状态 Actor-owned；
2. 删除共享 Room 多锁写入口；
3. canonical domain event pipeline；
4. migration runner 成为唯一 schema 来源；
5. 插件进程隔离或明确只支持可信插件；
6. 插件签名 manifest 与版本兼容策略。

## 阶段 D：运维与供应链（P1）

1. PostgreSQL 一致性备份；
2. 自动 restore；
3. 定期恢复演练；
4. audit/deny 变为硬门禁；
5. SBOM 必须生成并发布；
6. 制品 checksum、签名、provenance；
7. 容器镜像扫描和签名；
8. 真实服务 smoke test；
9. 24–72 小时 soak；
10. 容量和故障注入报告。

## 阶段 E：文档产品化

1. 修复全部失效链接；
2. 建立唯一当前状态文档；
3. 历史报告放入 versioned archive；
4. guarantees 逐条绑定真实测试；
5. Schema 自动从 Rust config 生成或契约测试；
6. 将目标性能与实测性能分开；
7. Federation 标记 experimental；
8. 发布说明包含升级、回滚、已知限制和数据迁移。

---

# 9. 建议的生产验收门槛

只有全部满足后才允许标记 `production-ready`：

## 数据可靠性

- Touch/Judge commit 后 WAL 不再 replay；
- ACK I/O 连续失败时 readiness=false，shutdown=false；
- 队列满时无“等待重启才处理”的 accepted 事件；
- 完整无换行 WAL 帧不丢失；
- kill -9 后所有 admitted 事件可恢复；
- dead-letter 与 DB 双失败时 admission 保留。

## 稳定性

- 5,000 并发连接；
- 慢认证不影响 accept；
- 慢消费者不影响正常用户；
- 热点房间并发 join/leave/start 无状态破坏；
- 72 小时 soak 无未解释 RSS 增长和队列积压。

## 安全

- PPB 服务身份强制验证；
- RBAC 端到端测试；
- 插件 capability 默认拒绝；
- Federation peer identity 可验证；
- `verify_peer=false` 在 production profile 禁止；
- secrets 不进入日志、备份和错误响应。

## 运维

- live/ready/metrics 可用；
- PostgreSQL backup/restore 演练通过；
- 告警和 runbook 可执行；
- SIGTERM 在 deadline 内完成；
- Release 制品可复现、带 SBOM、checksum、签名和 provenance。

---

# 10. 最终判断

`(16)` 的进步是真实的，特别是：

- DB/replay 顺序；
- WAL 删除/清零 fail-closed；
- Telemetry ACK 架构骨架；
- Room Actor 迁移结构；
- provenance 和运维文档。

但当前最重要的“数据库提交—WAL ACK—Flush/Shutdown”链仍然没有成立，且新增 Federation 在身份、CA、超时和任务治理方面尚未达到安全使用条件。

因此最终结论是：

> **部分符合上一轮审计要求；仍是预生产加固候选版；正式生产发布 NO-GO。**

最优先需要修复的不是继续扩展 Federation 或新增产品功能，而是先关闭：

1. Telemetry `wal_id` 传递；
2. durable ACK；
3. shutdown 正确失败；
4. 队列 reserve/admit 原子性；
5. WAL 尾帧恢复；
6. service identity/readiness；
7. Room 全状态所有权。
