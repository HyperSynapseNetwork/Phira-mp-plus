//! 中央 HTTP/SSE/WS 服务器
//!
//! 插件通过 PluginContext 注册路由和推送 SSE 事件，
//! 统一在单个端口暴露，无需每个插件自建 HTTP 服务。
//! 支持运行时动态注册路由（所有请求通过 catch-all 处理器派发）。
//! 额外提供 /ws/live 和 /rooms/listen 端点供 web-monitor 使用。

use crate::server::PlusServerState;
use axum::{
    Router, Json,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::{StatusCode, Uri, Method},
    response::sse::{Event, KeepAlive, Sse},
    response::IntoResponse,
    routing::any,
};
use futures::stream::Stream;
use serde_json::Value;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt as _;
use tower_http::cors::CorsLayer;
use tracing::{error, info};
use phira_mp_plus_server_api as api;

/// SSE 事件
#[derive(Debug, Clone)]
pub struct SseEvent {
    pub event_type: String,
    pub data: String,
}

/// 通用 HTTP 处理器：接收 (请求体JSON, 路径参数) → 返回 JSON
pub type HttpHandler = Arc<dyn Fn(Option<Value>, Vec<String>) -> Result<Value, (u16, String)> + Send + Sync>;

struct RouteEntry {
    path: String,
    handler: HttpHandler,
}

/// 动态路由表（支持运行时注册和查找）
struct DynamicRouter {
    /// 注册的路由条目（保持注册顺序，精确优先于通配）
    entries: Vec<RouteEntry>,
}

impl DynamicRouter {
    fn new() -> Self {
        Self { entries: Vec::new() }
    }

    fn add(&mut self, path: String, handler: HttpHandler) {
        self.entries.push(RouteEntry { path, handler });
    }

    /// 查找匹配的路由处理器。
    /// 支持两种参数语法: `<param>` 和 `{param}`
    fn resolve(&self, _method: &Method, uri: &Uri) -> Option<(HttpHandler, Vec<String>)> {
        let path = uri.path();
        for entry in &self.entries {
            if let Some(params) = match_route(&entry.path, path) {
                return Some((Arc::clone(&entry.handler), params));
            }
        }
        None
    }
}

/// 匹配路由模式与请求路径。返回路径参数（如果有）。
/// 模式 "/api/round/last/<room_id>" 匹配 "/api/round/last/abc" → ["abc"]
fn match_route(pattern: &str, path: &str) -> Option<Vec<String>> {
    let p_segs: Vec<&str> = pattern.split('/').collect();
    let u_segs: Vec<&str> = path.split('/').collect();
    if p_segs.len() != u_segs.len() {
        return None;
    }
    let mut params = Vec::new();
    for (p, u) in p_segs.iter().zip(u_segs.iter()) {
        if p.starts_with('<') && p.ends_with('>') {
            params.push(u.to_string());
        } else if p.starts_with('{') && p.ends_with('}') {
            params.push(u.to_string());
        } else if p != u {
            return None;
        }
    }
    Some(params)
}

/// 中央 HTTP/SSE/WS 服务器
pub struct PluginHttpServer {
    /// 共享动态路由表
    router: Arc<RwLock<DynamicRouter>>,
    /// 通用 SSE 事件
    sse_tx: broadcast::Sender<SseEvent>,
    /// WebSocket live 事件（web-monitor 用）
    ws_live_tx: broadcast::Sender<Vec<u8>>,
    /// 房间 SSE 事件（web-monitor 用）
    room_sse_tx: broadcast::Sender<SseEvent>,
    port: u16,
}

impl PluginHttpServer {
    pub fn new(port: u16) -> Self {
        let (sse_tx, _) = broadcast::channel(256);
        let (ws_live_tx, _) = broadcast::channel(256);
        let (room_sse_tx, _) = broadcast::channel(256);
        Self {
            router: Arc::new(RwLock::new(DynamicRouter::new())),
            sse_tx,
            ws_live_tx,
            room_sse_tx,
            port,
        }
    }

    pub fn sse_sender(&self) -> broadcast::Sender<SseEvent> {
        self.sse_tx.clone()
    }

    /// WebSocket live 事件发送者（用于推送 Touches/Judges）
    pub fn ws_live_sender(&self) -> broadcast::Sender<Vec<u8>> {
        self.ws_live_tx.clone()
    }

    /// 房间 SSE 事件发送者（web-monitor /rooms/listen 用）
    pub fn room_sse_sender(&self) -> broadcast::Sender<SseEvent> {
        self.room_sse_tx.clone()
    }

    /// 注册 HTTP 路由（异步）
    pub async fn register_route(&self, path: &str, handler: HttpHandler) {
        self.router.write().await.add(path.to_string(), handler);
        info!("registered HTTP route: {path}");
    }

    /// 注册 HTTP 路由（同步，用于 init 等非 async 环境）
    pub fn register_route_sync(&self, path: &str, handler: HttpHandler) {
        let path = path.to_string();
        if let Ok(mut routes) = self.router.try_write() {
            routes.add(path.clone(), handler);
            info!("registered HTTP route: {path}");
        } else {
            // 极罕见 — 仅发生在另一个写入正在进行时。异步重试。
            let router = Arc::clone(&self.router);
            let path = path.clone();
            tokio::spawn(async move {
                router.write().await.add(path.clone(), handler);
                info!("registered HTTP route (deferred): {path}");
            });
        }
    }

