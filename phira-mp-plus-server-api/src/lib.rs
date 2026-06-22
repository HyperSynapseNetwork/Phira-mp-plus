//! Phira-mp+ 服务端插件 API
//!
//! 定义插件系统公共接口，打破服务端与插件之间的循环依赖。
//! 服务端和插件都依赖此 crate。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

// ── 事件 ──

/// 插件事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PluginEvent {
    UserConnect { user_id: i32, user_name: String, user_ip: String },
    UserDisconnect { user_id: i32, user_name: String },
    RoomCreate { user_id: i32, room_id: String },
    RoomJoin { user_id: i32, room_id: String, is_monitor: bool },
    RoomLeave { user_id: i32, room_id: String },
    RoomModify { user_id: i32, room_id: String, data: String },
    GameStart { user_id: i32, room_id: String },
    GameEnd { user_id: i32, user_name: String, room_id: String, score: i32, accuracy: f32 },
    PlayerTouches { user_id: i32, room_id: String, data: Vec<TouchEventPoint> },
    PlayerJudges { user_id: i32, room_id: String, data: Vec<JudgeEventItem> },
    /// 一轮游戏完成（所有玩家均已提交成绩）
    RoundComplete { room_id: String, chart_id: i32, chart_name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TouchEventPoint {
    pub time: f32,
    pub finger: i8,
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeEventItem {
    pub time: f32,
    pub line_id: u32,
    pub note_id: u32,
    pub judgement: String,
}

// ── 元数据 ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
}

// ── HTTP 路由注册 ──

/// HTTP 处理器：接收 (请求体JSON, 路径参数) → 返回 JSON 或错误
pub type HttpHandler = Arc<dyn Fn(Option<serde_json::Value>, Vec<String>) -> Result<serde_json::Value, (u16, String)> + Send + Sync>;

/// HttpHandle 内部 trait（用于类型擦除）
pub trait HttpHandleInner: Send + Sync {
    fn register(&self, path: &str, handler: HttpHandler);
}

/// HTTP 服务器句柄（插件通过它注册路由）
#[derive(Clone)]
pub struct HttpHandle {
    inner: Arc<dyn HttpHandleInner>,
}

impl HttpHandle {
    pub fn new(inner: impl HttpHandleInner + 'static) -> Self {
        Self { inner: Arc::new(inner) }
    }

    pub fn register_route(&self, path: &str, handler: HttpHandler) {
        self.inner.register(path, handler);
    }
}

// ── 插件间 API 调用 ──

/// 插件 API 处理器：接收方法名和 JSON 参数 → 返回 JSON
pub type PluginApiHandler = Arc<dyn Fn(&str, &[Value]) -> Result<Value, String> + Send + Sync>;

/// 插件 API 注册表（插件注册 API 供其他插件调用）
#[derive(Clone)]
pub struct PluginApiRegistry {
    inner: Arc<dyn Fn(&str, &str, &[Value]) -> Result<Value, String> + Send + Sync>,
}

impl PluginApiRegistry {
    pub fn new(
        inner: impl Fn(&str, &str, &[Value]) -> Result<Value, String> + Send + Sync + 'static,
    ) -> Self {
        Self { inner: Arc::new(inner) }
    }

    /// 调用其他插件注册的 API: (plugin_name, method, args) → result
    pub fn call(&self, plugin: &str, method: &str, args: &[Value]) -> Result<Value, String> {
        (self.inner)(plugin, method, args)
    }
}

// ── CLI 命令注册 ──

/// CLI 命令描述
pub struct CliCommandInfo {
    pub name: String,
    pub description: String,
    pub usage: String,
}

/// CLI 命令注册句柄
#[derive(Clone)]
pub struct CliHandle {
    inner: Arc<dyn Fn(&str, &str, &str, Arc<dyn Fn(&[&str]) -> Vec<String> + Send + Sync>) -> Result<(), String> + Send + Sync>,
}

impl CliHandle {
    pub fn new(inner: impl Fn(&str, &str, &str, Arc<dyn Fn(&[&str]) -> Vec<String> + Send + Sync>) -> Result<(), String> + Send + Sync + 'static) -> Self {
        Self { inner: Arc::new(inner) }
    }

    pub fn register(&self, name: &str, description: &str, usage: &str, handler: Arc<dyn Fn(&[&str]) -> Vec<String> + Send + Sync>) -> Result<(), String> {
        (self.inner)(name, description, usage, handler)
    }
}

// ── 服务端状态查询 ──

/// 服务端状态查询句柄（插件通过它读取房间/用户数据）
#[derive(Clone)]
pub struct ServerStateQuery {
    inner: Arc<dyn Fn(&str, &[Value]) -> Result<Value, String> + Send + Sync>,
}

impl ServerStateQuery {
    pub fn new(
        inner: impl Fn(&str, &[Value]) -> Result<Value, String> + Send + Sync + 'static,
    ) -> Self {
        Self { inner: Arc::new(inner) }
    }

    /// 调用查询。method: "rooms.list" / "rooms.info" / "rooms.by_user"
    /// 内部在新线程中执行，避免在 tokio 运行时中阻塞。
    pub fn call(&self, method: &str, args: &[Value]) -> Result<Value, String> {
        let inner = self.inner.clone();
        let method = method.to_string();
        let args = args.to_vec();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send((inner)(&method, &args));
        });
        rx.recv().unwrap_or(Err("query thread panicked".to_string()))
    }
}

// ── 插件上下文 ──

/// 插件上下文（传递给插件的 init / on_event）
pub struct PluginContext {
    pub plugin_name: String,
    pub http: Option<HttpHandle>,
    pub state: Option<ServerStateQuery>,
    pub api: Option<PluginApiRegistry>,
    pub cli: Option<CliHandle>,
    /// 发送聊天消息给指定用户 (user_id, message)
    pub send_chat: Option<Arc<dyn Fn(i32, String) + Send + Sync>>,
    /// 注册插件 API（供其他插件调用）
    pub register_api: Option<Arc<dyn Fn(&str, PluginApiHandler) + Send + Sync>>,
}

impl PluginContext {
    pub fn new(name: &str) -> Self {
        Self {
            plugin_name: name.to_string(),
            http: None,
            state: None,
            api: None,
            cli: None,
            send_chat: None,
            register_api: None,
        }
    }

    pub fn with_http(mut self, http: HttpHandle) -> Self { self.http = Some(http); self }
    pub fn with_state(mut self, state: ServerStateQuery) -> Self { self.state = Some(state); self }
    pub fn with_api(mut self, api: PluginApiRegistry) -> Self { self.api = Some(api); self }
    pub fn with_cli(mut self, cli: CliHandle) -> Self { self.cli = Some(cli); self }
    pub fn with_send_chat(mut self, f: Arc<dyn Fn(i32, String) + Send + Sync>) -> Self { self.send_chat = Some(f); self }
    pub fn with_register_api(mut self, f: Arc<dyn Fn(&str, PluginApiHandler) + Send + Sync>) -> Self { self.register_api = Some(f); self }
}

// ── 插件特征 ──

/// 原生插件特征 - 所有插件实现此特征
pub trait NativePlugin: Send + Sync {
    fn info(&self) -> PluginInfo;
    fn init(&mut self, ctx: &PluginContext) -> Result<(), String> {
        let _ = ctx;
        Ok(())
    }
    fn cleanup(&mut self) {}
    fn on_event(&self, ctx: &PluginContext, event: &PluginEvent) -> Vec<String> {
        let _ = (ctx, event);
        vec![]
    }
}
