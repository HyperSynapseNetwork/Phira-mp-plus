# WIT ABI 规范

## 当前状态

| 属性 | 值 |
|------|-----|
| **运行时 ABI** | `abi-json-v1` (JSON 内存桥) |
| **目标 ABI** | `abi-wit-v2` (WIT / Component Model) |
| **规范 WIT** | `wit/phira-plugin.wit` (工作区根目录) |
| **MIGRATION_PHASE** | `0` (默认, JSON 桥活跃; 启用 `wit-bindgen` feature 进入 phase 1) |
| **Host bindings** | `wasmtime::component::bindgen!` 已生成, 在 `wit-bindgen` feature 后 |
| **Host traits** | `WitPluginHost` 骨架实现 `phira_host::Host` (在 `wit-bindgen` 后) |
| **Plugin ABI 模块** | 拆分为 `plugin_abi/{mod,plan,json,dto}.rs` 含 typed DTO |

> ⚠️ 默认构建使用 `abi-json-v1`。添加 `--features wit-bindgen` 可编译 WIT 组件模型绑定和 typed host trait 实现。

## 规范 WIT 接口

规范 WIT 文件 (`wit/phira-plugin.wit`) 定义了以下接口:

- `phira-types` — 核心数据类型 (touch/judge 事件、插件信息、HTTP 响应、JSON 值)
- `phira-host` — 宿主提供给插件的功能 (日志、UUID、时间、API 调用、聊天、HTTP)
- `phira-events` — 插件事件类型 (连接/断开、房间生命周期、游戏事件)
- `phira-query` — 用户和房间数据查询
- `phira-room-mgmt` — 房间管理操作
- `phira-user-mgmt` — 用户管理 (踢出、封禁等)
- `phira-messaging` — 消息广播
- `phira-persistence` — 事件/快照查询
- `phira-admin` — 管理员 ID 管理
- `phira-config` — 插件配置 (键值、JSON、每个插件 config.json)
- `phira-simulation` — Simulation 控制
- `phira-runtime` — 运行时诊断

World: `phira-plugin-v2`

## 迁移计划

1. ✅ WIT 接口定义完成，匹配当前 JSON ABI
2. ✅ Host bindings 已生成 (`wasmtime::component::bindgen!`, 在 `wit-bindgen` feature 后)
3. ✅ Host trait 骨架 (`WitPluginHost` 在 `wit_host.rs`, 在 `wit-bindgen` 后)
4. ❌ 更新 Guest SDK (`phira-mp-plus-sdk`) 使用 WIT exports
5. ❌ 双 ABI 支持过渡期
6. ❌ 移除 JSON 桥

## 相关文件

- WIT 定义: `wit/phira-plugin.wit`
- Host bindings: `plugin_abi/mod.rs` → `wit_abi` 模块 (在 `wit-bindgen` 后)
- Host trait 实现: `wit_host.rs` (在 `wit-bindgen` 后)
- JSON 桥: `plugin_abi/json.rs` (默认路径)
