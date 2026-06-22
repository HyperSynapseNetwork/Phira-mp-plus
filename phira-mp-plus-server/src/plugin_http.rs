//! 中央 HTTP/SSE 服务器
//!
//! 插件通过 PluginContext 注册路由和推送 SSE 事件，
//! 统一在单个端口暴露，无需每个插件自建 HTTP 服务。

use crate::server::PlusServerState;
use axum::{
    Router, Json,
    http::{StatusCode, Uri},
    response::sse::{Event, KeepAlive, Sse},
    response::IntoResponse,
    routing::{any, get},
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

/// 中央 HTTP/SSE 服务器
pub struct PluginHttpServer {
    routes: RwLock<Vec<RouteEntry>>,
    sse_tx: broadcast::Sender<SseEvent>,
    port: u16,
}

impl PluginHttpServer {
    pub fn new(port: u16) -> Self {
        let (sse_tx, _) = broadcast::channel(256);
        Self {
            routes: RwLock::new(Vec::new()),
            sse_tx,
            port,
        }
    }

    pub fn sse_sender(&self) -> broadcast::Sender<SseEvent> {
        self.sse_tx.clone()
    }

    /// 注册 HTTP 路由（异步，在 async 上下文中调用）
    pub async fn register_route(&self, path: &str, handler: HttpHandler) {
        self.routes.write().await.push(RouteEntry {
            path: path.to_string(),
            handler,
        });
        info!("registered HTTP route: {path}");
    }

    /// 注册 HTTP 路由（同步，用于 NativePlugin::init 等非 async 环境）
    pub fn register_route_sync(&self, path: &str, handler: HttpHandler) {
        let path = path.to_string();
        if let Ok(mut routes) = self.routes.try_write() {
            routes.push(RouteEntry { path: path.clone(), handler });
            info!("registered HTTP route: {path}");
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
        let sse_tx = self.sse_tx.clone();
        let state = Arc::new(HttpAppState { server, sse_tx });

        // 构建插件路由转发器
        let routes = self.routes.read().await;
        let mut router = Router::new();
        for entry in routes.iter() {
            let handler = Arc::clone(&entry.handler);
            let path = entry.path.clone();
            // 提取路径参数（<param> → {param}）
            let axum_path = path.replace('<', "{").replace('>', "}");
            let axum_path_for_route = axum_path.clone();
            router = router.route(
                &axum_path_for_route,
                any(move |uri: Uri, body: Option<axum::extract::Json<Value>>| {
                    let h = Arc::clone(&handler);
                    let axum_path = axum_path.clone();
                    async move {
                        let params = extract_path_params(&axum_path, &uri.path());
                        let body = body.map(|j| j.0);
                        // 在 blocking 线程中运行处理器（避免 tokio RwLock blocking_read panic）
                        let result = tokio::task::spawn_blocking(move || {
                            h(body, params)
                        }).await.unwrap_or(Err((500, "handler panicked".to_string())));
                        match result {
                            Ok(json) => (StatusCode::OK, Json(json)).into_response(),
                            Err((code, msg)) => {
                                let err = serde_json::json!({"error": msg});
                                (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(err)).into_response()
                            }
                        }
                    }
                }),
            );
        }
        drop(routes);

        // 添加 SSE 端点
        router = router.route("/api/events", get(sse_handler));

        let router = router
            .layer(CorsLayer::permissive())
            .with_state(state);

        let addr = format!("0.0.0.0:{}", self.port);
        info!("Plugin HTTP/SSE on http://{}", addr);
        let listener = match tokio::net::TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(e) => { error!("Bind plugin HTTP: {e}"); return; }
        };
        if let Err(e) = axum::serve(listener, router).await {
            error!("Plugin HTTP server: {e}");
        }
    }
}

/// 从 URI 路径提取命名参数
fn extract_path_params(pattern: &str, path: &str) -> Vec<String> {
    let p_segs: Vec<&str> = pattern.split('/').collect();
    let u_segs: Vec<&str> = path.split('/').collect();
    let mut params = Vec::new();
    for (p, u) in p_segs.iter().zip(u_segs.iter()) {
        if p.starts_with('{') && p.ends_with('}') {
            params.push(u.to_string());
        }
    }
    params
}

/// 桥接：将 api::HttpHandleInner 连接到 PluginHttpServer
pub struct HttpHandleBridge(pub Arc<PluginHttpServer>);

impl api::HttpHandleInner for HttpHandleBridge {
    fn register(&self, path: &str, handler: api::HttpHandler) {
        self.0.register_route_sync(path, handler);
    }
}

/// Axum 共享状态
#[allow(dead_code)]
struct HttpAppState {
    server: Arc<PlusServerState>,
    sse_tx: broadcast::Sender<SseEvent>,
}

// SSE 端点
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
