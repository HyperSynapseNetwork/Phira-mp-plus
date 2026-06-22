use phira_mp_plus_server_api::{
    NativePlugin, PluginContext, PluginEvent, PluginInfo, ServerStateQuery,
};
use std::sync::Arc;
use serde_json::Value;
use tracing::info;

pub struct WebApiPlugin;

impl WebApiPlugin {
    pub fn create() -> Box<dyn NativePlugin> {
        Box::new(WebApiPlugin)
    }
}

impl NativePlugin for WebApiPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "room-info-web-api".to_string(),
            version: "0.1.0".to_string(),
            author: "Phira-mp+".to_string(),
            description: "room-info-web-api — REST API 查询房间信息".to_string(),
        }
    }

    fn init(&mut self, ctx: &PluginContext) -> Result<(), String> {
        info!("Web API plugin registering routes...");
        let http = ctx.http.as_ref().ok_or("HTTP server not available")?;
        let state = ctx.state.as_ref().ok_or("server state not available")?;

        // GET /api/rooms/info
        let s = state.clone();
        http.register_route("/api/rooms/info", Arc::new(move |_, _| {
            query_ok(&s, "rooms.list", &[])
        }));

        // GET /api/rooms/info/<name>
        let s = state.clone();
        http.register_route("/api/rooms/info/<name>", Arc::new(move |_, params| {
            let name = params.first().map(|s| s.as_str()).unwrap_or("");
            query_ok(&s, "rooms.by_name", &[Value::String(name.to_string())])
        }));

        // GET /api/rooms/user/<user_id>
        let s = state.clone();
        http.register_route("/api/rooms/user/<user_id>", Arc::new(move |_, params| {
            let uid = params.first().and_then(|p| p.parse::<i32>().ok()).unwrap_or(0);
            query_ok(&s, "rooms.by_user", &[Value::Number(serde_json::Number::from(uid))])
        }));

        info!("Web API plugin registered 3 routes");
        Ok(())
    }

    fn on_event(&self, _ctx: &PluginContext, _event: &PluginEvent) -> Vec<String> {
        vec![]
    }

    fn cleanup(&mut self) {
        info!("Web API plugin cleaned up");
    }
}

fn query_ok(state: &ServerStateQuery, method: &str, args: &[Value]) -> Result<Value, (u16, String)> {
    state.call(method, args).map_err(|e| (500u16, e))
}
