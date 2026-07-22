# Phira-mp+ 插件开发指南

> 旧版 JSON 内存桥 ABI（abi-json-v1）已移除。所有插件必须使用 WIT 组件模型（abi-wit-v2）。

## 快速开始

### 前置条件

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-tools
```

### 创建项目

```bash
cargo new my-plugin --lib
cd my-plugin
```

### Cargo.toml

```toml
[package]
name = "my-plugin"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
phira-plugin-sdk = { path = "../phira-plugin-sdk" }
serde_json = "1.0"
wit-bindgen = "0.58"

[features]
default = ["wit-bindgen"]
```

### 下载 SDK

```bash
wget https://github.com/HyperSynapseNetwork/Phira-mp-plus/releases/latest/download/phira-plugin-sdk.tar.gz
tar xzf phira-plugin-sdk.tar.gz
# 解压后得到 phira-plugin-sdk/ 和 wit/
```

### 编写插件

```rust
// src/lib.rs
phira_plugin_sdk::wit_bindgen!("phira-plugin-v2");
export!(MyPlugin);

use serde_json::{json, Value};
use crate::phira::plugin::phira_host;

struct MyPlugin;

fn host_api(method: &str, args: &[Value]) -> Result<Value, String> {
    let wit_args: Vec<JsonValue> = args.iter().map(json_value_to_wit).collect();
    match phira_host::api_call(method, &wit_args) {
        ApiResult::Ok(value) => Ok(wit_json_to_serde(&value)),
        ApiResult::Error(e) => Err(e),
    }
}

impl Guest for MyPlugin {
    fn init() -> Result<(), String> {
        Ok(())
    }

    fn get_info() -> PluginInfo {
        PluginInfo {
            name: "my-plugin".to_string(),
            version: "0.1.0".to_string(),
            author: "your-name".to_string(),
            description: "My plugin".to_string(),
        }
    }

    fn cleanup() {}
    fn on_event(_event: PluginEvent) -> Result<bool, String> { Ok(false) }
    fn on_api(method: String, args: Vec<JsonValue>) -> ApiResult {
        ApiResult::Ok(JsonValue::Null)
    }
}
```

### 构建

```bash
cargo build --target wasm32-unknown-unknown --release
wasm-tools component new \
  target/wasm32-unknown-unknown/release/my_plugin.wasm \
  -o target/wasm32-unknown-unknown/release/my_plugin.component.wasm
```

### 部署

```bash
cp target/wasm32-unknown-unknown/release/my_plugin.component.wasm \
   /path/to/phira-mp-plus/plugins/my-plugin.wasm
