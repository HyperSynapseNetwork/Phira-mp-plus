//! WAL instance marker persistence.
//!
//! The instance marker (`*.wal.instance`) detects accidental WAL deletion or
//! truncation between boots, and records the `max_sequence` high-water mark so
//! admission sequence numbers never regress across restarts/compactions.
//!
//! Extracted from `wal/mod.rs` to keep the core admit/ack/replay/compact state
//! machine separate from the instance-lifecycle bookkeeping.

use super::PersistenceWal;
use std::sync::atomic::Ordering;

impl PersistenceWal {
    /// Write an instance marker file next to the WAL so we can detect
    /// accidental deletion or truncation on subsequent starts.
    pub(crate) async fn write_instance_marker(&self) -> Result<(), String> {
        let marker_path = self.path.with_extension("wal.instance");
        if marker_path.exists() {
            return Ok(()); // already initialized
        }
        let max_sequence = self.admit_sequence.load(Ordering::Acquire);
        self.write_marker_inner(&marker_path, false, max_sequence).await
    }

    /// Write or overwrite the marker with the given clean state and high-water
    /// sequence.
    ///
    /// `max_sequence` is the highest assigned sequence, persisted so a
    /// subsequent boot can restore the counter to at least this high-water
    /// mark — even when the WAL has been compacted and no longer contains the
    /// historical high sequences (P1: sequence numbers must not regress across
    /// restarts).
    pub(crate) async fn write_marker_inner(
        &self,
        marker_path: &std::path::Path,
        clean: bool,
        max_sequence: u64,
    ) -> Result<(), String> {
        let marker = serde_json::json!({
            "version": 1,
            "created_at_ms": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            "wal_path": self.path.to_string_lossy(),
            "clean": clean,
            "max_sequence": max_sequence,
        });
        tokio::fs::write(marker_path, serde_json::to_vec(&marker).map_err(|e| format!("serialize instance marker: {e}"))?)
            .await
            .map_err(|e| format!("write instance marker: {e}"))?;
        Ok(())
    }

    /// Read the max_sequence recorded in the instance marker, if any.
    /// Returns 0 when the marker is absent or lacks the field.
    pub(crate) async fn read_marker_max_sequence(&self) -> u64 {
        let marker_path = self.path.with_extension("wal.instance");
        let Ok(content) = tokio::fs::read_to_string(&marker_path).await else {
            return 0;
        };
        serde_json::from_str::<serde_json::Value>(&content)
            .ok()
            .and_then(|v| v.get("max_sequence").and_then(|s| s.as_u64()))
            .unwrap_or(0)
    }

    /// Update the marker to active state (WAL intentionally exists).
    /// Called after admission/ACK when the marker was previously clean.
    pub(crate) async fn mark_marker_active(&self) -> Result<(), String> {
        let marker_path = self.path.with_extension("wal.instance");
        if !marker_path.exists() {
            return Ok(()); // no marker yet, will be created on first write
        }
        // Only rewrite if currently clean — avoids unnecessary I/O.
        let content = tokio::fs::read_to_string(&marker_path)
            .await
            .map_err(|e| format!("read marker: {e}"))?;
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
            if val.get("clean").and_then(|c| c.as_bool()).unwrap_or(false) {
                // Preserve the recorded high-water mark so it survives the
                // clean→active rewrite.
                let old_max = val
                    .get("max_sequence")
                    .and_then(|s| s.as_u64())
                    .unwrap_or(0);
                return self.write_marker_inner(&marker_path, false, old_max).await;
            }
        }
        Ok(())
    }

    /// Check if the instance marker exists but the WAL file is gone or empty.
    /// This indicates accidental WAL deletion after first use, UNLESS the
    /// marker has `"clean": true` which means compaction intentionally removed
    /// the WAL after all ACKs were confirmed.
    pub async fn check_instance_consistency(&self) -> Result<(), String> {
        let marker_path = self.path.with_extension("wal.instance");
        if !marker_path.exists() {
            return Ok(()); // first start, no instance yet
        }

        // Read the marker to check clean flag.
        let marker_content = tokio::fs::read_to_string(&marker_path)
            .await
            .map_err(|e| format!("read marker {}: {e}", marker_path.display()))?;
        let marker: serde_json::Value = serde_json::from_str(&marker_content)
            .map_err(|e| format!("parse marker {}: {e}", marker_path.display()))?;
        let is_clean = marker.get("clean").and_then(|c| c.as_bool()).unwrap_or(false);

        let wal_exists = tokio::fs::try_exists(&self.path).await.unwrap_or(false);
        if !wal_exists {
            if is_clean {
                // Compact-to-zero left the marker as clean.  Re-write it as
                // active so the next accidental deletion IS detected.
                // Preserve the recorded high-water mark — re-writing with this
                // instance's (possibly un-restored) counter would regress it.
                let old_max = marker
                    .get("max_sequence")
                    .and_then(|s| s.as_u64())
                    .unwrap_or(0);
                self.write_marker_inner(&marker_path, false, old_max).await?;
                return Ok(());
            }
            // Also accept markers created before the "clean" field existed
            // (backward compat: old markers simply have no clean field).
            return Err(format!(
                "WAL instance marker exists at {} but WAL file {} is missing. Remove the marker file manually to reinitialize.",
                marker_path.display(),
                self.path.display()
            ));
        }
        let metadata = tokio::fs::metadata(&self.path).await
            .map_err(|e| format!("stat WAL: {e}"))?;
        if metadata.len() == 0 {
            return Err(format!(
                "WAL instance marker exists but WAL file {} is empty (corruption or truncation).",
                self.path.display()
            ));
        }
        Ok(())
    }
}
