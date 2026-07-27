# PMP `(22)` 愿景对齐继续审计报告

## 1. 审计定位

本报告继续沿用已经确定的 PMP 项目愿景，而不是套用企业级后端平台标准：

> PMP 是以 Phira+ 为第一使用场景、全面依赖 PostgreSQL、保留完整插件 API 和原始多人游戏数据的稳定、高性能、高扩展 Phira 多人游戏服务端与运行时。项目当前处于快速迭代期，不承担历史兼容义务，应优先完成结构切换、删除迁移残渣，并用真实 Benchmark 证明能力。

本轮审计接受项目方已经确认的事实：

- 当前版本已经通过 `cargo check`；
- 当前版本已经完成 build；
- 当前版本已经通过现有 Clippy 门禁。

本轮没有重复消耗环境时间执行上述命令，也没有在未获确认的情况下把完整测试矩阵单独标记为通过。

PGO 不再作为当前项目生产化或发布的硬要求。PGO 属于性能优化工具，不是正确性、稳定性或扩展性的前提。

---

# 2. 总体结论

`(22)` 是近几版中清理最明显的一次更新。

相对于 `(21)`：

- 约 113 个文件发生变化；
- 新增约 5,028 行；
- 删除约 7,212 行；
- Server Rust 代码从约 44,167 行下降到约 41,771 行；
- 删除了一批旧 facade、旧 Benchmark、旧 Simulation 和迁移状态文件；
- PostgreSQL-only、HighFrequencyWriter、Room Actor 和 PROXY protocol 等方向继续推进。

当前版本已经不再是“只加新结构、不删旧结构”的纯叠加状态，但仍未完成全部 cutover。

最准确的当前定位是：

> **0.5 快速迭代开发版：核心方向正确、已具备可编译构建基础，但仍存在数个运行时正确性问题和真实性能验证缺口。**

不建议因为 PGO 失败而阻塞项目；也不能因为 check/build/Clippy 通过，就认为运行时语义和性能已经验证完毕。

---

# 3. 当前评分

| 领域 | 评分 | 结论 |
|---|---:|---|
| 愿景一致性 | 8.5/10 | PostgreSQL、完整插件 API、原始数据和 Benchmark 方向已经统一 |
| 代码清理 | 7/10 | 本轮删除量明显大于新增量，迁移垃圾继续减少 |
| PostgreSQL 数据架构 | 7/10 | 文件 RoundStore 基本退出，但高频 COPY 有事务一致性问题 |
| Room Actor | 7/10 | 权威状态继续向 Actor 收敛，成员切换仍有双路径与回滚问题 |
| 插件扩展性 | 8.5/10 | API 保持完整，插件 TCP 保留，符合愿景 |
| Benchmark 命令结构 | 7/10 | 新体系进一步成型 |
| Benchmark 真实性 | 4/10 | Real 已能走 TCP，但仍是单客户端冒烟；Simulation 仍偏 shadow-world |
| 代理部署能力 | 3/10 | 新增真实 PROXY parser，但当前实现存在会导致功能不可用的错误 |
| CI / 构建 | 7.5/10 | check/build/Clippy 已通过；PGO 应退出硬门禁 |
| 当前阶段 | Development Preview | 适合继续快速迭代，不宜宣称真实性能已证明 |

---

# 4. 本轮已经完成的重要改进

## 4.1 PostgreSQL-only 方向继续完成

`RoundStore` 已经转为 PostgreSQL 接口，不再以本地 JSON/JSONL 作为正常权威存储路径。

旧的无数据库运行模型已经明显减少，符合：

```text
PMP
→ PostgreSQL 必需
→ migration 成功
→ 服务启动
```

这一方向不能回退。

## 4.2 Touch/Judge 高频数据独立写入路径已成型

生产高频数据继续使用：

```text
Session telemetry
→ Room Actor cache
→ HighFrequencyWriter
→ PostgreSQL batch/COPY
```

并维护：

- received；
- committed；
- retrying；
- dropped；
- queue depth；
- oldest batch age。

这符合既定数据策略：

- Touch/Judge 必须持久化；
- 高频数据默认绕过 WAL；
- 正常运行和正常关闭尽可能完整提交；
- 极端崩溃窗口允许有限损失；
- 失败必须可观测。

## 4.3 Room Actor 状态进一步集中

Room Actor 已承载更多：

- lifecycle；
- chart；
- round；
- player data；
- display name；
- members ID；
- live 状态。

`Room` 正逐步退化为连接引用和广播基础设施，这是正确方向。

