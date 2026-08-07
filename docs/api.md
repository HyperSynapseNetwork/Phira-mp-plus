# Phira-mp+ 对外 API

本文档汇总 PMP 对外暴露的接口：

- **HTTP / SSE / WebSocket** — 内部 HTTP/SSE/WS 端口（默认 12347）
- **插件 API** — WASM 插件的 WIT host API 参考（自动生成自 `wit/phira-plugin.wit`）
- **Capability 映射表** — 插件能力声明与 WIT 方法的映射
- **OpenUDS** — Unix Domain Socket 管理 API

---

## 一、HTTP / SSE / WebSocket

> 这些端点只供可信反向代理或受控运维网络使用；生产部署不得直接把该端口开放给不可信客户端。

HTTP API 由 `plugin_http` 提供。

### SSE 事件流

#### `GET /api/events`

SSE（Server-Sent Events）实时事件流。连接后推送 `ready` 事件，随后转发所有广播事件。心跳每 15 秒。

### WebSocket

#### `GET /api/ws`

WebSocket 实时事件流。与 SSE 相同的事件内容，通过二进制 WebSocket 连接传输。

### 插件 SSE 端点

插件通过 `sse.register_stream` 注册 SSE 事件流后，宿主自动创建对应路由。
连接后推送 `ready` 事件，事件通过对应插件的 `on_api("sse:translate", ...)` 翻译后推送。
插件返回 `null` 的事件会被跳过。`event_types` 为空时接收全部事件，否则宿主会先按事件类型过滤。内置房间事件名为 `create_room`、`update_room`、`join_room`、`leave_room`、`new_round`。插件启用或重载后新增的 SSE 端点即时生效。心跳每 15 秒。

示例：HSNPhira 插件注册了 `/api/rooms/listen`。

### 插件路由

WASM 插件可在运行时通过 `http.register_route` 动态注册路由。路径缺少前导 `/` 时自动补全，重复注册同一路径会替换旧处理器；插件重载后无需重启 HTTP 服务。插件注册的路由通过 `/{*path}` 通配符派发。

### 健康探针

- **`GET /health/live`**：存活探针
- **`GET /health/ready`**：就绪探针（supervisor degraded → 503）

---

## 二、插件 API

> 自动生成自 `wit/phira-plugin.wit`。请勿手动编辑。
> 重新生成: `bash scripts/docgen.sh`（输出到 `docs/api/`）

### 接口概览

| 接口 | 方法数 | 描述 |
|---|---|---|
| phira-types | 0 | 核心数据类型 |
| phira-host | 6 | 核心主机 API |
| phira-events | 0 | 事件类型定义 |
| phira-query | 8 | 用户/房间数据查询 |
| phira-room-mgmt | 8 | 房间管理操作 |
| phira-user-mgmt | 5 | 用户管理与封禁 |
| phira-messaging | 3 | 消息发送与广播 |
| phira-persistence | 6 | 持久化数据查询 |
| phira-admin | 5 | 管理员 ID 配置 |
| phira-config | 5 | 插件配置管理 |
| phira-crypto | 4 | 密码学操作 |
| phira-timer | 2 | 非实时定时器 |
| phira-tcp | 4 | TCP 网络连接 |
| phira-runtime | 3 | 运行时诊断 |
| exports（插件导出） | 5 | 插件生命周期与事件回调 |

### phira-types

Core data types shared between host and guest.

本接口仅定义类型，不包含可调用方法。

#### record `touch-event-point`

#### record `judge-event-item`

#### record `plugin-info`

#### record `http-response`

#### record `game-end-record`

#### variant `json-value`

#### variant `api-result`

---
### phira-host

Host functions available to WASM plugins.

#### `log`

Log a message from the plugin.

**参数**:
- `level`: `string`
- `message`: `string`

**返回值**: `（无）`

**所需 Capability**: （无 — 公开 API）

#### `generate-uuid`

Generate a UUID v4 string.

**参数**:
（无）

