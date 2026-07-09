//! Hybrid in-memory + on-disk play history storage.
//!
//! Keeps the most recent N rounds in memory for fast display; older rounds
//! are flushed to a JSONL file asynchronously. Reads merge both sources.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;
use tracing::warn;

use crate::room::PlayRound;

/// Rounds kept in memory for instant display.
const MEMORY_CACHE_SIZE: usize = 10;

/// Max rounds retained on disk beyond the memory cache.
const FILE_RETAIN_MAX: usize = 50;

#[derive(Debug)]
pub struct PlayHistoryStore {
    /// Recent rounds kept in memory.
    recent: RwLock<VecDeque<PlayRound>>,
    /// Absolute path to the JSONL history file (set when a round is first flushed).
    file_path: RwLock<Option<PathBuf>>,
}

impl PlayHistoryStore {
    pub fn new() -> Self {
        Self {
            recent: RwLock::new(VecDeque::with_capacity(MEMORY_CACHE_SIZE + 1)),
            file_path: RwLock::new(None),
        }
    }

    /// Base directory for round history files.
    pub fn base_dir() -> &'static Path {
        Path::new("data/history")
    }

    /// Derive history file path from room UUID.
    fn file_path_for(uuid: &uuid::Uuid) -> PathBuf {
        Self::base_dir().join(format!("{uuid}.jsonl"))
    }

    /// Push a new round. If memory cache is full, flush oldest to disk.
    pub async fn push(&self, round: PlayRound, room_uuid: &uuid::Uuid) {
        let to_flush = {
            let mut recent = self.recent.write().await;
            if recent.len() >= MEMORY_CACHE_SIZE {
                recent.pop_front()
            } else {
                None
            }
        };

        // Flush outside the write lock
        if let Some(oldest) = to_flush {
            self.flush_one(room_uuid, &oldest).await;
        }

        self.recent.write().await.push_back(round);
    }

    /// Append one round to the JSONL file.
    async fn flush_one(&self, room_uuid: &uuid::Uuid, round: &PlayRound) {
        let path = {
            let mut fp = self.file_path.write().await;
            if fp.is_none() {
                if let Err(e) = tokio::fs::create_dir_all(Self::base_dir()).await {
                    warn!(?e, "play_history: failed to create base dir");
                    return;
                }
                *fp = Some(Self::file_path_for(room_uuid));
            }
            fp.clone().expect("just set")
        };

        let line = match serde_json::to_string(round) {
            Ok(s) => s + "\n",
            Err(e) => {
                warn!(?e, "play_history: failed to serialize round");
                return;
            }
        };

        let mut file = match tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
        {
            Ok(f) => f,
            Err(e) => {
                warn!(?e, "play_history: failed to open file for append");
                return;
            }
        };

        if let Err(e) = file.write_all(line.as_bytes()).await {
            warn!(?e, "play_history: append write failed");
        }
    }

    /// Read all rounds: disk (older) first, then memory (newer).
    pub async fn all(&self) -> Vec<PlayRound> {
        let mut result = Vec::new();

        // Read from disk first (older rounds)
        let path = self.file_path.read().await.clone();
        if let Some(path) = path {
            if let Ok(mut disk_rounds) = Self::read_file(&path).await {
                if disk_rounds.len() > FILE_RETAIN_MAX {
                    disk_rounds.drain(0..disk_rounds.len() - FILE_RETAIN_MAX);
                }
                result.append(&mut disk_rounds);
            }
        }

        // Then append memory (newer rounds)
        {
            let recent = self.recent.read().await;
            result.extend(recent.iter().cloned());
        }

        result
    }

    /// Total count (memory + approximate on-disk).
    pub async fn len(&self) -> usize {
        let mem = self.recent.read().await.len();
        let disk = self.disk_line_count().await;
        mem + disk
    }

    pub async fn is_empty(&self) -> bool {
        self.recent.read().await.is_empty() && self.disk_line_count().await == 0
    }

    /// Get the most recent round (memory only, fast path).
    pub async fn last(&self) -> Option<PlayRound> {
        self.recent.read().await.back().cloned()
    }

    async fn disk_line_count(&self) -> usize {
        let path = self.file_path.read().await.clone();
        match path {
            Some(p) => {
                let mut file = tokio::fs::File::open(&p).await.ok()?;
                let mut buf = Vec::new();
                file.read_to_end(&mut buf).await.ok()?;
                Some(buf.iter().filter(|&&b| b == b'\n').count())
            }
            None => None,
        }
        .unwrap_or(0)
    }

    /// Read all rounds from a JSONL file.
    async fn read_file(path: &Path) -> std::io::Result<Vec<PlayRound>> {
        let file = tokio::fs::File::open(path).await?;
        let reader = tokio::io::BufReader::new(file);
        let mut lines = reader.lines();
        let mut rounds = Vec::new();

        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            if let Ok(round) = serde_json::from_str::<PlayRound>(&line) {
                rounds.push(round);
            }
        }

        Ok(rounds)
    }
}