## 4.4 Benchmark Real 模式不再完全空白

新 Real 模式已经能：

- 连接真实 PMP TCP；
- 使用真实协议命令；
- 执行认证、建房、选谱、请求开始、准备和 Played；
- 启动本地 Mock Phira 服务。

相比 `(21)` 的纯 TODO，这是实质进步。

## 4.5 旧文件清理明显推进

本轮已经删除或替换：

- `actor_runtime.rs`；
- `idle.rs`；
-旧 `persistence_worker.rs` facade；
-旧 `proxy_protocol.rs` HTTP XFF 模块；
- `benchmark_simulation.rs`；
- `server/benchmark.rs`；
- `simulation_realistic.rs`；
-旧顶层 `telemetry.rs`。

同时增加了：

- `trusted_forwarded_http.rs`；
- `server/proxy_protocol.rs`；
- `session_lifecycle.rs`；
- `room_actor/lifecycle.rs`。

这符合“删除重构垃圾，而不是删除未来能力”的原则。

## 4.6 文档链接整洁度改善

本轮静态检查：

- 11 个 TOML 可解析；
- 6 个 YAML/YML 可解析；
- 2 个 JSON 可解析；
- 17 个 Markdown 文档未发现本地失效链接。

---

# 5. PGO：不再修，不再阻断

## 5.1 PGO 对 PMP 当前阶段不是必需项

PGO 只能在以下条件成立时产生可信收益：

1. 训练 workload 能代表真实运行；
2. 训练 profile 与最终目标平台一致；
3. 构建链稳定；
4. 能持续比较有无 PGO 的收益；
5. 不牺牲发布可靠性。

当前 PMP 的 Benchmark Real 仍只是单客户端冒烟，Simulation 也尚未完全走生产路径，因此即使 CI 成功生成 profile，也不能证明该 profile 对真实多人游戏负载有代表性。

所以目前继续修 PGO 的边际收益很低。

## 5.2 当前 CI PGO 设计本身存在问题

### Build workflow

PGO job：

- 没有安装 `llvm-tools-preview`；
- 假设 `llvm-profdata` 位于 `$(rustc --print sysroot)/bin`；
- 不依赖完整质量门禁；
- 训练 workload 代表性不足。

### Release workflow

存在更明显的产品语义问题：

- 只有一个目标可能实际采集 profile；
- musl、ARM64、Windows 等产物也被命名为 `pgo`；
- 没有 profile 时会回退普通构建，却仍使用 PGO 名称；
- PGO 训练启动了目标 Server，又由另一 Server 进程运行 benchmark；
- Benchmark 修改的是运行 CLI 的第二个 Server 的 Mock Phira endpoint，不是被压测目标 Server 的 endpoint；
- 被压测 Server 仍可能访问真实 Phira API；
- 当前硬编码 token 和单用户流程不适合作为训练负载。

这可以解释为什么 CI PGO 难以稳定工作。

## 5.3 推荐处理

当前直接执行：

1. 从普通 Build workflow 删除 PGO job；
2. 从 Release 必须步骤中删除 PGO；
3. 将所有 `-pgo` 制品名称恢复为普通平台名称；
4. 保留：

```toml
[profile.release]
lto = true
codegen-units = 1
```

5. Release 以普通 release build 为正式产物；
6. 将来 Benchmark 真实性完成后，再增加：

```text
workflow_dispatch
→ optional-pgo-x86_64-gnu
```

只支持一个明确的原生平台，失败不阻断发布，并输出 PGO 与非 PGO 的对比数据。

### 判定

> **CI PGO 失败不是当前版本缺陷，也不应计入 PMP 是否稳定、高性能、高扩展的否决条件。**

---

# 6. Benchmark 审计

## 6.1 Real 模式目前是“真实 TCP 冒烟”，不是完整 Benchmark

当前 Real 模式虽然真正连接了 TCP，但只创建一个连接，并执行固定流程：

```text
Authenticate
CreateRoom
SelectChart
RequestStart
Ready
Played
```

它没有使用大部分 Benchmark 配置：

- clients；
- rooms；
- duration；
- scenario；
- warmup；
- plugins；
- command rate。

也没有发送 Touch/Judge。

因此当前更准确的命名应是：

```text
real-smoke
```

而不是完整 Real Benchmark。

## 6.2 Real 报告命令数与实际命令不一致

代码实际发送的业务命令多于报告中的固定 `total_commands = 4`。

报告数据不能使用硬编码，必须由统一 Collector 实际累加。

