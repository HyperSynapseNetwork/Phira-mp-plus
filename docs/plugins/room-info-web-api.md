# room-info-web-api 插件

提供 REST API 和 SSE 接口查询房间信息、游玩记录等。

## 端点

### `GET /api/rooms/info`

获取所有房间列表。

```json
[
  {
    "name": "房间名",
    "data": {
      "host": 16,
      "users": [16],
      "lock": false,
      "cycle": false,
      "chart": 123,
      "chart_name": "谱面名",
      "state": "SELECTING_CHART | WAITING_FOR_READY | PLAYING",
      "playing_users": [],
      "rounds": [
        {
          "chart": 123,
          "records": [{ "player": 16, "score": 998000, "accuracy": 0.998, ... }]
        }
      ]
    }
  }
]
```

### `GET /api/rooms/info/{name}`

获取指定房间信息。路径参数为房间名称。

### `GET /api/rooms/user/{user_id}`

获取指定用户所在的房间信息。

### SSE 端点

#### `GET /api/rooms/listen`
#### `GET /api/events`

统一 SSE 事件流。事件类型：

| 事件 | 数据 | 说明 |
|------|------|------|
| `create_room` | `{room, data}` | 新房间 |
| `update_room` | `{room, data}` | 房间更新 |
| `join_room` | `{room, user}` | 用户加入 |
| `leave_room` | `{room, user}` | 用户离开 |
| `player_score` | `{room, record}` | 玩家完成 |
| `start_round` | `{room}` | 新一轮开始 |

## 配置

构建时通过 `--features webapi` 启用（默认开启）。
