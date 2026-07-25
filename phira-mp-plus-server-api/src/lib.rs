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
    UserConnect {
        user_id: i32,
        user_name: String,
        user_ip: String,
    },
    UserDisconnect {
        user_id: i32,
        user_name: String,
    },
    RoomCreate {
        user_id: i32,
        room_id: String,
    },
    RoomJoin {
        user_id: i32,
        room_id: String,
        is_monitor: bool,
    },
    RoomLeave {
        user_id: i32,
        room_id: String,
    },
    RoomModify {
        user_id: i32,
        room_id: String,
        data: String,
    },
    GameStart {
        user_id: i32,
        room_id: String,
    },
    GameEnd {
        user_id: i32,
        user_name: String,
        room_id: String,
        score: i32,
        accuracy: f32,
        perfect: i32,
        good: i32,
        bad: i32,
        miss: i32,
        max_combo: i32,
        full_combo: bool,
    },
    PlayerTouches {
        user_id: i32,
        room_id: String,
        data: Vec<TouchEventPoint>,
    },
    PlayerJudges {
        user_id: i32,
        room_id: String,
        data: Vec<JudgeEventItem>,
    },
    /// 一轮游戏完成（所有玩家均已提交成绩）
    RoundComplete {
        room_id: String,
        chart_id: i32,
        chart_name: String,
    },
}

impl PluginEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::UserConnect { .. } => "user_connect",
            Self::UserDisconnect { .. } => "user_disconnect",
            Self::RoomCreate { .. } => "room_create",
            Self::RoomJoin { .. } => "room_join",
            Self::RoomLeave { .. } => "room_leave",
            Self::RoomModify { .. } => "room_modify",
            Self::GameStart { .. } => "game_start",
            Self::GameEnd { .. } => "game_end",
            Self::PlayerTouches { .. } => "player_touches",
            Self::PlayerJudges { .. } => "player_judges",
            Self::RoundComplete { .. } => "round_complete",
        }
    }
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
pub type HttpHandler = Arc<
    dyn Fn(Option<serde_json::Value>, Vec<String>) -> Result<serde_json::Value, (u16, String)>
        + Send
        + Sync,
>;

/// HttpHandle 内部 trait（用于类型擦除）
pub trait HttpHandleInner: Send + Sync {
    fn register(&self, path: &str, handler: HttpHandler);
    /// Register a plugin-backed SSE stream.
    fn register_sse(&self, path: &str, plugin: &str, event_types: &[String]);
}

/// HTTP 服务器句柄（插件通过它注册路由）
#[derive(Clone)]
pub struct HttpHandle {
    inner: Arc<dyn HttpHandleInner>,
}

impl HttpHandle {
    pub fn new(inner: impl HttpHandleInner + 'static) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }

    pub fn register_route(&self, path: &str, handler: HttpHandler) {
        self.inner.register(path, handler);
    }

    pub fn register_sse_stream(&self, path: &str, plugin: &str, event_types: &[String]) {
        self.inner.register_sse(path, plugin, event_types);
    }
}

// ── 插件间 API（WASM 插件互调用） ──

/// 插件 API 处理器：接收方法名和 JSON 参数 → 返回 JSON
pub type PluginApiHandler = Arc<dyn Fn(&str, &[Value]) -> Result<Value, String> + Send + Sync>;

// ── 服务端状态查询 ──

/// 服务端状态查询句柄（插件通过它读取房间/用户数据）
#[derive(Clone)]
pub struct ServerStateQuery {
    #[allow(clippy::type_complexity)]
    inner: Arc<dyn Fn(&str, &[Value]) -> Result<Value, String> + Send + Sync>,
}

impl ServerStateQuery {
    pub fn new(
        inner: impl Fn(&str, &[Value]) -> Result<Value, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }

    /// 调用查询。method: "rooms.list" / "rooms.info" / "rooms.by_user"
    /// 内部在新线程中执行（使用 try_read 自旋，无需 tokio 运行时）。
    ///
    /// 注意：各 handler 内部设有自己的超时保护（压测/查询都有），
    /// 因此外层不做超时限制，以免切断长时间运行的操作。
    pub fn call(&self, method: &str, args: &[Value]) -> Result<Value, String> {
        let inner = self.inner.clone();
        let method = method.to_string();
        let args = args.to_vec();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send((inner)(&method, &args));
        });
        rx.recv().unwrap_or(Err("query timeout".to_string()))
    }
}
