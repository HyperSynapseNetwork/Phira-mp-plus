# Phira-mp+

> 基于 Phira-mp 二次开发的增强版 Phira 多人游戏服务端

[![License](https://img.shields.io/badge/License-AGPLv3-blue.svg)](LICENSE)

## 简介

**Phira-mp+** 是 [Phira-mp](https://github.com/team-phira/phira-mp) 的增强版本，具备完善的插件系统、TUI 管理控制台、黑名单管理、游玩记录追踪、Web API 等特性。

### 核心特性

- **🧩 插件系统** — 原生 Rust 插件（`NativePlugin` trait）+ WASM 插件（基于 wasmtime 动态加载），插件可注册路由、CLI 命令、Hook 事件
- **🖥️ TUI 管理控制台** — 基于 `ratatui` + `crossterm` 的终端界面，支持命令输入、日志实时显示
- **🌐 中央 HTTP/SSE 服务器** — 插件在统一端口注册路由和 SSE 事件推送
- **📊 内置插件** — 房间信息 Web API、玩家追踪、游玩时间统计、结算排行、欢迎语
- **🔨 黑名单系统** — 全局封禁 + 房间黑名单，连接时自动拦截
- **🆔 房间/轮次 UUID** — 每个房间和轮次分配唯一标识符
- **🗄️ PostgreSQL 支持** — 可选数据库后端（`--features postgres`）
- **🛡️ AGPLv3 开源协议**

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
| [sqlx](https://github.com/launchbadge/sqlx) | PostgreSQL 驱动（可选） |

## 快速开始

```bash
# 构建（所有功能默认开启）
cargo build

# 运行
./target/debug/phira-mp-plus-server

# 带 PostgreSQL（可选）
cargo build --features postgres
DATABASE_URL=postgres://user:pass@localhost:5432/phira_mp_plus ./target/debug/phira-mp-plus-server
```

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
  -h, --help                 显示帮助
  -V, --version              显示版本
```

## 项目结构

```
phira-mp-plus-server/          # 服务端
├── src/
│   ├── main.rs                # 入口点
│   ├── ban.rs                 # 黑名单管理
│   ├── cli.rs                 # TUI 管理控制台
│   ├── extensions.rs          # 扩展数据系统
│   ├── l10n.rs                # 本地化
│   ├── plugin.rs              # 插件管理器
│   ├── plugin_http.rs         # 中央 HTTP/SSE 服务器
│   ├── room.rs                # 房间状态机
│   ├── server.rs              # 服务器核心
│   └── session.rs             # 会话管理

phira-mp-plus-server-api/      # 插件 API 公共 crate
phira-mp-plus-sdk/             # 插件开发 SDK

plugins/                       # 独立插件
├── webapi-plugin/             # 房间信息 Web API
├── player-tracker/            # 玩家记录
├── playtime-tracker/          # 游玩时间统计
├── round-results/             # 结算排行
└── welcome-plugin/            # 欢迎语
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
| `postgres` | PostgreSQL 数据库支持 | ❌ |

## 许可证

AGPLv3 — 详见 [LICENSE](LICENSE)。
