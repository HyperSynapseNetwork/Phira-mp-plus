//! Phira-mp+ 本轮结算排行输出插件
//!
//! 监听 RoundComplete / GameEnd 事件，收集每轮结算数据。
//! 提供 CLI 和 Web API 查询本轮成绩排行、ACC排行等。

use phira_mp_plus_server_api::{
    NativePlugin, PluginContext, PluginEvent, PluginInfo,
};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::info;

/// 单次游戏得分记录
#[derive(Debug, Clone, Serialize)]
pub struct ScoreRecord {
    pub user_id: i32,
    pub user_name: String,
    pub score: i32,
    pub accuracy: f32,
    pub max_combo: i32,
    pub full_combo: bool,
    pub perfect: i32,
    pub good: i32,
    pub bad: i32,
    pub miss: i32,
}

/// 一轮完整结算
#[derive(Debug, Clone, Serialize)]
pub struct RoundResult {
    pub room_id: String,
    pub chart_id: i32,
    pub chart_name: String,
    pub scores: Vec<ScoreRecord>,
}

/// 按分数排行
fn rank_by_score(scores: &[ScoreRecord]) -> Vec<ScoreRecord> {
    let mut s = scores.to_vec();
    s.sort_by(|a, b| b.score.cmp(&a.score));
    s
}

/// 按准确率排行
fn rank_by_acc(scores: &[ScoreRecord]) -> Vec<ScoreRecord> {
    let mut s = scores.to_vec();
    s.sort_by(|a, b| b.accuracy.partial_cmp(&a.accuracy).unwrap_or(std::cmp::Ordering::Equal));
    s
}

pub struct RoundResultsPlugin {
    /// room_id → 当前未完成轮次的分数
    pending: Arc<Mutex<HashMap<String, Vec<ScoreRecord>>>>,
    /// 已完成轮次
    history: Arc<Mutex<Vec<RoundResult>>>,
}

impl RoundResultsPlugin {
    pub fn create() -> Box<dyn NativePlugin> {
        Box::new(RoundResultsPlugin {
            pending: Arc::new(Mutex::new(HashMap::new())),
            history: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn format_summary(room_id: &str, chart_name: &str, scores: &[ScoreRecord]) -> Vec<String> {
        let by_score = rank_by_score(scores);
        let by_acc = rank_by_acc(scores);
        let best_score = by_score.first();
        let best_acc = by_acc.first();

        let mut out = vec![
            format!("── 结算: {} ──", chart_name),
            format!(""),
            format!("■ 得分排行"),
        ];

        for (i, s) in by_score.iter().enumerate() {
            let fc = if s.full_combo { " FC" } else { "" };
            out.push(format!("   #{:<2} {}  {}  ACC:{:.2}%{}",
                i + 1, s.user_name, s.score, s.accuracy * 100.0, fc));
        }

        out.push(format!(""));
        out.push(format!("■ ACC 排行"));
        for (i, s) in by_acc.iter().enumerate() {
            out.push(format!("   #{:<2} {}  ACC:{:.2}%{}",
                i + 1, s.user_name, s.accuracy * 100.0,
                if s.full_combo { " FC" } else { "" }));
        }

        out.push(format!(""));
        out.push(format!("■ 最高分: {} ({})  ACC:{:.2}%",
            best_score.map_or("-", |s| &s.user_name),
            best_score.map_or(0, |s| s.score),
            best_score.map_or(0.0, |s| s.accuracy) * 100.0));
        out.push(format!("■ 最高ACC: {} ({:.2}%)",
            best_acc.map_or("-", |s| &s.user_name),
            best_acc.map_or(0.0, |s| s.accuracy) * 100.0));

        out
    }
}

impl NativePlugin for RoundResultsPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "round-results".to_string(),
            version: "0.1.0".to_string(),
            author: "Phira-mp+".to_string(),
            description: "每轮结算后输出成绩排行、ACC排行等".to_string(),
        }
    }

