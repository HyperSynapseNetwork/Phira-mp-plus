# Phira-mp+

> 基于 Phira-mp 二次开发的增强版 Phira 多人游戏服务端

[![License](https://img.shields.io/badge/License-AGPLv3-blue.svg)](LICENSE)

## 简介

**Phira-mp+** 是 [Phira-mp](https://github.com/team-phira/phira-mp) 的增强版本，具备完善的插件系统、TUI 管理控制台、黑名单管理、游玩记录追踪、Web API 等特性。

### 核心特性

- **🧩 插件系统** — 原生 Rust 插件（`NativePlugin` trait）+ WASM 插件（基于 wasmtime 动态加载），插件可注册路由、CLI 命令、Hook 事件
- **🖥️ TUI 管理控制台** — 基于 `ratatui` + `crossterm` 的终端界面，支持命令输入、日志实时显示
- **🌐 中央 HTTP/SSE 服务器** — 插件在统一端口注册路由和 SSE 事件推送，支持运行时动态注册
- **⚙️ YAML 配置文件** — 支持 `server_config.yml`，三层覆盖：YAML → 环境变量 → CLI 参数
- **🚦 速率限制** — 连接速率限制（按 IP 滑动窗口）+ 命令速率限制（令牌桶）
- **🔐 安全增强** — 令牌使用 SHA-256 哈希，最大会话数保护
- **🌍 本地化** — 支持多语言消息（en-US/zh-CN/zh-TW），基于 Fluent
- **📊 内置插件** — 房间信息 Web API、玩家追踪、游玩时间统计、结算排行、欢迎语
- **🔨 黑名单系统** — 全局封禁 + 房间黑名单，连接时自动拦截
- **🆔 房间/轮次 UUID** — 每个房间和轮次分配唯一标识符
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
| [serde_yaml](https://docs.rs/serde_yaml/) | YAML 配置解析 |

## 快速开始

```bash
# 构建（所有功能默认开启）
cargo build

# 运行（使用默认配置）
./target/debug/phira-mp-plus-server

# 指定配置文件
./target/debug/phira-mp-plus-server --config my_config.yml
```

YAML 配置文件支持自动生成，首次运行时会创建默认的 `server_config.yml`。

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
phira-mp-plus-server/          # 服务端
├── src/
│   ├── main.rs                # 入口点
│   ├── ban.rs                 # 黑名单管理
│   ├── cli.rs                 # CLI 命令处理器
│   ├── cli_tui.rs             # TUI 终端界面
│   ├── extensions.rs          # 扩展数据系统
│   ├── l10n.rs                # 本地化系统
│   ├── plugin.rs              # 插件管理器
│   ├── plugin_http.rs         # 中央 HTTP/SSE 服务器
│   ├── rate_limiter.rs        # 速率限制器
│   ├── room.rs                # 房间状态机
│   ├── server.rs              # 服务器核心 + 配置
│   └── session.rs             # 会话管理与命令处理
├── locales/                   # Fluent 翻译文件
│   ├── en-US.ftl
│   ├── zh-CN.ftl
│   └── zh-TW.ftl

phira-mp-plus-server-api/      # 插件 API 公共 crate

plugins/                       # 内置插件
├── webapi-plugin/             # 房间信息 Web API
├── player-tracker/            # 玩家记录
├── playtime-tracker/          # 游玩时间统计
├── round-results/             # 结算排行
└── welcome-plugin/            # 欢迎语

server_config.yml              # YAML 配置文件（自动生成）
data/                          # 运行时数据
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
| `server_name` | String | — | 服务器名称 |

## 许可证

AGPLv3 — 详见 [LICENSE](LICENSE)。
