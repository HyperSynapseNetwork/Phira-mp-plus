# Phira-mp+ 插件 API 参考

> 自动生成自 `wit/phira-plugin.wit`。请勿手动编辑。
> 重新生成: `bash scripts/docgen.sh`

## 接口概览

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
| phira-simulation | 4 | 模拟运行管理 |
| phira-crypto | 4 | 密码学操作 |
| phira-timer | 2 | 非实时定时器 |
| phira-tcp | 4 | TCP 网络连接 |
| phira-runtime | 3 | 运行时诊断 |
| exports（插件导出） | 5 | 插件生命周期与事件回调 |

## phira-types

Core data types shared between host and guest.

本接口仅定义类型，不包含可调用方法。

### record `touch-event-point`

### record `judge-event-item`

### record `plugin-info`

### record `http-response`

### record `game-end-record`

### variant `json-value`

### variant `api-result`

---
## phira-host

Host functions available to WASM plugins.

### `log`

Log a message from the plugin.

**参数**:  
- `level`: `string`
- `message`: `string`

**返回值**: `（无）`

**所需 Capability**: （无 — 公开 API）

### `generate-uuid`

Generate a UUID v4 string.

**参数**:  
（无）

**返回值**: `string`

**所需 Capability**: （无 — 公开 API）

### `current-time-ms`

Get current timestamp in milliseconds.

**参数**:  
（无）

**返回值**: `u64`

**所需 Capability**: （无 — 公开 API）

### `api-call`

Call a server API method. Matches the host's ServerStateQuery interface.

**参数**:  
- `method`: `string`
- `args`: `list<json-value>`

**返回值**: `api-result`

**所需 Capability**: （无 — 公开 API）

### `send-chat`

Send a chat message as system (user-id 0) or as a specific user.

**参数**:  
- `user-id`: `u32`
- `message`: `string`

**返回值**: `（无）`

**所需 Capability**: `send`

### `http-request`

Make an outbound HTTP request (sandboxed by WasmRuntimeConfig).

**参数**:  
- `url`: `string`
- `method`: `string`
- `headers`: `list<tuple<string, string>>`
- `body`: `list<u8>`

**返回值**: `result<http-response, string>`

**所需 Capability**: `http`

---
## phira-events

Events the host sends to plugins.

本接口仅定义类型，不包含可调用方法。

### record `user-connect-info`

### record `user-disconnect-info`

### record `room-user-event`

### record `room-modify-info`

### record `game-end-info`

### record `player-touches-info`

### record `player-judges-info`

### record `round-complete-info`

### record `room-join-info`

### variant `plugin-event`

---
## phira-query

User and room data query APIs available to plugins.

### `get-user`

Get user basic info (id, name, language, monitor status).

**参数**:  
- `user-id`: `u32`

**返回值**: `api-result`

**所需 Capability**: `state.read`

### `get-user-extra`

Get user's extra extension data by key.

**参数**:  
- `user-id`: `u32`
- `key`: `string`

**返回值**: `api-result`

**所需 Capability**: `ext`

### `set-user-extra`

Set user's extra extension data.

**参数**:  
- `user-id`: `u32`
- `key`: `string`
- `value`: `string`

**返回值**: `api-result`

**所需 Capability**: `ext`

### `get-room`

Get room basic info (id, host, players, state, endpoint).

**参数**:  
- `room-id`: `string`

**返回值**: `api-result`

**所需 Capability**: `state.read`

### `get-room-extra`

Get room's extra extension data by key.

**参数**:  
- `room-id`: `string`
- `key`: `string`

**返回值**: `api-result`

**所需 Capability**: `ext`

### `list-rooms`

List all active room IDs.

**参数**:  
（无）

**返回值**: `api-result`

**所需 Capability**: `state.read`

### `list-online-users`

List online user IDs.

**参数**:  
（无）

**返回值**: `api-result`

**所需 Capability**: `state.read`

### `is-user-online`

Check if a user is currently online.

**参数**:  
- `user-id`: `u32`

**返回值**: `bool`

**所需 Capability**: `state.read`

---
## phira-room-mgmt

Room management operations.

### `create-empty-room`

Create an empty persistent room with optional endpoint override.

**参数**:  
- `room-id`: `string`
- `endpoint`: `option<string>`

**返回值**: `api-result`

**所需 Capability**: `room.manage`

### `kick-from-room`

Kick a user from a room.

**参数**:  
- `room-id`: `string`
- `target-id`: `u32`

**返回值**: `api-result`

**所需 Capability**: `room.manage`

### `transfer-host`

Transfer host to another user in the room.

**参数**:  
- `room-id`: `string`
- `target-id`: `u32`

**返回值**: `api-result`

**所需 Capability**: `room.manage`

### `set-host`

Set room host (none = system ? host).

**参数**:  
- `room-id`: `string`
- `target-id`: `option<u32>`

**返回值**: `api-result`

**所需 Capability**: `room.manage`

