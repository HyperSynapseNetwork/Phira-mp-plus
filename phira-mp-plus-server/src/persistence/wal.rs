//! Crash-recovery write-ahead log for PersistenceWorker admission.
//!
//! Every data event is fsync'd before it is admitted to the in-memory queue.
//! Terminal processing writes an ACK record. Startup replays records without a
//! matching ACK, and compaction rewrites only outstanding admissions.

use crate::persistence::message::PersistenceEvent;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum WalRecord {
    Admission {
        id: uuid::Uuid,
        event: PersistenceEvent,
    },
    Ack {
        id: uuid::Uuid,
    },
}

#[derive(Debug)]
pub struct PersistenceWal {
    path: PathBuf,
    io_gate: Mutex<()>,
}

impl PersistenceWal {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            io_gate: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    async fn ensure_parent(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("create WAL directory {}: {e}", parent.display()))?;
        }
        Ok(())
    }

    async fn append_record(&self, record: &WalRecord) -> Result<(), String> {
        let _guard = self.io_gate.lock().await;
        self.ensure_parent().await?;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await
            .map_err(|e| format!("open WAL {}: {e}", self.path.display()))?;
        let mut line =
            serde_json::to_vec(record).map_err(|e| format!("serialize WAL record: {e}"))?;
        line.push(b'\n');
        file.write_all(&line)
            .await
            .map_err(|e| format!("append WAL {}: {e}", self.path.display()))?;
        file.flush()
            .await
            .map_err(|e| format!("flush WAL {}: {e}", self.path.display()))?;
        file.sync_data()
            .await
            .map_err(|e| format!("sync WAL {}: {e}", self.path.display()))?;
        Ok(())
    }

    pub async fn admit(&self, event: PersistenceEvent) -> Result<uuid::Uuid, String> {
        let id = uuid::Uuid::new_v4();
        self.append_record(&WalRecord::Admission { id, event })
            .await?;
        Ok(id)
    }

    pub async fn ack(&self, id: uuid::Uuid) -> Result<(), String> {
        self.append_record(&WalRecord::Ack { id }).await
    }

    pub async fn replay(&self) -> Result<Vec<(uuid::Uuid, PersistenceEvent)>, String> {
        let _guard = self.io_gate.lock().await;
        let bytes = match tokio::fs::read(&self.path).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("read WAL {}: {e}", self.path.display())),
        };
        let mut admitted = Vec::new();
        let mut acked = HashSet::new();
        for (index, line) in bytes.split(|b| *b == b'\n').enumerate() {
            if line.is_empty() {
                continue;
            }
            let record: WalRecord = serde_json::from_slice(line).map_err(|e| {
                format!(
                    "corrupt WAL {} line {}: {e}",
                    self.path.display(),
                    index + 1
                )
            })?;
            match record {
                WalRecord::Admission { id, event } => admitted.push((id, event)),
                WalRecord::Ack { id } => {
                    acked.insert(id);
                }
            }
        }
        Ok(admitted
            .into_iter()
            .filter(|(id, _)| !acked.contains(id))
            .collect())
    }

    pub async fn compact(&self) -> Result<usize, String> {
        let pending = self.replay().await?;
        let _guard = self.io_gate.lock().await;
        self.ensure_parent().await?;
        let temp = self.path.with_extension("wal.tmp");
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temp)
            .await
            .map_err(|e| format!("create WAL temp {}: {e}", temp.display()))?;
        for (id, event) in &pending {
            let mut line = serde_json::to_vec(&WalRecord::Admission {
                id: *id,
                event: event.clone(),
            })
            .map_err(|e| format!("serialize compacted WAL: {e}"))?;
            line.push(b'\n');
            file.write_all(&line)
                .await
                .map_err(|e| format!("write WAL temp {}: {e}", temp.display()))?;
        }
        file.flush()
            .await
            .map_err(|e| format!("flush WAL temp {}: {e}", temp.display()))?;
        file.sync_data()
            .await
            .map_err(|e| format!("sync WAL temp {}: {e}", temp.display()))?;
        drop(file);
        tokio::fs::rename(&temp, &self.path)
            .await
            .map_err(|e| format!("replace WAL {}: {e}", self.path.display()))?;
        Ok(pending.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn replays_only_unacknowledged_events_and_compacts() {
        let path = std::env::temp_dir().join(format!("pmp-wal-{}.jsonl", uuid::Uuid::new_v4()));
        let wal = PersistenceWal::new(&path);
        let first = PersistenceEvent::ServerEvent {
            kind: "first".into(),
            payload: Arc::new(json!({"n":1})),
            simulation: false,
        };
        let second = PersistenceEvent::ServerEvent {
            kind: "second".into(),
            payload: Arc::new(json!({"n":2})),
            simulation: false,
        };
        let first_id = wal.admit(first).await.unwrap();
        let _second_id = wal.admit(second).await.unwrap();
        wal.ack(first_id).await.unwrap();
        let replay = wal.replay().await.unwrap();
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].1.kind(), "second");
        assert_eq!(wal.compact().await.unwrap(), 1);
        assert_eq!(wal.replay().await.unwrap().len(), 1);
        let _ = tokio::fs::remove_file(path).await;
    }
}