## 6.3 等待响应没有超时

`wait_for_response()` 没有 deadline。

只要目标 Server 没有返回期望命令，Benchmark 就可能永久等待，CI 或本地运行会表现为卡死。

每个协议步骤应支持：

- step timeout；
- overall scenario timeout；
- 明确错误阶段；
- 最后收到的 ServerCommand；
-连接状态快照。

## 6.4 Mock Phira 已启动，但故障配置基本未生效

Mock 已经是实际 Axum 服务，这是进步。

但以下配置大多尚未真正执行：

- `delay_ms`；
- `jitter_ms`；
- `error_rate`；
- `timeout_ms`；
- `seed`；
- `response_size`；
-配置 listen address；
- verbose。

Mock 目前主要返回固定用户、固定谱面和固定成绩。

## 6.5 固定用户 ID 阻止多客户端测试

Mock 返回固定用户 ID，例如 999。

如果未来 Real 模式并发创建多个客户端，它们可能在 PMP 用户表中相互替换。

应根据 token 或 client sequence 生成确定性唯一用户：

```text
bench-token-000001 → user_id 1_000_001
```

## 6.6 Mock endpoint 修改目标错误

当 Benchmark 连接外部目标地址时，本地运行 Benchmark 的 Server state 与远端目标 Server 不同。

修改本地 `live_config.phira_api_endpoint` 不会改变远端目标的 Phira API endpoint。

推荐明确区分：

### 自托管模式

Benchmark runner 负责启动目标 PMP，并传入 Mock endpoint。

### 外部目标模式

要求目标 PMP 启动时已经配置 Mock endpoint，不允许 Benchmark 假装可以远程修改它。

## 6.7 固定房间 ID 会发生冲突

Real 模式使用固定 room ID。

连续运行、失败残留或并行 CI 时可能冲突。

应使用：

```text
bench-{run_id}-{worker_id}-{room_index}
```

## 6.8 Simulation 仍未完全走生产路径

Simulation 仍大量依赖旧 `SimulationManager` 和 shadow-world counters。

多个新场景仍只是旧 Balanced/RoundStorm/Idle 等负载的别名，并没有真正执行：

-真实连接；
-慢消费者；
-重连；
-WASM 插件；
-PostgreSQL；
-热点房间；
-混合负载。

报告中的部分延迟、队列、数据库和资源字段仍是默认值或零。

### 当前判定

- Benchmark 命令体系：基本成型；
- Real TCP smoke：已经存在；
- Real 多客户端 Benchmark：未完成；
- Simulation 生产路径：未完成；
- Benchmark 可用于证明高性能：暂时不能。

---

# 7. 高频 PostgreSQL 写入的关键问题

## 7.1 队列满时实际不是“丢弃”，而是等待

`HighFrequencyWriter::enqueue()` 使用：

```rust
tx.send(item).await
```

对于有界 Tokio mpsc：

- 队列满时，`send()` 会等待；
- 只有 channel 关闭时才返回错误。

但当前日志和指标将失败描述为：

```text
queue full; item dropped
```

这与真实行为不一致。

更重要的是，该调用位于 Session 高频命令路径。数据库变慢时可能导致：

```text
Touch/Judge
→ 等待持久化队列容量
→ Session 命令处理阻塞
→ 后续 packet/heartbeat 延迟
```

这与高性能多人游戏运行时目标冲突。

### 建议

明确一种产品策略：

#### 推荐策略

```text
try_send
→ 成功：进入队列
→ 满：短 timeout
→ 仍满：丢弃该高频批次并计数
```

或者增加每 Session 的短小 staging buffer。

高频数据不能无限期阻塞 Session/Room 热路径。

## 7.2 COPY 路径缺少原子事务

当前 COPY 大致依次写入：

1. runtime batch header；
2. runtime telemetry items；
3. canonical Touch/Judge 表。

这些操作在同一连接上执行，但没有处于同一 PostgreSQL 事务中。

如果第二步或第三步失败：

- 前面的 COPY 可能已经提交；
- 重试时出现部分数据；
- fallback 路径可能因 batch header 已存在而跳过；
- 数据可能永久缺少 item 或 canonical 表记录。

## 7.3 COPY fallback 的幂等逻辑可能把不完整批次当作完成

Fallback 使用：

```sql
ON CONFLICT(event_id) DO NOTHING
```

如果 COPY 已经插入 batch header，但后续失败，fallback 看到 event_id 冲突后可能直接跳过后续写入。

结果是：