### `set-room-lock`

Lock or unlock a room.

**参数**:  
- `room-id`: `string`
- `locked`: `bool`

**返回值**: `api-result`

**所需 Capability**: `room.manage`

### `set-room-hidden`

Hide or unhide a room (hidden rooms excluded from Web API).

**参数**:  
- `room-id`: `string`
- `hidden`: `bool`

**返回值**: `api-result`

**所需 Capability**: `room.manage`

### `close-room`

Close / disband a room.

**参数**:  
- `room-id`: `string`

**返回值**: `api-result`

**所需 Capability**: `room.manage`

### `set-room-phira-api-endpoint`

Set room-level phira_api_endpoint override.

**参数**:  
- `room-id`: `string`
- `endpoint`: `option<string>`

**返回值**: `api-result`

**所需 Capability**: `room.manage`

---
## phira-user-mgmt

User management and moderation.

### `kick-user`

Kick a user from the server.

**参数**:  
- `user-id`: `u32`
- `reason`: `string`

**返回值**: `api-result`

**所需 Capability**: `admin`

### `ban-user`

Ban a user.

**参数**:  
- `user-id`: `u32`
- `reason`: `string`

**返回值**: `api-result`

**所需 Capability**: `admin`

### `unban-user`

Unban a user.

**参数**:  
- `user-id`: `u32`

**返回值**: `api-result`

**所需 Capability**: `admin`

### `get-ban-list`

List banned users.

**参数**:  
（无）

**返回值**: `api-result`

**所需 Capability**: `admin`

### `is-banned`

Check if a user is banned.

**参数**:  
- `user-id`: `u32`

**返回值**: `bool`

**所需 Capability**: `admin`

---
## phira-messaging

Messaging — send messages and broadcast.

### `send-to-user`

Send a direct message to a specific user.

**参数**:  
- `user-id`: `u32`
- `message`: `string`

**返回值**: `api-result`

**所需 Capability**: `send`

### `send-to-room`

Broadcast a message to all users in a room.

**参数**:  
- `room-id`: `string`
- `message`: `string`

**返回值**: `api-result`

**所需 Capability**: `send`

### `send-to-all`

Broadcast a message to all connected users.

**参数**:  
- `message`: `string`

**返回值**: `api-result`

**所需 Capability**: `send`

---
## phira-persistence

Persistence read API — incremental event/snapshot queries.

### `query-events`

Query sequential events since a sequence number.

**参数**:  
- `since-sequence`: `u64`
- `limit`: `u32`
- `kind`: `option<string>`
- `room-id`: `option<string>`
- `user-id`: `option<u32>`

**返回值**: `api-result`

**所需 Capability**: `state.read`

### `query-room-snapshots`

Query room snapshots since a sequence number.

**参数**:  
- `since-sequence`: `u64`
- `limit`: `u32`

**返回值**: `api-result`

**所需 Capability**: `state.read`

### `query-touches`

Query touch batches.

**参数**:  
- `since-sequence`: `u64`
- `limit`: `u32`
- `round-uuid`: `option<string>`
- `player-id`: `option<u32>`

**返回值**: `api-result`

**所需 Capability**: `state.read`

### `query-judges`

Query judge batches.

**参数**:  
- `since-sequence`: `u64`
- `limit`: `u32`
- `round-uuid`: `option<string>`
- `player-id`: `option<u32>`

**返回值**: `api-result`

**所需 Capability**: `state.read`

### `get-playtime`

Get playtime for a user.

**参数**:  
- `user-id`: `u32`

**返回值**: `api-result`

**所需 Capability**: `state.read`

### `top-playtime`

Get top playtime ranking.

**参数**:  
- `limit`: `u32`

**返回值**: `api-result`

**所需 Capability**: `state.read`

---
## phira-admin

Admin Phira ID configuration.

### `list-admin-ids`

List admin Phira IDs.

**参数**:  
（无）

**返回值**: `api-result`

**所需 Capability**: `admin`

### `is-admin`

Check if a user is an admin.

**参数**:  
- `user-id`: `u32`

**返回值**: `bool`

**所需 Capability**: `admin`

### `add-admin-id`

Add an admin Phira ID.

**参数**:  
- `user-id`: `u32`

**返回值**: `api-result`

**所需 Capability**: `admin`

### `remove-admin-id`

Remove an admin Phira ID.

**参数**:  
- `user-id`: `u32`

**返回值**: `api-result`

**所需 Capability**: `admin`

### `set-admin-ids`

Set the full admin ID list.

**参数**:  
- `ids`: `list<u32>`

**返回值**: `api-result`

**所需 Capability**: `admin`

---
## phira-config

Plugin configuration (key-value, JSON, per-plugin config.json on disk).

### `get-config`

Returns null if the key does not exist.

**参数**:  
- `key-path`: `string`

**返回值**: `api-result`

**所需 Capability**: `config`

### `set-config`

Persisted to data/plugins/<name>/config.json.

