# Phira-mp+ 插件开发文档

## 概述

Phira-mp+ 支持两种插件开发方式：

1. **原生 Rust 插件** — 通过实现 `PhiraPlugin` trait 在服务端直接注册（适合开发测试）
2. **WASM 插件** — 编译为 WebAssembly 后动态加载（推荐生产环境，热加载支持）

---

## 快速开始（原生 Rust 插件）

### 1. 添加依赖

在 `Cargo.toml` 中添加 SDK 依赖：

```toml
[dependencies]
phira-mp-plus-sdk = { path = "../phira-mp-plus-sdk" }
```

### 2. 实现插件

```rust
use phira_mp_plus_sdk::*;

pub struct MyPlugin;

impl PhiraPlugin for MyPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "my-plugin".to_string(),
            version: "0.1.0".to_string(),
            author: "开发者名称".to_string(),
            description: "插件功能描述".to_string(),
        }
    }

    fn init(&mut self, ctx: &PluginContext) -> Result<(), String> {
        ctx.log("info", "MyPlugin 初始化完成");
        // 注册用户扩展字段
        ctx.log("info", "注册扩展字段...");
        Ok(())
    }

    fn on_connect(&self, ctx: &PluginContext, user_id: i32, user_name: &str) {
        ctx.log("info", &format!("用户 {} 已连接", user_name));
    }

    fn on_disconnect(&self, ctx: &PluginContext, user_id: i32, user_name: &str) {
        ctx.log("info", &format!("用户 {} 已断开连接", user_name));
    }

    fn on_room_create(&self, ctx: &PluginContext, user_id: i32, room_id: &str) {
        ctx.log("info", &format!("用户 {} 创建了房间 {}", user_id, room_id));
    }

    fn on_room_join(&self, ctx: &PluginContext, user_id: i32, room_id: &str, is_monitor: bool) {
        if is_monitor {
            ctx.log("info", &format!("用户 {} 以旁观者身份加入房间 {}", user_id, room_id));
        } else {
            ctx.log("info", &format!("用户 {} 加入房间 {}", user_id, room_id));
        }
    }

    fn cleanup(&self) {
        // 清理资源
    }
}
```

### 3. 注册插件到服务器

```rust
use phira_mp_plus_server::plugin::PluginManager;

// 在服务器初始化时注册
let plugin_manager = Arc::new(PluginManager::new("plugins", extensions));
plugin_manager.register_native(
    Box::new(MyPlugin),
    "my-plugin",
).await?;
```

---

## 插件 API 参考

### 插件特征 (`PhiraPlugin`)

```rust
pub trait PhiraPlugin: Send + Sync {
    fn info(&self) -> PluginInfo;                                          // 必需
    fn init(&mut self, _ctx: &PluginContext) -> Result<(), String>;        // 可选
    fn cleanup(&self) {}                                                    // 可选
    fn on_connect(&self, _ctx: &PluginContext, _user_id: i32, _user_name: &str) {}
    fn on_disconnect(&self, _ctx: &PluginContext, _user_id: i32, _user_name: &str) {}
    fn on_room_create(&self, _ctx: &PluginContext, _user_id: i32, _room_id: &str) {}
    fn on_room_join(&self, _ctx: &PluginContext, _user_id: i32, _room_id: &str, _is_monitor: bool) {}
    fn on_room_leave(&self, _ctx: &PluginContext, _user_id: i32, _room_id: &str) {}
    fn on_room_modify(&self, _ctx: &PluginContext, _user_id: i32, _room_id: &str, _data: &str) {}
    fn on_game_start(&self, _ctx: &PluginContext, _user_id: i32, _room_id: &str) {}
    fn on_game_end(&self, _ctx: &PluginContext, _user_id: i32, _room_id: &str, _score: i32, _accuracy: f32) {}
}
```

### 插件信息

```rust
pub struct PluginInfo {
    pub name: String,        // 插件名称（唯一标识符）
    pub version: String,     // 版本号
    pub author: String,      // 作者
    pub description: String, // 功能描述
}
```

### 插件上下文 API (`PluginContext`)

#### 日志记录

```rust
fn log(&self, level: &str, message: &str)
// level: "error" | "warn" | "info" | "debug" | "trace"
```

#### 用户信息获取

```rust
fn get_user(&self, user_id: i32) -> Option<UserData>
// 获取用户的基础信息

fn get_user_extra(&self, user_id: i32, key: &str) -> Option<String>
// 获取用户的扩展数据

fn set_user_extra(&self, user_id: i32, key: &str, value: &str) -> Result<(), String>
// 设置用户的扩展数据
```

#### 房间信息获取

```rust
fn get_room(&self, room_id: &str) -> Option<RoomData>
// 获取房间的基础信息
```

#### 消息发送

```rust
fn send_to_user(&self, user_id: i32, message: &str) -> Result<(), String>
// 向指定用户发送消息

fn send_to_room(&self, room_id: &str, message: &str) -> Result<(), String>
// 向指定房间广播消息

fn send_to_all(&self, message: &str) -> Result<(), String>
// 向所有在线用户广播消息
```

#### 工具方法

```rust
fn generate_uuid(&self) -> String
// 生成 UUID v4

fn current_time(&self) -> String
// 获取当前 ISO 8601 格式时间
```

---

## 数据模型

### 用户数据

```rust
pub struct UserData {
    pub id: i32,
    pub name: String,
    pub language: String,
    pub is_monitor: bool,
}
```

### 房间数据

