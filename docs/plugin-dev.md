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
