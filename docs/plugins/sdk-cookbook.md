# Plugin SDK Cookbook

## Quick Start

```rust
phira_plugin_sdk::wit_bindgen!("phira-plugin-v2");
export!(MyPlugin);

use serde_json::{json, Value};

struct MyPlugin;

impl Guest for MyPlugin {
    fn init() -> Result<(), String> {
        register_http_route("/api/my-endpoint");
        Ok(())
    }

    fn get_info() -> PluginInfo {
        PluginInfo {
            name: "my-plugin".into(),
            version: "0.1.0".into(),
            author: "me".into(),
            description: "My awesome plugin".into(),
        }
    }

    fn cleanup() {}

    fn on_event(_event: PluginEvent) -> Result<bool, String> { Ok(false) }

    fn on_api(method: String, args: Vec<JsonValue>) -> ApiResult {
        match method.as_str() {
            "/api/my-endpoint" => ApiResult::Ok(json_value_to_wit(&json!({"status": "ok"}))),
            _ => ApiResult::Error("unknown method".into()),
        }
    }
}
```

## Capabilities

See `capabilities.json` in the plugin directory:

```json
{
    "http": true,
    "crypto": false,
    "federation": false,
    "storage": true,
    "send": false
}
```

## Host API Methods

### HTTP Routes

```rust
host_api("http.register_route", &[json!({"path": "/api/foo", "plugin": "my-plugin"})])
```

### SSE Streams

```rust
host_api("sse.register_stream", &[json!({
    "path": "/api/stream",
    "plugin": "my-plugin",
    "event_types": ["RoomCreate", "RoomJoin"]
})])
```

### Crypto

```rust
host_api("crypto.sha256", &[json!("data")])
```

### Room Operations

```rust
host_api("rooms.list", &[])
host_api("rooms.by_name", &[json!("room-name")])
host_api("auth.visited_count", &[])
host_api("playtime.leaderboard", &[])
```

## Building

```bash
cargo build --target wasm32-unknown-unknown --release
wasm-tools component new \
  target/wasm32-unknown-unknown/release/my_plugin.wasm \
  -o my_plugin.component.wasm
```
