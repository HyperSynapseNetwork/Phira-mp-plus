# PMP Benchmark / Simulation 重构设计

## 目标

Benchmark 是 PMP 的正式能力验证系统。Simulation 是 Benchmark 的进程内模式。

## 命令

```bash
pmp benchmark list

pmp benchmark run \
  --mode simulation \
  --scenario gameplay \
  --preset standard

pmp benchmark run \
  --mode real \
  --scenario hot-room \
  --clients 100 \
  --rooms 1 \
  --duration 10m

pmp benchmark suite --preset standard

pmp benchmark compare old.json new.json
```

## 模式

### simulation

- 进程内；
- 本地 Mock Phira；
- 走生产 Command/Actor/Persistence 路径；
- 不经过 TCP；
- 固定 seed；
- 用于快速回归。

### real

- 启动真实 PMP；
- 模拟客户端走真实 TCP；
- 本地 Mock Phira HTTP；
- PostgreSQL；
- 可加载插件；
- 用于真实性能。

## 场景

| 场景 | 目的 |
|---|---|
| room-lifecycle | 建房、加入、选谱、准备、开局、结束、离开 |
| gameplay | 真实速率 Touch/Judge |
| connection | 真实 TCP 连接、认证、重连 |
| steady-state | 稳态房间、Ping、低频控制命令 |
| hot-room | 单热点房间广播与状态更新 |
| slow-consumer | 慢客户端隔离 |
| reconnect | 重连风暴与旧 Session 退出 |
| plugin-load | 插件事件和 API |
| database-write | PostgreSQL 与批处理 |
| mixed | 多种负载并发发生 |
| long-run | 6/24/72 小时稳定性 |

删除 `chat_storm`。

## Preset

### quick

- 50 clients
- 5 rooms
- 30s

### standard

- 1000 clients
- 100 rooms
- 10m

### stress

- 逐级升压直到错误率或 p99 越界

### soak

- 24h 默认
- 可配置 6h/72h

## Mock Phira

- 本地启动；
- 无 token；
- 固定用户、谱面和记录；
- 可配置延迟、抖动、错误和超时；
- 可配置 seed；
- 结果可复现。

## 代码结构

在现有 Server crate 内整理：

```text
benchmark/
  mod.rs
  command.rs
  config.rs
  runner.rs
  environment.rs
  mock_phira.rs
  profile.rs
  metrics.rs
  report.rs
  presets.rs

  modes/
    simulation.rs
    real.rs

  scenarios/
    room_lifecycle.rs
    gameplay.rs
    connection.rs
    steady_state.rs
    hot_room.rs
    slow_consumer.rs
    reconnect.rs
    plugin_load.rs
    database_write.rs
    mixed.rs
    long_run.rs
```

不调整 workspace。

## 必须走生产路径

Simulation 不得：

- 手动持有 Room lock 后调用内部 handler；
- 直接修改 Users/Rooms map；
- 绕过 Session mailbox；
- 绕过 Room Actor；
- 绕过插件事件入口；
- 绕过 PostgreSQL writer。

允许替换的只有：

- 网络 transport；
- 时间源；
- Mock Phira；
- 客户端实现。

## 游戏流量模型

Touch/Judge 按真实帧率发送：

- 60 Hz 默认；
- 可选 120 Hz；
- 支持 jitter；
- 支持 burst；
- 支持丢包/延迟，仅 real 模式。

`Played` 只在一次游戏结束时发送。

## 输出结构

```text
PMP Benchmark

环境
  Version
  Commit
  Rust
  OS
  CPU
  Memory
  PostgreSQL

配置
  Mode
  Scenario
  Seed
  Clients
  Rooms
  Duration
  Plugins
  Preset

总体
  Commands/s
  Messages/s
  Errors
  Invariant violations

延迟
  p50
  p95
  p99
  max

资源
  CPU
  RSS
  Allocation
  GC

队列
  Session send
  Session command
  Room mailbox
  Plugin event
  Persistence
  Telemetry

数据库
  Transactions/s
  Rows/s
  Touch committed
  Judge committed
  Retries
  Dropped

产物
  report.json
  report.md
  cpu.pprof
  heap.pprof
```

## 参考 Go 实现需要修正的点

- connection-storm 必须使用真实连接或改名；
- mixed 必须并发，不是顺序 suite；
- gameplay 不能无限速循环；
- room-cycle 不能重复 Played；
- steady-state 不能让每个客户端重复创建同名房间；
- Mock Session 必须线程安全；
- production dispatcher 不得被手工锁替代；
- CPU profile 必须在报告生成前停止和 flush；
- 删除未使用 `Verbose`；
- scenario 使用注册表而不是嵌套 switch；
- 每个场景单独文件。
