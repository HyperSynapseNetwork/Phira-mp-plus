//! Phira-mp+ 轮次数据持久化存储
//!
//! 将每轮游玩的 Touches/Judges 按轮次 UUID + Phira ID 写入磁盘，
//! 支持配置保存天数与后台清理。使用 JSONL 格式逐行追加，
//! 避免全量读写的性能开销。
//!
//! 目录结构:
//!   data/rounds/<round_uuid>/
//!     <player_id>/
//!       touches.jsonl   # 每行一个 TouchEventPoint JSON
//!       judges.jsonl    # 每行一个 JudgeEventItem JSON
//!     _meta.json        # 轮次元数据

use crate::plugin::{TouchEventPoint, JudgeEventItem};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::RwLock;
use tracing::{info, warn};

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
pub struct RoundStore {
    base_dir: PathBuf,
    /// 记录的轮次集合: round_uuid → 是否活跃（正在记录）
    active_rounds: RwLock<HashMap<String, bool>>,
    /// 保留天数（默认 7）
    retention_days: u32,
}

impl RoundStore {
    pub fn new(base_dir: &str, retention_days: u32) -> Self {
        let path = PathBuf::from(base_dir).join("rounds");
        let _ = std::fs::create_dir_all(&path);
        Self {
            base_dir: path,
            active_rounds: RwLock::new(HashMap::new()),
            retention_days,
        }
    }

    // ── 目录路径 ──

    fn round_dir(&self, round_uuid: &str) -> PathBuf {
        self.base_dir.join(round_uuid)
    }

    fn player_dir(&self, round_uuid: &str, player_id: i32) -> PathBuf {
        self.round_dir(round_uuid).join(player_id.to_string())
    }

    fn meta_path(&self, round_uuid: &str) -> PathBuf {
        self.round_dir(round_uuid).join("_meta.json")
    }

    fn touches_path(&self, round_uuid: &str, player_id: i32) -> PathBuf {
        self.player_dir(round_uuid, player_id).join("touches.jsonl")
    }

    fn judges_path(&self, round_uuid: &str, player_id: i32) -> PathBuf {
        self.player_dir(round_uuid, player_id).join("judges.jsonl")
    }

    // ── 轮次生命周期 ──

    /// 开始记录一轮数据
    pub async fn open_round(&self, meta: &RoundMeta) -> std::io::Result<()> {
        let dir = self.round_dir(&meta.round_uuid);
        tokio::fs::create_dir_all(&dir).await?;

        // 为每个玩家创建目录
        for pid in &meta.players {
            let pdir = self.player_dir(&meta.round_uuid, *pid);
            tokio::fs::create_dir_all(&pdir).await?;
        }

        // 写入元数据
        let json = serde_json::to_string_pretty(meta)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        tokio::fs::write(self.meta_path(&meta.round_uuid), json).await?;

        self.active_rounds.write().await.insert(meta.round_uuid.clone(), true);
        info!("round store: opened round {} (chart={})", meta.round_uuid, meta.chart_name);
        Ok(())
    }

