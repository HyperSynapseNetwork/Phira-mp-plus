# Phira-mp+ 插件开发文档

Phira-mp+ 支持两种插件开发方式：

1. **原生 Rust 插件** — 通过 `NativePlugin` trait 实现，编译进服务端或作为独立 crate
2. **WASM 插件** — 编译为 WebAssembly 后动态加载，支持热插拔

## 快速开始（原生插件）

### 1. 依赖

插件依赖 `phira-mp-plus-server-api` crate（定义插件的核心接口）：

```toml
[dependencies]
phira-mp-plus-server-api = { path = "../../phira-mp-plus-server-api" }
serde_json = "1.0"
tracing = "0.1"
```

### 2. 实现插件

```rust
use phira_mp_plus_server_api::{
    NativePlugin, PluginContext, PluginEvent, PluginInfo,
};

pub struct MyPlugin;

impl MyPlugin {
    pub fn create() -> Box<dyn NativePlugin> {
        Box::new(MyPlugin)
    }
}

impl NativePlugin for MyPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "my-plugin".to_string(),
            version: "0.1.0".to_string(),
            author: "me".to_string(),
            description: "我的插件".to_string(),
        }
    }

    fn init(&mut self, ctx: &PluginContext) -> Result<(), String> {
        // 注册 HTTP 路由
        if let Some(http) = &ctx.http {
            http.register_route("/api/hello", Arc::new(|_, _| {
                Ok(serde_json::json!({"hello": "world"}))
            }));
        }
        // 注册 CLI 命令
        if let Some(cli) = &ctx.cli {
            cli.register("hello", "打招呼", "hello", Arc::new(|_| {
                vec!["  Hello!".into()]
            }))?;
        }
        // 注册插件 API（供其他插件调用）
        if let Some(reg) = &ctx.register_api {
            reg("my-plugin", Arc::new(|method, _args| {
                match method {
                    "ping" => Ok(serde_json::json!({"pong": true})),
                    _ => Err("unknown".into()),
                }
            }));
        }
        Ok(())
    }

    fn on_event(&self, ctx: &PluginContext, event: &PluginEvent) -> Vec<String> {
        if let PluginEvent::UserConnect { user_id, user_name, .. } = event {
            // 发送欢迎消息
            if let Some(send) = &ctx.send_chat {
                send(*user_id, format!("欢迎 {}！", user_name));
            }
        }
        vec![]
    }
}
```

## PluginContext API

插件在 `init()` 和 `on_event()` 中接收 `&PluginContext`，提供以下能力：

| 字段 | 类型 | 说明 | 可用阶段 |
|------|------|------|---------|
| `http` | `Option<HttpHandle>` | 注册 HTTP 路由 | init |
| `cli` | `Option<CliHandle>` | 注册 CLI 命令 | init |
| `state` | `Option<ServerStateQuery>` | 查询房间/用户状态 | init / event |
| `api` | `Option<PluginApiRegistry>` | 调用其他插件的 API | init / event |
| `send_chat` | `Option<Arc<Fn(i32, String)>>` | 发送聊天消息给用户 | init / event |
| `register_api` | `Option<Arc<Fn(&str, Handler)>>` | 注册插件 API | init |

### HTTP 路由注册

```rust
http.register_route("/api/example", Arc::new(|body, params| {
    // body: Option<Value> — 请求体 JSON
    // params: Vec<String> — 路径参数（<param> 中的值）
    Ok(serde_json::json!({"status": "ok"}))
}));
```

### CLI 命令注册

```rust
cli.register("命令名", "描述", "用法", Arc::new(|args| {
    // args: &[&str] — 命令参数
    vec!["  输出一行".into()]
}))?;
```

### 插件间 API 调用

```rust
// 注册 API
reg("my-plugin", Arc::new(|method, args| {
    match method {
        "get_data" => Ok(serde_json::json!({"data": 42})),
        _ => Err("unknown".into()),
    }
}));

// 调用其他插件 API
let result = ctx.api.unwrap().call("other-plugin", "method", &[])?;
```

