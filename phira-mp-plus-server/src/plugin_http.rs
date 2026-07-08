//! HTTP extension routes and the canonical Runtime v2 SSE stream.

mod router;
mod sse;
mod websocket;

use crate::server::PlusServerState;
use crate::plugin::PluginManager;
use axum::{
    extract::State,
    http::{header, HeaderName, HeaderValue, Method, StatusCode, Uri},
    response::{
        sse::{Event as AxumSseEvent, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{any, get},
    Json, Router,
};
use phira_mp_plus_server_api as api;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use futures::{stream, Stream, StreamExt};
use tower_http::cors::CorsLayer;
use tracing::{error, info};

use router::DynamicRouter;
pub use router::HttpHandler;
pub use sse::{SseEvent, SseHub};

pub struct PluginHttpServer {
    router: Arc<RwLock<DynamicRouter>>,
    events: Arc<SseHub>,
    /// SSE streams registered by plugins: path → (plugin_name, event_types).
    sse_streams: Arc<RwLock<HashMap<String, SseStreamConfig>>>,
    port: u16,
    proxy_port: u16,
}

/// Configuration for a plugin-registered SSE stream.
#[derive(Clone, Debug)]
pub struct SseStreamConfig {
    pub plugin: String,
    pub event_types: Vec<String>,
}

impl PluginHttpServer {
    pub fn new(port: u16, proxy_protocol_port: u16, events: Arc<SseHub>) -> Self {
        Self {
            router: Arc::new(RwLock::new(DynamicRouter::default())),
            events,
            sse_streams: Arc::new(RwLock::new(HashMap::new())),
            port,
            proxy_port: proxy_protocol_port,
        }
    }

    pub fn sse_sender(&self) -> broadcast::Sender<SseEvent> {
        self.events.general_sender()
    }

    pub async fn register_route(&self, path: &str, handler: HttpHandler) {
        self.router.write().await.add(path.to_string(), handler);
        info!(path, "HTTP route registered");
    }

    /// Register an SSE stream endpoint backed by a plugin.
    /// The host manages the SSE connection; the plugin translates events
    /// via on_api("sse:translate", &[json!(event)]) callbacks.
    pub async fn register_sse_stream(&self, path: &str, config: SseStreamConfig) {
        self.sse_streams.write().await.insert(path.to_string(), config);
        info!(path, "SSE stream registered (plugin-backed)");
    }

    pub fn register_route_sync(&self, path: &str, handler: HttpHandler) {
        let path = path.to_string();
        if let Ok(mut router) = self.router.try_write() {
            router.add(path.clone(), handler);
            info!(path, "HTTP route registered");
            return;
        }

        let router = Arc::clone(&self.router);
        tokio::spawn(async move {
            router.write().await.add(path.clone(), handler);
            info!(path, "HTTP route registered");
        });
    }

    pub fn broadcast(&self, event_type: &str, data: &str) {
        self.events.publish(SseEvent::new(event_type, data));
    }

    pub async fn start(&self, server: Arc<PlusServerState>) {
        let state = Arc::new(HttpAppState {
            router: Arc::clone(&self.router),
            events: Arc::clone(&self.events),
            plugin_manager: Arc::clone(&server.plugin_manager),
            sse_streams: Arc::clone(&self.sse_streams),
        });
        let app = Router::new()
            .route("/api/events", get(general_sse_handler))
            .route("/api/ws", get(websocket::handler))
            .route("/newapi/rooms/listen", get(plugin_sse_handler))
            .route("/{*path}", any(dynamic_handler))
            .layer(CorsLayer::permissive())
            .with_state(state);

        // Direct HTTP port (no PROXY protocol)
        let address = format!("0.0.0.0:{}", self.port);
        let listener = match tokio::net::TcpListener::bind(&address).await {
            Ok(listener) => listener,
            Err(err) => {
                error!(%address, ?err, "failed to bind HTTP server");
                return;
            }
        };
        info!(%address, "HTTP server started (direct)");

        // PROXY protocol port — apply TrustForwardedFor middleware so
        // reverse-proxy-set X-Forwarded-For headers are trusted here.
        if self.proxy_port > 0 && self.proxy_port != self.port {
            let proxy_addr = format!("0.0.0.0:{}", self.proxy_port);
            let proxy_listener = match tokio::net::TcpListener::bind(&proxy_addr).await {
                Ok(l) => l,
                Err(err) => {
                    error!(%proxy_addr, ?err, "failed to bind PROXY protocol HTTP server");
                    return;
                }
            };
            let proxy_app = app
                .clone()
                .layer(crate::proxy_protocol::TrustForwardedForLayer);
            tokio::spawn(async move {
                info!(%proxy_addr, "HTTP server started (PROXY protocol, X-Forwarded-For trusted)");
                if let Err(err) = axum::serve(
                    proxy_listener,
                    proxy_app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .await
                {
                    error!(?err, "PROXY protocol HTTP server stopped");
                }
            });
        }

        if let Err(err) = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        {
            error!(?err, "HTTP server stopped unexpectedly");
        }
    }
}

pub struct HttpHandleBridge(pub Arc<PluginHttpServer>);

impl api::HttpHandleInner for HttpHandleBridge {
    fn register(&self, path: &str, handler: api::HttpHandler) {
        self.0.register_route_sync(path, handler);
    }

    fn register_sse(&self, path: &str, plugin: &str, event_types: &[String]) {
        let path = path.to_string();
        let config = SseStreamConfig {
            plugin: plugin.to_string(),
            event_types: event_types.to_vec(),
        };
        let server = Arc::clone(&self.0);
        tokio::spawn(async move {
            server.register_sse_stream(&path, config).await;
        });
    }
}

pub(super) struct HttpAppState {
    router: Arc<RwLock<DynamicRouter>>,
    events: Arc<SseHub>,
    plugin_manager: Arc<PluginManager>,
    sse_streams: Arc<RwLock<HashMap<String, SseStreamConfig>>>,
}

async fn general_sse_handler(State(state): State<Arc<HttpAppState>>) -> Response {
    sse_response(
        sse::general_stream(state.events.subscribe_general()),
        Duration::from_secs(15),
    )
}

fn sse_response(stream: sse::EventStream, interval: Duration) -> Response {
    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(interval).text("keep-alive"))
        .into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    response
}

/// SSE handler for plugin-registered event streams.
/// Each incoming SseEvent is forwarded to the plugin's on_api("sse:translate", …)
/// so the plugin can transform it into the HSNPhira v2 format.
async fn plugin_sse_handler(
    State(state): State<Arc<HttpAppState>>,
) -> Response {
    let config = state.sse_streams.read().await.get("/newapi/rooms/listen")
        .cloned();
    let pm = Arc::clone(&state.plugin_manager);
    let rx = state.events.subscribe_general();

    // Ready event sent on connection open.
    let ready = SseEvent::new("ready",
        serde_json::json!({"stream": "rooms/listen", "version": env!("CARGO_PKG_VERSION")}).to_string());

    let stream: sse::EventStream = Box::pin(
        stream::once(async move { Ok(ready.into_axum()) })
            .chain(plugin_sse_translate(rx, pm, config))
    );

    sse_response(stream, Duration::from_secs(15))
}

/// Translate SseHub events through the plugin's on_api("sse:translate", …)
/// and emit only the non-null translations as SSE.
fn plugin_sse_translate(
    rx: broadcast::Receiver<SseEvent>,
    pm: Arc<PluginManager>,
    config: Option<SseStreamConfig>,
) -> impl Stream<Item = Result<AxumSseEvent, Infallible>> {
    let plugin = config.as_ref().map(|c| c.plugin.clone());

    BroadcastStream::new(rx).filter_map(move |message| {
        let plugin = plugin.clone();
        let pm = Arc::clone(&pm);
        async move {
            let event = match message {
                Ok(e) => e,
                Err(BroadcastStreamRecvError::Lagged(skipped)) => return Some(Ok(
                    AxumSseEvent::default().event("stream_lagged")
                        .data(json!({"skipped": skipped}).to_string())
                )),
            };
            let plugin = match &plugin {
                Some(p) => p.clone(),
                None => return Some(Ok(event.into_axum())), // no plugin → raw
            };

            // Call plugin to translate the event
            let payload = json!({"event_type": event.event_type, "data": event.data});
            match pm.call_plugin_api(&plugin, "sse:translate", vec![payload]).await {
                Ok(result) => {
                    if result.is_null() {
                        None // plugin says skip this event
                    } else {
                        let event_type = result.get("type")
                            .and_then(|v| v.as_str()).unwrap_or("event");
                        let data = serde_json::to_string(&result).unwrap_or_default();
                        Some(Ok(AxumSseEvent::default().event(event_type).data(data)))
                    }
                }
                Err(_) => None, // plugin error → skip
            }
        }
    })
}

async fn dynamic_handler(
    State(state): State<Arc<HttpAppState>>,
    method: Method,
    uri: Uri,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let route = state.router.read().await.resolve(&method, &uri);
    let Some((handler, params)) = route else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("route not found: {}", uri.path())})),
        )
            .into_response();
    };

    match tokio::task::spawn_blocking(move || handler(body.map(|body| body.0), params)).await {
        Ok(Ok(value)) => (StatusCode::OK, Json(value)).into_response(),
        Ok(Err((status, message))) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(serde_json::json!({"error": message})),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("handler failed: {err}")})),
        )
            .into_response(),
    }
}
