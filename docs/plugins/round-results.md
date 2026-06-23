# round-results 插件

每轮游戏完成后输出成绩排行、ACC 排行、最高分、最高 ACC 等结算信息。

## 数据存储

自有内存存储，记录每轮结算结果。

## CLI 命令

### `round-last <room_id>`

查看房间最近一轮结算详情。

```
round-last 111
  结算: 看我超级搭路 (id=123)
  ■ 得分排行
     #1 FireflyF09  998000  ACC:99.80%
     #2 Player2     895000  ACC:89.50%
  ■ ACC 排行
     #1 FireflyF09  ACC:99.80% FC
     #2 Player2     ACC:89.50%
  ■ 最高分: FireflyF09 (998000)  ACC:99.80%
  ■ 最高ACC: FireflyF09 (99.80%)
```

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
