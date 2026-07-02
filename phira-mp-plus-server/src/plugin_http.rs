//! HTTP extension routes and the canonical Runtime v2 SSE stream.

mod router;
mod sse;

use crate::server::PlusServerState;
use axum::{
    extract::State,
    http::{header, HeaderName, HeaderValue, Method, StatusCode, Uri},
    response::{
        sse::{KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{any, get},
    Json, Router,
};
use phira_mp_plus_server_api as api;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};
use tower_http::cors::CorsLayer;
use tracing::{error, info};

use router::DynamicRouter;
pub use router::HttpHandler;
pub use sse::{SseEvent, SseHub};

pub struct PluginHttpServer {
    router: Arc<RwLock<DynamicRouter>>,
    events: Arc<SseHub>,
    port: u16,
}

impl PluginHttpServer {
    pub fn new(port: u16, events: Arc<SseHub>) -> Self {
        Self {
            router: Arc::new(RwLock::new(DynamicRouter::default())),
            events,
            port,
        }
    }

    pub fn sse_sender(&self) -> broadcast::Sender<SseEvent> {
        self.events.general_sender()
    }

    pub async fn register_route(&self, path: &str, handler: HttpHandler) {
        self.router.write().await.add(path.to_string(), handler);
        info!(path, "HTTP route registered");
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

    pub async fn start(&self, _server: Arc<PlusServerState>) {
        let state = Arc::new(HttpAppState {
            router: Arc::clone(&self.router),
            events: Arc::clone(&self.events),
        });
        let app = Router::new()
            .route("/api/events", get(general_sse_handler))
            .route("/{*path}", any(dynamic_handler))
            .layer(CorsLayer::permissive())
            .with_state(state);

        let address = format!("0.0.0.0:{}", self.port);
        let listener = match tokio::net::TcpListener::bind(&address).await {
            Ok(listener) => listener,
            Err(err) => {
                error!(%address, ?err, "failed to bind HTTP server");
                return;
            }
        };
        info!(%address, "HTTP server started");
        if let Err(err) = axum::serve(listener, app).await {
            error!(?err, "HTTP server stopped unexpectedly");
        }
    }
}

pub struct HttpHandleBridge(pub Arc<PluginHttpServer>);

impl api::HttpHandleInner for HttpHandleBridge {
    fn register(&self, path: &str, handler: api::HttpHandler) {
        self.0.register_route_sync(path, handler);
    }
}

pub(super) struct HttpAppState {
    router: Arc<RwLock<DynamicRouter>>,
    events: Arc<SseHub>,
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