```text
header 存在
items 缺失
canonical Touch/Judge 缺失
```

但该事件以后也无法自动补齐。

## 7.4 推荐短期方案

在实现正确 staging COPY 前，建议：

> **暂时关闭 COPY 快速路径，统一使用当前事务化批量 INSERT。**

性能可能下降，但数据语义正确且容易基准验证。

## 7.5 推荐长期方案

```text
COPY
→ temporary / unlogged staging table
→ transaction
→ INSERT/MERGE batch header
→ INSERT/MERGE items
→ INSERT canonical tables
→ COMMIT
```

同时使用稳定 event ID 和唯一键。

高频数据可以允许崩溃窗口损失，但不能允许正常数据库错误制造永久半写状态。

---

# 8. PostgreSQL cutover 仍有残余

## 8.1 database_url 仍允许为空并猜测连接参数

当前配置允许空 URL，并自动尝试：

- 本地 TCP；
- Unix socket；
-当前 OS 用户；
- `postgres`；
- `pmp`；
-常见开发密码。

既然 PostgreSQL 是必需项，推荐：

```text
database_url 必填
```

测试和开发配置也显式提供地址。

自动猜测可能连接到非预期数据库，也使启动错误难以理解。

## 8.2 degraded database 配置应删除

类似：

```text
allow_database_degraded_mode
```

与 PostgreSQL 必需的架构决定冲突。

如果只为测试存在，应放到测试构造器或 test-only feature，不能进入正式配置。

## 8.3 持久化仍保留不可能状态

代码中仍存在：

- `SkippedNoDatabase`；
-可选 DB 分支；
- `EventMirror`；
- `DirectWrite`；
-旧 mirror/cutover 名称。

这些名称不再反映最终架构，应一次性删除。

## 8.4 extensions 中仍存在 async 内 block_on

某扩展持久化函数在 async 上下文里使用：

```rust
futures::executor::block_on(worker.enqueue(...))
```

这可能阻塞执行线程，也没有必要。

应直接：

```rust
worker.enqueue(...).await
```

并同步重命名已经失真的 `enqueue_or_write_direct`。

## 8.5 本地 JSON 与 PostgreSQL 的权威关系需明确

当前部分管理、扩展和欢迎配置仍同时写本地 JSON 与 PostgreSQL。

需要给所有文件分类：

### 配置

手工编辑、Git 管理，可保留文件。

### 缓存

可删除、可重建，文件不是权威源。

### 导出

只用于查看或备份，不能被运行时重新读作权威数据。

### 业务数据

只进入 PostgreSQL。

不能继续让同一数据同时由 JSON 与 PostgreSQL 决定。

---

# 9. Room Actor 剩余一致性问题

## 9.1 Room 作为连接注册表可以保留

`Room.users` 和 `Room.monitors` 如果只保存 Weak Session/User 引用并用于广播，可以保留。

关键要求是：

-业务成员关系以 Actor member ID 为准；
-连接注册表不参与房主、准备、容量或生命周期判断。

## 9.2 创建空房间的配置时序错误

`create_empty_room` 中可能在：

- 房间尚未加入注册表；
- Actor 尚未注册；

之前调用房间 gateway 设置 Phira endpoint。

该调用可能失败但被忽略。

初始化参数应在 Actor spawn 时一次性传入，而不是先创建半初始化 Room，再调用运行期 gateway 补字段。

## 9.3 `persistent_empty` 返回成功但没有真正持久化

当前接口可能返回：

```json
{"ok": true}
```

并触发插件事件，但代码注释承认状态尚未真正保存。

这是错误的 API 语义。

只能二选一：

1. 实现 Room Actor command 和 PostgreSQL 字段；
2. 暂时删除该命令或返回 `not_implemented`。

不能继续“假成功”。

## 9.4 空房间首个用户成为房主的行为可能未实现

文档声称第一个普通玩家进入没有房主的空房间时成为房主，但现有 creator/host fallback 逻辑可能留下空 host。

必须增加业务级测试：

```text
create empty room
→ first normal user joins
→ actor host == user
```

## 9.5 Join 失败缺少回滚

当前流程可能先：

```text
Actor add member
```

再：

```text
Room connection registry add user
```

如果连接注册表因容量或其他原因失败，Actor 已经留下幽灵成员。

应由统一 gateway：

```text
reserve / validate
→ Actor transition
→ registry update
→ failure rollback
```

更理想的是连接注册表容量不再独立决定业务容量。

## 9.6 Leave 仍存在 direct fallback

