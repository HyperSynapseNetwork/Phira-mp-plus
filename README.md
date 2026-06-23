# Phira-mp +

> 使用AI开发

[![License](https://img.shields.io/badge/License-AGPLv3-blue.svg)](LICENSE)

## 简介

**Phira-mp +** 是基于 [Phira-mp By HSN](https://github.com/HyperSynapseNetwork/phira-mp) 开发的Phira多人游戏服务端，使用Rust开发，支持WASM插件系统，旨在提供稳定，高性能，高拓展性的Phira多人游戏服务端。

### 核心特性

- **🧩 插件系统** — 原生 Rust 插件（`NativePlugin` trait）+ WASM 插件（基于 wasmtime 动态加载），插件可注册路由、CLI 命令、Hook 事件
- **🖥️ TUI 管理控制台** — 基于 `ratatui` + `crossterm` 的终端界面，支持命令输入、日志实时显示
- **📊 内置插件** — 房间信息 Web API、玩家追踪、游玩时间统计、结算排行、欢迎语

## 技术栈

| 技术 | 用途 |
|------|------|
| [Rust](https://www.rust-lang.org/) | 主开发语言 (2024 Edition) |
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

确保已安装 Rust 工具链（建议 1.70+）：

```bash
# 安装/更新 Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup update stable

# 克隆仓库后进入目录
cd phira-mp-plus

# 构建（所有功能默认开启，首次编译约需 2-5 分钟）
cargo build

# 使用默认配置启动（debug 模式）
./target/debug/phira-mp-plus-server

# 指定自定义配置文件启动
./target/debug/phira-mp-plus-server --config my_config.yml

# 💡 release 模式对应路径为 ./target/release/phira-mp-plus-server
```

### 自定义配置

创建 `server_config.yml`（见项目根目录的示例文件）：

```yaml
port: 12346
http_port: 12347
monitors:
  - 12345
  - 67890
connection_rate_limit: 60
connection_rate_window: 10
server_name: "My Phira Server"
chat_enabled: true
```

配置加载顺序（后覆盖前）：YAML 配置文件 < CLI 参数。

### 命令行参数

```
phira-mp-plus-server [OPTIONS]

  -p, --port <PORT>          服务器监听端口 [默认: 12346]
  -d, --plugins-dir <DIR>    插件目录 [默认: "plugins"]
  -e, --ext-file <FILE>      扩展数据持久化文件 [默认: "data/extensions.json"]
      --no-cli               禁用 TUI 管理控制台
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
├── phira-mp-plus-server/        # ★ 服务端核心
│   ├── Cargo.toml               #   axum / tokio / wasmtime / clap 等依赖
│   ├── locales/                 #   Fluent i18n 翻译文件
│   │   ├── en-US.ftl
│   │   ├── zh-CN.ftl
│   │   └── zh-TW.ftl
│   └── src/
│       ├── main.rs              #   入口: CLI 解析 → 日志初始化 → TUI → accept 循环
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
│       ├── plugin_http.rs       #   中央 HTTP/SSE/WS 服务器: 动态路由 / SSE / WebSocket
│       ├── wasm_host.rs         #   WASM 运行时: wasmtime 实例、JSON ABI、host/api 桥接
│       ├── extensions.rs        #   扩展数据系统: 用户/房间 KV 存储 + auth 缓存持久化
│       ├── ban.rs               #   黑名单系统: 全局封禁 + 房间黑名单
│       ├── round_store.rs       #   轮次数据存储: JSONL 格式、按 round_uuid/player_id 组织
│       ├── rate_limiter.rs      #   速率限制: 滑动窗口 (连接) + 令牌桶 (命令)
│       ├── cli.rs               #   CLI 命令处理器: 30+ 管理命令、插件扩展命令
│       ├── cli_tui.rs           #   TUI 终端界面: ratatui + crossterm
│       └── l10n.rs              #   本地化: Fluent Bundle / tl! 宏
│
├── phira-mp-plus-server-api/    # ★ 插件 API 公共 crate（打破循环依赖）
│   └── src/lib.rs               #   PluginEvent / NativePlugin / PluginContext
│                                #   ServerStateQuery / HttpHandle / CliHandle / PluginApiRegistry
│
├── phira-mp/                    # ★ 上游 phira-mp By HSN 子模块 (协议层 + 原始服务端)
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
├── plugins/                     # ★ 内置原生插件
│   ├── webapi-plugin/           #   房间信息 REST API
│   │   └── src/lib.rs           #   GET /api/rooms/info, /api/rooms/info/{name}, /api/rooms/user/{id}
│   ├── player-tracker/          #   玩家记录
│   │   └── src/lib.rs           #   CLI: players, player-count  |  API: count, list
│   ├── playtime-tracker/        #   游玩时间统计
│   │   └── src/lib.rs           #   CLI: playtime, playtime-top  |  API: user_playtime, leaderboard
│   ├── round-results/           #   结算排行输出
│   │   └── src/lib.rs           #   CLI: round-last  |  API: /api/round/last/{room_id}
│   ├── welcome-plugin/          #   可配置欢迎语
│   │   └── src/lib.rs           #   占位符: [user_name] [player-count] [top_playtime] [active_rooms]
│   ├── web-monitor/             #   Web 监测 API（兼容 .phira-web-monitor）
│   │   └── src/lib.rs           #   /rooms/info, /rooms/listen, /ws/live, /auth/login, /chart/{id}
│   └── stress-test/             #   服务端压测
│       └── src/lib.rs           #   CLI: benchmark, bench-quick, bench-cleanup
│
├── docs/                        # 文档
│   ├── cli.md                   #   CLI 命令参考
│   ├── plugin-dev.md            #   插件开发指南 + WIT API 参考
│   └── plugins/                 #   各插件独立文档
│       ├── player-tracker.md
│       ├── playtime-tracker.md
│       ├── room-info-web-api.md
│       ├── round-results.md
│       └── welcome-plugin.md
│
├── server_config.yml            # YAML 配置文件 (同级副本, 运行时读取)
└── LICENSE
```

## 内置插件

详细文档见 [docs/plugins/](docs/plugins/)。

| 插件 | 功能 | CLI 命令 | Web API |
|------|------|---------|---------|
| **room-info-web-api** | 房间信息查询与 SSE 事件 | — | `GET /api/rooms/info` |
| **player-tracker** | 记录游玩过的玩家 | `players`, `player-count` | `GET /api/players/count`, `/api/players/list` |
| **playtime-tracker** | 游玩时间统计与排行 | `playtime <id>`, `playtime-top [n]` | `GET /api/user_rank/<id>` |
| **round-results** | 每轮结算排行输出 | `round-last <room_id>` | `GET /api/round/last/<room_id>` |
| **welcome-plugin** | 可配置欢迎语（支持占位符） | `welcome-config` | — |

## CLI 命令

详细文档见 [docs/cli.md](docs/cli.md)。快速概览：

| 分组 | 命令 | 说明 |
|------|------|------|
| 通用 | `help`, `exit`, `status` | 帮助/退出/状态 |
| 插件管理 | `plugins`, `plugin list/enable/disable/info/reload` | 插件管理 |
| 用户/房间 | `users`, `rooms`, `room-info`, `room-transfer`, `room-set`, `room-start`, `room-cancel`, `room-history`, `kick`, `close-room`, `broadcast` | 用户和房间管理 |
| 扩展数据 | `ext-list`, `ext-get` | 扩展字段 |
| 黑名单 | `ban`, `unban`, `banlist`, `room-ban`, `room-unban`, `room-banlist` | 封禁管理 |
| 插件扩展 | `players`, `player-count`, `playtime`, `playtime-top`, `round-last`, `welcome-config` | 插件命令 |

## 插件开发

详细文档见 [docs/plugin-dev.md](docs/plugin-dev.md)。

```rust
use phira_mp_plus_server_api::{
    NativePlugin, PluginContext, PluginEvent, PluginInfo,
};

pub struct MyPlugin;

impl NativePlugin for MyPlugin {
    fn info(&self) -> PluginInfo { /* ... */ }
    fn init(&mut self, ctx: &PluginContext) -> Result<(), String> {
        if let Some(http) = &ctx.http {
            http.register_route("/api/hello", Arc::new(|_, _| {
                Ok(serde_json::json!({"hello": "world"}))
            }));
        }
        Ok(())
    }
}
```

## 构建特性

| 特性 | 说明 | 默认 |
|------|------|------|
| `plugin-system` | WASM 插件支持（wasmtime） | ✅ |
| `webapi` | 房间信息 Web API 插件 | ✅ |
| `player-tracker` | 玩家记录插件 | ✅ |
| `playtime-tracker` | 游玩时间统计插件 | ✅ |
| `round-results` | 结算排行插件 | ✅ |
| `welcome-plugin` | 欢迎语插件 | ✅ |

## 配置参考

完整的配置项见 `server_config.yml`：

| 配置项 | 类型 | 默认值 | 说明 |
|--------|------|--------|------|
| `port` | u16 | `12346` | TCP 监听端口 |
| `http_port` | u16 | `12347` | HTTP/SSE 服务端口 |
| `monitors` | Vec<i32> | `[2]` | 允许旁观的用户 ID |
| `phira_api_endpoint` | String | `https://phira.5wyxi.com` | Phira API 端点 |
| `plugins_dir` | String | `plugins` | 插件目录 |
| `chat_enabled` | bool | `true` | 聊天功能开关 |
| `cli_enabled` | bool | `true` | CLI 控制台开关 |
| `connection_rate_limit` | u32 | `30` | 连接速率限制（窗口内允许次数） |
| `connection_rate_window` | u32 | `10` | 连接速率统计窗口（秒） |
| `max_users_per_room` | usize | `8` | 每房间最大玩家数 |
| `round_data_retention_days` | u32 | `7` | 轮次 Touches/Judges 保留天数（0=不保留） |
| `server_name` | String | — | 服务器名称 |

## 许可证

AGPLv3 — 详见 [LICENSE](LICENSE)。

## 致谢

感谢[TeamFlos](https://github.com/TeamFlos)开发并维护Phira,Phira-mp项目

感谢[tphira-mp](https://github.com/Pimeng/tphira-mp),[jphira-mp](https://github.com/lRENyaaa/jphira-mp)的启发

感谢所有资助过我们的用户[捐献页](https://phira.htadiy.com/about)
](https://github.com/HyperSynapseNetwork/Phira-mp-plus/)
