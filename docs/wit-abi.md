# WIT ABI 规范

> 本文档由 `wit/phira-plugin.wit` 驱动维护。修改 WIT 后请运行 `cargo test --test wit_abi_contracts` 验证接口列表与迁移状态。

## 当前状态

| 属性 | 值 |
|------|-----|
| **运行时 ABI** | `abi-wit-v2` (WIT / Component Model) |
| **规范 WIT** | `wit/phira-plugin.wit` |
| **World** | `phira-plugin-v2` |
| **MIGRATION_PHASE** | `2` (WIT-only skeleton; lifecycle 已接线, host API 已实现, cap enforcement 已加; 缺 integration tests 和 SDK 示例) |
| **接口数量** | `12` |
| **Host traits** | `WitPluginHost` 已完整实现 12 个接口；全部方法均有真实实现或 capability error；写能力方法强制 capability 检查 |
| **Plugin ABI 模块** | `plugin_abi/{mod,plan,json,dto}.rs` |

## 规范 WIT 接口

WIT 文件定义了以下接口，并由 world `phira-plugin-v2` 导入：

### `phira-types`

核心共享类型。

- `touch-event-point`
- `judge-event-item`
- `plugin-info`
- `http-response`
- `game-end-record`
- `json-value`
- `api-result`

### `phira-host`

宿主基础能力。

- `log`
- `generate-uuid`
- `current-time-ms`
- `api-call`
- `send-chat`
- `http-request`

### `phira-events`

插件事件载荷与事件枚举。

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

用户、房间与扩展数据查询。

- `get-user`
- `get-user-extra`
- `set-user-extra`
- `get-room`
- `get-room-extra`
- `list-rooms`
- `list-online-users`
- `is-user-online`

### `phira-room-mgmt`

房间管理操作。

- `create-empty-room`
- `kick-from-room`
- `transfer-host`
- `set-host`
- `set-room-lock`
- `set-room-hidden`
- `close-room`
- `set-room-phira-api-endpoint`

### `phira-user-mgmt`

用户管理与封禁操作。

- `kick-user`
- `ban-user`
- `unban-user`
- `get-ban-list`
- `is-banned`

### `phira-messaging`

消息发送与广播。

- `send-to-user`
- `send-to-room`
- `send-to-all`

### `phira-persistence`

持久化数据读取。

- `query-events`
- `query-room-snapshots`
- `query-touches`
- `query-judges`
- `get-playtime`
- `top-playtime`

### `phira-admin`

管理员 Phira ID 配置。

- `list-admin-ids`
- `is-admin`
- `add-admin-id`
- `remove-admin-id`
- `set-admin-ids`

### `phira-config`

插件配置接口。WIT 已声明，当前 host 行为仍是占位实现；目标行为见 [plugin-config.md](plugin-config.md)。

- `get-config`
- `set-config`
- `list-config`
- `reload-config`
- `poll-config-changes`

### `phira-simulation`

Simulation 管理接口。当前 host 已提供 `status` 查询，控制类函数仍需补齐。

- `status`
- `run`
- `stop`
- `cleanup`

### `phira-runtime`

运行时诊断接口。

- `status`
- `events`
- `commands`

## World

`phira-plugin-v2` 导入上述所有接口，插件组件必须导出：

- `init`
- `get-info`
- `cleanup`
- `on-event`
- `on-api`

## 迁移状态

1. ✅ WIT 接口定义完成。
2. ✅ Host bindings 已生成，受 `wit-bindgen` feature 控制。
3. ✅ Host trait 已完整实现：所有 12 个接口的方法均有真实实现或明确 capability error。
4. ✅ WIT lifecycle 已接线：`call_init`/`call_cleanup`/`call_on_event`/`call_api` 均完整实现。
5. ✅ Capability enforcement：room-mgmt 写需 `room.manage`，admin 写需 `admin`，config 写需 `config`，simulation 控制需 `admin`。
6. ❌ `phira-plugin-sdk` 的 WIT exports 示例仍需完善；所有新插件应面向 `abi-wit-v2` 和 `phira-plugin-v2` world 编译。

## 必要性与下一步

WIT ABI v2 仍然必要，因为 JSON-memory ABI 不适合继续承载长期插件能力：
typed events、capability、host API 权限和 component-model sandbox 都需要一个
稳定的 ABI 边界。

当前状态已可运行 WIT 插件。下一步只做以下收敛工作:

1. 为每个已实现 host API 方法补充集成测试（需要运行的 WASM 组件）。
2. 补充 capability enforcement 集成测试，验证未授权写被拒绝。
3. 更新 `phira-plugin-sdk` 示例，使 WIT/component model 成为唯一当前 ABI 路径。
3. 将 `wit_host.rs` 中剩余 `not yet implemented` 分成两类：真实实现或明确 capability error。
4. 为 WIT 写能力补 capability enforcement 合同测试，覆盖房间、管理员、配置与仿真控制接口。
5. 同步 `phira-plugin-sdk` 示例，避免继续把 JSON ABI 描述为当前路径。