**返回值**: `string`

**所需 Capability**: （无 — 公开 API）

#### `current-time-ms`

Get current timestamp in milliseconds.

**参数**:
（无）

**返回值**: `u64`

**所需 Capability**: （无 — 公开 API）

#### `api-call`

Call a server API method. Matches the host's ServerStateQuery interface.

**参数**:
- `method`: `string`
- `args`: `list<json-value>`

**返回值**: `api-result`

**所需 Capability**: （无 — 公开 API）

#### `send-chat`

Send a chat message as system (user-id 0) or as a specific user.

**参数**:
- `user-id`: `u32`
- `message`: `string`

**返回值**: `（无）`

**所需 Capability**: `send`

#### `http-request`

Make an outbound HTTP request (sandboxed by WasmRuntimeConfig).

**参数**:
- `url`: `string`
- `method`: `string`
- `headers`: `list<tuple<string, string>>`
- `body`: `list<u8>`

**返回值**: `result<http-response, string>`

**所需 Capability**: `http`

---
### phira-events

Events the host sends to plugins.

本接口仅定义类型，不包含可调用方法。

#### record `user-connect-info`

#### record `user-disconnect-info`

#### record `room-user-event`

#### record `room-modify-info`

#### record `game-end-info`

#### record `player-touches-info`

#### record `player-judges-info`

#### record `round-complete-info`

#### record `room-join-info`

#### variant `plugin-event`

---
### phira-query

User and room data query APIs available to plugins.

#### `get-user`

Get user basic info (id, name, language, monitor status).

**参数**:
- `user-id`: `u32`

**返回值**: `api-result`

**所需 Capability**: `state.read`

#### `get-user-extra`

Get user's extra extension data by key.

**参数**:
- `user-id`: `u32`
- `key`: `string`

**返回值**: `api-result`

**所需 Capability**: `ext`

#### `set-user-extra`

Set user's extra extension data.

**参数**:
- `user-id`: `u32`
- `key`: `string`
- `value`: `string`

**返回值**: `api-result`

**所需 Capability**: `ext`

#### `get-room`

Get room basic info (id, host, players, state, endpoint).

**参数**:
- `room-id`: `string`

**返回值**: `api-result`

**所需 Capability**: `state.read`

#### `get-room-extra`

Get room's extra extension data by key.

**参数**:
- `room-id`: `string`
- `key`: `string`

**返回值**: `api-result`

**所需 Capability**: `ext`

#### `list-rooms`

List all active room IDs.

**参数**:
（无）

**返回值**: `api-result`

**所需 Capability**: `state.read`

#### `list-online-users`

List online user IDs.

**参数**:
（无）

**返回值**: `api-result`

**所需 Capability**: `state.read`

#### `is-user-online`

Check if a user is currently online.

**参数**:
- `user-id`: `u32`

**返回值**: `bool`

**所需 Capability**: `state.read`

---
### phira-room-mgmt

Room management operations.

#### `create-empty-room`

Create an empty persistent room with optional endpoint override.

**参数**:
- `room-id`: `string`
- `endpoint`: `option<string>`

**返回值**: `api-result`

**所需 Capability**: `room.manage`

#### `kick-from-room`

Kick a user from a room.

**参数**:
- `room-id`: `string`
- `target-id`: `u32`

**返回值**: `api-result`

**所需 Capability**: `room.manage`

#### `transfer-host`

Transfer host to another user in the room.

**参数**:
- `room-id`: `string`
- `target-id`: `u32`

**返回值**: `api-result`

**所需 Capability**: `room.manage`

#### `set-host`

Set room host (none = system ? host).

**参数**:
- `room-id`: `string`
- `target-id`: `option<u32>`

**返回值**: `api-result`

**所需 Capability**: `room.manage`

#### `set-room-lock`

Lock or unlock a room.

**参数**:
- `room-id`: `string`
- `locked`: `bool`

**返回值**: `api-result`

**所需 Capability**: `room.manage`

