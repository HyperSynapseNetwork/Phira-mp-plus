//! Phira-mp+ Web API 插件 — 独立项目
//!
//! 通过中央 HTTP/SSE 服务器提供 REST API 查询房间信息。
//! 不在需要自己启动 HTTP 服务器，所有路由注册到 PluginHttpServer。

mod api;

use phira_mp_plus_server::plugin::{self, PluginEvent, PluginInfo, native::NativePlugin};
use phira_mp_plus_server::server::PlusServerState;
use std::sync::Arc;
use tracing::info;

/// Web API 插件
pub struct WebApiPlugin {
    state: Arc<PlusServerState>,
}

impl WebApiPlugin {
    /// 创建插件实例（工厂方法）
    pub fn create(state: Arc<PlusServerState>) -> Box<dyn NativePlugin> {
        Box::new(WebApiPlugin { state })
    }
}

impl NativePlugin for WebApiPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "webapi".to_string(),
            version: "0.1.0".to_string(),
            author: "Phira-mp+".to_string(),
            description: "REST API 查询房间信息（中央 HTTP 服务器）".to_string(),
        }
    }

    fn init(&mut self, ctx: &plugin::native::PluginContext) -> Result<(), String> {
        info!("Web API plugin registering routes...");

        let http = ctx.http.as_ref().ok_or("HTTP server not available")?;
        let state = Arc::clone(&self.state);

        // GET /api/rooms/info — 房间列表
        let s = Arc::clone(&state);
        http.register_route_sync("/api/rooms/info", Arc::new(move |_, _| {
            api::rooms_info(&s)
        }));

        // GET /api/rooms/info/<name> — 指定房间
        let s = Arc::clone(&state);
        http.register_route_sync("/api/rooms/info/<name>", Arc::new(move |_, params| {
            api::room_by_name(&s, params.get(0).map(|s| s.as_str()).unwrap_or(""))
        }));

        // GET /api/rooms/user/<user_id> — 用户所在房间
        let s = Arc::clone(&state);
        http.register_route_sync("/api/rooms/user/<user_id>", Arc::new(move |_, params| {
            let uid: i32 = params.get(0).and_then(|p| p.parse().ok()).unwrap_or(0);
            api::room_by_user(&s, uid)
        }));

        info!("Web API plugin registered 3 routes on central HTTP server");
        Ok(())
    }

    fn cleanup(&mut self) {
        info!("Web API plugin cleaned up");
    }

    fn on_event(&self, _ctx: &plugin::native::PluginContext, _event: &PluginEvent) -> Vec<String> {
        // 通过中央 HTTP 服务器的 SSE 总线广播事件
        // 这里可以直接访问 PluginContext，但在 on_event 中 ctx 的 http 可能不为 None
        // 因为我们只需要 SSE 发送器，所以需要在插件中持有它
        // 该功能在下一步实现
        vec![]
    }
}
