# PPB ↔ PMP 内部接口契约

## 架构

```
PPB（公网边缘）──→ PMP（游戏运行时）
    │                      │
    │  内部 HTTP           │  TCP 12346（游戏协议）
    │  （端口 12347）      │  WS /api/ws
    │  SSE /api/events     │  SSE /newapi/rooms/listen
    │  REST /api/*         │
```

## 网络边界

- PMP 默认绑定 `127.0.0.1`（生产环境）
- PPB 须在同一主机或受信网络
- PPB 与 PMP 之间无 TLS（假定为内网）
- 所有公网 TLS 终结在 PPB

## 认证

- （TODO）PPB → PMP：通过 `admin_token` 配置或 `PM_ADMIN_TOKEN` 环境变量的服务令牌
- PMP → PPB：默认无出站调用

## 端点

### Stream

| 端点 | 方法 | 说明 |
|------|------|------|
| `/api/events` | GET | SSE 实时事件流 |
| `/api/ws` | GET | WebSocket 实时更新 |

### 插件路由

插件在初始化时通过 `http.register_route` 注册路由：

- `/api/auth/visited/count` — 访客计数
- `/api/rooms/info` — 房间列表
- `/api/rooms/info/:name` — 房间详情
- `/rankapi/playtime_leaderboard` — 游玩时长排行
- `/newapi/rooms/listen` — SSE 房间事件

## SSE 事件格式

```json
{
    "event_type": "RoomCreate | RoomJoin | RoomLeave | RoomModify | GameEnd | RoundComplete",
    "data": { }
}
```

插件通过 `on_api("sse:translate", ...)` 转换格式。

## 版本兼容

- PMP SemVer 在 `--version` 输出
- PPB 应通过 check-config 检查兼容性
- 协议变更会递增游戏协议版本号
