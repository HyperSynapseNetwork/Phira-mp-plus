# Phira-mp+ 插件开发文档

Phira-mp+ 支持两种插件开发方式：

1. **原生 Rust 插件** — 通过 `NativePlugin` trait 实现，编译进服务端或作为独立 crate
2. **WASM 插件** — 编译为 WebAssembly 后动态加载（框架就绪，待完整实现）

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

## 已有插件参考

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