    /// 关闭一轮记录
    pub async fn close_round(&self, round_uuid: &str) {
        // 更新元数据中的完成时间
        if let Some(meta) = self.read_meta(round_uuid).await {
            let mut meta = meta;
            meta.finished_at = Some(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0));
            if let Ok(json) = serde_json::to_string_pretty(&meta) {
                let _ = tokio::fs::write(self.meta_path(round_uuid), json).await;
            }
        }
        self.active_rounds.write().await.insert(round_uuid.to_string(), false);
        info!("round store: closed round {round_uuid}");
    }

    // ── 数据追加 ──

    /// 追加触控数据到指定轮次+玩家
    pub async fn append_touches(
        &self,
        round_uuid: &str,
        player_id: i32,
        data: &[TouchEventPoint],
    ) {
        if data.is_empty() { return; }
        let path = self.touches_path(round_uuid, player_id);
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let mut file = match tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path).await
        {
            Ok(f) => f,
            Err(e) => { warn!("round store: append touches error: {e}"); return; }
        };

        for point in data {
            if let Ok(line) = serde_json::to_string(point) {
                let _ = file.write_all(format!("{line}\n").as_bytes()).await;
            }
        }
    }

    /// 追加判定数据到指定轮次+玩家
    pub async fn append_judges(
        &self,
        round_uuid: &str,
        player_id: i32,
        data: &[JudgeEventItem],
    ) {
        if data.is_empty() { return; }
        let path = self.judges_path(round_uuid, player_id);
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let mut file = match tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path).await
        {
            Ok(f) => f,
            Err(e) => { warn!("round store: append judges error: {e}"); return; }
        };

        for item in data {
            if let Ok(line) = serde_json::to_string(item) {
                let _ = file.write_all(format!("{line}\n").as_bytes()).await;
            }
        }
    }

    // ── 数据读取 ──

    /// 读取轮次元数据
    pub async fn read_meta(&self, round_uuid: &str) -> Option<RoundMeta> {
        let path = self.meta_path(round_uuid);
        let content = tokio::fs::read_to_string(path).await.ok()?;
        serde_json::from_str(&content).ok()
    }

    /// 读取指定轮次+玩家的全部触控和判定数据
    pub async fn read_player_data(
        &self,
        round_uuid: &str,
        player_id: i32,
    ) -> Option<RoundPlayerData> {
        let touches = self.read_touches(round_uuid, player_id).await;
        let judges = self.read_judges(round_uuid, player_id).await;

        Some(RoundPlayerData {
            round_uuid: round_uuid.to_string(),
            player_id,
            touches,
            judges,
        })
    }

    /// 读取指定轮次+玩家的触控数据
    pub async fn read_touches(&self, round_uuid: &str, player_id: i32) -> Vec<TouchEventPoint> {
        let path = self.touches_path(round_uuid, player_id);
        read_jsonl_file(&path).await.unwrap_or_default()
    }

    /// 读取指定轮次+玩家的判定数据
    pub async fn read_judges(&self, round_uuid: &str, player_id: i32) -> Vec<JudgeEventItem> {
        let path = self.judges_path(round_uuid, player_id);
        read_jsonl_file(&path).await.unwrap_or_default()
    }

    /// 列出所有已记录的轮次
    pub async fn list_rounds(&self) -> Vec<RoundMeta> {
        let mut rounds = Vec::new();
        let mut entries = match tokio::fs::read_dir(&self.base_dir).await {
            Ok(e) => e,
            Err(_) => return rounds,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                let uuid = entry.file_name().to_string_lossy().to_string();
                if let Some(meta) = self.read_meta(&uuid).await {
                    rounds.push(meta);
                }
            }
        }
        rounds.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        rounds
    }

    // ── 保留策略 ──

    /// 清理超过保留天数的轮次数据
    pub async fn cleanup_expired(&self) {
        let max_age = chrono::Duration::days(self.retention_days as i64);
        let now = chrono::Utc::now();
        let mut removed = 0usize;

        let mut entries = match tokio::fs::read_dir(&self.base_dir).await {
            Ok(e) => e,
            Err(_) => return,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            if !entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let uuid = entry.file_name().to_string_lossy().to_string();

            // 跳过仍在活跃记录的轮次
            if self.active_rounds.read().await.get(&uuid).copied().unwrap_or(false) {
                continue;
            }

            // 检查 meta 中的完成时间
            if let Some(meta) = self.read_meta(&uuid).await {
                let finished = meta.finished_at.unwrap_or(meta.started_at);
                let finished_time = chrono::DateTime::from_timestamp_millis(finished)
                    .unwrap_or(chrono::Utc::now());
                if now.signed_duration_since(finished_time) > max_age {
                    // 删除整个轮次目录
                    if tokio::fs::remove_dir_all(entry.path()).await.is_ok() {
                        removed += 1;
                    }
                }
            }
        }

        if removed > 0 {
            info!("round store: cleaned up {removed} expired round(s) (retention={}d)", self.retention_days);
        }
    }
}

/// 从 JSONL 文件读取一批数据
async fn read_jsonl_file<T>(path: &Path) -> std::io::Result<Vec<T>>
where
    T: serde::de::DeserializeOwned,
{
    let file = tokio::fs::File::open(path).await?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();
    let mut items = Vec::new();

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() { continue; }
        if let Ok(item) = serde_json::from_str::<T>(&line) {
            items.push(item);
        }
    }

    Ok(items)
}
