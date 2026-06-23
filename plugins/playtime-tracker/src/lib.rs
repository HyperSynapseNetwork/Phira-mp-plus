//! Phira-mp+ 游玩时间统计插件
//!
//! 记录每个用户在服务器的游玩时间，提供排行榜和 Web API。
//! 通过 UserConnect / UserDisconnect 事件追踪。

use phira_mp_plus_server_api::{
    NativePlugin, PluginContext, PluginEvent, PluginInfo,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::info;

/// 持久化文件路径
const DATA_PATH: &str = "data/playtime-tracker.json";

/// 玩家游玩数据
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PlayerTime {
    total_seconds: u64,
    /// 当前会话开始时间戳（None 表示不在游戏中）
    session_start: Option<u64>,
}

/// 排行条目
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
struct RankEntry {
    user_id: i32,
    playtime_seconds: u64,
    playtime_hours: f64,
}

pub struct PlaytimeTracker {
    data: Arc<Mutex<HashMap<i32, PlayerTime>>>,
}

impl PlaytimeTracker {
    pub fn create() -> Box<dyn NativePlugin> {
        // 尝试从文件加载
        let data = load_data().unwrap_or_default();
        info!("PlaytimeTracker loaded {} records from disk", data.len());
        Box::new(PlaytimeTracker {
            data: Arc::new(Mutex::new(data)),
        })
    }

    fn save(&self) {
        let guard = self.data.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(json) = serde_json::to_string_pretty(&*guard) {
            if let Some(parent) = Path::new(DATA_PATH).parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::write(DATA_PATH, json).ok();
        }
    }

    fn now() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
    }

    /// 获取排行列表（降序）
    #[allow(dead_code)]
    fn ranked_list(&self) -> Vec<(i32, u64)> {
        let guard = self.data.lock().unwrap_or_else(|e| e.into_inner());
        let mut list: Vec<(i32, u64)> = guard.iter()
            .map(|(&uid, p)| {
                let total = p.total_seconds + p.session_start.map_or(0, |start| Self::now().saturating_sub(start));
                (uid, total)
            })
            .collect();
        list.sort_by(|a, b| b.1.cmp(&a.1));
        list
    }
}

