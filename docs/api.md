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

#### `GET /rooms/listen`

房间事件流。连接建立时的发送顺序如下：

1. `ready`：表示流已建立；
2. 每个现有房间对应一条 `update_room` 快照；
3. 持续发送后续房间事件。

事件数据与 `phira-web-monitor` 的房间事件结构一致：

- `create_room`：`{ "room": string, "data": RoomData }`
- `update_room`：`{ "room": string, "data": PartialRoomData | RoomData }`
- `join_room`：`{ "room": string, "user": number }`
- `leave_room`：`{ "room": string, "user": number }`
- `new_round`：`{ "room": string, "round": RoundData }`
- `stream_lagged`：消费者落后于广播缓冲区，`skipped` 表示丢弃的事件数

验证命令：

```bash
curl -N http://127.0.0.1:12347/rooms/listen
```

即使当前没有房间，也应立即看到 `event: ready`，而不是只有 keep-alive 注释。响应包含 `Cache-Control: no-cache` 与 `X-Accel-Buffering: no`，用于避免常见反向代理缓冲事件流。

### WebSocket

#### `GET /ws/live`
实时游戏数据 WebSocket（web-monitor 兼容）。

---

## CLI 命令

在 TUI 或 stdin CLI 中输入以下命令：

### 系统命令

| 命令 | 说明 |
|------|------|
| `help` | 显示所有可用命令 |
| `exit` / `quit` | 关闭服务器 |
| `benchmark [dur_s=30] [rooms=100]` | 压力测试 |

### 房间管理

| 命令 | 说明 |
|------|------|
| `rooms` / `r` | 列出所有房间 |
| `room info <id>` / `room i <id>` | 查看房间详情 |
| `room kick <id> <user_id>` | 踢出用户 |
| `room close <id>` | 关闭房间 |
| `room transfer <id> <user_id>` | 转移房主 |
| `room set <id> <字段> <值>` | 修改房间设置（lock/cycle 等） |
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

### 游玩统计

| 命令 | 说明 |
|------|------|
| `playtime <user_id>` | 查询用户游玩时间 |
| `player-count` | 游玩过的玩家总数 |
| `room history <room_id>` | 查看房间游玩记录（替代废弃的 round-last） |

### 欢迎语

| 命令 | 说明 |
|------|------|
| `welcome-config` | 查看欢迎语配置与占位符 |

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

```yaml
# 网络
port: 12346                    # TCP 监听端口
http_port: 12347               # HTTP/SSE 端口

# 认证
monitors: [2]                  # 允许旁观的 Phira 用户 ID
phira_api_endpoint: "https://phira.5wyxi.com"

# 插件
plugins_dir: plugins

# 功能
chat_enabled: true
cli_enabled: true

# 限制
connection_rate_limit: 30      # 连接速率限制
connection_rate_window: 10     # 统计窗口（秒）
round_data_retention_days: 7   # 轮次数据保留天数

# 数据库
# database_url: "postgres://user:pass@localhost:5432/phira_mp_plus"
```

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
| `room.transfer_host` | 转移房主 |
| `room.set_lock` | 设置锁定 |
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
| `test.run_benchmark` | 压测 |
| `test.cleanup` | 清理测试数据 |
