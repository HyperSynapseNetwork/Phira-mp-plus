//! Phira-mp+ 轮次数据持久化存储（PostgreSQL 接口）
//!
//! 将每轮游玩的 Touches/Judges 通过 PostgreSQL repository 进行持久化。
//! 不再使用本地 JSON/JSONL 文件写入。

use crate::plugin::{JudgeEventItem, TouchEventPoint};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::RwLock;
use tracing::info;

/// 轮次元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundMeta {
    pub round_uuid: String,
    pub chart_id: i32,
    pub chart_name: String,
    pub room_id: String,
    pub players: Vec<i32>,
    pub started_at: i64, // Unix timestamp ms
    pub finished_at: Option<i64>,
}

/// 轮次数据读取器 — 查询某轮某玩家的全部触控/判定记录
#[derive(Debug, Clone, Serialize)]
pub struct RoundPlayerData {
    pub round_uuid: String,
    pub player_id: i32,
    pub touches: Vec<TouchEventPoint>,
    pub judges: Vec<JudgeEventItem>,
}

/// 轮次数据存储管理器
///
/// 所有读写操作统一通过 PostgreSQL repository 完成。
/// 本地 JSON/JSONL 文件写入已在 PostgreSQL cutover 中移除。
pub struct RoundStore {
    /// 记录的轮次集合: round_uuid → 是否活跃（正在记录）
    active_rounds: RwLock<HashMap<String, bool>>,
}

impl Default for RoundStore {
    fn default() -> Self {
        Self::new()
    }
}

impl RoundStore {
    pub fn new() -> Self {
        Self {
            active_rounds: RwLock::new(HashMap::new()),
        }
    }

    // ── 轮次生命周期 ──

    /// 开始记录一轮数据
    pub async fn open_round(&self, meta: &RoundMeta) -> std::io::Result<()> {
        let db = crate::internal_hooks::DB.get().expect("DB not initialized");
        if !db.open_round(meta).await {
            return Err(std::io::Error::other(
                "PostgreSQL round-open transaction failed",
            ));
        }
        self.active_rounds
            .write()
            .await
            .insert(meta.round_uuid.clone(), true);
        info!(
            "round store: opened round {} (chart={})",
            meta.round_uuid, meta.chart_name
        );
        Ok(())
    }

    /// 关闭一轮记录
    pub async fn close_round(&self, round_uuid: &str) {
        let db = crate::internal_hooks::DB.get().expect("DB not initialized");
        if !db.close_round(round_uuid).await {
            tracing::warn!(round_uuid, "PostgreSQL round-close transaction failed");
        }
        self.active_rounds
            .write()
            .await
            .insert(round_uuid.to_string(), false);
        info!("round store: closed round {round_uuid}");
    }

    // ── 数据追加 ──

    /// 追加触控数据到指定轮次+玩家
    pub async fn append_touches(
        &self,
        round_uuid: &str,
        player_id: i32,
        data: &[TouchEventPoint],
    ) -> bool {
        if data.is_empty() {
            return true;
        }
        let db = crate::internal_hooks::DB.get().expect("DB not initialized");
        db.append_touches(round_uuid, player_id, data).await
    }

    /// 追加判定数据到指定轮次+玩家
    pub async fn append_judges(
        &self,
        round_uuid: &str,
        player_id: i32,
        data: &[JudgeEventItem],
    ) -> bool {
        if data.is_empty() {
            return true;
        }
        let db = crate::internal_hooks::DB.get().expect("DB not initialized");
        db.append_judges(round_uuid, player_id, data).await
    }

    // ── 数据读取 ──

    /// 读取指定轮次+玩家的全部触控和判定数据
    pub async fn read_player_data(
        &self,
        round_uuid: &str,
        player_id: i32,
    ) -> Option<RoundPlayerData> {
        let db = crate::internal_hooks::DB.get().expect("DB not initialized");
        db.read_round_player_data(round_uuid, player_id).await
    }

    /// 列出所有已记录的轮次
    pub async fn list_rounds(&self) -> Vec<RoundMeta> {
        let db = crate::internal_hooks::DB.get().expect("DB not initialized");
        db.list_rounds(1000).await
    }
}