#### `set-room-hidden`

Hide or unhide a room (hidden rooms excluded from Web API).

**参数**:
- `room-id`: `string`
- `hidden`: `bool`

**返回值**: `api-result`

**所需 Capability**: `room.manage`

#### `close-room`

Close / disband a room.

**参数**:
- `room-id`: `string`

**返回值**: `api-result`

**所需 Capability**: `room.manage`

#### `set-room-phira-api-endpoint`

Set room-level phira_api_endpoint override.

**参数**:
- `room-id`: `string`
- `endpoint`: `option<string>`

**返回值**: `api-result`

**所需 Capability**: `room.manage`

---
### phira-user-mgmt

User management and moderation.

#### `kick-user`

Kick a user from the server.

**参数**:
- `user-id`: `u32`
- `reason`: `string`

**返回值**: `api-result`

**所需 Capability**: `admin`

#### `ban-user`

Ban a user.

**参数**:
- `user-id`: `u32`
- `reason`: `string`

**返回值**: `api-result`

**所需 Capability**: `admin`

#### `unban-user`

Unban a user.

**参数**:
- `user-id`: `u32`

**返回值**: `api-result`

**所需 Capability**: `admin`

#### `get-ban-list`

List banned users.

**参数**:
（无）

**返回值**: `api-result`

**所需 Capability**: `admin`

#### `is-banned`

Check if a user is banned.

**参数**:
- `user-id`: `u32`

**返回值**: `bool`

**所需 Capability**: `admin`

---
### phira-messaging

Messaging — send messages and broadcast.

#### `send-to-user`

Send a direct message to a specific user.

**参数**:
- `user-id`: `u32`
- `message`: `string`

**返回值**: `api-result`

**所需 Capability**: `send`

#### `send-to-room`

Broadcast a message to all users in a room.

**参数**:
- `room-id`: `string`
- `message`: `string`

**返回值**: `api-result`

**所需 Capability**: `send`

#### `send-to-all`

Broadcast a message to all connected users.

**参数**:
- `message`: `string`

**返回值**: `api-result`

**所需 Capability**: `send`

---
### phira-persistence

Persistence read API — incremental event/snapshot queries.

#### `query-events`

Query sequential events since a sequence number.

**参数**:
- `since-sequence`: `u64`
- `limit`: `u32`
- `kind`: `option<string>`
- `room-id`: `option<string>`
- `user-id`: `option<u32>`

**返回值**: `api-result`

**所需 Capability**: `state.read`

#### `query-room-snapshots`

Query room snapshots since a sequence number.

**参数**:
- `since-sequence`: `u64`
- `limit`: `u32`

**返回值**: `api-result`

**所需 Capability**: `state.read`

#### `query-touches`

Query touch batches.

**参数**:
- `since-sequence`: `u64`
- `limit`: `u32`
- `round-uuid`: `option<string>`
- `player-id`: `option<u32>`

**返回值**: `api-result`

**所需 Capability**: `state.read`

#### `query-judges`

Query judge batches.

**参数**:
- `since-sequence`: `u64`
- `limit`: `u32`
- `round-uuid`: `option<string>`
- `player-id`: `option<u32>`

**返回值**: `api-result`

**所需 Capability**: `state.read`

#### `get-playtime`

Get playtime for a user.

**参数**:
- `user-id`: `u32`

**返回值**: `api-result`

**所需 Capability**: `state.read`

#### `top-playtime`

Get top playtime ranking.

**参数**:
- `limit`: `u32`

**返回值**: `api-result`

**所需 Capability**: `state.read`

---
### phira-admin

Admin Phira ID configuration.

#### `list-admin-ids`

List admin Phira IDs.

**参数**:
（无）

**返回值**: `api-result`

**所需 Capability**: `admin`

#### `is-admin`

Check if a user is an admin.

**参数**:
- `user-id`: `u32`

**返回值**: `bool`

**所需 Capability**: `admin`

#### `add-admin-id`

Add an admin Phira ID.