```

启动服务器后插件自动加载。控制台执行 `plugin list` 确认。

---


## Capability 清单

宿主使用插件文件的稳定 ID 读取同目录 sidecar。例如 `plugins/my-plugin.wasm` 对应：

```json
// plugins/my-plugin.capabilities.json
{
  "capabilities": ["state.read", "send", "http"]
}
```

允许值为 `state.read`、`send`、`ext`、`config`、`file.read`、`file.write`、`plugin.call`、`plugin.register`、`http`、`room.manage`、`admin`、`simulation`。未知值会拒绝加载。`room.manage`、`admin`、`simulation` 和 `http` 等能力必须显式授予；不要用可变的插件显示名称作为授权身份。

缺少 sidecar 时仅获得兼容性默认能力，不包含 `http`、`room.manage`、`admin` 或 `simulation`。插件应显式提交最小 capability 清单，不要依赖默认集合。

## 资源与超时语义

- 每次 guest 调用有 fuel 预算，线性内存、实例、内存与表数量受 Store limiter 限制。
- 同一插件同时只执行一个 init/event/API 调用；并发请求会快速失败。
- 调用超过 `call_timeout_ms` 后插件会被 quarantine，后续调用被拒绝。
- quarantine 是故障隔离，不是线程强杀。若插件进入任意阻塞宿主函数，进程内运行时无法像独立进程一样强制终止它。
- 完全不可信插件应部署到独立进程/容器，不应只依赖 PMP 进程内沙箱。

## 注册 HTTP 路由

插件通过 `api-call` 注册 HTTP 路由：

```rust
fn init() -> Result<(), String> {
    host_api("http.register_route", &[json!({
        "path": "/api/hello",
        "plugin": "my-plugin"
    })]);
    Ok(())
}
```

请求到来时，宿主调用 `on_api(method, args)`：

```rust
fn on_api(method: String, args: Vec<JsonValue>) -> ApiResult {
    match method.as_str() {
        "/api/hello" => ApiResult::Ok(json_value_to_wit(&json!({"msg": "hello"}))),
        "/api/greet/:name" => {
            let name = args.get(0).and_then(|v| match v { JsonValue::Text(s) => Some(s.clone()), _ => None })
                .unwrap_or("world".to_string());
            ApiResult::Ok(json_value_to_wit(&json!({"greeting": format!("hello {name}")})))
        }
        _ => ApiResult::Error("unknown route".to_string()),
    }
}
```

## 注册 SSE 事件流

插件可以通过 `sse.register_stream` 注册 SSE（Server-Sent Events）端点。宿主自动为每个注册的流创建 HTTP 路由，客户端连接后事件会通过插件的 `on_api("sse:translate", …)` 翻译后推送：

```rust
fn init() -> Result<(), String> {
    host_api("sse.register_stream", &[json!({
        "path": "/api/rooms/listen",
        "plugin": "my-plugin",
        "event_types": ["create_room", "join_room", "leave_room", "new_round"],
    })]);
    Ok(())
}
```

注册后，客户端可连接 `GET /api/rooms/listen` 接收 SSE 事件。宿主收到每个 `MpEvent` 后调用插件 `on_api("sse:translate", &[json!({"event_type": ..., "data": ...})])`，插件返回翻译后的事件对象（或 `null` 跳过该事件）。
`event_types` 会在调用插件前由宿主执行过滤；空数组表示接收全部事件。内置房间事件名称为 `create_room`、`update_room`、`join_room`、`leave_room`、`new_round`。为兼容旧配置，`CreateRoom`/`RoomCreate` 等历史写法仍可识别。插件启用或重载后新增的 SSE 路由立即生效，不需要重启 HTTP 服务。

路由路径中支持 `:param`、`<param>`、`{param}` 参数占位符。路径缺少开头 `/` 时宿主会自动补全；重复注册同一路径会替换原处理器。普通 HTTP 路由与 SSE 路由均可在插件重载后即时生效。

---

## SSE 翻译回调

宿主维护长连接，并通过 `on_api` 回调插件翻译事件。

```rust
fn init() -> Result<(), String> {
    host_api("sse.register_stream", &[json!({
        "path": "/api/events/rooms",
        "plugin": "my-plugin",
        "event_types": ["create_room", "join_room", "leave_room"],
    })]);
    Ok(())
}
```

事件发生时，宿主调用 `on_api("sse:translate", &[event_json])`：

```rust
fn on_api(method: String, args: Vec<JsonValue>) -> ApiResult {
    match method.as_str() {
        "sse:translate" => {
            let obj = wit_json_to_serde(&args[0]).as_object().cloned().unwrap_or_default();
            let raw_type = obj.get("event_type").and_then(|v| v.as_str()).unwrap_or("");
            let raw_data: Value = obj.get("data")
                .and_then(|v| v.as_str())
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(json!({}));
            let translated = match raw_type {
                "join_room" => json!({"type": "join_room", "room": raw_data.get("room"), "user": raw_data.get("user")}),
                _ => json!(null), // null = 跳过此事件
            };
            ApiResult::Ok(json_value_to_wit(&translated))
        }
        _ => ApiResult::Error("unknown route".to_string()),
    }
}
```

**注意**：SSE 是长连接，插件本身不处理 HTTP 流式响应。宿主负责连接管理，插件只负责事件翻译。`on_api` 返回 `null` 时宿主跳过该事件不发送。

---

## WIT ABI 参考

插件使用 WIT 接口与宿主通信。定义文件：`wit/phira-plugin.wit`，World：`phira-plugin-v2`。

### 插件导出（需实现）

| 函数 | 签名 | 说明 |
|------|------|------|
| `init` | `func() -> result<_, string>` | 初始化，返回 ok/error |
| `get-info` | `func() -> plugin-info` | 元数据（名称/版本/作者/描述） |
| `cleanup` | `func()` | 卸载时清理 |
| `on-event` | `func(event: plugin-event) -> result<bool, string>` | 事件处理 |
| `on-api` | `func(method: string, args: list<json-value>) -> api-result` | API 调用入口 |

### 宿主导入（12 个接口，53 个函数）

#### phira-host

| 函数 | 说明 |
|------|------|
| `log(level, message)` | 日志 |
| `generate-uuid()` | UUID v4 |
| `current-time-ms()` | Unix 毫秒 |
| `api-call(method, args)` | 通用查询 |
| `send-chat(user-id, message)` | 聊天消息 |
| `http-request(url, method, headers, body)` | 沙箱 HTTP 请求 |

#### phira-query

`get-user` / `get-room` / `list-rooms` / `list-online-users` / `is-user-online` / `get-user-extra` / `set-user-extra` / `get-room-extra`

#### phira-room-mgmt

`create-empty-room` / `kick-from-room` / `transfer-host` / `set-host` / `set-room-lock` / `set-room-hidden` / `close-room` / `set-room-phira-api-endpoint`

#### phira-user-mgmt / phira-messaging / phira-persistence / phira-admin / phira-config / phira-simulation / phira-runtime

全部 53 个函数的详细签名见 [WIT 定义文件](../wit/phira-plugin.wit)。

---

## 参考插件

完整插件示例：[HSNPhira-v2-PMP-plugin](https://github.com/FireflyF09/HSNPhira-v2-PMP-plugin)
——HSNPhira v2 前端的 Web API 插件，展示了路由注册、API 处理、JSON 转换等全部模式。

---

## 参考

### 生命周期

PMP 插件状态机：`加载 → 验证 → 启用 → 运行 → 禁用 → 移除`

**加载**：启动时扫描 `plugins_dir/*.wasm`，CLI 命令 `plugin reload` 手动重载。

**资源限制**：

| 限制 | 默认值 | 说明 |
|------|--------|------|
| 燃料 | 10,000,000 | 每次调用消耗，耗尽后 trap |
| 内存 | 64 MB | 线性内存上限 |
| 超时 | 2000 ms | 同步调用超时 |
| 并发 | 8 | 同事件内最大并行插件数 |

**故障隔离 (Quarantine)**：连续超时或 trap 后进入隔离状态，不再接收新事件。

### Manifest 与 Capabilities

插件需在 `.wasm` 同级目录放置 `{name}.capabilities.json`：

```json
{
    "http": true,
    "crypto": false,
    "storage": true,
    "send": false,
    "max_concurrent_calls": 1
}
```

| 权限 | 说明 |
|------|------|
| `http` | 注册 HTTP 路由 |
| `crypto` | 调用 sign/verify/sha256 |
| `storage` | 读写扩展数据 |
| `send` | 发送聊天消息 |
| `tcp` | 发起 TCP 连接 |
| `max_concurrent_calls` | 并发 API 调用数（默认 1） |

默认拒绝所有权限。

### 构建与部署

```bash
cargo build --target wasm32-unknown-unknown --release
wasm-tools component new \
  target/wasm32-unknown-unknown/release/my_plugin.wasm \
  -o my_plugin.component.wasm
cp my_plugin.component.wasm plugins/
cp my_plugin.capabilities.json plugins/
```

热加载：`plugin reload my_plugin`

### 插件配置

服务器可通过 `server_config.yml` 为插件提供全局配置：

```yaml
wasm_runtime:
  max_memory_mb: 64
  fuel_per_call: 10000000
  http_timeout_secs: 10
  max_http_response_bytes: 2097152
  event_queue_capacity: 2048
```

插件内通过 API 读取配置：

```rust
let config = self.api_call("config.get".into(), vec![json!("my_key")]);
```

### 插件管理 CLI

```bash
plugin list                    # 列出所有插件
plugin info <name>             # 查看插件详情
plugin enable <name>           # 启用
plugin disable <name>          # 禁用
plugin reload <name>           # 热重载
plugin remove <name>           # 移除（保留文件）
plugin purge <name>            # 彻底清理
```

`remove` 只禁用和卸载，`purge` 才删除文件和数据。建议先 remove 确认无影响后再 purge。

### SDK Cookbook

SDK 宏路径：`phira_plugin_sdk::wit_bindgen!("phira-plugin-v2")`

```rust
// 原生 SDK 用法（在 PMP 仓库外开发时）
wit_bindgen::generate!({
    path: "path/to/wit/phira-plugin.wit",
    world: "phira-plugin-v2",
});
```

完整 host API 列表见 [WIT ABI 规范](wit-abi.md)。
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
| **接口数量** | `15` |

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

---

# 插件示例

本文档收录 PMP 官方插件示例。每个示例展示一种典型用法：

- [欢迎插件](#欢迎插件) — 玩家加入房间时发送欢迎消息
- [游玩时间追踪](#游玩时间追踪) — 记录玩家总游玩时间
- [轮次结果输出](#轮次结果输出) — 每轮结束将结果写入 JSON 文件
- [房间信息 Web API](#房间信息-web-api) — 注册 HTTP 端点暴露房间信息
- [玩家触控追踪](#玩家触控追踪) — 追踪玩家实时触控数据

---

## 欢迎插件

当玩家加入房间时发送系统消息：

```rust
fn on_event(&mut self, event: PluginEvent) -> Result<bool, String> {
    match event {
        PluginEvent::RoomJoin(info) => {
            let msg = format!("欢迎 {} 加入房间！", info.user_id);
            self.api_call("send".into(), vec![
                json!(0),    // user_id = 0 (system)
                json!(msg),
            ]);
            Ok(true)
        }
        _ => Ok(false),
    }
}
```

## 游玩时间追踪

通过 `ext`（扩展 KV 存储）跨会话累加游玩时间：

```rust
fn on_api(&mut self, method: String, args: Vec<JsonValue>) -> ApiResult {
    match method.as_str() {
        "playtime.get" => {
            let uid = args[0].as_i64().unwrap_or(0);
            let key = format!("playtime:{}", uid);
            let data = self.api_call("ext.get".into(), vec![json!(key)]);
            ApiResult::Ok(data)
        }
        _ => ApiResult::Error("unknown method".into()),
    }
}
```

## 轮次结果输出

游戏结束后将结果写入文件：

```rust
fn on_event(&mut self, event: PluginEvent) -> Result<bool, String> {
    if let PluginEvent::RoundComplete(info) = event {
        let filename = format!("round_{}.json", info.round_id);
        let content = serde_json::to_string(&info).unwrap_or_default();
        self.api_call("file.write".into(), vec![
            json!(filename),
            json!(content),
        ]);
    }
    Ok(false)
}
```

## 房间信息 Web API

注册 `/api/rooms/info` 端点返回房间状态：

```rust
fn init(&mut self) -> Result<(), String> {
    self.register_route("GET", "/api/rooms/info")?;
    Ok(())
}

fn on_api(&mut self, method: String, _args: Vec<JsonValue>) -> ApiResult {
    if method == "room_info" {
        let info = self.api_call("get_room_info".into(), vec![]);
        return ApiResult::Ok(info);
    }
    ApiResult::Error("not found".into())
}
```

## 玩家触控追踪

通过 `player_touches` 事件追踪实时触控数据：

```rust
fn on_event(&mut self, event: PluginEvent) -> Result<bool, String> {
    if let PluginEvent::PlayerTouches(info) = event {
        let total = info.data.len();
        println!("玩家 {} 发送了 {} 个触控点", info.user_id, total);
    }
    Ok(false)
}
```

> 完整源码：[HSNPhira-v2-PMP-plugin](https://github.com/FireflyF09/HSNPhira-v2-PMP-plugin)
