# PMP 愿景对齐审计报告 V2

## 1. 项目重新定位

PMP 的目标不是企业级通用后端平台，也不是单纯的 Phira 私服实现，而是：

> 以 Phira+ 框架为第一使用场景，稳定、高性能、高扩展的 Phira 多人游戏服务端与多人游戏运行时；同时保持足够开放的能力，使其未来可以承载超出 Phira+ 的社区插件、玩法与基础设施创新。

当前项目处于几乎没有外部用户的快速迭代期，因此：

- 不需要承担历史兼容包袱；
- 不应为了旧结构保留 legacy、fallback、mirror、runtime_v2 等长期迁移层；
- 可以在 1.0 前主动实施破坏性重构；
- 重点是形成一个结构清晰、能力完整、可快速继续扩展的基线。

## 2. 本轮修正后的核心原则

### 2.1 插件 API 不缩减

现有插件 API 是为 Phira+ 与社区开发者铺路，API 数量本身不是主要维护成本。

保留并继续扩展：

- 房间、用户与运行时查询；
- 消息发送；
- 管理能力；
- 配置能力；
- 持久化能力；
- HTTP client；
- timer；
- crypto；
- 插件原始 TCP；
- Simulation/Benchmark 相关能力；
- 未来新增的社区扩展能力。

真正需要治理的不是 API 数量，而是：

- 接口命名；
- 类型一致性；
- capability 边界；
- 错误码；
- 文档自动生成；
- API 版本号；
- conformance tests；
- 调用路径是否统一。

不划分 stable/experimental 两套世界。1.0 前允许整体 API 发生破坏性更新，发布时统一提升插件 API 版本。

### 2.2 PostgreSQL 是必需基础设施

Phira+ 全面使用 PostgreSQL，因此 PMP 不再维护无数据库运行语义。

应删除：

- `DbManager::None`；
- 本地文件数据库 fallback；
- 无数据库兼容分支；
- direct-only / worker-preferred / worker-authoritative 等迁移模式；
- mirror/cutover 统计；
- 双 schema、双写和过渡性兼容路径。

启动规则：

```text
PostgreSQL 连接失败
→ migration 失败
→ 必需表或扩展不可用
→ PMP 拒绝启动
```

数据库 schema 只保留一个真实来源。

### 2.3 原始 Touch/Judge 必须持久化

Touch、Judge 等原始数据是未来 Phira+ 分析、回放、风控、训练和新玩法的基础，不能删除。

但不要求所有数据都经过 WAL。

建议按数据语义分级：

#### A 类：需要 WAL 的关键数据

- round open/close；
- 最终成绩；
- 用户加入/离开；
- 封禁和管理操作；
- 关键房间状态；
- 插件关键持久化数据。

语义：

```text
WAL admission
→ PostgreSQL transaction
→ durable ACK
```

#### B 类：绕过 WAL 的高频数据

- raw Touch；
- raw Judge；
- 高频帧数据；
- 高频运行时遥测。

语义：

```text
有界内存批次
→ PostgreSQL COPY / batch insert
→ 有限内存重试
→ 可观测的失败计数
```

允许进程突然崩溃时损失尚未提交的小窗口数据，但正常运行、数据库短暂抖动和正常关闭时应尽量完整提交。

应显式记录：

- received；
- committed；
- retrying；
- dropped；
- queue depth；
- oldest batch age。

### 2.4 无历史包袱，立即完成 cutover

删除所有仅用于“新旧结构同时运行”的层：

- legacy；
- orig；
- fallback；
- mirror；
- runtime_v2；
- migration_in_progress；
- 旧字段 aliases；
- 旧配置兼容解析；
- 双事件入口；
- 双状态源；
- 双持久化路径。

当前阶段应一次性完成结构切换，而不是继续维护两代系统。

### 2.5 TUI 保留

TUI 已基本完成，未来更新成本不高，可以保留。

只要求：

- 命令按领域重新分组；
- 删除重复命令；
- TUI 不直接绕过 Room/Session/Plugin/Persistence 的统一入口；
- TUI 只作为控制界面，不拥有业务逻辑。

### 2.6 Benchmark 和 Simulation 保留在核心工程

Benchmark 是证明 PMP 稳定性与性能的核心产品能力，不应移出工程。

Simulation 应从属于 Benchmark，作为 Benchmark 的一种运行模式，而不是平级系统。

推荐模型：

```text
Benchmark
├─ simulation 模式
├─ real 模式
├─ suite
├─ metrics
├─ report
└─ profile
```

### 2.7 Backup 分离

Backup/Restore 不属于多人游戏运行时热路径。

移动为独立管理工具或脚本：

```text
pmp-admin backup
pmp-admin restore
```

但仍放在同一仓库，随版本发布和测试。

### 2.8 Federation 更名为插件原始 TCP

当前功能不是集群 Federation，而是给插件开放受控 TCP 能力。

统一命名为：

- `plugin_tcp`
- `plugin-raw-tcp`
- `插件原始 TCP`

删除所有 Federation、peer federation、cluster federation 等误导性命名和文档。

### 2.9 工作区结构不做大迁移

