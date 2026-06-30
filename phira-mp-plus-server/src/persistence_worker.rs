//! Runtime v2 persistence worker skeleton.
//!
//! The existing `db.rs` direct write paths are still active.  This worker is a
//! bounded queue and stats holder for gradually migrating high-frequency writes
//! to batched background persistence without changing current database behavior.

use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, warn};

#[derive(Debug, Clone)]
pub enum PersistenceEvent {
    RoomSnapshot { room_id: String, payload: Value, simulation: bool },
    ServerEvent { kind: String, payload: Value, simulation: bool },
    TouchBatch { round_id: String, user_id: i32, payload: Value, simulation: bool },
    JudgeBatch { round_id: String, user_id: i32, payload: Value, simulation: bool },
    Flush,
    Shutdown,
}

#[derive(Debug, Clone, Default)]
pub struct PersistenceStats {
    pub queued: u64,
    pub processed: u64,
    pub dropped: u64,
    pub last_error: Option<String>,
}

#[derive(Debug)]
pub struct PersistenceWorker {
    tx: mpsc::Sender<PersistenceEvent>,
    stats: Arc<RwLock<PersistenceStats>>,
}

impl PersistenceWorker {
    pub fn spawn(queue_capacity: usize) -> Arc<Self> {
        let capacity = queue_capacity.max(16);
        let (tx, mut rx) = mpsc::channel::<PersistenceEvent>(capacity);
        let stats = Arc::new(RwLock::new(PersistenceStats::default()));
        let worker_stats = Arc::clone(&stats);

        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    PersistenceEvent::Shutdown => {
                        debug!("persistence worker shutdown requested");
                        break;
                    }
                    PersistenceEvent::Flush => {
                        debug!("persistence worker flush marker received");
                    }
                    other => {
                        debug!(?other, "persistence worker skeleton consumed event");
                    }
                }
                worker_stats.write().await.processed += 1;
            }
        });

        Arc::new(Self { tx, stats })
    }

    pub async fn enqueue(&self, event: PersistenceEvent) -> Result<(), PersistenceEvent> {
        match self.tx.try_send(event) {
            Ok(()) => {
                self.stats.write().await.queued += 1;
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(event)) => {
                let mut stats = self.stats.write().await;
                stats.dropped += 1;
                stats.last_error = Some("persistence worker queue is full".to_string());
                warn!("persistence worker queue is full; event dropped");
                Err(event)
            }
            Err(mpsc::error::TrySendError::Closed(event)) => {
                let mut stats = self.stats.write().await;
                stats.dropped += 1;
                stats.last_error = Some("persistence worker queue is closed".to_string());
                warn!("persistence worker queue is closed; event dropped");
                Err(event)
            }
        }
    }

    pub async fn stats(&self) -> PersistenceStats {
        self.stats.read().await.clone()
    }
}