**参数**:
- `user-id`: `u32`

**返回值**: `api-result`

**所需 Capability**: `admin`

#### `remove-admin-id`

Remove an admin Phira ID.

**参数**:
- `user-id`: `u32`

**返回值**: `api-result`

**所需 Capability**: `admin`

#### `set-admin-ids`

Set the full admin ID list.

**参数**:
- `ids`: `list<u32>`

**返回值**: `api-result`

**所需 Capability**: `admin`

---
### phira-config

Plugin configuration (key-value, JSON, per-plugin config.json on disk).

#### `get-config`

Returns null if the key does not exist.

**参数**:
- `key-path`: `string`

**返回值**: `api-result`

**所需 Capability**: `config`

#### `set-config`

Persisted to data/plugins/`<name>`/config.json.

**参数**:
- `key-path`: `string`
- `value`: `string`

**返回值**: `api-result`

**所需 Capability**: `config`

#### `list-config`

List all keys at the given prefix.

**参数**:
- `prefix`: `string`

**返回值**: `api-result`

**所需 Capability**: `config`

#### `reload-config`

Reload config.json from disk.

**参数**:
（无）

**返回值**: `api-result`

**所需 Capability**: `config`

#### `poll-config-changes`

Poll for config changes since a version counter.

**参数**:
- `since-version`: `u64`

**返回值**: `api-result`

**所需 Capability**: `config`

---
### phira-crypto

Non-realtime timer for plugin-internal scheduling. Cryptographic operations (host-side key management).

#### `sign`

The private key never leaves the host process.

**参数**:
- `payload`: `list<u8>`

**返回值**: `result<list<u8>, string>`

**所需 Capability**: `crypto`

#### `verify`

Verify a signature against a public key.

**参数**:
- `pubkey`: `list<u8>`
- `payload`: `list<u8>`
- `signature`: `list<u8>`

**返回值**: `result<bool, string>`

**所需 Capability**: `crypto`

#### `sha256`

SHA-256 hash of arbitrary data.

**参数**:
- `data`: `list<u8>`

**返回值**: `result<list<u8>, string>`

**所需 Capability**: `crypto`

#### `get-node-public-key`

Get the server's node public key (for peer verification).

**参数**:
（无）

**返回值**: `result<list<u8>, string>`

**所需 Capability**: （无 — 公开 API）

---
### phira-timer

#### `set-timer`

Set a one-shot timer. When fired, host calls on-api("timer:fired", [timer-id]).

**参数**:
- `delay-ms`: `u64`
- `timer-id`: `string`

**返回值**: `result<_, string>`

**所需 Capability**: （无 — 公开 API）

#### `clear-timer`

Cancel a pending timer. No-op if timer already fired or unknown.

**参数**:
- `timer-id`: `string`

**返回值**: `result<_, string>`

**所需 Capability**: （无 — 公开 API）

---
### phira-tcp

Plain TCP networking — connect/listen/send/close for WASM plugins.

#### `connect`

Connect to a remote TCP endpoint. Returns a connection handle.

**参数**:
- `addr`: `string`

**返回值**: `result<u64, string>`

**所需 Capability**: `tcp`

#### `listen`

Start a TCP listener. Returns a listener handle.

**参数**:
- `addr`: `string`

**返回值**: `result<u64, string>`

**所需 Capability**: `tcp`

#### `send`

Send raw bytes on an established connection.

**参数**:
- `handle`: `u64`
- `bytes`: `list<u8>`

**返回值**: `result<_, string>`

**所需 Capability**: `tcp`

#### `close`

Close a connection or stop a listener by handle.

**参数**:
- `handle`: `u64`

**返回值**: `result<_, string>`

**所需 Capability**: `tcp`

---
### phira-runtime

Runtime diagnostics.

#### `status`

Get runtime status summary (event bus, worker, registry).

**参数**:
（无）

**返回值**: `api-result`

**所需 Capability**: `state.read`

#### `events`

