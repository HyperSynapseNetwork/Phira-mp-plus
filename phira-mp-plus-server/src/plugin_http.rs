//! HTTP extension routes and the canonical Runtime v2 SSE stream.

mod router;
mod sse;
mod websocket;

use crate::plugin::PluginManager;
use crate::server::PlusServerState;
use axum::{
    extract::Extension,
    http::{header, HeaderName, HeaderValue, Method, StatusCode, Uri},
    response::{
        sse::{Event as AxumSseEvent, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{any, get},
    Json, Router,
};
use futures::{stream, Stream, StreamExt};
use phira_mp_plus_server_api as api;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, RwLock as StdRwLock};
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use tower_http::cors::CorsLayer;
use tracing::{error, info};

pub use router::HttpHandler;
use router::{normalize_route_path, DynamicRouter};
pub use sse::{SseEvent, SseHub};

pub struct PluginHttpServer {
    router: Arc<StdRwLock<DynamicRouter>>,
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
            router: Arc::new(StdRwLock::new(DynamicRouter::default())),
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
        self.register_route_sync(path, handler);
    }

    /// Register an SSE stream endpoint backed by a plugin.
    /// The host manages the SSE connection; the plugin translates events
    /// via on_api("sse:translate", &[json!(event)]) callbacks.
    pub async fn register_sse_stream(&self, path: &str, config: SseStreamConfig) {
        let path = normalize_route_path(path);
        self.sse_streams.write().await.insert(path.clone(), config);
        info!(path = %path, "SSE stream registered (plugin-backed)");
    }

    pub fn register_route_sync(&self, path: &str, handler: HttpHandler) {
        let mut router = self.router.write().unwrap_or_else(|err| err.into_inner());
        let registered_path = router.add(path, handler);
        info!(path = %registered_path, "HTTP route registered");
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

        // Build Router<()> with all routes (static + dynamically registered SSE streams).
        // State is passed via Extension, so Router stays Router<()> and into_make_service() works.
        let mut app = Router::new()
            .route("/api/events", get(general_sse_handler))
            .route("/api/ws", get(websocket::handler))
            .route("/{*path}", any(dynamic_handler))
            .layer(CorsLayer::permissive());

        // Dynamically add SSE stream routes from plugin registrations.
        for (path, _) in self.sse_streams.read().await.iter() {
            info!(path, "adding SSE stream route (plugin-backed)");
            app = app.route(path, get(plugin_sse_handler));
        }

        // Wrap entire router with state so handlers can access HttpAppState via Extension.
        app = app.layer(Extension(state));

        // Direct internal HTTP port (does not trust forwarded client headers)
        let address = format!("0.0.0.0:{}", self.port);
        let listener = match tokio::net::TcpListener::bind(&address).await {
            Ok(listener) => listener,
            Err(err) => {
                error!(%address, ?err, "failed to bind HTTP server");
                return;
            }
        };
        info!(%address, "HTTP server started (direct)");

        // Trusted-forwarded-header compatibility port. This is not HAProxy
        // PROXY v1/v2; it trusts X-Forwarded-For only behind PPB/a trusted proxy.
        if self.proxy_port > 0 && self.proxy_port != self.port {
            let proxy_addr = format!("0.0.0.0:{}", self.proxy_port);
            let proxy_listener = match tokio::net::TcpListener::bind(&proxy_addr).await {
                Ok(l) => l,
                Err(err) => {
                    error!(%proxy_addr, ?err, "failed to bind trusted-forwarded-header HTTP server");
                    return;
                }
            };
            let proxy_app = app
                .clone()
                .layer(crate::proxy_protocol::TrustForwardedForLayer);
            tokio::spawn(async move {
                info!(%proxy_addr, "HTTP server started (trusted X-Forwarded-For compatibility port)");
                if let Err(err) = axum::serve(proxy_listener, proxy_app.into_make_service()).await {
                    error!(?err, "trusted-forwarded-header HTTP server stopped");
                }
            });
        }

        if let Err(err) = axum::serve(listener, app.into_make_service()).await {
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
    router: Arc<StdRwLock<DynamicRouter>>,
    events: Arc<SseHub>,
    plugin_manager: Arc<PluginManager>,
    sse_streams: Arc<RwLock<HashMap<String, SseStreamConfig>>>,
}

async fn general_sse_handler(Extension(state): Extension<Arc<HttpAppState>>) -> Response {
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
/// so the plugin can transform it into the HSNPhira v1/v2 format.
/// Each registered SSE stream has its own route; the path is read from the request URI.
async fn plugin_sse_handler(Extension(state): Extension<Arc<HttpAppState>>, uri: Uri) -> Response {
    plugin_sse_response(state, uri).await
}

async fn plugin_sse_response(state: Arc<HttpAppState>, uri: Uri) -> Response {
    let path = normalize_route_path(uri.path());
    let Some(config) = state.sse_streams.read().await.get(&path).cloned() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("SSE stream not found: {path}")})),
        )
            .into_response();
    };
    let pm = Arc::clone(&state.plugin_manager);
    let rx = state.events.subscribe_general();

    let stream_name = path.trim_start_matches('/');
    let ready = SseEvent::new(
        "ready",
        serde_json::json!({"stream": stream_name, "version": env!("CARGO_PKG_VERSION")})
            .to_string(),
    );

    let stream: sse::EventStream = Box::pin(
        stream::once(async move { Ok(ready.into_axum()) })
            .chain(plugin_sse_translate(rx, pm, config)),
    );

    sse_response(stream, Duration::from_secs(15))
}