    fn init(&mut self, ctx: &PluginContext) -> Result<(), String> {
        info!("RoundResults plugin initializing...");

        // Web API: GET /api/round/last/<room_id>
        if let Some(http) = &ctx.http {
            let history = self.history.clone();
            http.register_route("/api/round/last/<room_id>", Arc::new(move |_, params| {
                let rid = params.first().map(|s| &s[..]).unwrap_or("");
                let guard = history.lock().unwrap_or_else(|e| e.into_inner());
                let round = guard.iter().rev().find(|r| r.room_id == rid);
                match round {
                    Some(r) => Ok(serde_json::to_value(r).unwrap_or_default()),
                    None => Ok(serde_json::json!({"error": "no rounds"})),
                }
            }));
            info!("registered /api/round/last/<room_id>");
        }

        // CLI
        if let Some(cli) = &ctx.cli {
            let history = self.history.clone();
            let _ = cli.register(
                "round-last", "查看房间最近一轮结算", "round-last <room_id>",
                Arc::new(move |args| {
                    let rid = args.first().copied().unwrap_or("");
                    let guard = history.lock().unwrap_or_else(|e| e.into_inner());
                    let round = guard.iter().rev().find(|r| r.room_id == rid);
                    match round {
                        Some(r) => {
                            let mut out = format!("  结算: {} (id={})", r.chart_name, r.chart_id);
                            let by_score = rank_by_score(&r.scores);
                            let by_acc = rank_by_acc(&r.scores);
                            out.push_str(&format!("\n  ■ 得分排行"));
                            for (i, s) in by_score.iter().enumerate() {
                                out.push_str(&format!("\n     #{:<2} {}  {}  ACC:{:.2}%{}",
                                    i + 1, s.user_name, s.score, s.accuracy * 100.0,
                                    if s.full_combo { " FC" } else { "" }));
                            }
                            out.push_str(&format!("\n  ■ ACC 排行"));
                            for (i, s) in by_acc.iter().enumerate() {
                                out.push_str(&format!("\n     #{:<2} {}  ACC:{:.2}%{}",
                                    i + 1, s.user_name, s.accuracy * 100.0,
                                    if s.full_combo { " FC" } else { "" }));
                            }
                            out.push_str(&format!("\n  ■ 最高分: {} ({})",
                                by_score.first().map_or("-", |s| &s.user_name),
                                by_score.first().map_or(0, |s| s.score)));
                            out.lines().map(|l| format!("  {l}")).collect()
                        }
                        None => vec!["  · 该房间暂无结算记录".into()],
                    }
                }),
            );
            info!("registered CLI: round-last");
        }

        info!("RoundResults plugin initialized");
        Ok(())
    }

    fn on_event(&self, _ctx: &PluginContext, event: &PluginEvent) -> Vec<String> {
        match event {
            PluginEvent::GameEnd { user_id, user_name, room_id, score, accuracy } => {
                let mut guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
                let entry = guard.entry(room_id.clone()).or_insert_with(Vec::new);
                if !entry.iter().any(|r| r.user_id == *user_id) {
                    entry.push(ScoreRecord {
                        user_id: *user_id,
                        user_name: user_name.clone(),
                        score: *score,
                        accuracy: *accuracy,
                        max_combo: 0,
                        full_combo: false,
                        perfect: 0, good: 0, bad: 0, miss: 0,
                    });
                }
            }
            PluginEvent::RoundComplete { room_id, chart_id, chart_name } => {
                let scores = {
                    let mut guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
                    guard.remove(room_id).unwrap_or_default()
                };
                if scores.is_empty() {
                    return vec![];
                }
                let round = RoundResult {
                    room_id: room_id.clone(),
                    chart_id: *chart_id,
                    chart_name: chart_name.clone(),
                    scores,
                };

                // 保存历史
                self.history.lock().unwrap_or_else(|e| e.into_inner()).push(round.clone());

                // 组装结算消息发送到房间
                let summary = Self::format_summary(room_id, chart_name, &round.scores);
                // 发送（通过 state 或 send_chat 发送给所有房间成员）
                if let Some(state) = &_ctx.state {
                    let text = summary.join("\n");
                    state.call("send_room_chat", &[
                        serde_json::json!(room_id),
                        serde_json::json!(text),
                    ]).ok();
                }
            }
            _ => {}
        }
        vec![]
    }

    fn cleanup(&mut self) {
        info!("RoundResults plugin cleaned up");
    }
}
