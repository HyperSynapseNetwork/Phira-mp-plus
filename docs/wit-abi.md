# WIT ABI 规范

> **自动生成**: 本文档由 `wit/phira-plugin.wit` 驱动，接口列表由 `tests/wit_abi_contracts.rs`
> 中的 `generate_wit_docs()` 生成。不要手动编辑接口列表。
>
> 验证命令: `cargo test --test wit_abi_contracts`

## 当前状态

| 属性 | 值 |
|------|-----|
| **运行时 ABI** | `abi-json-v1` (JSON 内存桥) |
| **目标 ABI** | `abi-wit-v2` (WIT / Component Model) |
| **规范 WIT** | `wit/phira-plugin.wit` |
| **MIGRATION_PHASE** | `0` (JSON 桥活跃; 启用 `wit-bindgen` feature 进入 phase 1) |
| **Host bindings** | `wasmtime::component::bindgen!` 已生成 (`wit-bindgen` feature) |
| **Host traits** | `WitPluginHost` 骨架实现 `phira_host::Host` (`wit-bindgen` feature) |
| **Plugin ABI 模块** | `plugin_abi/{mod,plan,json,dto}.rs` 含 typed DTO |
| **接口数量** | 12 |

> ⚠️ 默认构建使用 `abi-json-v1`。添加 `--features wit-bindgen` 编译 WIT 绑定。

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

## 迁移计划

1. ✅ WIT 接口定义完成
2. ✅ Host bindings 已生成 (`wit-bindgen` feature)
3. ✅ Host trait 骨架已实现 (`wit_host.rs`)
4. ❌ Guest SDK (`phira-plugin-sdk`) 使用 WIT exports
5. ❌ 双 ABI 支持过渡期
6. ❌ 移除 JSON 桥
