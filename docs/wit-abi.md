# WIT ABI 规范

> 本文档由 `wit_abi_contracts::generate_wit_docs()` 自动生成规范，请勿手动编辑。
> 更新方式: 修改 WIT 文件后运行 `cargo test --test wit_abi_contracts` 验证一致性。

## 当前状态

| 属性 | 值 |
|------|-----|
| **运行时 ABI** | `abi-wit-v2` (WIT / Component Model) |
| **目标 ABI** | `abi-wit-v2` |
| **规范 WIT** | `wit/phira-plugin.wit` |
| **MIGRATION_PHASE** | `3` (JSON bridge removed, WIT-only component ABI) |
| **接口数量** | `16` |

## 规范 WIT 接口

WIT 文件定义了以下接口与 world `phira-plugin-v2`:

### `phira-types`

Core data types shared between host and guest.

**导出:**

- `touch-event-point`
- `judge-event-item`
- `plugin-info`
- `http-response`
- `game-end-record`
- `json-value`
- `api-result`

### `phira-host`

Host functions available to WASM plugins.

**导出:**

- `log`
- `generate-uuid`
- `current-time-ms`
- `api-call`
- `send-chat`
- `http-request`

### `phira-events`

Events the host sends to plugins.

**导出:**

- `user-connect-info`
- `user-disconnect-info`
- `room-user-event`
- `room-modify-info`
- `game-end-info`
- `player-touches-info`
- `player-judges-info`
- `round-complete-info`
- `room-join-info`
- `plugin-event`

### `phira-query`

User and room data query APIs available to plugins.

**导出:**

- `get-user`
- `get-user-extra`
- `set-user-extra`
- `get-room`
- `get-room-extra`
- `list-rooms`
- `list-online-users`
- `is-user-online`

### `phira-room-mgmt`

Room management operations.

**导出:**

- `create-empty-room`
- `kick-from-room`
- `transfer-host`
- `set-host`
- `set-room-lock`
- `set-room-hidden`
- `close-room`
- `set-room-phira-api-endpoint`

### `phira-user-mgmt`

User management and moderation.

**导出:**

- `kick-user`
- `ban-user`
- `unban-user`
- `get-ban-list`
- `is-banned`

### `phira-messaging`

Messaging — send messages and broadcast.

**导出:**

- `send-to-user`
- `send-to-room`
- `send-to-all`

### `phira-persistence`

Persistence read API — incremental event/snapshot queries.

**导出:**

- `query-events`
- `query-room-snapshots`
- `query-touches`
- `query-judges`
- `get-playtime`
- `top-playtime`

### `phira-admin`

Admin Phira ID configuration.

**导出:**

- `list-admin-ids`
- `is-admin`
- `add-admin-id`
- `remove-admin-id`
- `set-admin-ids`

### `phira-config`

Plugin configuration (key-value, JSON, per-plugin config.json on disk).

**导出:**

- `get-config`
- `set-config`
- `list-config`
- `reload-config`
- `poll-config-changes`

### `phira-simulation`

Simulation management.

**导出:**

- `status`
- `run`
- `stop`
- `cleanup`



**导出:**


### `phira-crypto`

Cryptographic operations (host-side key management).

**导出:**

- `sign`
- `verify`
- `sha256`
- `get-node-public-key`


Federated networking — plugin-controlled TLS connections.

**导出:**

- `connect`
- `listen`
- `send`
- `set-read-timeout`
- `close`

### `phira-timer`

Non-realtime timer for plugin-internal scheduling.

**导出:**

- `set-timer`
- `clear-timer`

### `phira-runtime`

Runtime diagnostics.

**导出:**

- `status`
- `events`
- `commands`

## World

`phira-plugin-v2` — 导入上述所有接口，导出 `init`、`get-info`、`cleanup`、`on-event`、`on-api`。

## Capability 边界

宿主按插件稳定 ID 绑定 capability。关键映射：

| 能力 | 典型接口 |
|---|---|
| `state.read` | 用户、房间、runtime、持久化查询 |
| `send` | 消息发送 |
| `ext` | extension 数据 |
| `config` | 插件配置读写/重载 |
| `file.read` / `file.write` | 插件私有文件 |
| `plugin.call` / `plugin.register` | 插件 API 调用与内部路由/SSE 注册 |
| `http` | 出站 HTTP |
| `room.manage` | 房间管理写操作 |
| `admin` | 踢人、封禁、管理员写操作 |
| `simulation` | Simulation 控制 |

未知方法映射为拒绝，未知 capability 也拒绝加载。管理员和房间管理能力不会因缺少 sidecar 自动授予。

## 资源与故障语义

- Wasmtime 启用 fuel；每次 guest 调用重新设置预算。
- Store limiter 限制线性内存、实例、内存和表数量。
- 每插件只允许一个执行中的调用；并发调用快速失败。
- API/event 超时后插件进入 quarantine，后续调用被拒绝，直到显式重新启用或重载。
- 进程内 `spawn_blocking` 不能强杀已经进入任意阻塞宿主函数的线程。完全不可信插件必须迁移到独立进程或容器边界。

## 兼容性规则

- 仅支持 `abi-wit-v2`
- WIT 的破坏性修改必须提升 package/ABI 版本，而不是只修改 Rust 实现。
- 新增字段优先使用可选类型或新增接口，避免改变已有 record/variant 的二进制契约。
- 插件元数据中的显示名称不是安全身份；授权与清理均使用稳定插件 ID。

## 发布门禁

至少执行：

```bash
cargo check --locked --workspace --all-targets
cargo check --locked -p phira-mp-plus-server --no-default-features --features wit-bindgen
cargo test --locked --workspace
cargo test --locked -p phira-mp-plus-server --test wit_abi_contracts
cargo clippy --locked --workspace --all-targets
```

另需覆盖无限循环、内存增长、越权、初始化超时、事件超时、API 超时、trap 后重载和卸载清理。