### 发送聊天消息

```rust
if let Some(send) = &ctx.send_chat {
    send(user_id, "消息内容".into());
}
```

### 状态查询

```rust
if let Some(state) = &ctx.state {
    let rooms = state.call("rooms.list", &[])?;
    let user_name = state.call("user_name", &[json!(16)])?;
}
```

## 事件系统

```rust
fn on_event(&self, ctx: &PluginContext, event: &PluginEvent) -> Vec<String> {
    match event {
        PluginEvent::UserConnect { user_id, user_name, user_ip } => { /* 用户连接 */ }
        PluginEvent::UserDisconnect { user_id, user_name } => { /* 用户断开 */ }
        PluginEvent::RoomCreate { user_id, room_id } => { /* 房间创建 */ }
        PluginEvent::RoomJoin { user_id, room_id, is_monitor } => { /* 加入房间 */ }
        PluginEvent::RoomLeave { user_id, room_id } => { /* 离开房间 */ }
        PluginEvent::RoomModify { user_id, room_id, data } => { /* 房间修改 */ }
        PluginEvent::GameStart { user_id, room_id } => { /* 游戏开始 */ }
        PluginEvent::GameEnd { user_id, user_name, room_id, score, accuracy } => { /* 游戏结束 */ }
        PluginEvent::RoundComplete { room_id, chart_id, chart_name } => { /* 轮次完成 */ }
        PluginEvent::PlayerTouches { .. } => { /* 玩家触摸 */ }
        PluginEvent::PlayerJudges { .. } => { /* 玩家判定 */ }
        _ => {}
    }
    vec![]
}
```

## 独立插件项目结构

插件可以作为独立 crate 放在 `plugins/` 目录下：

```
plugins/my-plugin/
├── Cargo.toml
└── src/
    └── lib.rs
```

在服务端注册（`server.rs`）：

```rust
state.plugin_manager.register_native(
    my_plugin::MyPlugin::create(),
    "my-plugin",
).await;
```

## WASM 插件开发

WASM 插件编译为 `.wasm` 文件，放置在 `plugins/` 目录下，在服务器启动时自动加载。
使用基于 wasmtime 的 JSON ABI 接口与服务器通信。

### 插件需要导出的函数

WASM 插件必须导出线性内存 `memory` 和以下函数：

| 函数名 | 签名 | 说明 |
|--------|------|------|
| `phira_init` | `() -> i32` | 初始化，返回 0 表示成功 |
| `phira_get_info` | `() -> ()` | 获取插件信息（写入内存偏移 0） |
| `phira_cleanup` | `() -> ()` | 清理资源 |
| `phira_on_event` | `(ptr: i32, len: i32) -> i32` | 事件处理，返回 0 表示已处理 |
| `phira_alloc` | `(size: i32) -> i32` | 分配内存，返回指针 |

### 插件可以导入的宿主函数

| 函数名 | 签名 | 说明 |
|--------|------|------|
| `phira:host/log` | `(level_ptr, level_len, msg_ptr, msg_len)` | 记录日志 |
| `phira:host/uuid` | `(out_ptr, out_len)` | 生成 UUID v4 |
| `phira:host/time` | `() -> i64` | 获取当前时间戳（毫秒） |
| `phira:host/api` | `(method_ptr,method_len,args_ptr,args_len,out_ptr,out_len) -> i32` | 通用 API 桥接 |

### 数据交换约定

1. **插件元数据**：`phira_get_info` 调用后，WASM 模块应在内存偏移 0 处写入 `[len: i32][json_bytes]`，格式为 JSON：
   ```json
   {"name": "my-plugin", "version": "0.1.0", "author": "me", "description": "My plugin"}
   ```

