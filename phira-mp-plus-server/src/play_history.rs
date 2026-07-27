//! In-memory play history storage.
//!
//! Keeps the most recent N rounds in memory for fast display.
//! Round results are written to PostgreSQL via `save_round_history()`.

use std::collections::VecDeque;
use tokio::sync::RwLock;

use crate::room::PlayRound;

/// Rounds kept in memory for instant display.
const MEMORY_CACHE_SIZE: usize = 10;

#[derive(Debug)]
pub struct PlayHistoryStore {
    /// Recent rounds kept in memory.
    recent: RwLock<VecDeque<PlayRound>>,
}

impl Default for PlayHistoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl PlayHistoryStore {
    pub fn new() -> Self {
        Self {
            recent: RwLock::new(VecDeque::with_capacity(MEMORY_CACHE_SIZE + 1)),
        }
    }

    /// Push a new round. Drops oldest if memory cache is full.
    pub async fn push(&self, round: PlayRound) {
        let mut recent = self.recent.write().await;
        if recent.len() >= MEMORY_CACHE_SIZE {
            recent.pop_front();
        }
        recent.push_back(round);
    }

    /// Read all cached rounds (newest last).
    pub async fn all(&self) -> Vec<PlayRound> {
        self.recent.read().await.iter().cloned().collect()
    }

    /// Sync read of in-memory rounds only (for non-async query contexts).
    /// Returns at most `MEMORY_CACHE_SIZE` most recent rounds.
    pub fn recent_sync(&self) -> Vec<PlayRound> {
        self.recent
            .try_read()
            .ok()
            .map(|r| r.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Total count (in-memory rounds only).
    pub async fn len(&self) -> usize {
        self.recent.read().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.recent.read().await.is_empty()
    }

    /// Get the most recent round (memory only, fast path).
    pub async fn last(&self) -> Option<PlayRound> {
        self.recent.read().await.back().cloned()
    }
}
