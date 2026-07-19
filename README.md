# Phira-mp+

> Phira+ 架构中的实时多人游戏运行时 — 高并发会话、房间状态、可信插件与可靠事件持久化。

[![License](https://img.shields.io/badge/License-AGPLv3-blue.svg)](LICENSE)
[![Build](https://github.com/HyperSynapseNetwork/Phira-mp-plus/actions/workflows/build.yml/badge.svg)](https://github.com/HyperSynapseNetwork/Phira-mp-plus/actions/workflows/build.yml)

## 简介

**Phira-mp+（PMP）** 是 [phira-mp](https://github.com/HyperSynapseNetwork/phira-mp) 的增强版多人游戏服务端。在 Phira+ 架构中，PMP 负责游戏协议、房间运行时、WASM 插件与游戏数据持久化；**Phira+ Backend（PPB）负责公共 Web API、认证、限流、网关与边缘暴露**。因此 PMP 保留的 HTTP/SSE/WebSocket 仅用于受控网络中的兼容、诊断和内部集成，不作为公网边缘接口。

> **当前状态：预生产加固候选版本（v0.4.x）** — 已完成多项 P0/P1 加固，适合受控测试和内部灰度部署。完整生产就绪状态追踪见 [`PMP_PRODUCTION_PRODUCTIZATION_AUDIT.md`](PMP_PRODUCTION_PRODUCTIZATION_AUDIT.md)。

### 核心特性

- **有界连接接入** — TCP accept 与认证解耦，认证并发和在线会话由信号量预留，慢认证连接不会串行阻塞全局 accept
- **WASM 插件边界** — 基于 wasmtime 组件模型（WIT ABI v2），逐插件 capability、fuel、线性内存/实例/表限制、有界事件队列与超时 quarantine 已接线
- **可靠生命周期** — 插件事件和持久化使用有界队列；Flush/Shutdown 带确认；数据库重试耗尽后写入本地 dead-letter；后台任务由 Supervisor 统一跟踪并在关闭时取消和等待
- **严格命令入口** — Session 与 Room 管理命令只经 mailbox 执行；mailbox 缺失、关闭、拥塞或结果不确定时显式失败，不再切换到直接处理路径
- **一致房间控制面** — host/lock/cycle/hidden/endpoint/容量等控制字段共享同一快照和 generation。成员、谱面与轮次的完整 Actor ownership 仍在迁移中
- **慢消费者隔离** — 会话发送采用有界队列和非阻塞路径；网络读取与业务处理通过有界命令队列解耦
- **PMP 内部接口** — 保留房间信息 HTTP、SSE、WebSocket 和插件动态路由，供 PPB/受控网络调用，不承担公共边缘安全职责
- **jemalloc 分配器** — Linux 下使用 jemalloc 替代 musl malloc，降低长期运行中的 RSS 膨胀风险

## 文档

| 分类 | 文档 |
|------|------|
| **产品** | [产品概览](docs/product/overview.md) · [当前保证](docs/guarantees.md) |
| **快速开始** | [构建与运行](docs/getting-started/quick-start.md) |
| **运维** | [配置参考](docs/operations/configuration-reference.md) |
| **开发** | [架构](docs/development/architecture.md) |
| **插件** | [生命周期](docs/plugins/lifecycle.md) |
| **其他** | [CHANGELOG](CHANGELOG.md) · [审计报告](PMP_PRODUCTION_PRODUCTIZATION_AUDIT.md) |

## 技术栈

| 技术 | 用途 |
|------|------|
| [Rust](https://www.rust-lang.org/) | 主开发语言（2021 Edition） |
| [Tokio](https://tokio.rs/) | 异步运行时 |
| [ratatui](https://ratatui.rs/) + [crossterm](https://github.com/crossterm-rs/crossterm) | TUI 终端界面 |
| [Clap](https://clap.rs/) | CLI 参数解析 |
| [Axum](https://github.com/tokio-rs/axum) | HTTP/SSE 服务器 |
| [wasmtime](https://wasmtime.dev/) | WASM 运行时（可选） |
| [fluent](https://projectfluent.org/) | 本地化 (i18n) |
| [reqwest](https://docs.rs/reqwest/) | HTTP 客户端 |
| [tracing](https://docs.rs/tracing/) | 日志与诊断 |
| [serde_yaml](https://docs.rs/serde_yaml/) | YAML 配置解析 |

## 快速开始


确保已安装当前稳定版 Rust 工具链：

```bash
# 安装/更新 Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup update stable

# 添加 musl 目标平台并安装链接器（Ubuntu/Debian）
rustup target add x86_64-unknown-linux-musl
sudo apt update && sudo apt install -y musl-tools

# 克隆仓库后进入目录
cd phira-mp-plus

# 构建静态链接二进制文件（首次编译约需 5-10 分钟）
cargo build --release --target x86_64-unknown-linux-musl

# 使用默认配置启动
./target/x86_64-unknown-linux-musl/release/phira-mp-plus-server

# 指定自定义配置文件启动
./target/x86_64-unknown-linux-musl/release/phira-mp-plus-server --config my_config.yml
```

### 自定义配置

创建或修改 `server_config.yml`（见项目根目录的示例文件）：

```yaml
port: 12346
http_port: 12347
monitors:
  - 12345
  - 67890
phira_api_endpoint: "https://phira.5wyxi.com"
plugins_dir: plugins
connection_rate_limit: 60
connection_rate_window: 10
round_data_retention_days: 7
server_name: "My Phira Server"
chat_enabled: true
cli_enabled: true

# Real Benchmark 是高级兼容性测试，默认不推荐；Simulation 不需要 token。
# 如需使用，请看 docs/benchmark-real.md。
```

架构加固的第一阶段范围见 [HARDENING_REPORT.md](HARDENING_REPORT.md)，第二阶段的严格 mailbox、持久化幂等与 dead-letter 修改见 [PHASE2_HARDENING_REPORT.md](PHASE2_HARDENING_REPORT.md)。

配置加载规则：默认读取 `server_config.yml`，也可通过 `--config <FILE>` 指定；配置文件缺失时使用内置默认值，配置文件存在但格式、字段名或取值无效时拒绝启动。只有用户显式提供的命令行参数才覆盖 YAML，避免 CLI 默认值意外覆盖配置文件。完整说明见 [docs/configuration.md](docs/configuration.md)。

### 命令行参数

```
phira-mp-plus-server [OPTIONS]

  -p, --port <PORT>          覆盖 TCP 监听端口（内置默认 12346）
  -d, --plugins-dir <DIR>    覆盖插件目录（内置默认 "plugins"）
  -e, --ext-file <FILE>      覆盖扩展数据文件（内置默认 "data/extensions.json"）
  -l, --log-file <NAME>      日志文件基础名称 [默认: "phira-mp-plus"]
  -m, --monitor <IDS>...     覆盖允许旁观的用户 ID
      --http-port <PORT>     覆盖 HTTP/SSE 端口（内置默认 12347）
      --proxy-port <PORT>    覆盖可信 X-Forwarded-For 兼容监听端口（内置默认 0）
      --no-cli               禁用交互式管理控制台
  -c, --config <FILE>        YAML 配置文件路径 [默认: "server_config.yml"]
  -h, --help                 显示帮助
  -V, --version              显示版本

空载模式仅改变非关键后台活动的调度偏好，不会暂停权威持久化或可靠插件事件。更多配置项见 [docs/configuration.md](docs/configuration.md)。
```

## 项目结构

```
Phira-mp-plus/
│
├── Cargo.toml                       # 工作区根 (workspace)
├── Cargo.lock
├── LICENSE                          # AGPL-3.0
├── README.md
├── server_config.yml                # YAML 配置文件
├── wit/                             # WIT 接口定义
│   └── phira-plugin.wit             #   Plugin ABI v2 WIT (12 interfaces, 53 methods)
│
├── data/                            # 运行时数据
│   ├── extensions.json              #   插件扩展数据
│   ├── rounds/                      #   轮次 Touches/Judges 记录
│   ├── persistence-dead-letter.jsonl #   数据库重试耗尽后的失败事件保全
│   └── plugins/                     #   插件私有数据
├── log/                             # 运行日志（每小时轮转）
│
├── docs/                            # 文档
│   ├── cli.md                       #   CLI 命令参考
│   ├── configuration.md             #   YAML 配置说明
│   ├── plugin-dev.md                #   插件开发指南
│   ├── plugin-config.md             #   插件配置说明
│   ├── wit-abi.md                   #   WIT ABI 规范
│   ├── api.md                       #   HTTP API 参考
│   ├── simulation.md                #   Simulation 架构
│   ├── benchmark-real.md            #   真实压测说明
│   ├── migrate-from-hsnphira.md     #   HSNPhira 迁移
│   └── plugins/                     #   插件文档
│
├── phira-mp-plus-server/            # 服务端核心 (crate)
│   ├── Cargo.toml
│   ├── locales/                     #   Fluent i18n (en/zh-CN/zh-TW)
│   └── src/
│       ├── main.rs                  #   进程入口 & 生命周期
│       ├── lib.rs                   #   模块导出
│       ├── server/                   #   Server 模块 (分解自原 server.rs)
│       │   ├── mod.rs                #    模块声明 + 公共 re-export
│       │   ├── config.rs             #    PlusConfig / LiveConfig / RuntimeV2Config / Chart / Record
│       │   ├── benchmark.rs          #    BenchRequest / HybridBenchmarkConfig / token helpers
│       │   ├── events.rs             #    事件订阅者 (runtime / plugin observer)
│       │   ├── snapshot.rs           #    RoomSnapshot / UserSnapshot / build_snapshot
│       │   ├── state.rs              #    PlusServerState 字段定义
│       │   └── orig.rs               #    遗留代码 (逐步分解中)
│       ├── server_query.rs          #   Admin ID 等查询函数
│       ├── cli.rs                   #   CLI 生命周期、输入循环
│       ├── cli/dispatch.rs          #   顶层命令路由
│       ├── cli/commands/            #   命令模块
│       │   ├── admin.rs             #   admin-id / ban / extension
│       │   ├── benchmark.rs         #   benchmark 命令
│       │   ├── broadcast.rs         #   消息广播
│       │   ├── plugin.rs            #   WASM 插件管理
│       │   ├── room.rs              #   房间管理
│       │   ├── runtime/             #   runtime 诊断 (actors/commands/events/persistence/schema/status/...)
│       │   └── simulation/          #   simulation 压测 (reports/runner/world)
│       ├── cli_tui.rs               #   TUI 终端 (ratatui + crossterm)
│       ├── command_registry.rs      #   命令注册表
│       ├── session.rs               #   会话生命周期
│       ├── session_auth.rs          #   会话认证
│       ├── session_dispatch.rs      #   命令分发
│       ├── session_permissions.rs   #   会话权限
│       ├── session_room.rs          #   房间协议
│       ├── session_telemetry.rs     #   遥测处理
│       ├── session_actor.rs         #   Session Actor mailbox
│       ├── supervisor_actor.rs      #   后台任务注册、退出检测与有序关闭
│       ├── room.rs                  #   房间状态机 (InternalRoomState / Room)
│       ├── room_actor/              #   Room Actor 命令网关
│       │   ├── mod.rs,mailbox.rs,command.rs,handler.rs,context.rs,result.rs,audit.rs
│       │   └── ops/                 #   操作: control.rs, membership.rs, settings.rs
│       ├── idle.rs                  #   空载模式 (IdleConfig / IdleMonitor)
│       ├── persistence/             #   持久化 Worker 管道
│       │   ├── mod.rs               #   入口 & pipeline
│       │   └── admin/benchmark/diagnostics/events/message/mirror/queries/rounds/schema
│       ├── proxy_protocol.rs        #   可信 X-Forwarded-For 兼容监听（非 PROXY v1/v2）
│       ├── telemetry_batcher.rs     #   TelemetryBatcher
│       ├── telemetry_batcher_test.rs#   Batcher 测试
│       ├── round_store.rs           #   轮次数据存储 (DB优先, 文件回退)
│       ├── internal_hooks.rs        #   内部静态注册 (欢迎语/playtime/DB)
│       ├── plugin.rs                #   插件管理器 (PluginManager / PluginHost)
│       ├── plugin_abi/              #   Plugin ABI 边界
│       │   ├── mod.rs               #   导出 / wit_abi bindgen
│       │   └── plan.rs              #   ABI 版本常量 / MIGRATION_PHASE
│       ├── plugin_http/             #   HTTP 动态路由
│       │   ├── mod.rs               #   PluginHttpServer / SSE handler
│       │   ├── router.rs            #   DynamicRouter (支持 :param)
│       │   ├── sse.rs               #   SseHub / EventStream
│       │   └── websocket.rs         #   WebSocket handler
│       ├── wasm_host.rs             #   WASM 运行时 (WitPluginComponent)
│       ├── wasm_host_helpers.rs     #   SSRF/capability/config helpers
│       ├── wit_host.rs              #   WIT host trait 实现 (WitPluginHost)
│       ├── extensions.rs            #   扩展 KV 存储
│       ├── ban.rs                   #   封禁系统
│       ├── phira_client.rs          #   Phira HTTP RetryClient
│       ├── rate_limiter.rs          #   速率限制 (滑动窗口)
│       ├── rate_limiter_test.rs
│       ├── event_bus.rs             #   EventBus 运行时脊椎
│       ├── simulation.rs            #   Simulation 管理器
│       ├── simulation_realistic.rs  #   Realistic 场景
│       ├── simulation_realistic_test.rs
│       ├── actor_runtime.rs         #   Actor 边界蓝图
│       ├── runtime_diagnostics.rs   #   Runtime 诊断常量
│       ├── benchmark_report.rs      #   Benchmark report 类型
│       ├── benchmark_snapshot.rs    #   Benchmark snapshot
│       ├── db.rs                    #   PostgreSQL 持久化
│       ├── error.rs                 #   错误类型
│       ├── l10n.rs                  #   Fluent i18n
│       ├── logging.rs               #   tracing 配置
│       └── terminal.rs              #   终端检测
│   └── tests/                       # 集成 & 合约测试
│       ├── admin_command_contracts.rs
│       ├── cli_command_contracts.rs
│       ├── command_surface_contracts.rs
│       ├── docs_contracts.rs
│       ├── persistence_contracts.rs
│       ├── phira_http_contracts.rs
│       ├── room_state_machine_tests.rs
│       ├── simulation_contracts.rs
│       ├── telemetry_cutover_contracts.rs
│       ├── wit_abi_contracts.rs
│       ├── wasm_lifecycle_tests.rs
│       ├── wasm_api_tests.rs
│       ├── sse_tests.rs
│       ├── test-plugin.component.wasm  # WASM 测试夹具
│       └── test-plugin/                # 测试 WASM 插件源
│           ├── Cargo.toml, Makefile
│           └── src/lib.rs
│
├── phira-mp-plus-server-api/      # 共享类型 crate
│   └── src/lib.rs                 # PluginEvent / HttpHandle / ServerStateQuery
│
├── phira-plugin-sdk/              # WASM 插件 SDK
│   ├── Cargo.toml
│   └── src/lib.rs                 # wit_bindgen! 宏
│
├── phira-mp/                      # 上游 phira-mp 协议层
│   ├── phira-mp-common/           #   网络协议
│   │   └── src/                   #   ClientCommand / ServerCommand / Stream 帧协议
│   └── phira-mp-macros/           #   #[derive(BinaryData)] 过程宏
│
├── phira-mp/                    # 上游 phira-mp 子模块（协议层与原始服务端）
│   ├── phira-mp-common/         #   网络协议: 二进制编码 (BinaryData trait)、
│   │   └── src/                 #     命令定义 (ClientCommand / ServerCommand)、
│   │       ├── lib.rs           #     Stream 帧协议、RoomId / RoomState / 消息类型
│   │       ├── command.rs
│   │       └── bin.rs           #     BinaryReader / BinaryWriter (LEB128, 小端)
│   └── phira-mp-macros/         #   #[derive(BinaryData)] 过程宏
│
├── wit/                         # WIT 定义
│   └── phira-plugin.wit         #   Plugin ABI v2 WIT / Component Model
│
├── docs/                        # 文档
│   ├── api.md                   #   HTTP API 文档
│   ├── benchmark-real.md        #   Real Benchmark 使用说明
│   ├── cli.md                   #   CLI 命令参考
│   ├── configuration.md         #   配置文件与运行时参数说明
│   ├── migrate-from-hsnphira.md #   从 HSN Phira 迁移指南
│   ├── plugin-config.md         #   插件配置说明
│   ├── plugin-dev.md            #   WASM 插件开发指南
│   ├── runtime-v2-actor-roadmap.md # Actor 迁移路线图
│   ├── simulation.md            #   Simulation 说明
│   └── wit-abi.md               #   WIT ABI 说明
```

## 终端兼容性

启动时会检测 stdin/stdout、`TERM`、`STY` 与 `TMUX`。GNU Screen、Linux console、`ansi`/`cons25` 等环境使用保守 TUI：禁用备用屏幕、鼠标捕获和 Bracketed Paste，并修正 Ctrl+H Backspace；如果 TUI 初始化失败，会自动降级到逐行兼容控制台。tmux、xterm、WezTerm、iTerm、Kitty 等普通终端继续使用完整 TUI。项目遵循 `NO_COLOR`，逐行输出会再次过滤残留控制序列；非交互环境同样使用逐行控制台。

## PMP 内部 SSE 房间事件

> 该接口面向 PPB 或受控运维网络。公网 Web API 由 PPB 提供。

`GET /rooms/listen` 建立连接后先发送 `ready`，随后以 `update_room` 补发当前房间快照，再持续推送 `create_room`、`update_room`、`join_room`、`leave_room` 和 `new_round`。可用下列命令直接检查数据流：

```bash
curl -N http://127.0.0.1:12347/rooms/listen
```

## CLI 命令

详细文档见 [docs/cli.md](docs/cli.md)。快速概览：

| 分组 | 命令 | 说明 |
|------|------|------|
| 通用 | `help`, `exit`, `status` | 帮助/退出/状态 |
| 诊断/压测 | `benchmark`, `simulation` | 真实网络压测（默认路径：simulation） |
| WASM 插件 | `plugin list/enable/disable/info/reload` | 插件管理 |
| 用户 | `users`, `kick`, `broadcast` | 用户管理和消息 |
| 房间 | `rooms`, `room info/start/force-start/cancel/kick/host/force-move/hide/set/close/history` | 房间管理子命令 |
| 黑名单 | `ban`, `unban`, `banlist`, `room ban/unban/banlist` | 全局与房间封禁管理 |

## WASM 插件开发

详细文档见 [docs/plugin-dev.md](docs/plugin-dev.md)。

WASM 插件通过 `phira:host/api` 和 `phira:host/log` 等导入函数与宿主通信。

## 配置参考

完整的配置项见 `server_config.yml` 与 [docs/configuration.md](docs/configuration.md)。常用项如下：

| 配置项 | 类型 | 默认值 | 说明 |
|--------|------|--------|------|
| `port` | u16 | `12346` | TCP 监听端口 |
| `http_port` | u16 | `12347` | PMP 内部 HTTP/SSE/WebSocket 兼容端口；公共接口由 PPB 提供 |
| `proxy_protocol_port` | u16 | `0` | 可信 X-Forwarded-For 兼容监听端口；不是 PROXY v1/v2 |
| `monitors` | Vec<i32> | `[2]` | 允许旁观的用户 ID |
| `phira_api_endpoint` | String | `https://phira.5wyxi.com` | 全局 Phira API 端点；房间可临时覆盖 |
| `plugins_dir` | String | `plugins` | WASM 插件目录 |
| `database_url` | String | — | PostgreSQL 统一持久化连接串 |
| `persistence_retention_days` | u32 | 30 | PG 历史数据保留天数，0 为不清理 |
| `touch_judge_retention_days` | u32? | 未设置 | Touches/Judges 高频遥测独立保留天数；未设置时遵循 `persistence_retention_days`，0 为不清理 |
| `runtime_v2.persistence_dead_letter_path` | String? | `data/persistence-dead-letter.jsonl` | 数据库重试耗尽后的失败事件 JSONL；`null` 禁用，但不等同于 WAL |
| `runtime_v2.telemetry_cutover_mode` | enum | `direct_only` | `direct_only`、`worker_preferred` 或显式前置校验后的 `worker_authoritative` |

`worker_preferred` 会先执行直写：直写成功时 Worker 记录为迁移镜像；直写失败但 Worker 队列接受时，该批次由 Worker 作为权威补偿路径。Worker 入队只表示持久化管线已接收，不等同于 PostgreSQL commit；最终结果以 batcher/DB ACK、重试与 dead-letter 指标为准。
| `admin_phira_ids` | Vec<i32> | [] | 游戏内管理员 ID，可使用 `_命令` 入口 |
| `chat_enabled` | bool | `true` | 聊天功能开关 |
| `cli_enabled` | bool | `true` | TUI/CLI 控制台开关 |
| `connection_rate_limit` | u32 | `30` | 连接速率限制（窗口内允许次数） |
| `connection_rate_window` | u32 | `10` | 连接速率统计窗口（秒） |
| `max_rooms` | usize | — | 最大房间数（未设置为不限制） |
| `max_users_per_room` | usize | `100` | 每房间最大玩家数 |
| `max_sessions` | usize | `4096` | 在线/已注册会话硬上限 |
| `max_pending_auth` | usize | `256` | 并发认证握手上限，且不得超过 `max_sessions` |
| `graceful_shutdown_timeout_secs` | u64 | `15` | 有序关闭共享总时限 |
| `round_data_retention_days` | u32 | `7` | 轮次 Touches/Judges 保留天数（0=不保留） |
| `server_name` | String | — | 服务器名称 |
| `wasm_runtime.*` | object | 见文档 | WASM 插件 fuel、内存、事件队列、并发和调用期限 |

真实网络压测（`benchmark run real`）是高级兼容性测试，详见 docs/benchmark-real.md。默认压测推荐使用 Simulation，不需要 token。

本轮实用功能可靠性审计、修复范围和回归检查清单见 [docs/functional-reliability-audit.md](docs/functional-reliability-audit.md)。

本轮架构加固的边界、实现、不变量、剩余风险和验收门槛见 [docs/architecture-hardening.md](docs/architecture-hardening.md)。静态验证结果见 [docs/static-verification.md](docs/static-verification.md)。

## 许可证

Phira-mp+ 整体采用 **GNU Affero General Public License v3.0** — 详见 [LICENSE](LICENSE)。

协议层（`phira-mp-common`、`phira-mp-macros`）基于 [phira-mp](https://github.com/TeamFlos/phira-mp) 衍生；
`phira-plugin-sdk`（WASM 插件 SDK）亦按 **Apache License, Version 2.0** 授权 — 详见 [LICENSE-APACHE](LICENSE-APACHE)。

完整的版权归属和第三方依赖许可证声明见 [NOTICE](NOTICE)。

## 致谢

感谢 [TeamFlos](https://github.com/TeamFlos) 开发和维护 Phira、phira-mp 项目，以及 [tphira-mp](https://github.com/Pimeng/tphira-mp) 与 [jphira-mp](https://github.com/lRENyaaa/jphira-mp) 提供的实现思路，还有所有支持本项目的用户。详见 [NOTICE](NOTICE)。

### Persistence crash recovery

Runtime v2 now includes an fsync-before-admission PersistenceWorker WAL (`runtime_v2.persistence_wal_path`). Unacknowledged ordinary events are replayed on restart, and graceful flush/shutdown compacts the journal. This closes the previous in-memory-queue crash window for ordinary worker events. Touch/Judge telemetry still requires batch commit acknowledgements for a full end-to-end commit guarantee.