2. **事件格式**：`phira_on_event` 接收的 JSON 字符串格式：
   ```json
   {"type": "user_connect", "user_id": 1, "user_name": "Alice", "user_ip": "127.0.0.1"}
   {"type": "user_disconnect", "user_id": 1, "user_name": "Alice"}
   {"type": "room_create", "user_id": 1, "room_id": "room123"}
   {"type": "room_join", "user_id": 1, "room_id": "room123", "is_monitor": false}
   {"type": "room_leave", "user_id": 1, "room_id": "room123"}
   {"type": "room_modify", "user_id": 1, "room_id": "room123", "data": "..."}
   {"type": "game_start", "user_id": 1, "room_id": "room123"}
   {"type": "game_end", "user_id": 1, "room_id": "room123", "score": 1000000, "accuracy": 0.98}
   {"type": "player_touches", "user_id": 1, "room_id": "room123", "data": [...]}
   {"type": "player_judges", "user_id": 1, "room_id": "room123", "data": [...]}
   {"type": "round_complete", "room_id": "room123", "chart_id": 42, "chart_name": "Chart"}
   ```

### Rust WASM 插件示例

使用 `wasm32-unknown-unknown` 目标编译：

```rust
use std::mem;

// 内存布局
static mut INFO_JSON: &[u8] = br#"{"name":"my-wasm-plugin","version":"0.1.0","author":"me","description":"WASM plugin"}"#;

#[no_mangle]
pub extern "C" fn phira_alloc(size: i32) -> *mut u8 {
    let mut buf = Vec::with_capacity(size as usize);
    let ptr = buf.as_mut_ptr();
    mem::forget(buf);
    ptr
}

#[no_mangle]
pub extern "C" fn phira_init() -> i32 {
    0 // 成功
}

#[no_mangle]
pub extern "C" fn phira_get_info() {
    unsafe {
        let len = INFO_JSON.len() as i32;
        let ptr = 4 as *mut u8; // 偏移 4 字节写入
        // 写入长度前缀 + JSON
        let len_bytes = len.to_le_bytes();
        core::ptr::copy_nonoverlapping(len_bytes.as_ptr(), 0 as *mut u8, 4);
        core::ptr::copy_nonoverlapping(INFO_JSON.as_ptr(), ptr, INFO_JSON.len());
    }
}

#[no_mangle]
pub extern "C" fn phira_cleanup() {}

#[no_mangle]
pub extern "C" fn phira_on_event(ptr: i32, len: i32) -> i32 {
    // 解析事件 JSON 并处理
    0 // 已处理
}
```

编译命令：
```bash
rustup target add wasm32-unknown-unknown
cargo build --target wasm32-unknown-unknown --release
cp target/wasm32-unknown-unknown/release/my_plugin.wasm plugins/
```

### WIT 接口定义（高级用法）

项目在 `wit/phira/mpplus.wit` 中定义了完整的 WIT 接口规范，支持组件模型。
插件开发者可以使用 `cargo-component` 或 `wit-bindgen` 生成类型安全的绑定代码。

WIT 世界 `plugin-world` 包含：
- **imports（宿主服务）**: user-info, room-info, messaging, room-management, user-management, utilities, database, plugin-config
- **exports（插件实现）**: plugin, user-events, cli

| 插件 | 文件 | 特性 |
|------|------|------|
| room-info-web-api | `plugins/webapi-plugin/` | HTTP 路由、状态查询 |
| player-tracker | `plugins/player-tracker/` | 事件监听、CLI 命令、API 注册 |
| playtime-tracker | `plugins/playtime-tracker/` | 事件监听、Web API、CLI、API 注册 |
| round-results | `plugins/round-results/` | 事件监听、Web API、CLI |
| welcome-plugin | `plugins/welcome-plugin/` | 事件监听、CLI、占位符替换 |

## 最佳实践

1. **错误处理**: 通过 `Result<(), String>` 返回错误，服务器自动记录
2. **事件处理**: 保持轻量，避免阻塞（使用 `send_chat` 是异步的）
3. **API 注册**: 在 `init()` 中通过 `register_api` 注册，在 `on_event` 中通过 `ctx.api` 调用
4. **状态查询**: 使用 `ctx.state.call()` 查询房间/用户数据
5. **sned 消息**: 使用 `ctx.send_chat` 发送消息给用户
