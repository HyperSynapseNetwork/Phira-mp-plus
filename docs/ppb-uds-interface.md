# PMP OpenUDS API

> 版本: 0.1 | Linux only | 不支持 Windows

## 概述

PMP 通过 Unix Domain Socket 暴露全部管理能力给外部工具（PPB、Web 后端、运维脚本等）。

**设计原则：**
- PMP 不依赖任何消费者——没有连接时 PMP 正常运行
- 接口能力 = CLI 能做的事 + 事件订阅 + 数据流
- 不支持 Windows（UDS 是 Linux 特性）
- 消费者无关——接口不绑定 外部工具，任何外部工具都可以接入

## 架构

```
┌─────────────────────────────────────────┐
│              PMP 现有模块                  │
│  room_commands / ban_manager / CLI / ... │
└──────────┬──────────────────────┬────────┘
           │ dispatch             │ events
┌──────────▼──────────────────────▼────────┐
│              OpenUDS API 模块                 │
│  ┌─────────┐  ┌──────────┐  ┌─────────┐  │
│  │dispatch  │  │  events  │  │streams  │  │
│  │ 命令路由 │  │ 事件推送  │  │高频数据流│  │
│  └────┬────┘  └────┬─────┘  └────┬────┘  │
│       │            │             │       │
│  ┌────▼────────────▼─────────────▼────┐  │
│  │           Session                  │  │
│  │  认证 / 帧编码 / 流控 / 订阅       │  │
│  └────────────────┬──────────────────┘  │
└───────────────────┼─────────────────────┘
                    │ UDS
┌───────────────────▼─────────────────────┐
│              客户端                   │
└─────────────────────────────────────────┘
```

## 传输协议

- **传输**: Unix Domain Socket (`tokio::net::UnixStream`)
- **帧格式**: 长度前缀 (u32 LE) + JSON (UTF-8)

```
┌───────────────┬──────────────────────────────┐
│  payload_len  │        payload (JSON)          │
│   (4 bytes)   │     (payload_len bytes)        │
└───────────────┴──────────────────────────────┘
```

- 单帧上限: 16 MiB
- 控制流和高频数据流建议使用不同连接

## 认证

### Token 模式（自动）

```
外部工具                          PMP
  │                           │
  ├─ Authenticate ───────────►│ { token: "xxx" }
  │◄─ Authenticated ─────────┤ { session_id, server_version }
```

### CLI 审批模式（手动）

```
外部工具                          PMP
  │                           │
  ├─ Authenticate ───────────►│ { client_name: "我的工具" }
  │◄─ AuthPending ───────────┤ { pending_id: "abc" }
  │         管理员: _approve openuds abc
  │◄─ Authenticated ─────────┤ { session_id, server_version }
```

## 命令（外部工具 → PMP）

### 房间管理

| 命令 | 参数 | 说明 |
|------|------|------|
| `room.create` | `{room_id, endpoint?, persistent_empty?}` | 创建空房间 |
| `room.close` | `{room_id}` | 解散房间 |
| `room.start` | `{room_id}` | 强制开始 |
| `room.cancel_start` | `{room_id}` | 取消开始 |
| `room.ready` | `{room_id, user_id}` | 强制准备 |
| `room.lock` | `{room_id, locked}` | 锁定/解锁 |
| `room.cycle` | `{room_id, cycle}` | 轮换开关 |
| `room.set_host` | `{room_id, host_id}` | 设置房主 |
| `room.kick` | `{room_id, user_id, reason?}` | 踢人 |
| `room.force_move` | `{room_id, user_id, monitor?}` | 强制移入 |
| `room.info` | `{room_id}` | 房间详情 |
| `room.list` | `{filters?}` | 房间列表 |

### 玩家管理

| 命令 | 参数 | 说明 |
|------|------|------|
| `player.ban` | `{user_id, reason?}` | 封禁用户 |
| `player.unban` | `{user_id}` | 解封 |
| `player.banlist` | — | 封禁列表 |
| `player.ban_ip` | `{target, reason?}` | 封禁 IP（ID 或 IP）|
| `player.unban_ip` | `{ip}` | 解封 IP |
| `player.ip_history` | `{user_id}` | IP 历史 |
| `player.info` | `{user_id}` | 用户信息 |
| `player.kick` | `{user_id}` | 踢出 |

