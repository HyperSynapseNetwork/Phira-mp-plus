# Phira-mp+ CLI 命令文档

## 启动参数

```
phira-mp-plus-server [OPTIONS]

  -p, --port <PORT>          服务器监听端口 [默认: 12346]
  -d, --plugins-dir <DIR>    WASM 插件目录路径 [默认: "plugins"]
  -e, --ext-file <FILE>      扩展数据持久化 JSON 文件路径 [默认: "data/extensions.json"]
      --no-cli               禁用交互式 CLI 管理控制台
  -l, --log-file <NAME>      日志文件基础名称 [默认: "phira-mp-plus"]
  -m, --monitor <IDS>...     允许旁观的用户 ID（可多次指定，如 `-m 1 -m 2`）
      --http-port <PORT>     HTTP/SSE 服务端口 [默认: 12347]
  -c, --config <FILE>        YAML 配置文件路径 [默认: "server_config.yml"]
  -h, --help                 显示帮助信息
  -V, --version              显示版本号
```

配置加载顺序（后覆盖前）：YAML 配置文件 < CLI 参数。

## 交互式管理控制台

服务器启动后自动进入 TUI（基于 ratatui）管理控制台。支持命令输入和日志实时显示。

### 命令列表

#### 通用命令

| 命令 | 别名 | 说明 |
|------|------|------|
| `help` | `h`, `?` | 显示帮助信息 |
| `exit` | `quit`, `q` | 关闭服务器 |
| `status` | `st` | 显示服务器状态 |

#### 插件管理

| 命令 | 别名 | 说明 |
|------|------|------|
| `plugins` | `pl` | 列出所有已加载的插件 |
| `plugin list` | — | 列出所有插件 |
| `plugin enable <名>` | `pe` | 启用指定插件 |
| `plugin disable <名>` | `pd` | 禁用指定插件 |
| `plugin info <名>` | — | 显示插件详细信息 |
| `plugin reload` | `pr` | 重载所有插件 |

#### 用户 / 房间管理

| 命令 | 别名 | 说明 |
|------|------|------|
| `users` | `u` | 列出在线用户 |
| `rooms` | `r` | 列出活跃房间 |
| `room-info <id>` | `ri` | 房间详情（状态、房主、谱面、历史） |
| `room-transfer <id> <uid>` | `rt` | 转移房主 |
| `room-set <id> <字段> <值>` | — | 修改房间设置（lock/cycle/chart-id） |
| `room-start <id>` | `rs` | 强制开始游戏 |
| `room-cancel <id>` | `rc` | 取消准备状态 |
| `room-history <id>` | `rh` | 查看游玩记录 |
| `kick <房间ID> <用户ID>` | `k` | 踢出用户（从房间或服务器） |
| `close-room <id>` | `cr` | 解散房间 |
| `broadcast [作用域] <消息>` | `bc` | 广播消息 |

##### broadcast 作用域

```
broadcast all <消息>           广播给所有用户
broadcast room <房间ID> <消息>   广播给指定房间
broadcast user <用户ID> <消息>   发送给指定用户
```

#### 扩展数据

| 命令 | 别名 | 说明 |
|------|------|------|
| `ext-list` | `el` | 列出所有注册的扩展数据字段 |
| `ext-get <ID> <key>` | `eg` | 获取指定用户/房间的扩展数据 |

#### 黑名单管理

| 命令 | 别名 | 说明 |
|------|------|------|
| `ban <用户ID> [原因]` | — | 封禁用户 |
| `unban <用户ID>` | — | 解封用户 |
| `banlist` | `bl` | 列出封禁列表 |
| `room-ban <房间ID> <用户ID>` | `rb` | 房间加入黑名单 |
| `room-unban <房间ID> <用户ID>` | `ru` | 房间移出黑名单 |
| `room-banlist <房间ID>` | `rbl` | 房间黑名单列表 |

#### 插件扩展命令

| 命令 | 说明 |
|------|------|
| `players [页码]` | 列出所有游玩过的玩家（翻页） |
| `player-count` | 游玩过的玩家总数 |
| `playtime <用户ID>` | 查询指定用户的游玩时间 |
| `playtime-top [数量]` | 游玩时间排行榜前 N |
| `round-last <房间ID>` | 查看房间最近一轮结算 |
| `welcome-config` | 查看欢迎语配置 |

## Web API

中央 HTTP/SSE 服务器监听配置的 `--http-port`（默认 12347）。

| 端点 | 说明 |
|------|------|
| `GET /api/rooms/info` | 房间列表（含详情） |
| `GET /api/rooms/info/{name}` | 指定房间信息 |
| `GET /api/rooms/user/{user_id}` | 用户所在房间 |
| `GET /api/rooms/listen` | SSE 事件流 |
| `GET /api/events` | 统一 SSE 端点 |
| `GET /api/players/count` | 游玩过的玩家数 |
| `GET /api/players/list?page=1` | 玩家列表（翻页） |
| `GET /api/user_rank/{user_id}` | 用户游玩时间排名 |
| `GET /api/user_playtime_ranking` | 游玩时间前 10 |
| `GET /api/playtime_leaderboard` | 全部游玩时间排行 |
| `GET /api/playtime_leaderboard/top/{n}` | 前 N 名游玩时间排行 |
| `GET /api/round/last/{room_id}` | 房间最近一轮结算 |

## 日志文件

日志文件存储在 `log/` 目录下，按小时轮转。

日志级别通过 `RUST_LOG` 环境变量控制：

```bash
RUST_LOG=info phira-mp-plus-server
RUST_LOG=debug phira-mp-plus-server
```