不为了形式大拆 crates。

保留当前 workspace，只在现有 Server crate 内按领域整理模块。

### 2.10 语言规范

建议：

- Rust 标识符、WIT 标识符：英文；
- 代码注释：中文；
- CLI/TUI：中文；
- 日志说明：中文，结构化字段名保持英文；
- 用户文档：中文为主；
- 插件 SDK API 文档：中文为主，必要时补英文摘要。

这样既符合项目维护偏好，也不破坏 Rust/WIT 工具链生态。

## 3. Benchmark / Simulation 重构方案

### 3.1 单一命令入口

推荐：

```text
pmp benchmark run
pmp benchmark suite
pmp benchmark compare
pmp benchmark list
```

`simulation` 不再成为顶级命令。

示例：

```bash
pmp benchmark run \
  --mode simulation \
  --scenario gameplay \
  --clients 200 \
  --rooms 20 \
  --duration 60s
```

```bash
pmp benchmark run \
  --mode real \
  --scenario gameplay \
  --clients 200 \
  --rooms 20 \
  --duration 60s
```

```bash
pmp benchmark suite --preset standard
```

### 3.2 运行模式

#### simulation

进程内直接使用生产 Room/Session/Command 路径：

- 不经过真实 TCP；
- 使用本地 Mock Phira API；
- 可重复；
- 适合测核心状态机、Actor、插件和持久化；
- 不允许手动绕过生产 dispatcher 或自行持锁调用内部函数。

#### real

启动真实 PMP Server，并使用模拟客户端连接真实 TCP：

- 完整握手；
- 认证；
- packet encode/decode；
- Session queue；
- Room mailbox；
- broadcast；
- PostgreSQL；
- 插件；
- 本地 Mock Phira API。

Real 模式不访问真实 Phira API，避免公网波动、限流和 token 污染结果。

### 3.3 本地 Mock Phira API

Benchmark 运行时启动本地 Mock：

- 用户信息；
- 谱面；
- 成绩记录；
- 可配置延迟；
- 可配置错误率；
- 可配置超时；
- 可配置响应大小。

删除 Benchmark token 配置。

Mock 应支持固定 seed，保证结果可重复。

### 3.4 场景

保留并重命名：

- `room-lifecycle`
- `gameplay`
- `connection`
- `steady-state`
- `mixed`

删除 `chat_storm`。

新增推荐场景：

- `hot-room`
- `reconnect`
- `slow-consumer`
- `plugin-load`
- `database-write`
- `long-run`

`mixed` 必须代表多个负载同时发生，而不是顺序执行多个场景。顺序执行应命名为 `suite`。

### 3.5 Preset

```text
quick
standard
stress
soak
```

每个 preset 固定：

- clients；
- rooms；
- players per room；
- command rate；
- frame rate；
- duration；
- plugin set；
- PostgreSQL batch；
- Mock API delay。

### 3.6 输出

控制台输出建议分为：

1. 环境
2. 配置
3. 总体结果
4. 场景结果
5. 瓶颈
6. 正确性
7. 产物

关键字段：

- version / git commit；
- CPU / memory / OS；
- mode / scenario / seed；
- clients / rooms / duration；
- commands/s；
- messages/s；
- p50 / p95 / p99 / max；
- connect latency；
- Room mailbox depth；
- Session send queue depth；
- PostgreSQL rows/s；
- Touch/Judge committed/dropped；
- errors；
- invariant violations；
- RSS / allocation / GC；
- profile 文件。

支持：

```text
human
json
markdown
```

JSON 只输出机器数据到 stdout，日志和进度写 stderr。

## 4. 参考 Benchmark 文件的评价

参考实现中值得吸收：

- 单一 `benchConfig`；
- 单入口参数；
- JSON 与人类可读输出分离；
- 统一 Collector 和 Report；
- CPU/heap/goroutine/mutex/block profile；
- 本地 Mock Phira；
- 场景函数独立；
- mixed/suite 组合思路。

但不能直接照搬以下行为：

- `connection-storm` 只构造 User 并写入 map，不是实际连接压测；
- `mixed` 实际是顺序 suite；
- gameplay 以 CPU 极限速度循环，不符合 Phira 客户端帧率；
- room-cycle 在循环中反复发送 Played，不符合真实生命周期；
- steady-state 对同一房间 ID 反复 CreateRoom，场景语义不可靠；
- 手动持有全局锁/房间锁调用 handler，可能绕过正式 Actor/mailbox 路径；
- Mock Session 的 `sentCmds` 并发写入缺少同步；
- profiler 在 CPU profile 停止前就生成产物列表；
- `Verbose` 未使用；
- profile 参数语义不一致；
- 全部场景集中在一个 runner 文件，后续会再次变乱。

## 5. 插件系统治理

不缩减 API，但必须做到：

- 每个 WIT interface 对应一个清晰领域；
- 所有 API 使用类型化接口；
- 通用 JSON `api-call` 如果保留，只作为插件 API 快速迭代桥接，不允许绕过 capability；
- 每个方法声明 capability；
- 统一错误类型；
- 统一异步/同步语义；
- 自动生成 API 文档；
- 自动生成 capability 表；
- SDK 与 Server 版本绑定；
- conformance suite 覆盖所有 API；
- 插件 API 版本整体升级；
- 1.0 前可破坏性更新，不维护旧 ABI。

