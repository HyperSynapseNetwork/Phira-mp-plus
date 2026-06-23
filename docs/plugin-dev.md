# Phira-mp+ 插件开发文档

Phira-mp+ 支持两种插件开发方式：

1. **原生 Rust 插件** — 通过 `NativePlugin` trait 实现，编译进服务端或作为独立 crate
2. **WASM 插件** — 编译为 WebAssembly 后动态加载，支持热插拔

> ⚠️ 旧版 SDK crate (`phira-mp-plus-sdk`) 已废弃，
> 请使用 `phira-mp-plus-server-api` crate 开发插件。

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
    /// 工厂方法——服务器使用它创建插件实例
    pub fn create() -> Box<dyn NativePlugin> {
        Box::new(MyPlugin)
    }
}

impl NativePlugin for MyPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "my-plugin".into(),
            version: "0.1.0".into(),
            author: "me".into(),
            description: "我的第一个插件".into(),
        }
    }

    fn init(&mut self, ctx: &PluginContext) -> Result<(), String> {
        // 在 init 中注册 HTTP 路由、CLI 命令等
        if let Some(http) = &ctx.http {
            http.register_route("/api/my-endpoint", Arc::new(|body, params| {
                Ok(serde_json::json!({"status": "ok"}))
            }));
        }
        if let Some(cli) = &ctx.cli {
            cli.register("my-command", "我的命令", "my-command <arg>",
                Arc::new(|args| vec!["Hello from plugin!".into()])
            )?;
        }
        Ok(())
    }

    fn on_event(&self, ctx: &PluginContext, event: &PluginEvent) -> Vec<String> {
        match event {
            PluginEvent::UserConnect { user_id, user_name, user_ip } => {
                info!("用户 {}({}) 连接，IP: {}", user_name, user_id, user_ip);
            }
            PluginEvent::GameEnd { user_id, user_name, score, accuracy, .. } => {
                info!("用户 {} 完成游戏: 分数={}, ACC={}%", user_name, score, accuracy * 100.0);
            }
            _ => {}
        }
        vec![]
    }
}
```

### 3. 注册插件

在 `phira-mp-plus-server/src/server.rs` 的 `PlusServer::new()` 中添加：

```rust
// 使用 register_native 注册原生插件
let _ = state.plugin_manager.register_native(
    my_plugin::MyPlugin::create(),
    "my-plugin",
).await;
```

插件也支持 feature gate：

```rust
#[cfg(feature = "my-plugin")]
{
    state.plugin_manager.register_native(
        my_plugin::MyPlugin::create(),
        "my-plugin",
    ).await;
}
```

## 插件事件

`PluginEvent` 枚举定义了所有可监听的事件：

| 事件 | 触发时机 | 数据 |
|------|---------|------|
| `UserConnect` | 用户认证通过后 | user_id, user_name, user_ip |
| `UserDisconnect` | 用户断开连接 | user_id, user_name |
| `RoomCreate` | 创建房间 | user_id, room_id |
| `RoomJoin` | 加入房间 | user_id, room_id, is_monitor |
| `RoomLeave` | 离开房间 | user_id, room_id |
| `RoomModify` | 修改房间设置 | user_id, room_id, data |
| `GameStart` | 游戏开始 | user_id, room_id |
| `GameEnd` | 玩家提交成绩 | user_id, user_name, room_id, score, accuracy, perfect, good, bad, miss, max_combo, full_combo |
| `PlayerTouches` | 玩家触控事件 | user_id, room_id, data (触控点数组) |
| `PlayerJudges` | 玩家判定事件 | user_id, room_id, data (判定数组) |
| `RoundComplete` | 一轮游戏完成 | room_id, chart_id, chart_name |

## PluginContext API

`PluginContext` 提供插件可用的各种能力（通过 builder 模式组合）：

```rust
// 构建上下文——通常由服务器完成
let mut ctx = PluginContext::new("my-plugin");

// HTTP 路由注册
ctx = ctx.with_http(http_handle);

// 服务端状态查询（房间/用户数据）
ctx = ctx.with_state(state_query);

// 其他插件 API 调用
ctx = ctx.with_api(api_registry);

// CLI 命令注册
ctx = ctx.with_cli(cli_handle);

// 发送聊天消息
ctx = ctx.with_send_chat(send_chat_fn);

// 注册 API 供其他插件调用
ctx = ctx.with_register_api(register_api_fn);
```

## HTTP 路由

插件通过 `PluginContext.http` 注册路由，支持动态注册（即使 HTTP 服务器已启动）。

### 路径参数

使用 `<param>` 或 `{param}` 语法定义路径参数：

```rust
// 路径: /api/items/<id>
// 匹配: /api/items/42     → params = ["42"]
// 匹配: /api/items/hello  → params = ["hello"]
http.register_route("/api/items/<id>", Arc::new(|body, params| {
    let id = params.first().cloned().unwrap_or_default();
    Ok(serde_json::json!({"id": id}))
}));
```

## CLI 命令

插件通过 `PluginContext.cli` 注册自定义命令：

```rust
cli.register(
    "my-cmd",                    // 命令名
    "描述信息",                    // 描述
    "my-cmd <arg>",              // 用法
    Arc::new(|args| {
        let arg = args.first().copied().unwrap_or("");
        vec![format!("参数: {}", arg)]
    }),
)?;
```

## 服务端配置

插件可通过配置文件 `server_config.yml` 获取自定义配置项：

```yaml
# 示例：自定义配置
my_plugin_option: "value"
```

服务端读取解析 YAML 后可通过 `PlusConfig` 扩展字段访问。

## 构建为独立项目

插件也可以作为独立的 Rust 项目开发：

```toml
[package]
name = "my-phira-plugin"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["lib"]

[dependencies]
phira-mp-plus-server-api = { git = "https://github.com/your-repo/phira-mp-plus" }
serde_json = "1.0"
tracing = "0.1"
```

## WASM 插件

WASM 插件通过 wasmtime 运行时动态加载，部署在 `plugins/` 目录下。

### JSON ABI

WASM 插件通过 JSON 字符串与宿主通信：

- **导出函数**：`phira_init`, `phira_get_info`, `phira_cleanup`, `phira_on_event`, `phira_alloc`, `phira_dealloc`
- **导入函数**：`phira:host/log`, `phira:host/uuid`, `phira:host/time`, `phira:host/api`

详见 `src/wasm_host.rs` 中的接口定义。

## 最佳实践

1. **错误处理**：`init()` 返回 `Err` 表示插件初始化失败，服务器会记录警告但不会崩溃
2. **状态隔离**：使用 `Arc<Mutex<>>` 管理内部状态
3. **日志**：使用 `tracing` crate 记录日志，与服务器统一格式
4. **持久化**：使用 `ExtensionManager` 存储少量 KV 数据，大量数据使用独立文件
5. **性能**：事件处理器应尽快返回，耗时操作使用 `tokio::spawn`