```rust
pub struct RoomData {
    pub id: String,
    pub host_id: i32,
    pub host_name: String,
    pub player_count: u32,
    pub monitor_count: u32,
    pub state: String,     // "SelectChart" | "WaitingForReady" | "Playing"
    pub locked: bool,
    pub cycling: bool,
}
```

### 插件事件

```rust
pub enum PluginEvent {
    UserConnect { user_id: i32, user_name: String },
    UserDisconnect { user_id: i32, user_name: String },
    RoomCreate { user_id: i32, room_id: String },
    RoomJoin { user_id: i32, room_id: String, is_monitor: bool },
    RoomLeave { user_id: i32, room_id: String },
    RoomModify { user_id: i32, room_id: String, data: String },
    GameStart { user_id: i32, room_id: String },
    GameEnd { user_id: i32, room_id: String, score: i32, accuracy: f32 },
}
```

---

## WASM 插件开发

### WIT 接口规范

WIT (WebAssembly Interface Types) 是定义 WASM 组件接口的标准语言。Phira-mp+ 的 WIT 规范位于 `phira-mp-plus-server/wit/phira-mp-plus.wit`。

完整规范包含以下接口：

| 接口 | 说明 |
|------|------|
| `user-events` | 用户行为事件监听（连接、断开、创建房间等） |
| `user-info` | 用户信息获取与扩展数据 |
| `room-info` | 房间信息获取与扩展数据 |
| `messaging` | 消息发送（用户/房间/全体） |
| `room-management` | 房间管理（踢出、转让、锁定、关闭） |
| `user-management` | 用户管理（踢出、封禁、解封） |
| `utilities` | 工具函数（UUID、HTTP、文件操作） |
| `database` | 数据库访问 |
| `plugin-config` | 插件配置管理 |
| `plugin` | 插件主入口（init/cleanup/info） |

### WASM 插件编译

```bash
# 使用 wit-bindgen 生成绑定
wit-bindgen rust --out-dir src/bindings wit/phira-mp-plus.wit
```

### 示例：Rust WASM 插件

```rust
// 使用 wit-bindgen 生成的绑定
use crate::bindings::exports::phira::mpplus::plugin::Plugin;
use crate::bindings::phira::mpplus::user_events::UserEvents;

struct MyWasmPlugin;

impl Plugin for MyWasmPlugin {
    fn init() -> Result<(), String> {
        Ok(())
    }

    fn get_info() -> PluginInfo {
        PluginInfo {
            name: "wasm-plugin".to_string(),
            version: "0.1.0".to_string(),
            author: "开发者".to_string(),
            description: "WASM 示例插件".to_string(),
        }
    }

    fn cleanup() {}
}

// 导出插件入口
export_plugin!(MyWasmPlugin);
```

### 其他语言 WASM 插件

任何支持 WASM 编译的语言（C/C++、Go、Python、AssemblyScript 等）都可以开发 Phira-mp+ 插件，只需遵循 WIT 规范实现对应接口即可。

---

## 最佳实践

### 1. 错误处理

始终通过 `Result<(), String>` 返回错误信息，服务器会自动记录并处理。

### 2. 性能考虑

- 事件处理函数应尽量轻量，避免阻塞
- 耗时操作应异步处理
- 大量数据使用扩展数据系统而非事件参数

### 3. 安全

- 插件在 WASM 沙箱中运行（WASM 模式），不能直接访问文件系统
- 所有外部操作（文件、网络）必须通过 SDK API
- 原生插件应验证所有输入数据

### 4. 配置管理

```rust
fn init(&mut self, ctx: &PluginContext) -> Result<(), String> {
    // 注册默认配置
    // 配置会持久化到服务器配置存储中
    Ok(())
}
```

### 5. 示例插件

#### 连接统计插件

```rust
struct StatsPlugin {
    connect_count: std::sync::atomic::AtomicI32,
}

impl PhiraPlugin for StatsPlugin {
    fn info(&self) -> PluginInfo { /* ... */ }

    fn on_connect(&self, ctx: &PluginContext, _user_id: i32, _user_name: &str) {
        self.connect_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        ctx.log("info", &format!(
            "第 {} 次连接",
            self.connect_count.load(std::sync::atomic::Ordering::SeqCst)
        ));
    }
}
```

#### 黑名单插件

```rust
struct BanListPlugin;

impl PhiraPlugin for BanListPlugin {
    fn info(&self) -> PluginInfo { /* ... */ }

    fn on_connect(&self, ctx: &PluginContext, user_id: i32, _user_name: &str) {
        if let Some(status) = ctx.get_user_extra(user_id, "ban-status") {
            if status == "banned" {
                ctx.log("warn", &format!("被 ban 的用户尝试连接: {}", user_id));
            }
        }
    }
}
```

---

## 调试与测试

### 使用事件日志插件

服务器默认注册了 `event-logger` 插件，会记录所有插件事件到日志文件中。

### 日志查看

```bash
# 查看服务器日志
tail -f log/phira-mp-plus.*.log

# 开启 debug 日志
RUST_LOG=debug phira-mp-plus-server
```

### 常见问题

**Q: 插件加载失败怎么办？**
A: 检查插件目录路径是否正确，WASM 文件是否完整，日志中会记录详细错误信息。

**Q: 插件事件收不到？**
A: 确认插件已被正确启用（`plug-enable <name>`），并使用 `plugins` 命令查看插件状态。

**Q: 如何在 WASM 插件中使用第三方库？**
A: WASM 插件可以包含第三方库，但需要注意 WASM 目标平台限制（不支持网络、文件系统等）。通过 Phira-mp+ SDK 提供的 API 进行这些操作。
