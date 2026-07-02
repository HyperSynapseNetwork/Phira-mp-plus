# Phira-mp+ API 文档

## HTTP API（端口 12347）

所有 HTTP API 通过 `plugin_http` 提供，支持 JSON 请求/响应。

### 房间信息

#### `GET /api/rooms`
返回所有活跃房间列表及服务器统计。

**响应：** `200 OK` JSON
```json
{
  "rooms": [
    {
      "name": "room1",
      "host": 16,
      "users": [16, 17],
      "lock": false,
      "cycle": false,
      "chart": 12345,
      "state": "SELECTING_CHART",
      "rounds": []
    }
  ],
  "player_count": 5,
  "total_players": 42
}
```

| 字段 | 说明 |
|------|------|
| `rooms` | 活跃房间列表 |
| `player_count` | 当前在线玩家数 |
| `total_players` | 连接过服务器的总玩家数 |

#### `GET /api/rooms/<name>`
返回指定房间详情。

**参数：** `<name>` — 房间名（URL 路径）

**响应：** `200 OK` JSON
```json
{
  "host": 16,
  "users": [16, 17],
  "lock": false,
  "cycle": false,
  "chart": 12345,
  "state": "SELECTING_CHART",
  "rounds": []
}
```

#### `GET /api/user_name/<id>`
根据 Phira ID 获取用户名。

**参数：** `<id>` — Phira 用户 ID

**响应：** `200 OK`
```
用户名
```

#### `GET /api/players/count`
获取当前在线玩家数。

**响应：** `200 OK`
```json
{"count": 5}
```

#### `GET /api/players/all`
获取所有连接过服务器的玩家 ID 列表。

**响应：** `200 OK`
```json
{
  "total": 42,
  "players": [16, 17, 18, 19]
}
```

### SSE 事件

#### `GET /api/events`

统一事件流。连接建立后立即发送 `ready`，房间生命周期事件也会进入该流。

旧 web-monitor 的 `/rooms/listen` 与 `/ws/live` 端点已在 Runtime v2 测试分支移除。
请使用 `/api/events` 和插件注册的明确 HTTP 路由。

---

## CLI 命令

在 TUI 或 stdin CLI 中输入以下命令：

### 系统命令

| 命令 | 说明 |
|------|------|
| `help` | 显示所有可用命令 |
| `exit` | 关闭服务器 |
| `benchmark <seconds> [rooms]` | 真实网络压力测试（需 Phira token，advanced 级别） |
| `benchmark modes` | 查看三种压测模式说明 |
| `benchmark run real <seconds> [rooms]` | 显式真实 TCP 兼容性测试 |
| `admin-id list/add/remove/set` | 查看或修改游戏内管理员 Phira ID |

### 房间管理

| 命令 | 说明 |
|------|------|
| `rooms` / `r` | 列出所有房间 |
| `room info <id>` / `room i <id>` | 查看房间详情 |
| `room kick <id> <user_id>` | 踢出用户 |
| `room close <id>` | 关闭房间 |
| `room host <id> <user_id|?>` / `room transfer <id> <user_id|?>` | 设置房主；`?` 表示系统房主 |
| `room force-move <id> <user_id> [monitor]` | 强制迁移用户到房间 |
| `room hide <id>` / `room unhide <id>` | 设置房间隐藏状态 |
| `room set <id> <字段> <值>` | 修改房间设置（lock/cycle/hidden 等） |
| `room history <id>` | 房间游玩历史（替代废弃的 round-last） |
| `room uuid <id>` | 房间 UUID |
| `room ban <id> <user_id>` | 拉黑用户 |
| `room unban <id> <user_id>` | 取消拉黑 |
| `room banlist <id>` | 房间黑名单 |

### 玩家管理

| 命令 | 说明 |
|------|------|
| `users` / `u` | 列出所有在线玩家 |
| `user-rooms <user_id>` / `rh <user_id>` | 玩家的房间访问历史 |
| `ban <user_id> [reason]` | 封禁用户 |
| `unban <user_id>` | 解封用户 |
| `kick <user_id>` | 踢出用户 |
| `pardon <user_id>` | 解封 |


### 占位符（欢迎语模板）

| 占位符 | 说明 |
|--------|------|
| `[user_name]` | 用户名 |
| `[user_id]` | Phira ID |
| `[player-count]` | 当前在线数 |
| `[players]` | 同上 |
| `[time]` | Unix 时间戳 |
| `[playtime]` | 该用户游玩时间 |
| `[playtime <id>]` | 指定用户游玩时间 |
| `[top_playtime]` | 游玩时间排行榜（前10） |
| `[active_rooms]` | 活跃房间详情 |

---

## TCP 二进制协议（端口 12346）

