# playtime-tracker 插件

统计每个用户在服务器上的游玩时间，提供排行榜和排名查询。

## 数据存储

自有内存存储（`Arc<Mutex<HashMap<i32, PlayerTime>>>`）。

## CLI 命令

### `playtime <user_id>`

查询指定用户的游玩时间。

```
playtime 16
  ◆ 用户 16 游玩时间
  │ 总计: 713 秒 (0.20 小时)
```

### `playtime-top [数量]`

游玩时间排行榜前 N（默认 10）。

```
playtime-top 5
  ◆ 游玩时间排行榜 TOP 5
  ──────────────────────────
  │ #1   用户 16       0.20 小时
  │ #2   用户 42       0.05 小时
```

## Web API

### `GET /api/user_rank/{user_id}`

查询用户排名及总游玩时间（考虑并列情况）。

```json
{
  "success": true,
  "data": {
    "user_id": 16,
    "rank": 1,
    "total_playtime_seconds": 713,
    "total_playtime_hours": 0.2
  }
}
```

### `GET /api/user_playtime_ranking`

获取前 10 名排行榜。

```json
{
  "success": true,
  "data": [
    { "user_id": 16, "playtime_seconds": 713, "playtime_hours": 0.2 }
  ],
  "count": 1
}
```

### `GET /api/playtime_leaderboard`

获取所有用户的游玩时间排行榜。

```json
{
  "success": true,
  "data": [
    { "user_id": 16, "total_playtime": 713 }
  ],
  "timestamp": "...",
  "total_users": 1
}
```

### `GET /api/playtime_leaderboard/top/{limit}`

获取前 N 名排行榜。

```json
{
  "success": true,
  "data": [
    { "user_id": 16, "total_playtime": 713 }
  ],
  "timestamp": "...",
  "total_users": 1
}
```

## 插件 API

其他插件可通过 `ctx.api.call("playtime-tracker", ...)` 调用：

| 方法 | 参数 | 返回 |
|------|------|------|
| `user_playtime` | `[user_id]` | `{"user_id": 16, "total_seconds": 713}` |
| `leaderboard` | `[limit]` | `{"data": [{"user_id": 16, "total_playtime": 713}]}` |

## 事件

- `UserConnect` — 记录会话开始时间
- `UserDisconnect` — 累计游玩时间

## 配置

构建时通过 `--features playtime-tracker` 启用（默认开启）。
