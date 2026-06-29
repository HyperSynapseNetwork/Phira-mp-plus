# Phira-mp +

[![License](https://img.shields.io/badge/License-AGPLv3-blue.svg)](LICENSE)

## 简介

**Phira-mp +** 是基于 [phira-mp](https://github.com/HyperSynapseNetwork/phira-mp) 扩展的 Phira 多人游戏服务端，提供 WASM 插件、管理控制台、HTTP API 与监控数据流。

### 核心特性

- **WASM 插件系统** — 基于 wasmtime 动态加载，通过 `phira:host/api` 访问全部服务端能力
- **TUI 管理控制台** — 基于 `ratatui` + `crossterm` 的终端界面，支持命令输入、日志实时显示
- **内置功能** — 房间信息 Web API、黑名单管理、轮次数据持久化、速率限制等均集成在核心中

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

# 真实网络压测账号。也可以在 TUI/CLI 中执行 benchmark-bind <token1[,token2...]> 生成 data/benchmark-auth.json。
benchmark_phira_tokens:
  - "your-phira-token"
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
├── server_config.yml            # YAML 配置文件（首次运行自动生成默认模板）
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
│   └── src/
│       ├── main.rs              #   进程入口与生命周期
│       ├── logging.rs           #   tracing 输出与日志轮转
│       ├── terminal.rs          #   终端能力检测与 Screen 降级策略
│       ├── lib.rs               #   模块导出
│       ├── server.rs            #   服务器核心: PlusConfig / PlusServerState / PlusServer
│       │                        #     accept 循环、压测方法 (run_benchmark_sync)、
│       │                        #     状态查询分发 (server_state_query_inner)
│       ├── session.rs           #   会话管理: Session / User 模型、认证、命令处理 (process)
│       │                        #     Touches/Judges 数据流向插件事件 + 磁盘存储
│       ├── room.rs              #   房间状态机: InternalRoomState / Room
│       │                        #     选谱→准备→游玩→结算、玩家实时数据缓存
│       ├── plugin.rs            #   插件管理器 + WASM 宿主: PluginManager / PluginHost trait
│       │                        #     插件加载、事件分发、CLI/HTTP/API 注册
│       ├── plugin_http.rs       #   HTTP 服务装配与动态请求分发
│       ├── plugin_http/
│       │   ├── router.rs        #   动态路由匹配
│       │   ├── sse.rs           #   SSE 事件总线、快照与流转换
│       │   └── websocket.rs     #   实时 WebSocket 桥接
│       ├── wasm_host.rs         #   WASM 运行时: wasmtime 实例、JSON ABI、host/api 桥接
│       ├── extensions.rs        #   扩展数据系统: 用户/房间 KV 存储 + auth 缓存持久化
│       ├── ban.rs               #   黑名单系统: 全局封禁 + 房间黑名单
│       ├── round_store.rs       #   轮次数据存储: JSONL 格式、按 round_uuid/player_id 组织
│       ├── rate_limiter.rs      #   速率限制: 滑动窗口 (连接) + 令牌桶 (命令)
│       ├── cli.rs               #   CLI 命令处理器: 30+ 管理命令、插件扩展命令
│       ├── cli_tui.rs           #   TUI 终端界面: ratatui + crossterm
│       └── l10n.rs              #   本地化: Fluent Bundle / tl! 宏
│
├── phira-mp-plus-server-api/    # WASM 插件共享类型 crate
│   └── src/lib.rs               #   PluginEvent / PluginInfo / HttpHandle
│                                #   ServerStateQuery / PluginApiHandler
│
├── phira-mp/                    # 上游 phira-mp 子模块（协议层与原始服务端）
│   ├── phira-mp-common/         #   网络协议: 二进制编码 (BinaryData trait)、
│   │   └── src/                 #     命令定义 (ClientCommand / ServerCommand)、
│   │       ├── lib.rs           #     Stream 帧协议、RoomId / RoomState / 消息类型
│   │       ├── command.rs
│   │       ├── bin.rs           #     BinaryReader / BinaryWriter (LEB128, 小端)
│   │       └── framing.rs       #     打包/拆包 (VARINT 长度前缀)
│   ├── phira-mp-macros/         #   #[derive(BinaryData)] 过程宏
│   ├── phira-mp-server/         #   原始单机服务端 (reference)
│   └── phira-mp-client/         #   TCP 客户端库 (供游戏集成)
│
├── docs/                        # 文档
│   ├── configuration.md         #   配置文件、压测 token 与运行时参数说明
│   ├── cli.md                   #   CLI 命令参考
│   └── plugin-dev.md            #   WASM 插件开发指南 + WIT API 参考
│
├── server_config.yml            # YAML 配置文件 (同级副本, 运行时读取)
└── LICENSE
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
| 诊断/压测 | `benchmark`, `benchmark-bind`, `benchmark-cleanup` | 真实网络压测和账号绑定 |
| WASM 插件 | `plugin list/enable/disable/info/reload` | 插件管理 |
| 用户 | `users`, `kick`, `broadcast` | 用户管理和消息 |
| 房间 | `room list/info/start/cancel/kick/transfer/force-move/hide/set/close/history` | 房间管理子命令 |
| 黑名单 | `ban`, `unban`, `banlist` | 封禁管理 |
| 扩展数据 | `ext-list`, `ext-get` | 扩展字段 |

## WASM 插件开发

详细文档见 [docs/plugin-dev.md](docs/plugin-dev.md)。

WASM 插件通过 `phira:host/api` 和 `phira:host/log` 等导入函数与宿主通信。

## 构建特性

| 特性 | 说明 | 默认 |
|------|------|------|
| `plugin-system` | WASM 插件支持（wasmtime） | 是 |

## 配置参考

完整的配置项见 `server_config.yml` 与 [docs/configuration.md](docs/configuration.md)。常用项如下：

| 配置项 | 类型 | 默认值 | 说明 |
|--------|------|--------|------|
| `port` | u16 | `12346` | TCP 监听端口 |
| `http_port` | u16 | `12347` | HTTP/SSE/WebSocket 服务端口 |
| `monitors` | Vec<i32> | `[2]` | 允许旁观的用户 ID |
| `phira_api_endpoint` | String | `https://phira.5wyxi.com` | 全局 Phira API 端点；房间可临时覆盖 |
| `plugins_dir` | String | `plugins` | WASM 插件目录 |
| `extensions_file` | String | `data/extensions.json` | 扩展数据持久化文件 |
| `benchmark_phira_tokens` | Vec<String> | `[]` | 真实网络压测使用的 Phira token 列表 |
| `benchmark_phira_token` | String | — | 单账号压测 token 兼容写法 |
| `chat_enabled` | bool | 示例为 `true` | 聊天功能开关 |
| `cli_enabled` | bool | `true` | TUI/CLI 控制台开关 |
| `connection_rate_limit` | u32 | `30` | 连接速率限制（窗口内允许次数） |
| `connection_rate_window` | u32 | `10` | 连接速率统计窗口（秒） |
| `max_rooms` | usize | — | 最大房间数（未设置为不限制） |
| `max_users_per_room` | usize | `8` | 每房间最大玩家数 |
| `round_data_retention_days` | u32 | `7` | 轮次 Touches/Judges 保留天数（0=不保留） |
| `database_url` | String | — | PostgreSQL 连接串，未设置时回退文件存储 |
| `server_name` | String | — | 服务器名称 |
| `wasm_runtime.*` | object | 见文档 | WASM 插件资源限制 |

压测 token 可直接写入 `server_config.yml`，也可通过 `benchmark-bind <token1[,token2...]>` 写入 `data/benchmark-auth.json`；如果配置文件中已有 token，压测优先使用配置文件。隐藏房间通过房间名前缀 `-`、`room hide/unhide` 或 `room.set_hidden` 管理，不是全局配置项。房间也可以用 `room set <房间ID> phira_api_endpoint <url>` 临时覆盖全局 Phira API；服务端按房间获取谱面名、用户名、欢迎语与记录校验时会立即使用该 endpoint，`default`/`clear` 可恢复全局配置。`/me` 登录认证仍使用全局 endpoint。无人持久房间可通过 `room create-empty <ID> [API]` 或 `room.create_empty` 创建，最后一名玩家离开后不会自动删除。

## 许可证

AGPLv3 — 详见 [LICENSE](LICENSE)。

## 致谢

感谢 [TeamFlos](https://github.com/TeamFlos) 开发和维护 Phira、phira-mp 项目。

感谢 [tphira-mp](https://github.com/Pimeng/tphira-mp) 与 [jphira-mp](https://github.com/lRENyaaa/jphira-mp) 提供的实现思路，以及所有支持本项目的用户。