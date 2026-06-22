//! Phira-mp+ 玩家记录插件
//!
//! 记录所有游玩过服务器的玩家 Phira ID，自有内存存储。

use phira_mp_plus_server_api::{
    NativePlugin, PluginContext, PluginEvent, PluginInfo,
};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::Arc;
use tracing::info;

const PAGE_SIZE: usize = 20;

#[derive(Debug, Clone, Serialize)]
struct PlayerRecord {
    phira_id: i32,
    first_seen: String,
    last_seen: String,
    play_count: u64,
}

pub struct PlayerTracker {
    players: Arc<Mutex<HashMap<i32, PlayerRecord>>>,
}

impl PlayerTracker {
    pub fn create() -> Box<dyn NativePlugin> {
        Box::new(PlayerTracker {
            players: Arc::new(Mutex::new(HashMap::new())),
        })
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

        // Web API
        if let Some(http) = &ctx.http {
            let players = self.players.clone();
            http.register_route("/api/players/count", Arc::new(move |_, _| {
                let count = players.lock().unwrap_or_else(|e| e.into_inner()).len();
                Ok(serde_json::json!({"count": count}))
            }));

            let players = self.players.clone();
            http.register_route("/api/players/list", Arc::new(move |_, params| {
                let page = params.first().and_then(|p| p.parse::<u64>().ok()).unwrap_or(1).max(1);
                let offset = (page - 1) * PAGE_SIZE as u64;
                let guard = players.lock().unwrap_or_else(|e| e.into_inner());
                let mut list: Vec<&PlayerRecord> = guard.values().collect();
                list.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
                let page_data: Vec<Value> = list.into_iter()
                    .skip(offset as usize).take(PAGE_SIZE)
                    .map(|r| serde_json::json!({
                        "phira_id": r.phira_id, "first_seen": r.first_seen,
                        "last_seen": r.last_seen, "play_count": r.play_count,
                    }))
                    .collect();
                Ok(serde_json::json!({"page": page, "page_size": PAGE_SIZE, "players": page_data}))
            }));
            info!("registered /api/players/count and /api/players/list");
        }

        // CLI 命令
        if let Some(cli) = &ctx.cli {
            let players = self.players.clone();
            let _ = cli.register(
                "players", "列出所有游玩过的玩家（翻页）", "players [页码]",
                Arc::new(move |args| {
                    let page: u64 = args.first().and_then(|p| p.parse().ok()).unwrap_or(1).max(1);
                    let guard = players.lock().unwrap_or_else(|e| e.into_inner());
                    let total = guard.len();
                    let mut list: Vec<&PlayerRecord> = guard.values().collect();
                    list.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
                    let offset = (page - 1) * PAGE_SIZE as u64;
                    let mut out = vec![format!("  ◆ 玩家 ({total})"), "  ──────────────────────────────────────".into()];
                    for r in list.into_iter().skip(offset as usize).take(PAGE_SIZE) {
                        out.push(format!("  │ {:<8}  最近: {}  游玩: {}", r.phira_id, r.last_seen, r.play_count));
                    }
                    out
                }),
            );

            let players = self.players.clone();
            let _ = cli.register(
                "player-count", "游玩过的玩家总数", "player-count",
                Arc::new(move |_| {
                    let count = players.lock().unwrap_or_else(|e| e.into_inner()).len();
                    vec![format!("  ◆ 玩家总数: {count}")]
                }),
            );
            info!("registered CLI: players, player-count");
        }

        Ok(())
    }

    fn on_event(&self, _ctx: &PluginContext, event: &PluginEvent) -> Vec<String> {
        if let PluginEvent::UserConnect { user_id, user_name } = event {
            let mut guard = self.players.lock().unwrap_or_else(|e| e.into_inner());
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs().to_string())
                .unwrap_or_else(|_| "now".into());
            if let Some(record) = guard.get_mut(user_id) {
                record.last_seen = now.clone();
                record.play_count += 1;
            } else {
                guard.insert(*user_id, PlayerRecord {
                    phira_id: *user_id,
                    first_seen: now.clone(),
                    last_seen: now,
                    play_count: 1,
                });
            }
        }
        vec![]
    }

    fn cleanup(&mut self) {
        info!("PlayerTracker plugin cleaned up");
    }
}
