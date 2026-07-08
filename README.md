# Phira-mp+

> 本项目使用 AI 开发，仍处于快速迭代期，不建议直接用于生产环境。

[![License](https://img.shields.io/badge/License-AGPLv3-blue.svg)](LICENSE)

## 简介

**Phira-mp+** 是基于 [phira-mp](https://github.com/HyperSynapseNetwork/phira-mp) 扩展的 Phira 多人游戏服务端，提供 WASM 插件、管理控制台、HTTP API 与监控数据流。

### 核心特性

- **轻内核 + 重服务按需唤醒** — TCP listener + CLI 常驻；HTTP/插件/PersistenceWorker 等重服务空闲时自动挂起；活动恢复后按需启动
- **WASM 插件系统** — 基于 wasmtime 组件模型（WIT ABI v2），12 个宿主接口集成测试通过，capability 强制已启用
- **TUI 管理控制台** — 基于 `ratatui` + `crossterm` 的终端界面，支持命令输入、日志实时显示
- **Actor 模型房间命令** — 7 个房间命令全 mailboxed，lock/cycle/host owned-tracked；12 个会话命令 WriteRouted
- **内置功能** — 房间信息 Web API、SSE 实时事件流（SseHub）、blacklist/IP 封禁、轮次数据持久化、速率限制
- **jemalloc 分配器** — Linux 下使用 jemalloc 替代 musl malloc，主动归还空闲内存，避免 RSS 虚高

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

配置加载规则：默认读取 `server_config.yml`，也可通过 `--config <FILE>` 指定；配置文件缺失时使用默认值；命令行参数会覆盖端口、插件目录、监控 ID、扩展数据路径和 CLI 开关等同名设置。完整说明见 [docs/configuration.md](docs/configuration.md)。

### 命令行参数

```
phira-mp-plus-server [OPTIONS]

  -p, --port <PORT>          服务器监听端口 [默认: 12346]
  -d, --plugins-dir <DIR>    插件目录 [默认: "plugins"]
  -e, --ext-file <FILE>      扩展数据持久化文件 [默认: "data/extensions.json"]
      --no-cli               禁用管理控制台
  -l, --log-file <NAME>      日志文件基础名称 [默认: "phira-mp-plus"]
  -m, --monitor <IDS>...     允许旁观的用户 ID
      --http-port <PORT>     HTTP/SSE 服务端口 [默认: 12347]
      --proxy-port <PORT>    PROXY protocol 端口 [默认: 0=禁用, 典型值 12344]
  -c, --config <FILE>        YAML 配置文件路径 [默认: "server_config.yml"]
  -h, --help                 显示帮助
  -V, --version              显示版本
```

## 项目结构

```
Phira-mp-plus/
│
├── Cargo.toml                   # 工作区根 (workspace)
├── Cargo.lock
├── LICENSE                      # AGPL-3.0
├── README.md
│
├── server_config.yml            # YAML 配置文件
├── data/                        # 运行时数据目录
│   ├── extensions.json          #   插件扩展数据持久化
│   ├── rounds/                  #   轮次 Touches/Judges 记录
│   └── plugins/                 #   插件私有数据
├── log/                         # 运行日志（每小时轮转）
│
├── phira-mp-plus-server/        # 服务端核心
│   ├── Cargo.toml               #   axum / tokio / wasmtime / clap 等依赖
│   ├── locales/                 #   Fluent i18n 翻译文件
│   │   ├── en-US.ftl
│   │   ├── zh-CN.ftl
│   │   └── zh-TW.ftl
│   ├── src/
│   │   ├── main.rs                  #   进程入口与生命周期
│   │   ├── lib.rs                   #   模块导出
│   │   ├── cli.rs                   #   CLI 生命周期、输入循环、输出 helper
│   │   ├── cli/dispatch.rs          #   顶层命令路由
│   │   ├── cli/commands/            #   按功能分组的命令模块
│   │   │   ├── admin.rs             #   admin-id、ban、extension 管理
│   │   │   ├── benchmark.rs         #   benchmark 命令
│   │   │   ├── broadcast.rs         #   broadcast 消息广播
│   │   │   ├── plugin.rs            #   WASM 插件管理
│   │   │   ├── room.rs              #   房间管理子命令
│   │   │   ├── runtime/             #   runtime 诊断子命令
│   │   │   └── simulation/          #   simulation 压测子命令
│   │   ├── cli_tui.rs               #   TUI 终端界面: ratatui + crossterm
│   │   ├── server.rs                #   服务器核心: PlusConfig / PlusServerState / PlusServer
│   │   ├── server_query.rs          #   Admin ID 等查询函数
│   │   ├── command_registry.rs      #   Runtime v2 命令注册表
│   │   ├── session*.rs              #   会话、认证、房间协议、遥测处理、session actor
│   │   ├── session_actor.rs         #   Session Actor 命令 mailbox
│   │   ├── supervisor_actor.rs      #   服务监督者 mailbox
│   │   ├── room.rs                  #   房间状态机: InternalRoomState / Room
│   │   ├── room_actor/              #   Room Actor 命令网关与 per-room mailbox
│   │   ├── idle.rs                  #   空载模式: IdleConfig / IdleMonitor
│   │   ├── persistence/             #   持久化 Worker 管道
│   │   ├── proxy_protocol.rs        #   PROXY protocol 解析与 TrustForwardedFor 中间件
│   │   ├── telemetry*.rs            #   TelemetryBatcher、cutover 模式、策略
│   │   ├── round_store.rs           #   轮次数据存储 (DB 优先, 文件回退)
│   │   ├── internal_hooks.rs        #   内部静态注册 (DB / playtime)
│   │   ├── plugin.rs                #   插件管理器: PluginManager / PluginHost trait
│   │   ├── plugin_abi/              #   Plugin ABI 边界、WIT 元数据
│   │   ├── plugin_http/             #   HTTP 动态路由、SSE 与 WebSocket
│   │   ├── wasm_host*.rs            #   WASM 运行时与 host 辅助函数
│   │   ├── wit_host.rs              #   WIT/component-model host trait 实现
│   │   ├── extensions.rs            #   扩展数据系统: 用户/房间/全局 KV 存储
│   │   ├── ban.rs                   #   黑名单系统: 全局封禁 + 房间黑名单 + IP 封禁
│   │   ├── phira_client.rs          #   统一 Phira HTTP RetryClient
│   │   ├── rate_limiter.rs          #   速率限制: 滑动窗口 + 令牌桶
│   │   ├── event_bus.rs             #   EventBus 运行时脊椎
│   │   ├── simulation*.rs           #   Simulation 管理器与 realistic 场景
│   │   ├── l10n.rs                  #   本地化: Fluent Bundle / tl! 宏
│   │   └── logging.rs               #   tracing 输出、日志轮转、速率限制、敏感数据脱敏
│   ├── tests/                       #   集成与合约测试
│
├── phira-mp-plus-server-api/    # WASM 插件共享类型 crate
│   └── src/lib.rs               #   PluginEvent / PluginInfo / HttpHandle 等共享类型
│
├── phira-plugin-sdk/            # WASM 插件开发 SDK
│
├── phira-mp/                    # 上游 phira-mp 子模块（协议层与原始服务端）
│   ├── phira-mp-common/         #   网络协议: 二进制编码 (BinaryData trait)、
│   │   └── src/                 #     命令定义 (ClientCommand / ServerCommand)、
│   │       ├── lib.rs           #     Stream 帧协议、RoomId / RoomState / 消息类型
│   │       ├── command.rs
│   │       └── bin.rs           #     BinaryReader / BinaryWriter (LEB128, 小端)
│   ├── phira-mp-macros/         #   #[derive(BinaryData)] 过程宏
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

## SSE 房间事件

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
| 房间 | `rooms`, `room info/start/cancel/kick/host/force-move/hide/set/close/history` | 房间管理子命令 |
| 黑名单 | `ban`, `unban`, `banlist`, `room ban/unban/banlist` | 全局与房间封禁管理 |

## WASM 插件开发

详细文档见 [docs/plugin-dev.md](docs/plugin-dev.md)。

WASM 插件通过 `phira:host/api` 和 `phira:host/log` 等导入函数与宿主通信。

## 构建特性

| 特性 | 说明 | 默认 |
|------|------|------|
| `plugin-system` | WASM 插件支持（wasmtime） | 是 |
| `postgres` | PostgreSQL 统一持久化 | 是 |

## 配置参考

完整的配置项见 `server_config.yml` 与 [docs/configuration.md](docs/configuration.md)。常用项如下：

| 配置项 | 类型 | 默认值 | 说明 |
|--------|------|--------|------|
| `port` | u16 | `12346` | TCP 监听端口 |
| `http_port` | u16 | `12347` | HTTP/SSE/WebSocket 服务端口 |
| `proxy_protocol_port` | u16 | `0` | PROXY protocol 端口（0=禁用，典型值 12344） |
| `monitors` | Vec<i32> | `[2]` | 允许旁观的用户 ID |
| `phira_api_endpoint` | String | `https://phira.5wyxi.com` | 全局 Phira API 端点；房间可临时覆盖 |
| `plugins_dir` | String | `plugins` | WASM 插件目录 |
| `database_url` | String | — | PostgreSQL 统一持久化连接串 |
| `persistence_retention_days` | u32 | 30 | PG 历史数据保留天数，0 为不清理 |
| `touch_judge_retention_days` | u32? | 未设置 | Touches/Judges 高频遥测独立保留天数；未设置时遵循 `persistence_retention_days`，0 为不清理 |
| `admin_phira_ids` | Vec<i32> | [] | 游戏内管理员 ID，可使用 `_命令` 入口 |
| `chat_enabled` | bool | 示例为 `true` | 聊天功能开关 |
| `cli_enabled` | bool | `true` | TUI/CLI 控制台开关 |
| `connection_rate_limit` | u32 | `30` | 连接速率限制（窗口内允许次数） |
| `connection_rate_window` | u32 | `10` | 连接速率统计窗口（秒） |
| `max_rooms` | usize | — | 最大房间数（未设置为不限制） |
| `max_users_per_room` | usize | `8` | 每房间最大玩家数 |
| `round_data_retention_days` | u32 | `7` | 轮次 Touches/Judges 保留天数（0=不保留） |
| `server_name` | String | — | 服务器名称 |
| `wasm_runtime.*` | object | 见文档 | WASM 插件资源限制 |

真实网络压测（`benchmark run real`）是高级兼容性测试，详见 docs/benchmark-real.md。默认压测推荐使用 Simulation，不需要 token。

## 许可证

AGPLv3 — 详见 [LICENSE](LICENSE)。

## 致谢

感谢 [TeamFlos](https://github.com/TeamFlos) 开发和维护 Phira、phira-mp 项目。

感谢 [tphira-mp](https://github.com/Pimeng/tphira-mp) 与 [jphira-mp](https://github.com/lRENyaaa/jphira-mp) 提供的实现思路，以及所有支持本项目的用户。