Actor remove 失败后仍可能走直接 Room 修改。

这重新引入双执行路径。

快速迭代阶段不需要兼容：Actor 不可用就显式失败或关闭 Session，不能换另一套状态模型继续执行。

---

# 10. 游戏 TCP PROXY protocol 实现存在逻辑错误

本版终于为玩家 TCP 添加了真正的 PROXY protocol 解析方向，这是必要能力。

但当前实现尚不能可靠使用。

## 10.1 PROXY v1 前缀被重复

流程先用 `peek()` 判断开头为：

```text
PROXY 
```

随后 `read_proxy_v1()` 又预先把 buffer 填入 `PROXY `，再从 socket 开头读取。

由于 peek 不消费数据，最终内容可能变成：

```text
PROXY PROXY TCP4 ...
```

导致解析失败。

## 10.2 Tokio socket 转 std 后仍是 nonblocking

`TcpStream::into_std()` 得到的 socket仍为非阻塞。

设置 `set_read_timeout()` 不会自动把它变成阻塞 socket。

因此：

- `peek()`；
- `read_exact()`；

都可能在数据稍晚到达时立即返回 `WouldBlock`。

PROXY v1/v2 都可能随机失败。

## 10.3 错误分支可能 panic

某个 `into_std` 错误分支尝试连接：

```text
127.0.0.1:1
```

并 `expect()` 生成替代 socket。

该连接大概率失败，导致 panic。

错误分支不能通过制造一个必然失败的连接来恢复所有权。

## 10.4 限流发生在解析代理地址之前

当前连接限流可能先按代理服务器 IP 计算。

所有通过同一 HAProxy 的玩家会共享一份 IP 限额，而 forwarded player IP 没有再次独立限制。

应分别维护：

- proxy peer 限额；
- forwarded client IP 限额；
-全局 pending auth 限额。

## 10.5 推荐重写为纯 async parser

不要在 async 接入路径中转换成 std socket。

推荐：

```text
Tokio TcpStream
→ timeout 内读取有界前缀
→ 判断 PROXY v1/v2/无代理
→ parser
→ 保存未消费 payload
→ 交给原 Phira packet decoder
```

并增加真实 socket 测试：

- v1 完整头；
- v1 分段到达；
- v2 完整头；
- v2 分段到达；
-无 PROXY；
-错误签名；
-超长头；
-超时；
-不受信代理；
-IPv4/IPv6；
-解析后首个 Phira packet 不丢字节。

当前 parser 的纯字节单元测试不足以发现这些接入错误。

---

# 11. 插件系统审计

## 11.1 API 保持完整是正确的

本轮不建议缩减任何现有 WIT API。

## 11.2 插件 TCP 需要资源配额

继续补齐：

- 每插件 listener 上限；
-每插件连接上限；
-每连接读写预算；
-总字节速率；
- connect/listen allowlist；
-任务数；
-事件队列；
-unload 清理；
-按插件 metrics。

## 11.3 WIT 集成测试存在静默跳过

部分测试采用：

```rust
let Ok(component) = try_load_component() else {
    return;
};
```

或者在加载失败时什么也不做。

这意味着：

> WIT fixture 已过期、插件无法加载时，测试仍可能显示成功。

插件 API 是项目战略能力，因此 conformance test 必须：

- fixture 加载失败即失败；
- Server/WIT/SDK 版本不一致即失败；
- CI 自动重建 fixture；
-每个 API 至少有一个真实调用；
-capability 拒绝路径必须验证。

check/build/Clippy 不能替代插件 ABI 兼容测试。

---

# 12. 代码整洁度

## 12.1 本轮清理成绩明显

Server Rust 代码减少约 2,396 行，说明项目已经开始真正删除迁移代码，而不只是堆叠新文件。

这是正确趋势。

## 12.2 仍存在超大文件

目前仍有多个约 800–1,500 行文件，包括：

- `simulation.rs`；
- `plugin.rs`；
- WAL；
- WIT host；
- TUI；
- Room CLI；
- Room actor handler；
- Benchmark CLI；
- Benchmark report；
- persistence worker；
- config。

不要求机械按 500 行拆分，但每个大文件都应回答：

> 它是否包含两个以上不同业务所有权？

按领域拆分，而不是按函数数量拆。

## 12.3 Clippy 通过的含义需要准确表述

`lib.rs` 仍全局允许约 30 类 Clippy lint，其中包含：

- `clone_on_copy`；
- `useless_format`；
- `manual_map`；
- `useless_vec`；
- `collapsible_match`；
- `too_many_arguments`；
- `type_complexity`；
- `large_enum_variant`。