impl NativePlugin for PlaytimeTracker {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "playtime-tracker".to_string(),
            version: "0.1.0".to_string(),
            author: "Phira-mp+".to_string(),
            description: "统计用户游玩时间并提供排行榜".to_string(),
        }
    }

    fn init(&mut self, ctx: &PluginContext) -> Result<(), String> {
        info!("PlaytimeTracker plugin initializing...");

        // 内部 API（供其他插件查询）
        if let Some(api) = &ctx.api {
            let data = self.data.clone();
            let handler: phira_mp_plus_server_api::PluginApiHandler = Arc::new(move |method, args| {
                let guard = data.lock().unwrap_or_else(|e| e.into_inner());
                match method {
                    "user_playtime" => {
                        let uid = args.first().and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                        let now = PlaytimeTracker::now();
                        let total = guard.get(&uid).map_or(0, |p| {
                            p.total_seconds + p.session_start.map_or(0, |s| now.saturating_sub(s))
                        });
                        Ok(serde_json::json!({"user_id": uid, "total_seconds": total}))
                    }
                    "leaderboard" => {
                        let limit = args.first().and_then(|v| v.as_u64()).unwrap_or(10) as usize;
                        let mut list: Vec<(&i32, &PlayerTime)> = guard.iter().collect();
                        list.sort_by(|a, b| {
                            let ta = a.1.total_seconds + a.1.session_start.map_or(0, |s| PlaytimeTracker::now().saturating_sub(s));
                            let tb = b.1.total_seconds + b.1.session_start.map_or(0, |s| PlaytimeTracker::now().saturating_sub(s));
                            tb.cmp(&ta)
                        });
                        let top: Vec<Value> = list.iter().take(limit).map(|(&uid, p)| {
                            let total = p.total_seconds + p.session_start.map_or(0, |s| PlaytimeTracker::now().saturating_sub(s));
                            serde_json::json!({"user_id": uid, "total_playtime": total})
                        }).collect();
                        Ok(serde_json::json!({"data": top, "total_users": guard.len()}))
                    }
                    _ => Err(format!("unknown method: {method}")),
                }
            });
            let _ = api.call("playtime-tracker", "register_handler", &[]); // placeholder
            let _ = handler; // keep alive
            info!("internal API handler ready");
        }

        // Web API
        if let Some(http) = &ctx.http {
            let data = self.data.clone();
            http.register_route("/api/user_rank/<user_id>", Arc::new(move |_, params| {
                let uid = params.first().and_then(|p| p.parse::<i32>().ok()).unwrap_or(0);
                let (rank, total) = {
                    let guard = data.lock().unwrap_or_else(|e| e.into_inner());
                    let now = PlaytimeTracker::now();
                    let totals: Vec<(i32, u64)> = guard.iter().map(|(&id, p)| {
                        (id, p.total_seconds + p.session_start.map_or(0, |s| now.saturating_sub(s)))
                    }).collect();
                    let user_total = guard.get(&uid).map(|p| {
                        p.total_seconds + p.session_start.map_or(0, |s| now.saturating_sub(s))
                    }).unwrap_or(0);
                    // 计算排名（考虑并列）
                    let rank = 1 + totals.iter().filter(|(_, t)| *t > user_total).count() as u64;
                    (if user_total > 0 { rank } else { 0 }, user_total)
                };
                if total == 0 {
                    return Ok(serde_json::json!({"error": "user not found"}));
                }
                Ok(serde_json::json!({
                    "success": true,
                    "data": {
                        "user_id": uid,
                        "rank": rank,
                        "total_playtime_seconds": total,
                        "total_playtime_hours": (total as f64) / 3600.0,
                    }
                }))
            }));
            info!("registered /api/user_rank/<user_id>");

            let data = self.data.clone();
            http.register_route("/api/user_playtime_ranking", Arc::new(move |_, _| {
                let guard = data.lock().unwrap_or_else(|e| e.into_inner());
                let now = PlaytimeTracker::now();
                let mut entries: Vec<RankEntry> = guard.iter().map(|(&uid, p)| {
                    let secs = p.total_seconds + p.session_start.map_or(0, |s| now.saturating_sub(s));
                    RankEntry { user_id: uid, playtime_seconds: secs, playtime_hours: (secs as f64 / 3600.0 * 100.0).round() / 100.0 }
                }).collect();
                entries.sort_by(|a, b| b.playtime_seconds.cmp(&a.playtime_seconds));
                entries.truncate(10);
                Ok(serde_json::json!({"success": true, "data": entries, "count": entries.len()}))
            }));
            info!("registered /api/user_playtime_ranking");

            let data = self.data.clone();
            http.register_route("/api/playtime_leaderboard", Arc::new(move |_, _| {
                let guard = data.lock().unwrap_or_else(|e| e.into_inner());
                let now = PlaytimeTracker::now();
                let now_iso = iso_now();
                let data_list: Vec<Value> = guard.iter().map(|(&uid, p)| {
                    let total = p.total_seconds + p.session_start.map_or(0, |s| now.saturating_sub(s));
                    serde_json::json!({"user_id": uid, "total_playtime": total})
                }).collect();
                Ok(serde_json::json!({
                    "success": true, "data": data_list,
                    "timestamp": now_iso, "total_users": guard.len(),
                }))
            }));
            info!("registered /api/playtime_leaderboard");

            let data = self.data.clone();
            http.register_route("/api/playtime_leaderboard/top/<limit>", Arc::new(move |_, params| {
                let limit: usize = params.first().and_then(|p| p.parse().ok()).unwrap_or(10).max(1);
                let guard = data.lock().unwrap_or_else(|e| e.into_inner());
                let now = PlaytimeTracker::now();
                let now_iso = iso_now();
                let mut entries: Vec<(i32, u64)> = guard.iter().map(|(&uid, p)| {
                    let total = p.total_seconds + p.session_start.map_or(0, |s| now.saturating_sub(s));
                    (uid, total)
                }).collect();
                entries.sort_by(|a, b| b.1.cmp(&a.1));
                entries.truncate(limit);
                let data_list: Vec<Value> = entries.iter().map(|(uid, total)| {
                    serde_json::json!({"user_id": uid, "total_playtime": total})
                }).collect();
                Ok(serde_json::json!({
                    "success": true, "data": data_list,
                    "timestamp": now_iso, "total_users": data_list.len(),
                }))
            }));
            info!("registered /api/playtime_leaderboard/top/<limit>");
        }

        // 注册插件 API（供其他插件查询）
        if let Some(reg) = &ctx.register_api {
            let data = self.data.clone();
            let handler: phira_mp_plus_server_api::PluginApiHandler = Arc::new(move |method, args| {
                let guard = data.lock().unwrap_or_else(|e| e.into_inner());
                let now = PlaytimeTracker::now();
                match method {
                    "user_playtime" => {
                        let uid = args.first().and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                        let total = guard.get(&uid).map_or(0, |p| {
                            p.total_seconds + p.session_start.map_or(0, |s| now.saturating_sub(s))
                        });
                        Ok(serde_json::json!({"user_id": uid, "total_seconds": total}))
                    }
                    "leaderboard" => {
                        let limit = args.first().and_then(|v| v.as_u64()).unwrap_or(10) as usize;
                        let mut list: Vec<Value> = guard.iter().map(|(&uid, p)| {
                            let total = p.total_seconds + p.session_start.map_or(0, |s| now.saturating_sub(s));
                            serde_json::json!({"user_id": uid, "total_playtime": total})
                        }).collect();
                        list.sort_by(|a, b| b.get("total_playtime").and_then(|v| v.as_u64())
                            .cmp(&a.get("total_playtime").and_then(|v| v.as_u64())));
                        list.truncate(limit);
                        Ok(serde_json::json!({"data": list}))
                    }
                    _ => Err(format!("unknown: {method}")),
                }
            });
            reg("playtime-tracker", handler);
            info!("registered plugin API: playtime-tracker");
        }

        // CLI
        if let Some(cli) = &ctx.cli {
            let data = self.data.clone();
            let _ = cli.register(
                "playtime", "查询用户游玩时间", "playtime <user_id>",
                Arc::new(move |args| {
                    let uid = args.first().and_then(|p| p.parse::<i32>().ok()).unwrap_or(0);
                    let guard = data.lock().unwrap_or_else(|e| e.into_inner());
                    let now = PlaytimeTracker::now();
                    match guard.get(&uid) {
                        Some(p) => {
                            let total = p.total_seconds + p.session_start.map_or(0, |s| now.saturating_sub(s));
                            vec![
                                format!("  ◆ 用户 {} 游玩时间", uid),
                                format!("  │ 总计: {} 秒 ({:.2} 小时)", total, total as f64 / 3600.0),
                            ]
                        }
                        None => vec![format!("  · 未找到用户 {}", uid)],
                    }
                }),
            );

            let data = self.data.clone();
            let _ = cli.register(
                "playtime-top", "游玩时间排行榜前 N", "playtime-top [数量]",
                Arc::new(move |args| {
                    let limit = args.first().and_then(|p| p.parse::<usize>().ok()).unwrap_or(10).max(1);
                    let guard = data.lock().unwrap_or_else(|e| e.into_inner());
                    let now = PlaytimeTracker::now();
                    let mut list: Vec<(i32, u64)> = guard.iter().map(|(&id, p)| {
                        (id, p.total_seconds + p.session_start.map_or(0, |s| now.saturating_sub(s)))
                    }).collect();
                    list.sort_by(|a, b| b.1.cmp(&a.1));
                    let mut out = vec![format!("  ◆ 游玩时间排行榜 TOP {}", list.len().min(limit)), "  ──────────────────────────".into()];
                    for (i, (uid, secs)) in list.iter().enumerate().take(limit) {
                        out.push(format!("  │ #{:<3} 用户 {:<8}  {:.2} 小时", i + 1, uid, *secs as f64 / 3600.0));
                    }
                    out
                }),
            );
            info!("registered CLI: playtime, playtime-top");
        }

        Ok(())
    }

    fn on_event(&self, _ctx: &PluginContext, event: &PluginEvent) -> Vec<String> {
        match event {
            PluginEvent::UserConnect { user_id, .. } => {
                let mut guard = self.data.lock().unwrap_or_else(|e| e.into_inner());
                let entry = guard.entry(*user_id).or_insert(PlayerTime {
                    total_seconds: 0,
                    session_start: None,
                });
                // 先提交上一段的游玩时间（防止重连时覆盖 session_start 导致丢失）
                if let Some(start) = entry.session_start {
                    entry.total_seconds += Self::now().saturating_sub(start);
                }
                entry.session_start = Some(Self::now());
                drop(guard);
                self.save();
            }
            PluginEvent::UserDisconnect { user_id, .. } => {
                let mut guard = self.data.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(entry) = guard.get_mut(user_id) {
                    if let Some(start) = entry.session_start {
                        entry.total_seconds += Self::now().saturating_sub(start);
                        entry.session_start = None;
                    }
                }
                drop(guard);
                self.save();
            }
            _ => {}
        }
        vec![]
    }

    fn cleanup(&mut self) {
        self.save();
        info!("PlaytimeTracker plugin cleaned up");
    }
}

fn load_data() -> Option<HashMap<i32, PlayerTime>> {
    let path = Path::new(DATA_PATH);
    if path.exists() {
        std::fs::read_to_string(path).ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    } else {
        None
    }
}

fn iso_now() -> String {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs();
    // 简单 ISO 8601 格式
    format!("{}T00:00:00Z", secs)
}