与 Phira 游戏客户端通信的二进制协议。使用自定义二进制格式（`BinaryData` derive），详见 `phira-mp-common`。

### 连接流程

1. 客户端发送版本字节（1）
2. 客户端发送 `Authenticate { token: Varchar<32> }`
3. 服务端回复 `Authenticate(Ok((UserInfo, Option<ClientRoomState>)))`
4. 心跳：客户端每 3 秒发送 `Ping`，服务端回复 `Pong`

### 客户端命令

| 命令 | 说明 |
|------|------|
| `Ping` | 心跳 |
| `Authenticate { token }` | 认证 |
| `Chat { message }` | 发送聊天消息 |
| `Touches { frames }` | 触控数据 |
| `Judges { judges }` | 判定数据 |
| `CreateRoom { id }` | 创建房间 |
| `JoinRoom { id, monitor }` | 加入房间 |
| `LeaveRoom` | 离开房间 |
| `LockRoom { lock }` | 锁定/解锁房间 |
| `CycleRoom { cycle }` | 循环模式 |
| `SelectChart { id }` | 选谱 |
| `RequestStart` | 请求开始 |
| `Ready` | 准备 |
| `CancelReady` | 取消准备 |
| `Played { id }` | 游玩完成 |
| `Abort` | 中止 |
| `QueryRoomInfo` | 查询房间列表 |

### 服务端命令

| 命令 | 说明 |
|------|------|
| `Pong` | 心跳回复 |
| `Authenticate(...)` | 认证结果 |
| `Chat(...)` | 聊天结果 |
| `Touches { player, frames }` | 玩家触控 |
| `Judges { player, judges }` | 玩家判定 |
| `Message(...)` | 房间消息 |
| `ChangeState(...)` | 房间状态变更 |
| `ChangeHost(...)` | 房主变更 |
| `RoomResponse(...)` | 房间列表（QueryRoomInfo 回复） |
| `RoomEvent(...)` | 房间事件（room monitor 用） |
| `UserVisit(...)` | 用户访问通知（room monitor 用） |

### 房间 Monitor 协议

room monitor 通过 `RoomMonitorAuthenticate { key }` 连接，连接后可接收：
- `RoomEvent`：房间创建/更新/加入/离开/新轮次
- `UserVisit`：用户访问通知
- 通过 `QueryRoomInfo` 获取房间列表

---

## 配置文件 (`server_config.yml`)

完整配置说明见 [configuration.md](configuration.md)。这里仅列出与 HTTP/API、压测和房间公开状态最相关的项目：

```yaml
# TCP 游戏协议端口
port: 12346

# HTTP API / SSE / WebSocket 端口
http_port: 12347

# 允许旁观的 Phira 用户 ID
monitors:
  - 2

# Phira API 地址。认证、查谱面、查成绩会使用它。
phira_api_endpoint: "https://phira.5wyxi.com"

# Real Benchmark 是高级兼容性测试，默认不推荐；详见 docs/benchmark-real.md。

# WASM 插件目录与扩展数据文件
plugins_dir: plugins
extensions_file: data/extensions.json

# 交互控制台和聊天开关
cli_enabled: true
chat_enabled: true

# 限速与数据保留
connection_rate_limit: 30
connection_rate_window: 10
round_data_retention_days: 7

# 可选容量限制
# max_rooms: 100
# max_users_per_room: 8

# 可选数据库；未配置时使用 JSON/文件存储
# database_url: "postgres://user:pass@localhost:5432/phira_mp_plus"
```

### 压测 token

`benchmark` 是高级兼容性测试，需要显式配置 Phira token。默认压测推荐使用 Simulation（不访问 Phira，不需要 token）。

当 token 数量少于目标房间数时，benchmark 会重复使用已有 token：离开/断开上一个 `bench-*` 客户端，再重连创建下一个房间，用于反复覆盖创建/重建逻辑。



### 统一持久化读取 API

启用 PostgreSQL 后，WIT/host API 可读取统一持久化数据：

| 方法 | 参数 | 说明 |
|---|---|---|
| `persist.events` | `since_sequence`, `limit`, `kind?`, `room_id?`, `user_id?` | 增量读取事件流。 |
| `persist.rooms` | `since_sequence`, `limit` | 增量读取房间快照。 |
| `persist.playtime` | `user_id` | 读取指定用户游玩时间。 |
| `persist.top_playtime` | `limit` | 读取游玩时间排行。 |
| `persist.touches` | `since_sequence?`, `limit?`, `round_uuid?`, `player_id?` | 从 PG Touches 批次表增量读取触控数据。 |
| `persist.judges` | `since_sequence?`, `limit?`, `round_uuid?`, `player_id?` | 从 PG Judges 批次表增量读取判定数据。 |
| `admin.ids` | — | 读取管理员 Phira ID。 |
| `admin.is_admin` | `user_id` | 判断某用户是否管理员。 |
| `admin.add_id` / `admin.remove_id` / `admin.set_ids` | `user_id` 或 `ids` | 修改管理员 Phira ID。 |