### 服务器管理

| 命令 | 参数 | 说明 |
|------|------|------|
| `server.stats` | — | 运行时统计 |
| `server.status` | — | 服务器状态 |
| `server.config_reload` | — | 重载配置 |
| `server.shutdown` | — | 关闭服务器 |
| `server.roomcreation` | `{enabled}` | 建房开关 |

### 广播

| 命令 | 参数 | 说明 |
|------|------|------|
| `broadcast.all` | `{message}` | 全服广播 |
| `broadcast.room` | `{room_id, message}` | 房间广播 |
| `broadcast.user` | `{user_id, message}` | 私信 |

### 插件

| 命令 | 参数 | 说明 |
|------|------|------|
| `plugin.list` | — | 插件列表 |
| `plugin.enable` | `{name}` | 启用插件 |
| `plugin.disable` | `{name}` | 禁用插件 |
| `plugin.reload` | — | 重载全部 |
| `plugin.info` | `{name}` | 插件详情 |
| `plugin.remove` | `{name}` | 删除插件 |
| `plugin.call` | `{name, method, args}` | 调用插件 API |

### 运行时诊断

| 命令 | 参数 | 说明 |
|------|------|------|
| `runtime.status` | — | 运行时诊断 |
| `runtime.actors` | — | Actor 状态 |
| `runtime.persistence` | — | 持久化统计 |
| `runtime.phira` | — | Phira 客户端统计 |

### 事件订阅

| 命令 | 参数 | 说明 |
|------|------|------|
| `subscribe` | `{event_types: ["room.*"]}` | 订阅事件 |
| `unsubscribe` | `{event_types}` | 取消订阅 |
| `subscribe_stream` | `{stream: "touches", room_id?}` | 订阅数据流 |

所有命令的标准响应格式：

```json
{
    "type": "response",
    "id": "req-uuid",
    "ok": true,
    "data": { ... }
}

{
    "type": "response",
    "id": "req-uuid",
    "ok": false,
    "error": { "code": "ROOM_NOT_FOUND", "message": "..." }
}
```

## 事件（PMP → 外部工具）

| 事件 | 数据 | 说明 |
|------|------|------|
| `room.created` | `{room_id, uuid, data}` | 房间创建 |
| `room.joined` | `{room_id, user_id, monitor}` | 玩家加入 |
| `room.left` | `{room_id, user_id, reason}` | 玩家离开 |
| `room.updated` | `{room_id, changes}` | 配置变化 |
| `round.scored` | `{room_id, user_id, score, chart_id, ...}` | 玩家完成 |
| `round.completed` | `{room_id, round_id, results}` | 轮次结束 |
| `user.online` | `{user_id, name}` | 玩家上线 |
| `user.offline` | `{user_id}` | 玩家离线 |
| `server.heartbeat` | `{users, rooms, sessions, ...}` | 定期统计 |
| `stream.touches` | `{user_id, frames}` | 触控数据流 |
| `stream.judges` | `{user_id, events}` | 判定数据流 |

## 订阅模型

外部工具 可以精确控制想收的事件类型，避免不必要的数据传输：

```json
// 外部工具 订阅房间和轮次事件
{ "type": "subscribe", "event_types": ["room.*", "round.*"] }
// PMP 确认
{ "type": "subscribed", "active": ["room.*", "round.*"] }
```

通配符: `room.*` = 全部房间事件, `round.scored` = 仅单类型

## 实现

PMP 侧新增 `src/openuds/` 模块：

```
src/openuds/
├── mod.rs       # 模块入口
├── server.rs    # UDS listener + 连接管理
├── session.rs   # 每个客户端连接的会话
├── protocol.rs  # 帧编码/解码
├── auth.rs      # 认证（token / CLI 审批）
├── dispatch.rs  # 命令路由（复用 PMP 现有 handler）
├── events.rs    # 事件订阅 + 推送
└── streams.rs   # 高频数据流（touches/judges）
```

## 配置

```yaml
openuds:
  enabled: true
  socket_path: "/var/run/pmp-openuds.sock"
  auth_token: ""
  max_connections: 4
  event_buffer_size: 1024
  heartbeat_interval_secs: 60
```
