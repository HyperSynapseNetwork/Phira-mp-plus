# player-tracker 插件

记录所有游玩过该服务器的玩家 Phira ID。

## 数据存储

自有内存存储（`Arc<Mutex<HashMap<i32, PlayerRecord>>>`），非持久化。

## CLI 状态

当前主线不再注册该插件的旧顶层 CLI 命令。请优先使用 Web API、插件 API 或持久化查询读取玩家统计。

## Web API

### `GET /api/players/count`

```json
{ "count": 2 }
```

### `GET /api/players/list?page=1`

```json
{
  "page": 1,
  "page_size": 20,
  "players": [
    { "phira_id": 16, "first_seen": "...", "last_seen": "...", "play_count": 5 }
  ]
}
```

## 插件 API

其他插件可通过 `ctx.api.call("player-tracker", ...)` 调用：

| 方法 | 参数 | 返回 |
|------|------|------|
| `count` | `[]` | `{"count": 42}` |
| `list` | `[]` | `{"players": [...]}` |

## 事件

监听 `UserConnect` 事件，用户连接时自动记录。

## 配置

构建时通过 `--features player-tracker` 启用（默认开启）。
