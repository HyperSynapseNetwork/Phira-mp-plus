# WIT ABI 规范

> **自动生成**: 本文档由 `wit/phira-plugin.wit` 驱动，接口列表由 `tests/wit_abi_contracts.rs`
> 中的 `generate_wit_docs()` 生成。不要手动编辑接口列表。
>
> 验证命令: `cargo test --test wit_abi_contracts`

## 当前状态

| 属性 | 值 |
|------|-----|
| **运行时 ABI** | `abi-wit-v2` (WIT / Component Model) |
| **JSON 桥** | 已移除 (`MIGRATION_PHASE=2`) |
| **规范 WIT** | `wit/phira-plugin.wit` |
| **Host bindings** | `wasmtime::component::bindgen!` 已生成 |
| **Host traits** | `WitPluginHost` — 12 个接口全部实现 |
| **组件加载** | `WitPluginComponent` — 完整加载/初始化/清理/事件/API 生命周期 |
| **Plugin ABI 模块** | `plugin_abi/{mod,plan,json,dto}.rs` |
| **接口数量** | 12 |

## 规范 WIT 接口

WIT 文件定义了以下接口与 world `phira-plugin-v2`:

| 接口 | 说明 | 关键导出 |
|------|------|---------|
| `phira-types` | 核心数据类型 | touch-event-point, judge-event-item, plugin-info, json-value, api-result |
| `phira-host` | 宿主功能 | log, generate-uuid, current-time-ms, api-call, send-chat, http-request |
| `phira-events` | 插件事件 | user-connect-info, game-end-info, round-complete-info, player-touches-info |
| `phira-query` | 数据查询 | get-user, get-room, list-rooms, is-user-online |
| `phira-room-mgmt` | 房间管理 | create-empty-room, kick, transfer-host, set-host, set-room-lock, close-room |
| `phira-user-mgmt` | 用户管理 | kick-user, ban-user, unban-user, get-ban-list |
| `phira-messaging` | 消息广播 | send-to-user, send-to-room, send-to-all |
| `phira-persistence` | 持久化查询 | query-events, query-room-snapshots, get-playtime, top-playtime |
| `phira-admin` | 管理员管理 | list-admin-ids, is-admin, add-admin-id, set-admin-ids |
| `phira-config` | 插件配置 | get-config, set-config, list-config, reload-config |
| `phira-simulation` | Simulation 控制 | status, run, stop, cleanup |
| `phira-runtime` | 运行时诊断 | status, events, commands |

World: `phira-plugin-v2` — 导入上述所有接口，导出 `init`, `get-info`, `cleanup`, `on-event`, `on-api`。

## 迁移状态

1. ✅ WIT 接口定义完成
2. ✅ Host bindings 已生成
3. ✅ Host traits — 12 接口全部实现
4. ✅ 组件加载 — 编译/实例化/初始化/清理全流程
5. ✅ JSON 桥已移除（`MIGRATION_PHASE=2`）
6. ✅ WIT-only ABI — 所有插件必须使用 `phira-plugin-v2` world
7. ❌ Guest SDK (`phira-plugin-sdk`) WIT exports — 进行中
8. ❌ `WitPluginComponent::call_on_event` — WIT 事件参数转换待完善
9. ❌ `WitPluginComponent::call_api` — `on-api` 导出调用待完善
