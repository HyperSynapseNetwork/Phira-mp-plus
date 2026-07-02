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

配置加载规则：默认读取 `server_config.yml`，也可通过 `--config <FILE>` 指定；命令行参数会覆盖 YAML 中对应字段。`RUST_LOG`、`NO_COLOR` 等环境变量只影响日志或终端显示。完整配置说明见 [configuration.md](configuration.md)。

## 交互式管理控制台

服务器在普通交互式终端和 tmux 中启动 ratatui 管理控制台。GNU Screen、Linux console、`ansi`/`cons25` 等兼容性较差的终端会进入保守 TUI：不启用备用屏幕、鼠标捕获或 Bracketed Paste，并修正 Ctrl+H Backspace；如果 TUI 初始化失败，会自动回落到逐行兼容控制台。重定向、systemd 和其他非 TTY 环境始终使用逐行控制台。设置 `NO_COLOR` 可关闭颜色。

TUI 快捷键：`Tab` 补全、`Ctrl+A/E` 跳到行首/行尾、`Ctrl+B/F` 左右移动、`Alt+←/→` 按词移动、`Ctrl+W` 删除前一个词、`Alt+Delete` 删除后一个词、`Ctrl+K` 删除到行尾、`Ctrl+L` 清屏、`PgUp/PgDn` 或 `Shift+↑/↓` 滚动日志。

### 命令列表

#### 通用命令

| 命令 | 说明 |
|------|------|
| `help` | 显示帮助信息 |
| `exit` | 关闭服务器 |
| `status` | 显示服务器状态 |

#### 诊断 / 压测

| 命令 | 说明 |
|------|------|
| `benchmark [秒=30] [房间=100]` | 运行显式真实 TCP 网络压测（需 Phira token，advanced 级别） |
| `benchmark modes` | 查看三种压测模式说明 |
| `benchmark run real [秒] [房间]` | 显式真实 TCP 兼容性测试 |
| `benchmark report` | 查看 Benchmark 报告 |
| `benchmark history` | 查看已持久化的 BenchmarkReport 历史记录 |

这些是 advanced 级别命令。默认推荐使用 `simulation` 进行压力测试（隔离本地，不访问 Phira，不需要 token）。Real Benchmark 是高级兼容性测试，详见 docs/benchmark-real.md。


### 游戏内 `_` 管理入口

`admin_phira_ids` 中的管理员可以在客户端”创建房间”弹窗输入 `_<CLI命令>`。服务端不会创建房间，而是执行 CLI 命令，并把输出通过聊天消息发回该客户端。非管理员输入 `_...` 会正常创建同名房间。


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
| `broadcast <作用域> <消息>` | 广播消息；作用域必须显式写明 |

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
| `room info <房间ID>` | 房间详情（状态、房主、谱面、Phira API、无人保留、历史） |
| `room create-empty <房间ID> [phira_api_endpoint]` | 创建无人持久空房间；首个普通玩家加入后静默成为房主，不再弹出 `? 成为房主` |
| `room start <房间ID>` | 由服务端发起游戏；等待所有玩家和监控端完成谱面加载后开始 |
| `room cancel <房间ID>` | 取消准备状态 |
| `room kick <房间ID> <用户ID>` | 从房间踢出用户 |
| `room host <房间ID> <用户ID|?>` | 设置房主；`?` 表示系统房主 |
| `room force-move <房间ID> <用户ID> [monitor]` | 强制迁移在线用户到指定房间 |
| `room hide <房间ID>` | 隐藏房间 |
| `room unhide <房间ID>` | 取消隐藏房间 |
| `room set <房间ID> <字段> <值>` | 修改房间设置（lock/cycle/hidden/persistent/host/chart-id/phira_api_endpoint） |
| `room close <房间ID>` | 解散房间 |
| `room history <房间ID>` | 查看游玩记录 |
| `room ban <房间ID> <用户ID>` | 房间加入黑名单 |
| `room unban <房间ID> <用户ID>` | 房间移出黑名单 |
| `room banlist <房间ID>` | 房间黑名单列表 |

> 服务端选谱后会同步完整房间状态。`room start` 不再跳过客户端的下载与准备阶段，避免客户端在本地尚无谱面时直接进入游玩。

