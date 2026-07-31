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
    ///
    /// The first-boot marker is written CLEAN (no WAL yet).  It only becomes
    /// active after the first admission (mark_marker_active).  This prevents a
    /// run that never persists any event from leaving an active marker with no
    /// WAL file — which the next boot would misread as accidental deletion
    /// (PMP37 P0-A).
    pub(crate) async fn write_instance_marker(&self) -> Result<(), String> {
        let marker_path = self.path.with_extension("wal.instance");
        if marker_path.exists() {
            return Ok(()); // already initialized
        }
        let max_sequence = self.admit_sequence.load(Ordering::Acquire);
        self.write_marker_inner(&marker_path, true, max_sequence).await
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
        // Atomic + durable write (P0-E): write to a temp sibling, fsync, then
        // atomically rename over the marker, then fsync the parent directory.
        // A crash mid-write leaves the OLD marker intact (never a torn file).
        use tokio::io::AsyncWriteExt;
        let tmp_path = marker_path.with_extension("wal.instance.tmp");
        let result = async {
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&tmp_path)
                .await
                .map_err(|e| format!("create instance marker tmp {}: {e}", tmp_path.display()))?;
            // Tighten permissions to owner read/write.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = file
                    .set_permissions(std::fs::Permissions::from_mode(0o600))
                    .await;
            }
            file.write_all(&serde_json::to_vec(&marker).map_err(|e| format!("serialize instance marker: {e}"))?)
                .await
                .map_err(|e| format!("write instance marker: {e}"))?;
            file.sync_all()
                .await
                .map_err(|e| format!("sync instance marker: {e}"))?;
            drop(file);
            tokio::fs::rename(&tmp_path, marker_path)
                .await
                .map_err(|e| format!("rename instance marker {} -> {}: {e}", tmp_path.display(), marker_path.display()))?;
            // Parent fsync failures are propagated (P1).
            if let Some(parent) = marker_path.parent().filter(|p| !p.as_os_str().is_empty()) {
                let dir = tokio::fs::File::open(parent)
                    .await
                    .map_err(|e| format!("open marker parent {}: {e}", parent.display()))?;
                dir.sync_all()
                    .await
                    .map_err(|e| format!("sync marker parent {}: {e}", parent.display()))?;
            }
            Ok::<(), String>(())
        }
        .await;
        // Clean up a leftover temp file regardless of outcome.
        let _ = tokio::fs::remove_file(&tmp_path).await;
        result
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
    /// Called after admission when the marker was previously clean or missing.
    ///
    /// P0-D: the marker must exist once the WAL holds durable data.  A runtime
    /// missing marker (manual deletion, partial directory loss, mount failure)
    /// is abnormal and is recreated ACTIVE with the current high-water
    /// sequence inside the io_gate (the caller `admit` holds it).  Recreation
    /// failure degrades the deletion guard — it must NOT return a plain Ok.
    ///
    /// An already-ACTIVE marker is verified (parsed) and the MARKER degraded
    /// reason is cleared, self-healing the case where a rename succeeded but
    /// the parent fsync failed (audit §9 P0-05).
    pub(crate) async fn mark_marker_active(&self) -> Result<(), String> {
        let marker_path = self.path.with_extension("wal.instance");
        if !marker_path.exists() {
            // WAL has durable data but the deletion guard is missing.  Recreate
            // it as ACTIVE (clean=false) — a CLEAN marker would mask the loss.
            let max_sequence = self.admit_sequence.load(Ordering::Acquire);
            match self.write_marker_inner(&marker_path, false, max_sequence).await {
                Ok(()) => {
                    self.clear_marker_degraded();
                    Ok(())
                }
                Err(e) => {
                    self.mark_degraded(super::DEGRADED_MARKER);
                    Err(format!(
                        "recreate active instance marker after runtime loss: {e}"
                    ))
                }
            }
        } else {
            // Marker exists — read and verify it.
            let content = tokio::fs::read_to_string(&marker_path).await.map_err(|e| {
                self.mark_degraded(super::DEGRADED_MARKER);
                format!("read marker: {e}")
            })?;
            match serde_json::from_str::<serde_json::Value>(&content) {
                Ok(val) => {
                    if val.get("clean").and_then(|c| c.as_bool()).unwrap_or(false) {
                        // Preserve the recorded high-water mark so it survives
                        // the clean→active rewrite.
                        let old_max = val
                            .get("max_sequence")
                            .and_then(|s| s.as_u64())
                            .unwrap_or(0);
                        let r = self.write_marker_inner(&marker_path, false, old_max).await;
                        if r.is_ok() {
                            // Marker successfully rewritten active — clear the
                            // marker degraded reason (P1).
                            self.clear_marker_degraded();
                        }
                        return r;
                    }
                    // Already ACTIVE.  Verified it parses — clear the marker
                    // degraded reason.  This self-heals the parent-fsync-failure
                    // case where the marker is actually active on disk while
                    // MARKER degraded was latched (P0-D / §9 P0-05).
                    self.clear_marker_degraded();
                    Ok(())
                }
                Err(e) => {
                    // Marker exists but is corrupt/unreadable — mark degraded.
                    self.mark_degraded(super::DEGRADED_MARKER);
                    Err(format!("marker parse failed: {e}"))
                }
            }
        }
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
                // Compact-to-zero removed the WAL and marked the marker clean.
                // KEEP it clean — the marker only becomes active once the first
                // new admission succeeds (mark_marker_active is called from
                // admit).  Previously this rewrote clean→active eagerly, so two
                // consecutive idle restarts would make a legitimately-missing
                // WAL look like accidental deletion (PMP36 P0-03).
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
            if is_clean {
                // P1 (§17/§24): a CLEAN marker + empty WAL is equivalent to
                // "no WAL".  This can happen when the first admission fails
                // and rolls back to length 0, leaving an empty file behind
                // after an unclean exit (no compact ran).  Delete the empty
                // file and continue — the instance never held durable data.
                let _ = tokio::fs::remove_file(&self.path).await;
                return Ok(());
            }
            return Err(format!(
                "WAL instance marker exists but WAL file {} is empty (corruption or truncation).",
                self.path.display()
            ));
        }
        Ok(())
    }
}
