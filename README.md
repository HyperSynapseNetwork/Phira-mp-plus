<div align="center">

# Phira-mp+

<img src="docs/logo/phira-mp-plus-banner.png" width="60%" alt="Phira-mp+ banner" />

**Phira-mp+（PMP）** — 基于 [phira-mp](https://github.com/HyperSynapseNetwork/phira-mp) 的高性能 Phira 多人游戏服务端 · Rust / WASM 插件 / WAL 先行 / Actor 模型

[![License: AGPLv3](https://img.shields.io/badge/License-AGPLv3-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-2021-dea584.svg?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Build](https://github.com/HyperSynapseNetwork/Phira-mp-plus/actions/workflows/build.yml/badge.svg)](https://github.com/HyperSynapseNetwork/Phira-mp-plus/actions/workflows/build.yml)
[![Tokio](https://img.shields.io/badge/Tokio-异步-e0c0a0.svg?logo=rust&logoColor=white)](https://tokio.rs/)
[![Docker](https://img.shields.io/badge/Docker-Ready-2496ED.svg?logo=docker&logoColor=white)](https://www.docker.com/)
[![PostgreSQL](https://img.shields.io/badge/PostgreSQL-数据库-336791.svg?logo=postgresql&logoColor=white)](https://www.postgresql.org/)
[![Axum](https://img.shields.io/badge/Axum-HTTP%2FSSE%2FWS-0b0b0b.svg?logo=rust&logoColor=white)](https://github.com/tokio-rs/axum)
[![wasmtime](https://img.shields.io/badge/wasmtime-WASM%20插件-2E3B4E.svg?logo=rust&logoColor=white)](https://wasmtime.dev/)
[![WIT](https://img.shields.io/badge/插件%20ABI-WIT%20component-7f52ff.svg)](https://component-model.bytecodealliance.org/)
[![i18n](https://img.shields.io/badge/i18n-fluent-1b6ac9.svg)](https://projectfluent.org/)
[![GitHub stars](https://img.shields.io/github/stars/HyperSynapseNetwork/Phira-mp-plus?style=social)](https://github.com/HyperSynapseNetwork/Phira-mp-plus/stargazers)
[![Last commit](https://img.shields.io/github/last-commit/HyperSynapseNetwork/Phira-mp-plus)](https://github.com/HyperSynapseNetwork/Phira-mp-plus/commits/main)

</div>

## 简介

**Phira-mp+（PMP）** 是 [phira-mp](https://github.com/HyperSynapseNetwork/phira-mp) 的增强版多人游戏服务端。在 Phira+ 架构中，PMP 负责游戏协议、房间运行时、WASM 插件与游戏数据持久化。HTTP/SSE/WebSocket 端口用于兼容、诊断和内部集成。

### 核心特性

- **WAL 先行 + 崩溃恢复**：权威事件落盘确认后才回包，崩溃后重放不丢数据、失败不静默（fail-closed）
- **Actor 模型**：每房间独立 mailbox 串行化状态 + 快照缓存，无锁高并发
- **WASM 插件系统**：WIT component 动态加载，插件可注册 HTTP/SSE、订阅事件、调 host API，运行时热加载
- **丰富的拓展功能**：PMP相对于原版Phira-mp提供了大量新功能以优化用户与运维体验，详见[功能总览](docs/features.md)与[CLI 手册](docs/cli.md)

## 文档

| 分类 | 文档 |
|------|------|
| **功能总览** | [PMP 相对 Phira-mp 新增功能](docs/features.md) |
| **部署与运维** | [部署/配置/运维](docs/deployment.md) · [配置 JSON Schema](docs/operations/config-schema.json) |
| **对外 API** | [HTTP/SSE/WS · 插件 API · 能力表 · OpenUDS](docs/api.md) |
| **CLI 手册** | [CLI 命令参考](docs/cli.md)（含[基准测试](docs/cli.md#基准测试)） |
| **插件开发** | [插件开发指南](docs/plugin-dev.md)（含 WIT ABI、示例） |
| **开发** | [架构](docs/development/architecture.md) · [测试指南](docs/development/testing.md) · [CLI 错误码 (EN)](docs/development/error-codes.en.md) |

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

### 下载发行版（推荐）

从 [Releases](https://github.com/HyperSynapseNetwork/Phira-mp-plus/releases) 或 CI 构建产物下载：
- `phira-mp-plus-server-linux-glibc`（Linux glibc，通用）
- `phira-mp-plus-server-linux`（Linux musl，更便携）
- `phira-mp-plus-server-linux-arm64-glibc`（Linux ARM64）
- `phira-mp-plus-server-windows-x86_64`（Windows x86_64）

> **平台说明**：Windows 版本**不编译、不支持 OpenUDS**（Unix Domain Socket 是 Unix 特性，模块已 `#[cfg(unix)]` 排除）；其余功能与 Linux 版一致。

**环境配置：**

```bash
# 1. 安装 PostgreSQL（Ubuntu/Debian）
sudo apt update && sudo apt install -y postgresql
sudo systemctl start postgresql

# 2. 配置数据库（database_url 必填，不能留空）
sudo -u postgres psql -c "ALTER USER postgres PASSWORD 'your_password';"
sudo -u postgres createdb phira_mp_plus

# 3. 下载 phira-mp-plus-server-linux-glibc 并赋予执行权限
chmod +x phira-mp-plus-server-linux-glibc

# 4. 启动（默认配置 + PM_DATABASE_URL 指定数据库即可）
PM_DATABASE_URL="postgres://postgres:your_password@localhost:5432/phira_mp_plus" ./phira-mp-plus-server-linux-glibc
# （如需自定义其它配置，可用 --config 指定 server_config.yml）
```

> `database_url` **必填**（留空会启动失败）：PMP 需要 PostgreSQL 连接。
> 数据库需先创建（`createdb`），PMP 启动后自动 sqlx 迁移建表。
> 以非 postgres 用户运行时，`localhost` 走密码认证，需先设置 postgres 密码（`ALTER USER`）。

### Docker 部署（推荐）

需要 Docker 和 Docker Compose：

```bash
# 克隆仓库
git clone https://github.com/HyperSynapseNetwork/Phira-mp-plus.git
cd Phira-mp-plus

# Docker Compose 会自动配置 database_url，默认配置即可
# 一键启动（PostgreSQL + PMP）
docker compose up -d

# 查看日志
docker compose logs -f phira-mp-plus

# 停止
docker compose down
```

Docker Compose 会自动创建 PostgreSQL 容器并初始化数据库。配置文件通过 `server_config.yml` 挂载，数据持久化在 Docker volumes 中。

### 手动部署（从源码编译）

**1. 安装依赖**

```bash
# 安装 Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup update stable

# 安装 PostgreSQL（Ubuntu/Debian）
sudo apt update && sudo apt install -y postgresql
sudo systemctl start postgresql

# 对于二进制部署，安装 musl 目标并构建（可选）
rustup target add x86_64-unknown-linux-musl
sudo apt install -y musl-tools
```

**2. 构建**

```bash
git clone https://github.com/HyperSynapseNetwork/Phira-mp-plus.git
cd Phira-mp-plus
cargo build --release --target x86_64-unknown-linux-musl
```

**3. 配置**

创建 `server_config.yml`：

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
```

**4. 启动**

```bash
# database_url 留空时自动连接本地 PostgreSQL（Unix socket peer auth，无需密码）
# 数据库不存在时自动创建
./target/x86_64-unknown-linux-musl/release/phira-mp-plus-server

# 指定自定义配置启动
./target/x86_64-unknown-linux-musl/release/phira-mp-plus-server --config my_config.yml

# 指定数据库连接串（覆盖配置文件）
PM_DATABASE_URL="postgres://user:pass@host:5432/phira_mp_plus" ./phira-mp-plus-server
```

> **PostgreSQL 设置**：首次启动会自动创建 `phira_mp_plus` 数据库和所有表。如果自动建库失败，可以手动创建：
> ```bash
> sudo -u postgres psql -c "CREATE DATABASE phira_mp_plus;"
> ```

配置加载规则：默认读取 `server_config.yml`，也可通过 `--config <FILE>` 指定；配置文件缺失时使用内置默认值，配置文件存在但格式、字段名或取值无效时拒绝启动。只有用户显式提供的命令行参数才覆盖 YAML，避免 CLI 默认值意外覆盖配置文件。完整说明见 [docs/deployment.md](docs/deployment.md)。

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

空载模式仅改变非关键后台活动的调度偏好，不会暂停权威持久化或可靠插件事件。更多配置项见 [docs/deployment.md](docs/deployment.md)。
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
│   ├── features.md                  #   功能总览（相对 Phira-mp 新增 + 兼容矩阵）
│   ├── deployment.md                #   部署/配置/运维
│   ├── api.md                       #   对外 API（HTTP/SSE/WS + 插件 API + 能力表 + OpenUDS）
│   ├── cli.md                       #   CLI 命令参考
│   ├── plugin-dev.md                #   插件开发指南
│   ├── operations/
│   │   └── config-schema.json       #   配置 JSON Schema
│   └── development/                 #   开发文档
│
├── phira-mp-plus-server/            # 服务端核心 (crate)
│   ├── Cargo.toml
│   ├── locales/                     #   Fluent i18n (en/zh-CN/zh-TW)
│   └── src/
│       ├── main.rs                  #   进程入口 & 生命周期
│       ├── lib.rs                   #   模块导出
│       ├── bin/
│       │   └── pmp-admin.rs         #   独立管理工具 (backup/restore)
│       ├── server/                  #   Server 模块 (9 子模块)
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
│       ├── benchmark/               #   Benchmark 模块
│       │   ├── mod.rs               #    模块入口
│       │   ├── mode.rs              #    模式与参数（fixed/ramp）
│       │   ├── harness.rs           #    进程内负载生成器
│       │   ├── sampler.rs           #    进程内 CPU/RAM 采样
│       │   ├── report.rs            #    报告生成与格式化
│       │   ├── environment.rs       #    环境检测
│       │   ├── mock_phira.rs        #    本地 Mock Phira
│       │   ├── profile.rs           #    CPU/heap profiling
│       │   ├── metrics.rs           #    指标采集
│       │   ├── report.rs            #    报告生成 (text/json/markdown)
│       │   ├── presets.rs           #    预设参数 (quick/standard/stress/soak)
│       │   ├── modes/               #    运行模式
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
│       │   ├── benchmark.rs         #   benchmark run (fixed/ramp)
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
│       ├── room.rs                  #   房间广播接口 (Actor 已独占 members/monitors/live 状态)
│       ├── backup.rs                #   备份与恢复 (仅 pmp-admin binary)
│       ├── crypto.rs                #   HMAC 签名 (sha2)
│       ├── plugin_tcp.rs            #   插件原始 TCP Actor
│       ├── play_history.rs          #   游玩历史
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
│       ├── runtime_diagnostics.rs   #   Runtime 诊断常量
│       ├── benchmark/               #   基准测试模块（模式/负载生成器/采样/报告）
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