Get EventBus stats.

**参数**:
- `limit`: `option<u32>`

**返回值**: `api-result`

**所需 Capability**: `state.read`

#### `commands`

Get registered command stats.

**参数**:
（无）

**返回值**: `api-result`

**所需 Capability**: `state.read`

---

### exports（插件导出）

插件必须实现的导出函数（由主机调用）。

#### `init`

**参数**:
（无）

**返回值**: `result<_, string>`

**所需 Capability**: （无 — 插件自身实现）

#### `get-info`

**参数**:
（无）

**返回值**: `plugin-info`

**所需 Capability**: （无 — 插件自身实现）

#### `cleanup`

**参数**:
（无）

**返回值**: `（无）`

**所需 Capability**: （无 — 插件自身实现）

#### `on-event`

**参数**:
- `event`: `plugin-event`

**返回值**: `result<bool, string>`

**所需 Capability**: （无 — 插件自身实现）

#### `on-api`

**参数**:
- `method`: `string`
- `args`: `list<json-value>`

**返回值**: `api-result`

**所需 Capability**: （无 — 插件自身实现）

---

## 三、Capability 映射表

> 自动生成。每项 Capability 对应一组 WIT 方法，主机根据插件的 manifest 授予。

| Capability | 覆盖方法 | 默认可用 |
|---|---|---|
| `state.read` | phira-query.`get-user`, phira-query.`get-room`, phira-query.`list-rooms`, phira-query.`list-online-users`, phira-query.`is-user-online`, phira-persistence.`query-events`, phira-persistence.`query-room-snapshots`, phira-persistence.`query-touches`, phira-persistence.`query-judges`, phira-persistence.`get-playtime`, phira-persistence.`top-playtime`, phira-runtime.`status`, phira-runtime.`events`, phira-runtime.`commands` | ✅ |
| `send` | phira-host.`send-chat`, phira-messaging.`send-to-user`, phira-messaging.`send-to-room`, phira-messaging.`send-to-all` | ✅ |
| `ext` | phira-query.`get-user-extra`, phira-query.`set-user-extra`, phira-query.`get-room-extra` | ✅ |
| `config` | phira-config.`get-config`, phira-config.`set-config`, phira-config.`list-config`, phira-config.`reload-config`, phira-config.`poll-config-changes` | ✅ |
| `file.read` | （无） | ✅ |
| `file.write` | （无） | ✅ |
| `plugin.call` | （无） | ✅ |
| `plugin.register` | （无） | ✅ |
| `http` | phira-host.`http-request` | ❌ 需 manifest |
| `room.manage` | phira-room-mgmt.`create-empty-room`, phira-room-mgmt.`kick-from-room`, phira-room-mgmt.`transfer-host`, phira-room-mgmt.`set-host`, phira-room-mgmt.`set-room-lock`, phira-room-mgmt.`set-room-hidden`, phira-room-mgmt.`close-room`, phira-room-mgmt.`set-room-phira-api-endpoint` | ❌ 需 manifest |
| `admin` | phira-user-mgmt.`kick-user`, phira-user-mgmt.`ban-user`, phira-user-mgmt.`unban-user`, phira-user-mgmt.`get-ban-list`, phira-user-mgmt.`is-banned`, phira-admin.`list-admin-ids`, phira-admin.`is-admin`, phira-admin.`add-admin-id`, phira-admin.`remove-admin-id`, phira-admin.`set-admin-ids` | ❌ 需 manifest |
| `crypto` | phira-crypto.`sign`, phira-crypto.`verify`, phira-crypto.`sha256` | ❌ 需 manifest |
| `timer` | （无） | ❌ 需 manifest |
| `tcp` | phira-tcp.`connect`, phira-tcp.`listen`, phira-tcp.`send`, phira-tcp.`close` | ❌ 需 manifest |

---

## 四、OpenUDS（Unix Domain Socket API）

> 版本: 0.1 | Linux only | 不支持 Windows

### 概述