**参数**:  
- `key-path`: `string`
- `value`: `string`

**返回值**: `api-result`

**所需 Capability**: `config`

### `list-config`

List all keys at the given prefix.

**参数**:  
- `prefix`: `string`

**返回值**: `api-result`

**所需 Capability**: `config`

### `reload-config`

Reload config.json from disk.

**参数**:  
（无）

**返回值**: `api-result`

**所需 Capability**: `config`

### `poll-config-changes`

Poll for config changes since a version counter.

**参数**:  
- `since-version`: `u64`

**返回值**: `api-result`

**所需 Capability**: `config`

---
## phira-simulation

Simulation management.

### `status`

Get current simulation status.

**参数**:  
（无）

**返回值**: `api-result`

**所需 Capability**: `simulation`

### `run`

Start a simulation run.

**参数**:  
- `preset`: `string`
- `users`: `option<u32>`
- `rooms`: `option<u32>`
- `duration`: `option<u32>`

**返回值**: `api-result`

**所需 Capability**: `simulation`

### `stop`

Stop the current simulation run.

**参数**:  
（无）

**返回值**: `api-result`

**所需 Capability**: `simulation`

### `cleanup`

Clean up all simulation data.

**参数**:  
（无）

**返回值**: `api-result`

**所需 Capability**: `simulation`

---
## phira-crypto

Non-realtime timer for plugin-internal scheduling. Cryptographic operations (host-side key management).

### `sign`

The private key never leaves the host process.

**参数**:  
- `payload`: `list<u8>`

**返回值**: `result<list<u8>, string>`

**所需 Capability**: `crypto`

### `verify`

Verify a signature against a public key.

**参数**:  
- `pubkey`: `list<u8>`
- `payload`: `list<u8>`
- `signature`: `list<u8>`

**返回值**: `result<bool, string>`

**所需 Capability**: `crypto`

### `sha256`

SHA-256 hash of arbitrary data.

**参数**:  
- `data`: `list<u8>`

**返回值**: `result<list<u8>, string>`

**所需 Capability**: `crypto`

### `get-node-public-key`

Get the server's node public key (for peer verification).

**参数**:  
（无）

**返回值**: `result<list<u8>, string>`

**所需 Capability**: （无 — 公开 API）

---
## phira-timer

### `set-timer`

Set a one-shot timer. When fired, host calls on-api("timer:fired", [timer-id]).

**参数**:  
- `delay-ms`: `u64`
- `timer-id`: `string`

**返回值**: `result<_, string>`

**所需 Capability**: （无 — 公开 API）

### `clear-timer`

Cancel a pending timer. No-op if timer already fired or unknown.

**参数**:  
- `timer-id`: `string`

**返回值**: `result<_, string>`

**所需 Capability**: （无 — 公开 API）

---
## phira-tcp

Plain TCP networking — connect/listen/send/close for WASM plugins.

### `connect`

Connect to a remote TCP endpoint. Returns a connection handle.

**参数**:  
- `addr`: `string`

**返回值**: `result<u64, string>`

**所需 Capability**: `tcp`

### `listen`

Start a TCP listener. Returns a listener handle.

**参数**:  
- `addr`: `string`

**返回值**: `result<u64, string>`

**所需 Capability**: `tcp`

### `send`

Send raw bytes on an established connection.

**参数**:  
- `handle`: `u64`
- `bytes`: `list<u8>`

**返回值**: `result<_, string>`

**所需 Capability**: `tcp`

### `close`

Close a connection or stop a listener by handle.

**参数**:  
- `handle`: `u64`

**返回值**: `result<_, string>`

**所需 Capability**: `tcp`

---
## phira-runtime

Runtime diagnostics.

### `status`

Get runtime v2 status summary (event bus, worker, registry).

**参数**:  
（无）

**返回值**: `api-result`

**所需 Capability**: `state.read`

### `events`

Get EventBus stats.

**参数**:  
- `limit`: `option<u32>`

**返回值**: `api-result`

**所需 Capability**: `state.read`

### `commands`

Get registered command stats.

**参数**:  
（无）

**返回值**: `api-result`

**所需 Capability**: `state.read`

---

## exports（插件导出）

插件必须实现的导出函数（由主机调用）。

### `init`

**参数**:  
（无）

**返回值**: `result<_, string>`

**所需 Capability**: （无 — 插件自身实现）

### `get-info`

**参数**:  
（无）

**返回值**: `plugin-info`

**所需 Capability**: （无 — 插件自身实现）

### `cleanup`

**参数**:  
（无）

**返回值**: `（无）`

**所需 Capability**: （无 — 插件自身实现）

### `on-event`

**参数**:  
- `event`: `plugin-event`

**返回值**: `result<bool, string>`

**所需 Capability**: （无 — 插件自身实现）

### `on-api`

**参数**:  
- `method`: `string`
- `args`: `list<json-value>`

**返回值**: `api-result`

**所需 Capability**: （无 — 插件自身实现）

