//! Phira-mp+ Web Monitor 插件
//!
//! 兼容 .phira-web-monitor 的 HTTP/SSE/WS API，使前端可直接连接服务端。

use phira_mp_plus_server_api::{
    NativePlugin, PluginContext, PluginEvent, PluginInfo,
};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tracing::info;

// ── 访问记录 ──

struct VisitedUsers {
    users: Mutex<HashSet<i32>>,
}

impl VisitedUsers {
    fn new() -> Self {
        Self { users: Mutex::new(HashSet::new()) }
    }
    fn add(&self, uid: i32) {
        let _ = self.users.lock().map(|mut g| { g.insert(uid); });
    }
    fn list(&self) -> Vec<i32> {
        self.users.lock().map(|g| {
            let mut list: Vec<i32> = g.iter().copied().collect();
            list.sort();
            list
        }).unwrap_or_default()
    }
    fn count(&self) -> usize {
        self.users.lock().map(|g| g.len()).unwrap_or(0)
    }
}

// ── 插件主结构 ──

pub struct WebMonitorPlugin {
    visited: Arc<VisitedUsers>,
}

impl WebMonitorPlugin {
    pub fn create() -> Box<dyn NativePlugin> {
        Box::new(Self {
            visited: Arc::new(VisitedUsers::new()),
        })
    }
}

impl NativePlugin for WebMonitorPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "web-monitor".to_string(),
            version: "0.1.0".to_string(),
            author: "Phira-mp+".to_string(),
            description: "Web 监测 HTTP/SSE/WS API".to_string(),
        }
    }

    fn init(&mut self, ctx: &PluginContext) -> Result<(), String> {
        info!("WebMonitor plugin initializing...");
        let state = ctx.state.clone();
        let visited = self.visited.clone();

        if let Some(http) = &ctx.http {
            // GET /rooms/info
            let st = state.clone();
            http.register_route("/rooms/info", Arc::new(move |_, _| {
                let sq = match &st {
                    Some(s) => s,
                    None => return Err((503, "state query unavailable".to_string())),
                };
                match sq.call("rooms.list", &[]) {
                    Ok(v) => Ok(v),
                    Err(e) => Err((500, e)),
                }
            }));
            info!("registered /rooms/info");

            // GET /rooms/info/<id>
            let st = state.clone();
            http.register_route("/rooms/info/<id>", Arc::new(move |_, params| {
                let name = params.first().map(|s| &s[..]).unwrap_or("");
                let sq = match &st {
                    Some(s) => s,
                    None => return Err((503, "state query unavailable".to_string())),
                };
                match sq.call("rooms.by_name", &[serde_json::json!(name)]) {
                    Ok(v) => Ok(v),
                    Err(e) => Err((404, e)),
                }
            }));
            info!("registered /rooms/info/<id>");

            // GET /rooms/user/<id>
            let st = state.clone();
            http.register_route("/rooms/user/<id>", Arc::new(move |_, params| {
                let uid: i32 = params.first().and_then(|p| p.parse().ok()).unwrap_or(0);
                let sq = match &st {
                    Some(s) => s,
                    None => return Err((503, "state query unavailable".to_string())),
                };
                match sq.call("rooms.by_user", &[serde_json::json!(uid)]) {
                    Ok(v) => Ok(v),
                    Err(e) => Err((404, e)),
                }
            }));
            info!("registered /rooms/user/<id>");

            // GET /visited
            let vis = visited.clone();
            http.register_route("/visited", Arc::new(move |_, _| {
                let data: Vec<Value> = vis.list().iter().map(|id| {
                    serde_json::json!({"phira_id": id})
                }).collect();
                Ok(serde_json::json!({"success": true, "data": data, "count": vis.count()}))
            }));
            info!("registered /visited");

            // GET /chart/<id>
            let _st = state.clone();
            http.register_route("/chart/<id>", Arc::new(move |_, params| {
                let chart_id = params.first().map(|s| &s[..]).unwrap_or("0");
                let url = format!("https://phira.5wyxi.com/chart/{chart_id}");
                match reqwest::blocking::get(&url)
                    .and_then(|r| r.text())
                {
                    Ok(body) => {
                        match serde_json::from_str::<Value>(&body) {
                            Ok(json) => Ok(json),
                            Err(_) => Ok(serde_json::json!({"raw": body})),
                        }
                    }
                    Err(e) => Err((502, format!("chart proxy failed: {e}"))),
                }
            }));
            info!("registered /chart/<id>");

            // POST /auth/login
            http.register_route("/auth/login", Arc::new(move |body, _| {
                let email = body.as_ref().and_then(|b| b.get("email").and_then(|v| v.as_str())).unwrap_or("");
                let password = body.as_ref().and_then(|b| b.get("password").and_then(|v| v.as_str())).unwrap_or("");
                let client = reqwest::blocking::Client::new();
                match client.post("https://phira.5wyxi.com/login")
                    .json(&serde_json::json!({"email": email, "password": password}))
                    .send()
                {
                    Ok(resp) => {
                        match resp.json::<Value>() {
                            Ok(json) => Ok(json),
                            Err(e) => Err((502, format!("auth proxy parse failed: {e}"))),
                        }
                    }
                    Err(e) => Err((502, format!("auth proxy failed: {e}"))),
                }
            }));
            info!("registered /auth/login");

            // GET /auth/me
            http.register_route("/auth/me", Arc::new(move |body, _| {
                let token = body.as_ref()
                    .and_then(|b| b.get("token").and_then(|v| v.as_str()))
                    .unwrap_or("");
                let client = reqwest::blocking::Client::new();
                match client.get("https://phira.5wyxi.com/me")
                    .header("Authorization", format!("Bearer {token}"))
                    .send()
                {
                    Ok(resp) => {
                        match resp.json::<Value>() {
                            Ok(json) => Ok(json),
                            Err(e) => Err((502, format!("auth me parse failed: {e}"))),
                        }
                    }
                    Err(e) => Err((502, format!("auth me failed: {e}"))),
                }
            }));
            info!("registered /auth/me");
        }

        info!("WebMonitor plugin initialized");
        Ok(())
    }

    fn on_event(&self, _ctx: &PluginContext, event: &PluginEvent) -> Vec<String> {
        match event {
            PluginEvent::UserConnect { user_id, .. } => {
                self.visited.add(*user_id);
            }
            _ => {}
        }
        vec![]
    }

    fn cleanup(&mut self) {
        info!("WebMonitor plugin cleaned up");
    }
}