PMP 通过 Unix Domain Socket 暴露全部管理能力给外部工具（PPB、Web 后端、运维脚本等）。

**设计原则：**
- PMP 不依赖任何消费者——没有连接时 PMP 正常运行
- 接口能力 = CLI 能做的事 + 事件订阅 + 数据流
- 不支持 Windows（UDS 是 Linux 特性）
- 消费者无关——接口不绑定 外部工具，任何外部工具都可以接入

### 架构

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

### 传输协议

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

### 认证

#### Token 模式（自动）

```
外部工具                          PMP
  │                           │
  ├─ Authenticate ───────────►│ { token: "xxx" }
  │◄─ Authenticated ─────────┤ { session_id, server_version }
```

#### CLI 审批模式（手动）

```
外部工具                          PMP
  │                           │
  ├─ Authenticate ───────────►│ { client_name: "我的工具" }
  │◄─ AuthPending ───────────┤ { pending_id: "abc" }
  │         管理员: _approve openuds abc
  │◄─ Authenticated ─────────┤ { session_id, server_version }
```

### 命令（外部工具 → PMP）

#### 房间管理

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
| `room.set_tournament` | `{room_id, tournament}` | 赛事模式房间（禁用默认交互，交 PPB 编排） |
| `room.set_live` | `{room_id, live}` | 设置房间 live 状态（供 Panel/PPB 控制） |
| `room.kick` | `{room_id, user_id, reason?}` | 踢人 |
| `room.force_move` | `{room_id, user_id, monitor?}` | 强制移入 |
| `room.info` | `{room_id}` | 房间详情 |
| `room.list` | `{filters?}` | 房间列表 |
| `room.history` | `{room_id}` | 房间游玩历史（rounds + 完整记录，上限 `play_history_cache_size`） |

#### 玩家管理

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

#### 服务器管理

| 命令 | 参数 | 说明 |
|------|------|------|
| `server.stats` | — | 运行时统计 |
| `server.status` | — | 服务器状态 |
| `server.config_reload` | — | 重载配置 |
| `server.shutdown` | — | 关闭服务器 |
| `server.roomcreation` | `{enabled}` | 建房开关 |

#### 广播

| 命令 | 参数 | 说明 |
|------|------|------|
| `broadcast.all` | `{message}` | 全服广播 |
| `broadcast.room` | `{room_id, message}` | 房间广播 |
| `broadcast.user` | `{user_id, message}` | 私信 |

#### 插件

| 命令 | 参数 | 说明 |
|------|------|------|
| `plugin.list` | — | 插件列表 |
| `plugin.enable` | `{name}` | 启用插件 |
| `plugin.disable` | `{name}` | 禁用插件 |
| `plugin.reload` | — | 重载全部 |
| `plugin.info` | `{name}` | 插件详情 |
| `plugin.remove` | `{name}` | 删除插件 |
| `plugin.call` | `{name, method, args}` | 调用插件 API |

#### 运行时诊断

| 命令 | 参数 | 说明 |
|------|------|------|
| `runtime.status` | — | 运行时诊断 |
| `runtime.actors` | — | Actor 状态 |
| `runtime.persistence` | — | 持久化统计 |
| `runtime.phira` | — | Phira 客户端统计 |

#### 事件订阅

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

### 事件（PMP → 外部工具）

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

### 订阅模型

外部工具 可以精确控制想收的事件类型，避免不必要的数据传输：

```json
// 外部工具 订阅房间和轮次事件
{ "type": "subscribe", "event_types": ["room.*", "round.*"] }
// PMP 确认
{ "type": "subscribed", "active": ["room.*", "round.*"] }
```

通配符: `room.*` = 全部房间事件, `round.scored` = 仅单类型

### 实现

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

### 配置

```yaml
openuds:
  enabled: true
  socket_path: "/var/run/pmp-openuds.sock"
  auth_token: ""
  max_connections: 4
  event_buffer_size: 1024
  heartbeat_interval_secs: 60
```
