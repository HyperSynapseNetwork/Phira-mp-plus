# Phira-mp+

> Phira+ 架构中的实时多人游戏运行时 — 高并发会话、房间状态、可信插件与可靠事件持久化。

[![License](https://img.shields.io/badge/License-AGPLv3-blue.svg)](LICENSE)
[![Build](https://github.com/HyperSynapseNetwork/Phira-mp-plus/actions/workflows/build.yml/badge.svg)](https://github.com/HyperSynapseNetwork/Phira-mp-plus/actions/workflows/build.yml)

## 简介

**Phira-mp+（PMP）** 是 [phira-mp](https://github.com/HyperSynapseNetwork/phira-mp) 的增强版多人游戏服务端。在 Phira+ 架构中，PMP 负责游戏协议、房间运行时、WASM 插件与游戏数据持久化。HTTP/SSE/WebSocket 端口用于兼容、诊断和内部集成。

> **当前状态：基线版本（v0.4.x）** — Playout Infrastructure 已完成。所有迁移遗留清理（mirror/pipeline/runtime_v2/cutover/DbManager::None）、核心状态收敛（Room Actor 独占状态）、持久化定型（HighFrequencyWriter、WAL 简化为 A 类事件、PostgreSQL COPY）以及 Benchmark 重做（Simulation 降级、Mock Phira、11 场景、CLI 命令）均已落地。

### 核心特性

- **有界连接接入** — TCP accept 与认证解耦，认证并发和在线会话由信号量预留，慢认证连接不会串行阻塞全局 accept
- **WASM 插件边界** — 基于 wasmtime 组件模型（WIT ABI v2），逐插件 capability、fuel、线性内存/实例/表限制、有界事件队列与超时 quarantine 已接线
- **可靠生命周期** — 插件事件和持久化使用有界队列；Flush/Shutdown 带确认；数据库重试耗尽后写入本地 dead-letter；后台任务由 Supervisor 统一跟踪并在关闭时取消和等待
- **严格命令入口** — Session 与 Room 管理命令只经 mailbox 执行；mailbox 缺失、关闭、拥塞或结果不确定时显式失败，不再切换到直接处理路径
- **一致房间控制面** — host/lock/cycle/hidden/endpoint/容量等控制字段共享同一快照和 generation。ActorState 已是快照权威来源，全部 17 个命令走 actor_state
- **慢消费者隔离** — 会话发送采用有界队列和非阻塞路径；网络读取与业务处理通过有界命令队列解耦
- **内部接口** — 房间信息 HTTP、SSE、WebSocket 和插件动态路由
- **插件 TCP 连接 API** — 供 WASM 插件通过句柄建立和管理 TCP 连接（connect/listen/send/close），纯明文，无 TLS
- **jemalloc 分配器** — Linux 下使用 jemalloc 替代 musl malloc，降低长期运行中的 RSS 膨胀风险

## 文档

| 分类 | 文档 |
|------|------|
| **配置** | [配置说明](docs/configuration.md) · [JSON Schema](docs/operations/config-schema.json) |
| **运维** | [运维手册](docs/operations.md) |
| **插件** | [插件开发指南](docs/plugin-dev.md)（含 WIT ABI、示例） |
| **开发** | [架构](docs/development/architecture.md) · [CLI 手册](docs/cli.md) · [测试指南](docs/development/testing.md) · [CLI 错误码 (EN)](docs/development/error-codes.en.md) · [产品概览 (EN)](docs/overview.en.md) · [兼容矩阵 (EN)](docs/compatibility-matrix.en.md) |
| **API** | [事件 API](docs/api.md) |
| **仿真** | [仿真与压测](docs/simulation.md) |

## 许可

PMP 服务端采用 [AGPL-3.0](LICENSE) 开源。
插件 SDK（`phira-plugin-sdk`）采用 [Apache-2.0](LICENSE-APACHE) 许可。
第三方依赖的许可声明见 [NOTICE](NOTICE)。

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

架构加固的详细说明见 audit 报告 [PMP_PRODUCTION_PRODUCTIZATION_AUDIT.md](PMP_PRODUCTION_PRODUCTIZATION_AUDIT.md)。

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
│   └── phira-plugin.wit             #   Plugin ABI v2 WIT (15 interfaces)
│
├── scripts/
│   └── docgen.sh                    #   WIT → Markdown 文档生成脚本
│
├── data/                            # 运行时数据目录
│   ├── extensions.json              #   插件扩展数据
│   └── plugins/                     #   插件私有数据
├── log/                             # 运行日志（每小时轮转）
│
├── docs/                            # 文档
│   ├── api/                         #   自动生成的 API 文档
│   │   ├── plugin-api.md            #     WIT 插件 API 参考
│   │   └── capability-table.md      #     capability 映射表
│   ├── cli.md                       #    CLI 命令参考
│   ├── configuration.md             #    配置说明
│   ├── plugin-dev.md                #    插件开发指南
│   ├── simulation.md                #    Simulation 架构
│   ├── operations.md                #    运维手册
│   └── development/                 #    开发文档
│
├── phira-mp-plus-server/            # 服务端核心 (crate)
│   ├── Cargo.toml
│   ├── locales/                     #   Fluent i18n (en/zh-CN/zh-TW)
│   └── src/
│       ├── main.rs                  #   进程入口 & 生命周期
│       ├── lib.rs                   #   模块导出
│       ├── bin/
│       │   └── pmp-admin.rs         #   独立管理工具 (backup/restore)
│       ├── server/                  #   Server 模块 (10 子模块)
│       │   ├── mod.rs               #    模块声明 + re-export
│       │   ├── state.rs             #    PlusServerState/PlusServer 结构
│       │   ├── init.rs              #    PlusServer::new 初始化
│       │   ├── accept.rs            #    TCP 监听 accept 循环
│       │   ├── config.rs            #    PlusConfig / LiveConfig / RuntimeConfig
│       │   ├── events.rs            #    事件订阅/发布
│       │   ├── query.rs             #    ServerStateQuery dispatch
│       │   ├── snapshot.rs          #    RoomSnapshot / build_snapshot
│       │   ├── rooms.rs             #    房间管理方法
│       │   ├── disconnect.rs        #    disconnect_banned_user
│       │   └── benchmark.rs         #    Benchmark 执行
│       ├── benchmark/               #   Benchmark 模块
│       │   ├── mod.rs               #    模块入口
│       │   ├── command.rs           #    BenchmarkCommand/BenchmarkRunArgs
│       │   ├── config.rs            #    BenchmarkConfig
│       │   ├── runner.rs            #    顶级调度
│       │   ├── environment.rs       #    环境检测
│       │   ├── mock_phira.rs        #    本地 Mock Phira
│       │   ├── profile.rs           #    CPU/heap profiling
│       │   ├── metrics.rs           #    指标采集
│       │   ├── report.rs            #    报告生成 (text/json/markdown)
│       │   ├── presets.rs           #    预设参数 (quick/standard/stress/soak)
│       │   ├── modes/               #    运行模式
│       │   │   ├── simulation.rs    #      进程内模式
│       │   │   └── real.rs          #      真实 TCP 模式
│       │   └── scenarios/           #    负载场景 (11 个)
│       │       ├── common.rs        #      共享工具
│       │       ├── room_lifecycle.rs
│       │       ├── gameplay.rs
│       │       ├── connection.rs
│       │       ├── steady_state.rs
│       │       ├── hot_room.rs
│       │       ├── slow_consumer.rs
│       │       ├── reconnect.rs
│       │       ├── plugin_load.rs
│       │       ├── database_write.rs
│       │       ├── mixed.rs
│       │       └── long_run.rs
│       ├── cli.rs                   #   CLI 生命周期、输入循环
│       ├── cli/dispatch.rs          #   顶层命令路由
│       ├── cli/commands/            #   命令模块
│       │   ├── admin.rs             #   admin-id / ban / extension
│       │   ├── benchmark.rs         #   benchmark (list/run/suite/compare/simulation)
│       │   ├── benchmark_simulation.rs  #   simulation CLI 处理器
│       │   ├── broadcast.rs         #   消息广播
│       │   ├── plugin.rs            #   WASM 插件管理
│       │   ├── room.rs              #   房间管理
│       │   └── runtime/             #   runtime 诊断子命令
│       ├── cli_tui.rs               #   TUI 终端 (ratatui + crossterm)
│       ├── command_registry.rs      #   命令注册表
│       ├── session.rs               #   会话生命周期
│       ├── session_auth.rs          #   会话认证
│       ├── session_dispatch.rs      #   命令分发
│       ├── session_permissions.rs   #   会话权限
│       ├── session_room.rs          #   房间协议
│       ├── session_telemetry.rs     #   遥测处理 (Touch/Judge → HighFrequencyWriter)
│       ├── session_actor.rs         #   Session Actor mailbox
│       ├── supervisor_actor.rs      #   后台任务注册、退出检测与有序关闭
│       ├── room.rs                  #   房间广播接口 (Actor 已独占状态)
│       ├── backup.rs                #   备份与恢复 (仅 pmp-admin binary)
│       ├── crypto.rs                #   HMAC 签名 (sha2)
│       ├── plugin_tcp.rs            #   插件原始 TCP Actor
│       ├── play_history.rs          #   游玩历史
│       ├── telemetry.rs             #   遥测类型 & TelemetryBatcher
│       ├── room_actor/              #   Room Actor 命令网关
│       │   ├── mod.rs               #    RoomCommandGateway
│       │   ├── actor.rs             #    RoomActorState / RoomSnapshot
│       │   ├── mailbox.rs           #    per-room mailbox
│       │   ├── command.rs           #    RoomActorCommand 枚举
│       │   ├── handler.rs           #    命令执行
│       │   ├── context.rs           #    命令上下文
│       │   ├── result.rs            #    命令结果
│       │   ├── audit.rs             #    审计日志
│       │   └── ops/                 #    操作
│       │       ├── mod.rs
│       │       ├── control.rs       #      SetLock/SetCycle/SetHidden
│       │       ├── membership.rs    #      AddUser/RemoveUser
│       │       ├── session.rs       #      Chat/Create/Join/Leave
│       │       ├── settings.rs      #      SetHost/SetChart/SetEndpoint
│       │       └── telemetry.rs     #      AddTouches/AddJudges/SetDisplayName
│       ├── idle.rs                  #   空载模式
│       ├── persistence/             #   持久化管道
│       │   ├── mod.rs               #   模块入口
│       │   ├── pipeline.rs          #   写入管道分发
│       │   ├── wal.rs               #   Write-Ahead Log (A 类事件)
│       │   ├── high_frequency.rs    #   高频写入 (Touch/Judge, 绕过 WAL, PostgreSQL COPY)
│       │   ├── worker.rs            #   PersistenceWorker 主循环
│       │   ├── stats.rs             #   写入统计 (含 per-type 细分)
│       │   ├── message.rs           #   PersistenceEvent 枚举
│       │   ├── telemetry.rs         #   批量 INSERT
│       │   ├── rounds.rs            #   Round 持久化
│       │   ├── admin.rs             #   管理员数据
│       │   ├── benchmark.rs         #   Benchmark 报告持久化
│       │   ├── diagnostics.rs       #   队列健康诊断
│       │   ├── events.rs            #   事件持久化
│       │   ├── queries.rs           #   查询方法
│       │   ├── schema.rs            #   Schema 常量
│       │   ├── simulation.rs        #   Simulation 事件持久化
│       │   └── users.rs             #   用户数据持久化
│       ├── proxy_protocol.rs        #   可信代理支持
│       ├── round_store.rs           #   轮次数据存储
│       ├── internal_hooks.rs        #   内部静态注册
│       ├── plugin.rs                #   插件管理器
│       ├── plugin_abi/              #   Plugin ABI 边界
│       │   ├── mod.rs               #    导出 / wit_abi bindgen
│       │   └── plan.rs              #    ABI 版本常量（稳定）
│       ├── plugin_http/             #   HTTP 动态路由
│       │   ├── router.rs            #    DynamicRouter
│       │   ├── sse.rs               #    SseHub / EventStream
│       │   └── websocket.rs         #    WebSocket handler
│       ├── wasm_host.rs             #   WASM 运行时
│       ├── wasm_host_helpers.rs     #   capability/config helpers
│       ├── wit_host.rs              #   WIT host trait 实现
│       ├── extensions.rs            #   扩展 KV 存储
│       ├── ban.rs                   #   封禁系统
│       ├── phira_client.rs          #   Phira HTTP RetryClient
│       ├── rate_limiter.rs          #   速率限制
│       ├── event_bus.rs             #   EventBus (MpEvent 广播)
│       ├── simulation.rs            #   Simulation 管理器
│       ├── simulation_realistic.rs  #   Realistic 场景
│       ├── actor_runtime.rs         #   Actor 边界蓝图
│       ├── runtime_diagnostics.rs   #   Runtime 诊断常量
│       ├── benchmark_report.rs      #   Benchmark report 类型
│       ├── benchmark_snapshot.rs    #   Benchmark snapshot
│       ├── db.rs                    #   PostgreSQL 持久化 (DbManager)
│       ├── error.rs                 #   错误类型
│       ├── l10n.rs                  #   Fluent i18n
│       ├── logging.rs               #   tracing 配置
│       └── terminal.rs              #   终端检测
│   └── tests/                       # 集成 & 合约测试
│       ├── admin_command_contracts.rs
│       ├── command_surface_contracts.rs
│       ├── docs_contracts.rs
│       ├── persistence_contracts.rs
│       ├── phira_http_contracts.rs
│       ├── room_state_machine_tests.rs
│       ├── simulation_contracts.rs
│       ├── wit_abi_contracts.rs      #   15 接口 conformance (Phase 5)
│       ├── wasm_lifecycle_tests.rs
│       ├── wasm_api_tests.rs
│       ├── sse_tests.rs
│       ├── test-plugin.component.wasm
│       └── test-plugin/
│           ├── Cargo.toml, Makefile
│           └── src/lib.rs
│
├── phira-mp-plus-server-api/        # 共享类型 crate
│   └── src/lib.rs                   #   PluginEvent / HttpHandle / ServerStateQuery
│
├── phira-plugin-sdk/                # WASM 插件 SDK
│   ├── Cargo.toml
│   └── src/lib.rs                   #   wit_bindgen! 宏
│
├── phira-mp/                        # 上游 phira-mp 协议层
│   ├── phira-mp-common/             #   网络协议
│   │   └── src/                     #   ClientCommand / ServerCommand / Stream 帧协议
│   └── phira-mp-macros/             #   #[derive(BinaryData)] 过程宏
```

## 终端兼容性

启动时会检测 stdin/stdout、`TERM`、`STY` 与 `TMUX`。GNU Screen、Linux console、`ansi`/`cons25` 等环境使用保守 TUI：禁用备用屏幕、鼠标捕获和 Bracketed Paste，并修正 Ctrl+H Backspace；如果 TUI 初始化失败，会自动降级到逐行兼容控制台。tmux、xterm、WezTerm、iTerm、Kitty 等普通终端继续使用完整 TUI。项目遵循 `NO_COLOR`，逐行输出会再次过滤残留控制序列；非交互环境同样使用逐行控制台。

| `http_port` | u16 | `12347` | PMP HTTP/SSE/WebSocket 端口 |

## 许可证

Phira-mp+ 整体采用 **GNU Affero General Public License v3.0** — 详见 [LICENSE](LICENSE)。

协议层（`phira-mp-common`、`phira-mp-macros`）基于 [phira-mp](https://github.com/TeamFlos/phira-mp) 衍生；
`phira-plugin-sdk`（WASM 插件 SDK）亦按 **Apache License, Version 2.0** 授权 — 详见 [LICENSE-APACHE](LICENSE-APACHE)。

完整的版权归属和第三方依赖许可证声明见 [NOTICE](NOTICE)。

## 致谢

感谢 [TeamFlos](https://github.com/TeamFlos) 开发和维护 Phira、phira-mp 项目，以及 [tphira-mp](https://github.com/Pimeng/tphira-mp) 与 [jphira-mp](https://github.com/lRENyaaa/jphira-mp) 提供的实现思路，还有所有支持本项目的用户。详见 [NOTICE](NOTICE)。

