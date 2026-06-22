use phira_mp_plus_server_api::{
    NativePlugin, PluginContext, PluginEvent, PluginInfo, DatabaseHandle,
};
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{info, warn};

const PAGE_SIZE: usize = 20;

pub struct PlayerTracker {
    table_ready: AtomicBool,
}

impl PlayerTracker {
    pub fn create() -> Box<dyn NativePlugin> {
        Box::new(PlayerTracker { table_ready: AtomicBool::new(false) })
    }
}

impl NativePlugin for PlayerTracker {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "player-tracker".to_string(),
            version: "0.1.0".to_string(),
            author: "Phira-mp+".to_string(),
            description: "记录所有游玩过服务器的玩家 Phira ID".to_string(),
        }
    }

    fn init(&mut self, ctx: &PluginContext) -> Result<(), String> {
        info!("PlayerTracker plugin initializing...");

        let db = ctx.db.as_ref();
        if let Some(db) = db {
            if db.query("CREATE TABLE IF NOT EXISTS players (
                id SERIAL PRIMARY KEY,
                phira_id INTEGER NOT NULL UNIQUE,
                first_seen TIMESTAMP DEFAULT NOW(),
                last_seen TIMESTAMP DEFAULT NOW(),
                play_count INTEGER DEFAULT 1
            )", &[]).is_ok() {
                self.table_ready.store(true, Ordering::SeqCst);
                info!("PlayerTracker DB table ready");
            } else {
                warn!("PlayerTracker: failed to create table");
            }
        } else {
            warn!("PlayerTracker: no database, data won't be recorded");
        }

        // Web API — 不需要 DB 也能注册路由
        if let Some(http) = &ctx.http {
            // /api/players/count
            let d = db.cloned();
            http.register_route("/api/players/count", Arc::new(move |_, _| {
                let db = d.as_ref().ok_or((500u16, "DB unavailable".into()))?;
                let r = db.query("SELECT COUNT(*)::bigint as cnt FROM players", &[])
                    .map_err(|e| (500u16, e))?;
                let cnt = r.rows.first().and_then(|row| row.first())
                    .and_then(|v| v.as_u64()).unwrap_or(0);
                Ok(serde_json::json!({"count": cnt}))
            }));
            info!("registered /api/players/count");

            // /api/players/list
            let d = db.cloned();
            http.register_route("/api/players/list", Arc::new(move |_, params| {
                let db = d.as_ref().ok_or((500u16, "DB unavailable".into()))?;
                let page = params.first().and_then(|p| p.parse::<u64>().ok()).unwrap_or(1).max(1);
                let offset = (page - 1) * PAGE_SIZE as u64;
                let r = db.query(
                    "SELECT phira_id, first_seen, last_seen, play_count
                     FROM players ORDER BY last_seen DESC LIMIT $1 OFFSET $2",
                    &[Value::Number((PAGE_SIZE as u64).into()), Value::Number(offset.into())],
                ).map_err(|e| (500u16, e))?;
                let players: Vec<Value> = r.rows.iter().map(|row| serde_json::json!({
                    "phira_id": row.get(0), "first_seen": row.get(1),
                    "last_seen": row.get(2), "play_count": row.get(3),
                })).collect();
                Ok(serde_json::json!({"page": page, "page_size": PAGE_SIZE, "players": players}))
            }));
            info!("registered /api/players/list");
        }

        info!("PlayerTracker plugin initialized");
        Ok(())
    }

    fn on_event(&self, ctx: &PluginContext, event: &PluginEvent) -> Vec<String> {
        if let PluginEvent::GameEnd { user_id, .. } = event {
            if self.table_ready.load(Ordering::SeqCst) {
                if let Some(db) = ctx.db.as_ref() {
                    let _ = db.query(
                        "INSERT INTO players (phira_id) VALUES ($1)
                         ON CONFLICT(phira_id) DO UPDATE SET
                            last_seen = NOW(),
                            play_count = players.play_count + 1",
                        &[Value::Number(serde_json::Number::from(*user_id))],
                    );
                }
            }
        }
        vec![]
    }

    fn cleanup(&mut self) {
        info!("PlayerTracker plugin cleaned up");
    }
}