WIT 文件还提供了 `persistence` 与 `admin-config` import interface，便于组件插件按强类型方式调用。

### 房间独立 Phira API endpoint

房间可通过 CLI 或 WASM/host API 覆盖全局 `phira_api_endpoint`：

- CLI：`room set <room_id> phira_api_endpoint <url>`
- 创建无人持久房间：`room create-empty <room_id> [endpoint]`
- 清除覆盖：`room set <room_id> phira_api_endpoint default`
- Host API：`room.set_phira_api_endpoint`，参数 `{"room_id":"...","endpoint":"https://..."}`
- Host API 创建无人持久房间：`room.create_empty`，参数 `{"room_id":"...","endpoint":"https://..."}`
- Host API 设置无人保留：`room.set_persistent_empty`，参数 `{"room_id":"...","persistent":true}`
- Host API 清除：`room.set_phira_api_endpoint` 传 `endpoint: null`，或调用 `room.clear_phira_api_endpoint`
- 查询：`room.get_phira_api_endpoint`

`rooms.list`、`rooms.by_name`、`rooms.by_user` 和 `room.info` 会返回完整房间信息，包括 `uuid`、`created_at`、`live`、`locked`、`cycling`、`hidden`、`persistent_empty`、`max_users`、房主/玩家/旁观者详情、当前谱面、当前状态详情、当前轮次、历史轮次、当前生效的 `phira_api_endpoint` 与可选的 `phira_api_endpoint_override`。设置后立即生效：能确定所属房间的服务端请求、终端展示、欢迎语 `[active_rooms]`、Web API 中的谱面名与用户名刷新都会使用房间 endpoint；认证 `/me` 仍使用全局配置。MP 服务端不会尝试改写客户端本机 Phira API 请求。

### 隐藏房间

隐藏房间不会出现在 `GET /api/rooms`、`GET /api/rooms/<name>` 和欢迎语 `[active_rooms]` 中。房间名以 `-` 开头时默认隐藏，也可以通过 `room hide/unhide`、`room set <id> hidden true|false`、WASM/host API `room.set_hidden` 手动管理。隐藏只影响公开展示，不等于权限隔离。

---

## 内部路由 (`server_state_query`)

这些是通过 `PlusServerState` 内部调用的服务端查询方法，供 Web API 和 CLI 使用。

| 方法 | 说明 |
|------|------|
| `player.touches` | 查询玩家触控数据 |
| `player.judges` | 查询玩家判定数据 |
| `round.data` | 轮次数据 |
| `round.list` | 轮次列表 |
| `room.uuid` | 房间 UUID |
| `room.history` | 房间历史 |
| `room.round_info` | 轮次详情 |
| `room.list_since` | 指定时间后的房间 |
| `room.kick` | 踢出玩家 |
| `room.set_host` / `room.clear_host` | 设置房主；`target_id:null`/`?` 表示系统 `?` 房主 |
| `room.set_lock` | 设置锁定 |
| `room.force_move` | 强制迁移用户到房间 |
| `room.set_hidden` / `room.is_hidden` | 设置/查询隐藏状态 |
| `room.create_empty` / `room.set_persistent_empty` | 创建无人持久空房间；设置最后一名玩家离开后是否保留。首个普通玩家加入空房间时静默成为房主，不发送 `NewHost` 提示 |
| `room.set_phira_api_endpoint` / `room.get_phira_api_endpoint` / `room.clear_phira_api_endpoint` | 设置/查询/清除房间独立 Phira API endpoint |
| `room.close` | 关闭房间 |
| `admin.kick_user` | 管理员踢人 |
| `admin.ban_user` | 封禁 |
| `admin.unban_user` | 解封 |
| `admin.is_banned` | 查询封禁状态 |
| `admin.ban_list` | 封禁列表 |
| `admin.list_users` | 用户列表 |
| `user.room_history` | 用户房间历史 |
| `rooms.list` | 房间列表 |
| `rooms.by_name` | 按名称查找房间 |
| `rooms.by_user` | 按用户查找房间 |
| `user_name` | 用户名查询 |
| `test.run_benchmark` | 真实网络压测；CLI `benchmark` 通过核心命令后台调用，不再注册为 WASM 插件命令 |
| `test.bind_phira_tokens` | 绑定压测账号 token |
| `test.cleanup` | 清理测试数据 |