## 6. Proxy listener 的最终建议

不删除代理部署能力。

PPB 不应代理玩家与 PMP 的实时 TCP 流量。真实部署可以使用：

```text
Player
→ HAProxy / Nginx Stream / Envoy / LB
→ PMP TCP
```

PMP 应支持真正的：

- PROXY protocol v1/v2；
- trusted proxy allowlist；
- 独立 listener 或显式 transport 配置；
- 未受信代理拒绝；
- 原始 peer address 与 forwarded address 同时记录。

删除当前基于 X-Forwarded-For 或错误命名的 compatibility listener。

## 7. 应删除的重构垃圾

优先删除：

- `actor_runtime.rs` 中运行时迁移说明；
- `server/orig.rs`；
- Room 旧状态源和 Actor 镜像并存；
- Session inline fallback；
- Room inline fallback；
- Persistence mirror；
- cutover readiness；
- `runtime_v2` 命名；
- `DbManager::None`；
- 本地文件数据库 fallback；
- direct-only / worker-preferred / worker-authoritative；
- 双事件派发；
- 重复 gateway；
- 仅转发调用的薄门面；
- deprecated config aliases；
- 旧 schema 兼容代码；
- 源码字符串形态测试；
- 历史迁移状态文档；
- Federation 旧命名；
- Benchmark 旧命令和 chat_storm；
- Backup 运行时代码。

## 8. 明确保留的能力

不得因“简化”删除：

- 全量插件 API；
- PostgreSQL；
- raw Touch/Judge 持久化；
- TUI；
- Benchmark；
- Simulation；
- 插件原始 TCP；
- 内部 HTTP；
- Phira+ 所需的历史、房间、轮次、用户与插件持久化表；
- Profile 与性能报告；
- 代理部署支持。

## 9. 适合本项目的生产标准

### 稳定性

- 24 小时标准负载无崩溃；
- Room 不出现双房主、幽灵用户和重复开局；
- 慢客户端不拖累同房间正常客户端；
- 插件 trap 不影响 Server；
- PostgreSQL 短暂抖动可恢复；
- 正常关机完成关键事件、最终结果及高频批次 flush。

### 性能

首个公开基准：

```text
4 vCPU
8 GiB RAM
PostgreSQL
1000 clients
100 rooms
1 hot room
10 plugins
24 hours
```

公开：

- commands/s；
- messages/s；
- p50/p95/p99；
- CPU；
- RSS；
- GC；
- queue depth；
- PostgreSQL rows/s；
- raw Touch/Judge commit rate；
- error/drop rate。

### 扩展性

- 插件 API 完整；
- capability 真实生效；
- API 文档完整；
- 示例插件完整；
- API conformance suite；
- 插件异常有隔离和指标；
- 原始 TCP 能力受控；
- 新接口增加不要求改动核心 Room/Session。

### 可维护性

- 完成所有 cutover；
- 不存在 `orig/legacy/fallback/mirror/runtime_v2`；
- 单个领域只有一个状态源；
- 大文件按业务职责拆分；
- 中文注释和文档统一；
- TUI/Benchmark/Plugin API 命令组织一致。

## 10. 实施顺序

### 第一阶段：清理迁移垃圾

- 删除所有双路径；
- PostgreSQL 强制化；
- 删除无数据库和本地文件 fallback；
- 清理 legacy/orig/runtime_v2/mirror；
- Federation 全面更名 plugin_tcp；
- Backup 分离。

### 第二阶段：核心状态收敛

- Room Actor 独占 RoomState；
- Session 单一路径；
- canonical DomainEvent；
- TUI、插件、HTTP 全部调用统一 command/query gateway。

### 第三阶段：持久化定型

- 关键事件 WAL；
- raw Touch/Judge 绕过 WAL；
- PostgreSQL 单一写入模型；
- 批次与队列指标；
- 删除过渡模式和双表。

### 第四阶段：Benchmark 重做

- Simulation 从属于 Benchmark；
- local Mock Phira；
- real/simulation 两种模式；
- 删除 token；
- 删除 chat_storm；
- 修正场景真实性；
- 统一命令、报告和 preset。

### 第五阶段：插件与文档

- 不缩减 API；
- 统一 capability；
- 补齐 conformance；
- 生成式 API 文档；
- 中文文档统一；
- 清除所有旧架构陈述。

## 11. 最终结论

PMP 不需要为了“简化”牺牲未来能力。

真正需要做的是：

> 保留广阔的插件 API、PostgreSQL 数据基础、完整高频数据、TUI、Benchmark 和插件 TCP；删除快速重构留下的双路径、旧命名、旧状态源、无数据库兼容、镜像持久化与杂乱命令组织。

最终目标不是“小内核”，而是：

> **能力丰富但所有权清晰、路径唯一、接口统一、性能可证明的新一代 Phira 多人游戏运行时。**
