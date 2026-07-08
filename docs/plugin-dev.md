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

路由路径中支持 `:param`、`<param>`、`{param}` 参数占位符。

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