    /// 广播 SSE 事件
    pub fn broadcast(&self, event_type: &str, data: &str) {
        let _ = self.sse_tx.send(SseEvent {
            event_type: event_type.to_string(),
            data: data.to_string(),
        });
    }

    /// 启动服务器
    pub async fn start(&self, server: Arc<PlusServerState>) {
        let router = Arc::clone(&self.router);
        let sse_tx = self.sse_tx.clone();
        let ws_live_tx = self.ws_live_tx.clone();
        let room_sse_tx = self.room_sse_tx.clone();
        // 共享 SSE 发送器给 server state（供 session.rs 广播 RoomEvent）
        let (sse_str_tx, _) = tokio::sync::broadcast::channel::<String>(256);
        *server.room_sse_tx.write().await = Some(sse_str_tx.clone());
        // 转发 SseEvent → String SSE
        let mut rx = room_sse_tx.subscribe();
        tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                let _ = sse_str_tx.send(serde_json::json!({"type": ev.event_type, "data": ev.data}).to_string());
            }
        });
        let state = Arc::new(HttpAppState { router, sse_tx, ws_live_tx, room_sse_tx });

        let app = Router::new()
            // 通用 SSE 端点
            .route("/api/events", axum::routing::get(sse_handler))
            // 房间 SSE 端点（web-monitor 兼容）
            .route("/rooms/listen", axum::routing::get(room_sse_handler))
            // WebSocket 实时监测（web-monitor 兼容）
            .route("/ws/live", axum::routing::get(ws_live_handler))
            // 动态路由 — 所有其他请求通过匹配派发
            .route("/{*path}", any(dynamic_handler))
            .layer(CorsLayer::permissive())
            .with_state(state);

        let addr = format!("0.0.0.0:{}", self.port);
        info!("Plugin HTTP/SSE on http://{}", addr);
        let listener = match tokio::net::TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(e) => { error!("Bind plugin HTTP: {e}"); return; }
        };
        if let Err(e) = axum::serve(listener, app).await {
            error!("Plugin HTTP server: {e}");
        }
    }
}

/// 所有注册路由的 catch-all 处理器
async fn dynamic_handler(
    axum::extract::State(st): axum::extract::State<Arc<HttpAppState>>,
    method: Method,
    uri: Uri,
    body: Option<axum::extract::Json<Value>>,
) -> impl IntoResponse {
    // 尝试从动态路由表查找匹配
    let handler = {
        let guard = st.router.read().await;
        guard.resolve(&method, &uri)
    };

    match handler {
        Some((h, params)) => {
            let request_body = body.map(|j| j.0);
            // 在 blocking 线程中运行处理器
            let result = tokio::task::spawn_blocking(move || {
                h(request_body, params)
            }).await;

            match result {
                Ok(Ok(json)) => (StatusCode::OK, Json(json)).into_response(),
                Ok(Err((code, msg))) => {
                    let err = serde_json::json!({"error": msg});
                    (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(err)).into_response()
                }
                Err(_) => {
                    let err = serde_json::json!({"error": "handler panicked"});
                    (StatusCode::INTERNAL_SERVER_ERROR, Json(err)).into_response()
                }
            }
        }
        None => {
            let err = serde_json::json!({"error": format!("route not found: {}", uri.path())});
            (StatusCode::NOT_FOUND, Json(err)).into_response()
        }
    }
}

/// 桥接：将 api::HttpHandleInner 连接到 PluginHttpServer
pub struct HttpHandleBridge(pub Arc<PluginHttpServer>);

impl api::HttpHandleInner for HttpHandleBridge {
    fn register(&self, path: &str, handler: api::HttpHandler) {
        self.0.register_route_sync(path, handler);
    }
}

/// Axum 共享状态
struct HttpAppState {
    router: Arc<RwLock<DynamicRouter>>,
    sse_tx: broadcast::Sender<SseEvent>,
    ws_live_tx: broadcast::Sender<Vec<u8>>,
    room_sse_tx: broadcast::Sender<SseEvent>,
}

// ── 通用 SSE 端点 ──
async fn sse_handler(
    axum::extract::State(st): axum::extract::State<Arc<HttpAppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = st.sse_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(ev) => Some(Ok(Event::default().event(ev.event_type).data(ev.data))),
        Err(_) => None,
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ── 房间 SSE 端点（web-monitor 兼容） ──
async fn room_sse_handler(
    axum::extract::State(st): axum::extract::State<Arc<HttpAppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = st.room_sse_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(ev) => Some(Ok(Event::default().event(ev.event_type).data(ev.data))),
        Err(_) => None,
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ── WebSocket Live 端点（web-monitor 兼容） ──
async fn ws_live_handler(
    ws: WebSocketUpgrade,
    axum::extract::State(st): axum::extract::State<Arc<HttpAppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_socket(socket, st))
}

async fn handle_ws_socket(mut socket: WebSocket, st: Arc<HttpAppState>) {
    let mut rx = st.ws_live_tx.subscribe();
    let (_tx, rx2) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();

    // 读取 WebSocket 消息（心跳处理）
    let _recv_fut = Box::pin(async {
        use tokio_stream::StreamExt as _;
        let mut stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx2);
        while let Some(_) = stream.next().await {}
    });

    loop {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(data) => {
                        if socket.send(Message::Binary(axum::body::Bytes::from(data))).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_) | Message::Ping(_) | Message::Pong(_))) => break,
                    None | Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
}
