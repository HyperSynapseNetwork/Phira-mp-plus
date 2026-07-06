# round-results 插件

每轮游戏完成后输出成绩排行、ACC 排行、最高分、最高 ACC 等结算信息。

## 数据存储

自有内存存储，记录每轮结算结果。

## CLI 状态

当前主线不再注册该插件的旧顶层 CLI 命令。房间轮次信息请使用 `room rounds <room_id>`、`room round <round_uuid>` 或 Web API 查询。

## Web API

### `GET /api/round/last/{room_id}`

```json
{
  "room_id": "111",
  "chart_id": 123,
  "chart_name": "谱面名",
  "scores": [
    { "user_id": 16, "user_name": "FireflyF09", "score": 998000, "accuracy": 0.998, ... }
  ]
}
```

## 事件

- `GameEnd` — 收集每位玩家成绩
- `RoundComplete` — 所有玩家完赛，计算并发送结算消息

## 消息发送

结算结果自动通过聊天消息发送到房间（前缀 `[结算]`）。

## 配置

构建时通过 `--features round-results` 启用（默认开启）。