##### 房间独立 Phira API endpoint

每个房间可以临时覆盖全局 `phira_api_endpoint`，设置后立即生效，不需要重启服务器，也不需要重建房间。

```bash
room set <房间ID> phira_api_endpoint https://phira.example.com
room set <房间ID> phira_api_endpoint default   # 清除覆盖，恢复全局配置
room info <房间ID>                             # 查看当前生效 endpoint
```

覆盖后，服务端能确定房间上下文的 Phira API 访问都会使用房间 endpoint，例如房间命令查谱、服务端记录校验、终端/欢迎语/Web API 展示中的谱面名和用户名刷新。MP 协议无法可靠改写客户端本机对 Phira 的请求行为，因此不要把这个功能当成客户端 API 代理。用户登录认证 `/me` 仍使用全局 `server_config.yml` 的 `phira_api_endpoint`。

无人持久房间：

```bash
room create-empty <房间ID>
room create-empty <房间ID> https://phira.example.com
room set <房间ID> persistent true
room set <房间ID> persistent false
```

`persistent=true` 时房间即使没有玩家也不会因无人自动清除；没有房主的空房间会在首个普通玩家加入时静默把该玩家设为房主。用 `room host <房间ID> ?` 或 `room set <房间ID> host ?` 可显式设置系统 `?` 房主，此状态不会被后续加入者自动接管。

旧式顶层房间命令已经移除；测试阶段统一使用 `room <subcommand>` 命名空间，避免命令面继续膨胀。

#### 黑名单管理

| 命令 | 说明 |
|------|------|
| `ban <用户ID> [原因]` | 封禁用户 |
| `unban <用户ID>` | 解封用户 |
| `banlist` | 列出封禁列表 |


## Web API

中央 HTTP/SSE 服务器监听配置的 `--http-port`（默认 12347）。

| 端点 | 说明 |
|------|------|
| `GET /api/rooms` | 房间列表（含详情） |
| `GET /api/rooms/{name}` | 指定房间信息 |
| `GET /api/user_name/{id}` | 用户名称查询 |
| `GET /api/players/count` | 在线玩家数 |
| `GET /api/events` | 统一 SSE 端点 |

详细 API 文档见 [api.md](api.md)。

## WASM 插件系统

服务器支持通过 wasmtime 加载 `.wasm` 插件。插件需放置在 `plugins/` 目录（可通过 `-d` 自定义）。
插件通过 `phira:host/api` 导入函数访问服务器全部能力：

- 状态查询：rooms.list, player.touches, round.data 等
- 消息发送：send.to_user, send.to_room, send.to_all
- 房间管理：room.kick, room.set_host, room.clear_host, room.set_lock, room.force_move, room.set_hidden, room.is_hidden, room.close
- 用户管理：admin.kick_user, admin.ban_user, admin.unban_user, admin.is_banned
- 插件互调用：plugin.api_call, plugin.api_register
- 数据读写：ext.get/set, config.get/set, file.read/write
- HTTP 请求：http.get/post

具体接口定义见 `wit/phira-plugin.wit`（canonical ABI）；旧版指针见 `wit/phira/mpplus.wit`。

## 日志文件

日志文件存储在 `log/` 目录下，按小时轮转。

日志级别通过 `RUST_LOG` 环境变量控制：

```bash
RUST_LOG=info phira-mp-plus-server
RUST_LOG=debug phira-mp-plus-server
```

## Command surface policy

The Runtime v2 command registry separates command metadata into three surfaces:

- `primary`: shown by default in `help` and intended for normal operation.
- `advanced`: available for diagnostics and migration work, hidden from default
  help to avoid command bloat.

Useful help entry points:

```text
help                 # recommended primary commands
help all             # full registry, including advanced commands
help groups          # list command groups
help group rooms     # recommended commands in one group
help group rooms all # full group view
```

New commands should prefer existing namespaces and configuration-driven behavior
instead of adding more top-level commands. Compatibility spellings are not kept
because this project is still a test branch. Web management APIs remain out of
scope; CLI/TUI/in-game admin commands are still the management surfaces.
