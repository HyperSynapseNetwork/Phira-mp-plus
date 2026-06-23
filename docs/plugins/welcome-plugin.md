# welcome-plugin 插件

用户连接服务器时发送配置的欢迎语消息。支持占位符与其他插件联动。

## 配置文件

路径 `data/welcome-config.json`，自动创建。

```json
{
  "welcome_messages": [
    "欢迎 [user_name] 来到 Phira-mp+！当前在线 [player-count] 人",
    "[user_name] 来了！在线 [players] 人"
  ],
  "show_time": true,
  "time_format": "%Y-%m-%d %H:%M"
}
```

修改后重启服务器生效。

## 占位符

| 占位符 | 说明 | 数据来源 |
|--------|------|---------|
| `[user_id]` | 连接用户的 Phira ID | 事件 |
| `[user_name]` | 连接用户的用户名 | 事件 |
| `[user_ip]` | 连接用户的 IP 地址 | 事件 |
| `[player-count]` | 当前在线玩家数 | player-tracker API |
| `[players]` | 同 `[player-count]` | player-tracker API |
| `[playtime]` | 该用户的游玩时间 | playtime-tracker API |
| `[playtime <id>]` | 指定用户的游玩时间 | playtime-tracker API |
| `[top_playtime]` | 游玩时间前 10 名排行 | playtime-tracker API |
| `[active_rooms]` | 活跃房间列表及详情 | 服务端状态查询 |

## CLI 命令

### `welcome-config`

查看当前欢迎语配置和可用占位符。

```
welcome-config
  ◆ 欢迎语配置
  │ 配置文件: data/welcome-config.json
  │ 消息数量: 2
  │ [0] 欢迎 [user_name] 来到 Phira-mp+！当前在线 [player-count] 人
  │ [1] [user_name] 来了！在线 [players] 人
  │ 显示时间: true
  │ 时间格式: %Y-%m-%d %H:%M

  ■ 可用占位符:
  │ [user_id]           用户 Phira ID
  │ [user_name]         用户名
  │ [user_ip]           用户 IP 地址
  │ [player-count]      当前在线玩家数
  │ [players]           当前在线玩家数
  │ [playtime]          该用户的游玩时间
  │ [top_playtime]      游玩时间前 10 名排行
  │ [active_rooms]      活跃房间列表及详情
```

## 消息发送

用户连接后约 1 秒，欢迎语通过聊天消息发送给用户。

## 事件

- `UserConnect` — 发送配置中的欢迎语消息列表（按顺序全部发送）

## 配置

构建时通过 `--features welcome-plugin` 启用（默认开启）。
