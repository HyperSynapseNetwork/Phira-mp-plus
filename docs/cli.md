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

配置加载顺序（后覆盖前）：YAML 配置文件 < 环境变量 < CLI 参数。

## 交互式管理控制台

服务器在普通交互式终端和 tmux 中启动 ratatui 管理控制台。GNU Screen 环境自动切换为逐行兼容控制台，不输出颜色、备用屏幕、鼠标或 Bracketed Paste 控制序列；重定向、systemd 和其他非 TTY 环境也使用逐行控制台。设置 `NO_COLOR` 可在其他终端中关闭颜色。

### 命令列表

#### 通用命令

| 命令 | 别名 | 说明 |
|------|------|------|
| `help` | `h`, `?` | 显示帮助信息 |
| `exit` | `quit`, `q` | 关闭服务器 |
| `status` | `st` | 显示服务器状态 |

#### 插件管理（WASM）

| 命令 | 说明 |
|------|------|
| `plugin list` | 列出所有已加载的 WASM 插件 |
| `plugin enable <名>` | 启用指定插件 |
| `plugin disable <名>` | 禁用指定插件 |
| `plugin info <名>` | 显示插件详细信息 |
| `plugin reload` | 重载所有 WASM 插件 |

#### 用户管理

| 命令 | 说明 |
|------|------|
| `users` | 列出在线用户 |
| `kick <用户ID>` | 从服务器踢出用户 |
| `kick <房间ID> <用户ID>` | 从房间踢出用户 |
| `broadcast [作用域] <消息>` | 广播消息 |

##### broadcast 作用域

```
broadcast all <消息>             广播给所有用户
broadcast room <房间ID> <消息>    广播给指定房间
broadcast user <用户ID> <消息>    发送给指定用户
```

#### 房间管理（room 子命令）

| 命令 | 说明 |
|------|------|
| `rooms` / `room list` | 列出活跃房间 |
| `room info <房间ID>` | 房间详情（状态、房主、谱面、历史） |
| `room start <房间ID>` | 强制开始游戏 |
| `room cancel <房间ID>` | 取消准备状态 |
| `room kick <房间ID> <用户ID>` | 从房间踢出用户 |
| `room transfer <房间ID> <用户ID>` | 转移房主 |
| `room set <房间ID> <字段> <值>` | 修改房间设置（lock/cycle/chart-id） |
| `room close <房间ID>` | 解散房间 |
| `room history <房间ID>` | 查看游玩记录 |
| `room ban <房间ID> <用户ID>` | 房间加入黑名单 |
| `room unban <房间ID> <用户ID>` | 房间移出黑名单 |
| `room banlist <房间ID>` | 房间黑名单列表 |

兼容旧别名：`rooms`, `room-info` / `ri`, `room-start` / `rs`, `room-cancel` / `rc`,
`room-transfer` / `rt`, `room-history` / `rh`, `close-room` / `cr`,
`room-ban` / `rb`, `room-unban` / `ru`, `room-banlist` / `rbl`

#### 黑名单管理

| 命令 | 说明 |
|------|------|
| `ban <用户ID> [原因]` | 封禁用户 |
| `unban <用户ID>` | 解封用户 |
| `banlist` | 列出封禁列表 |

#### 扩展数据

| 命令 | 说明 |
|------|------|
| `ext-list` | 列出所有注册的扩展数据字段 |
| `ext-get <ID> <key>` | 获取指定用户/房间的扩展数据 |

## Web API

中央 HTTP/SSE 服务器监听配置的 `--http-port`（默认 12347）。

| 端点 | 说明 |
|------|------|
| `GET /api/rooms` | 房间列表（含详情） |
| `GET /api/rooms/{name}` | 指定房间信息 |
| `GET /api/user_name/{id}` | 用户名称查询 |
| `GET /api/players/count` | 在线玩家数 |
| `GET /api/events` | 统一 SSE 端点 |
| `GET /rooms/listen` | SSE 房间事件流（web-monitor 兼容） |
| `GET /ws/live` | WebSocket 实时监测（web-monitor 兼容） |

详细 API 文档见 [api.md](api.md)。

## WASM 插件系统

服务器支持通过 wasmtime 加载 `.wasm` 插件。插件需放置在 `plugins/` 目录（可通过 `-d` 自定义）。
插件通过 `phira:host/api` 导入函数访问服务器全部能力：

- 状态查询：rooms.list, player.touches, round.data 等
- 消息发送：send.to_user, send.to_room, send.to_all
- 房间管理：room.kick, room.transfer_host, room.set_lock, room.close
- 用户管理：admin.kick_user, admin.ban_user, admin.unban_user, admin.is_banned
- 插件互调用：plugin.api_call, plugin.api_register
- 数据读写：ext.get/set, config.get/set, file.read/write
- HTTP 请求：http.get/post

具体接口定义见 `wit/phira/mpplus.wit`。

## 日志文件

日志文件存储在 `log/` 目录下，按小时轮转。

日志级别通过 `RUST_LOG` 环境变量控制：

```bash
RUST_LOG=info phira-mp-plus-server
RUST_LOG=debug phira-mp-plus-server
```