因此当前结论应是：

> **代码通过了项目当前定义的 Clippy 策略。**

不能解释成：

> **所有代码风格问题均已清理。**

建议分两类处理：

### 直接修复

- clone_on_copy；
- useless_format；
- manual_map；
- useless_vec；
- collapsible_if/match；
- needless return 等。

### 局部允许

- too_many_arguments；
- type_complexity；
- large_enum_variant；
- async trait 相关结构。

每个局部 allow 写中文原因。

---

# 13. CI 和发布建议

## 13.1 当前必要门禁

保留并确保：

```text
cargo check --workspace --all-targets
cargo build --release
cargo clippy --workspace --all-targets
cargo test --workspace
```

如果完整 tests 已在 CI 通过，应在 Release 中以该 job 为依赖。

## 13.2 删除 PGO 硬门禁

PGO 相关 job：

- 从普通 build 删除；
-从 release 必需步骤删除；
-产物取消 `pgo` 后缀；
-文档不再承诺 PGO；
-失败不再让 CI 红灯。

## 13.3 将来可选 PGO

后续 Real Benchmark 完成后，可以单独添加：

```text
Manual Performance Build
```

输出：

- baseline throughput；
- PGO throughput；
- p99；
- CPU；
-二进制大小；
-构建日志；
-profile hash。

只有实际收益稳定时才采用。

---

# 14. 当前遗留文档与命名

虽然本地链接已经修复，但正文仍有旧内容：

- `runtime_v2`；
- mirror/cutover；
- direct/worker 模式；
- hybrid benchmark；
- benchmark token；
-旧 Simulation 描述；
-旧 EventMirror 名称；
-数据库 degraded mode。

这些不一定造成编译错误，但会继续误导开发者。

建议在下一次持久化和 Benchmark cutover 完成后统一删除，而不是继续写“新旧并存说明”。

---

# 15. 下一轮优先级

## P0：运行时正确性

1. 暂时关闭非事务 COPY，或改成 staging + transaction；
2. 修复 HighFrequencyWriter 队列满语义，不能无限阻塞 Session；
3. 重写 PROXY protocol async 接入；
4. 修复 Room join rollback；
5. 删除 Room leave direct fallback；
6. 实现或删除 persistent_empty 假成功；
7. 删除 extensions 中 async `block_on`。

## P1：Benchmark 真实性

1. 将当前 Real 重命名或视为 smoke；
2. 实现 N clients / N rooms；
3. 使用唯一 token/user/room；
4. 所有等待增加 timeout；
5. Gameplay 发送真实 60/120 Hz Touch/Judge；
6. 让 Mock fault 参数真正生效；
7. 明确自托管目标和外部目标模式；
8. Simulation 走生产 Actor/Plugin/Persistence 路径；
9. 删除 hybrid/token 和 shadow alias 场景；
10. 报告字段全部由 Collector 实际计算。

## P2：完成 PostgreSQL cutover

1. database_url 必填；
2.删除自动密码猜测；
3.删除 degraded database mode；
4.删除 no-database impossible state；
5.重命名 EventMirror/DirectWrite；
6.业务数据停止 JSON 双写；
7.所有扩展持久化 async 化。

## P3：插件与清理

1. WIT fixture 加载失败必须使测试失败；
2.完整 API conformance；
3.插件 TCP 配额；
4.清理全局 Clippy allow；
5.删除剩余迁移术语；
6.拆分剩余超大领域文件。

## P4：可选优化

1. 真实 Benchmark 稳定；
2.公开标准硬件结果；
3.根据 profile 处理热点；
4.最后再考虑手动 PGO。

---

# 16. 最终判断

`(22)` 的方向继续变好，本轮删除量、PostgreSQL-only、Room Actor、真实 TCP Benchmark 冒烟和玩家代理能力都说明项目正在走出迁移泥潭。

但最重要的当前事实是：

```text
check / build / Clippy 通过
≠ 高频数据库写入原子
≠ PROXY protocol 可用
≠ Room 成员切换无竞态
≠ Benchmark 已证明高性能
```

PGO 不应再占用开发精力。

接下来最值得投入的四件事是：

```text
高频数据事务正确性
→ PROXY 协议正确性
→ Room 成员切换一致性
→ Benchmark 真实性
```

这四项完成后，PMP 才真正接近：

> **能力完整、数据路径清晰、核心状态一致、性能可复现的新一代 Phira 多人游戏运行时。**