/// Translate matching SseHub events through the plugin's
/// on_api("sse:translate", …) callback.
fn plugin_sse_translate(
    rx: broadcast::Receiver<SseEvent>,
    pm: Arc<PluginManager>,
    config: SseStreamConfig,
) -> impl Stream<Item = Result<AxumSseEvent, Infallible>> {
    let plugin = config.plugin;
    let event_types = config.event_types;

    BroadcastStream::new(rx).filter_map(move |message| {
        let plugin = plugin.clone();
        let event_types = event_types.clone();
        let pm = Arc::clone(&pm);
        async move {
            let event = match message {
                Ok(e) => e,
                Err(BroadcastStreamRecvError::Lagged(skipped)) => {
                    return Some(Ok(AxumSseEvent::default()
                        .event("stream_lagged")
                        .data(json!({"skipped": skipped}).to_string())))
                }
            };
            if !event_types.is_empty()
                && !event_types
                    .iter()
                    .any(|allowed| event_type_matches(allowed, &event.event_type))
            {
                return None;
            }

            let payload = json!({"event_type": event.event_type, "data": event.data});
            match pm
                .call_plugin_api(&plugin, "sse:translate", vec![payload])
                .await
            {
                Ok(result) if result.is_null() => None,
                Ok(result) => {
                    let event_type = result
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("event");
                    let data = serde_json::to_string(&result).unwrap_or_default();
                    Some(Ok(AxumSseEvent::default().event(event_type).data(data)))
                }
                Err(err) => {
                    error!(plugin = %plugin, ?err, "plugin SSE translation failed");
                    None
                }
            }
        }
    })
}

fn event_type_matches(configured: &str, actual: &str) -> bool {
    fn compact(value: &str) -> String {
        value
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    }

    let configured = compact(configured);
    let actual_compact = compact(actual);
    if configured == actual_compact {
        return true;
    }

    // Compatibility with the historical documentation names RoomCreate,
    // RoomJoin, RoomLeave and RoomUpdate.
    let legacy_room_alias = actual
        .strip_suffix("_room")
        .map(|verb| format!("room{}", compact(verb)));
    legacy_room_alias.as_deref() == Some(configured.as_str())
}

async fn dynamic_handler(
    Extension(state): Extension<Arc<HttpAppState>>,
    method: Method,
    uri: Uri,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    // SSE registrations may be added after the Axum router has started (for
    // example after a plugin reload). Resolve them from the live registry.
    if method == Method::GET
        && state
            .sse_streams
            .read()
            .await
            .contains_key(&normalize_route_path(uri.path()))
    {
        return plugin_sse_response(Arc::clone(&state), uri).await;
    }

    let route = state
        .router
        .read()
        .unwrap_or_else(|err| err.into_inner())
        .resolve(&method, &uri);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_server() -> PluginHttpServer {
        PluginHttpServer::new(0, 0, Arc::new(super::SseHub::new()))
    }

    #[test]
    fn sse_stream_registration() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let server = make_server();
            let config = SseStreamConfig {
                plugin: "test-plugin".to_string(),
                event_types: vec!["RoomCreate".to_string(), "RoomJoin".to_string()],
            };
            server.register_sse_stream("/test/stream", config).await;
            let streams = server.sse_streams.read().await;
            let entry = streams.get("/test/stream").unwrap();
            assert_eq!(entry.plugin, "test-plugin");
            assert_eq!(entry.event_types, vec!["RoomCreate", "RoomJoin"]);
        });
    }

    #[test]
    fn sse_stream_normalizes_missing_leading_slash() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let server = make_server();
            let config = SseStreamConfig {
                plugin: "test-plugin".to_string(),
                event_types: vec![],
            };
            server.register_sse_stream("test/stream", config).await;
            assert!(server.sse_streams.read().await.contains_key("/test/stream"));
        });
    }

    #[test]
    fn sse_stream_overwrites_existing() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let server = make_server();
            let c1 = SseStreamConfig {
                plugin: "plugin-a".to_string(),
                event_types: vec![],
            };
            let c2 = SseStreamConfig {
                plugin: "plugin-b".to_string(),
                event_types: vec![],
            };
            server.register_sse_stream("/test", c1).await;
            server.register_sse_stream("/test", c2).await;
            let streams = server.sse_streams.read().await;
            assert_eq!(streams.get("/test").unwrap().plugin, "plugin-b");
        });
    }

    #[test]
    fn sse_event_type_filter_accepts_wire_and_legacy_names() {
        assert!(event_type_matches("create_room", "create_room"));
        assert!(event_type_matches("CreateRoom", "create_room"));
        assert!(event_type_matches("RoomCreate", "create_room"));
        assert!(!event_type_matches("join_room", "create_room"));
    }

    #[test]
    fn sse_stream_empty_registry() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let server = make_server();
            assert!(server.sse_streams.read().await.is_empty());
        });
    }
}
