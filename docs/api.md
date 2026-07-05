# Phira-mp+ API 文档

## HTTP API（端口 12347）

HTTP API 由 `plugin_http` 提供。支持 JSON 响应。所有带 `<param>` 参数的路由不区分 HTTP 方法（GET/POST 均可）。

### 系统

#### `GET /api/events`

SSE（Server-Sent Events）实时事件流。连接后推送 `ready` 事件，随后转发所有广播事件。心跳每 15 秒。

#### `/api/players/count`

连接过服务器的玩家总数。

**响应：**
```json
{ "count": 42 }
```

#### `/api/players/all`

所有连接过服务器的玩家 ID 列表。

**响应：**
```json
{
  "total": 42,
  "players": [1, 2, 3, 16]
}
```

#### `/api/user_name/<id>`

根据用户 ID 查询用户名。

**参数：** `<id>` — 用户数字 ID

**响应：**
```json
{ "name": "PlayerName" }
```

#### `/api/runtime`

Runtime v2 运行时诊断状态。包含模拟器、持久化、事件总线、命令注册表、房间命令通道和 Phira HTTP 客户端状态。

---

### 房间

#### `/api/rooms`

返回所有活跃房间列表及服务器统计。

**响应：**
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

#### `/api/rooms/<name>`

返回指定房间详情。

**参数：** `<name>` — 房间名（URL 路径）

**响应：**
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

---

### 模拟器

#### `/api/simulation`

返回模拟器当前状态（是否运行、配置、运行 ID 等）。

#### `/api/simulation/world`

返回模拟器影子世界数据（虚拟用户、房间、轮次）。默认限制 20 条。

---

### 基准测试

#### `/api/benchmark/reports`

返回最近的一组 Benchmark 报告快照。

#### `/api/benchmark/reports/history`

返回已持久化的 Benchmark 报告历史记录。可选参数：`<mode>`（过滤模式）、`<limit>`（条数限制）。

#### `/api/benchmark/reports/history/<mode>`

按模式筛选的历史基准报告。

**参数：** `<mode>` — `simulation` / `hybrid` / `real`

---

### 插件路由

WASM 插件可在运行时通过 `http_handle.register_route()` 动态注册更多路由。路由最终合并到全局动态路由器中。
