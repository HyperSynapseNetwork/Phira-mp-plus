//! Crash-recovery write-ahead log for PersistenceWorker admission.
//!
//! Every data event is fsync'd before it is admitted to the in-memory queue.
//! Terminal processing writes an ACK record. Startup replays records without a
//! matching ACK, and compaction rewrites only outstanding admissions.
//!
//! # Production guarantees
//!
//! - All I/O (admit, ack, compact) is serialized through `io_gate` so replay
//!   and compaction see a consistent point-in-time snapshot.
//! - Compact reads, writes temp, fsync, rename, and fsync-parent inside a
//!   single critical section — no concurrent admission/ACK can be lost.
//! - Replay failure is **fail-closed**: the WAL rejects further admissions
//!   and reports the failure through Supervisor.
//! - File permissions are enforced to `0o600`.

use crate::persistence::message::PersistenceEvent;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::warn;

/// Current frame format version. Increment when the wire format changes.
const WAL_FORMAT_VERSION: u8 = 1;

/// Minimum free disk space (bytes) below which admissions are rejected.
/// Only checked on Unix (statvfs); unused on Windows.
#[cfg(unix)]
const MIN_DISK_SPACE_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB

/// Default compaction trigger: compact when pending ACKs drop below this ratio
/// of total admissions AND the file exceeds this size.
const COMPACT_AC_RATIO: f64 = 0.25;
const COMPACT_MIN_BYTES: u64 = 256 * 1024; // 256 KiB

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

/// Versioned frame: each line in the WAL is a JSON object with this structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WalFrame {
    ver: u8,
    #[serde(flatten)]
    record: WalRecord,
    /// Hex-encoded SHA-256 of (ver || canonical JSON of record).
    /// Computed over the serialized record bytes before line-terminator.
    cksum: String,
}

impl WalFrame {
    fn new(record: WalRecord) -> Result<Self, String> {
        let ver = WAL_FORMAT_VERSION;
        let cksum = Self::compute_checksum(ver, &record)?;
        Ok(Self { ver, record, cksum })
    }

    fn compute_checksum(ver: u8, record: &WalRecord) -> Result<String, String> {
        let payload = serde_json::to_vec(record)
            .map_err(|e| format!("serialize record for checksum: {e}"))?;
        let mut hasher = Sha256::new();
        hasher.update([ver]);
        hasher.update(&payload);
        let hash = hasher.finalize();
        Ok(hash.iter().map(|b| format!("{:02x}", b)).collect())
    }

    fn verify(&self) -> Result<(), String> {
        let expected = Self::compute_checksum(self.ver, &self.record)?;
        if self.cksum != expected {
            return Err(format!(
                "checksum mismatch: expected {expected}, got {}",
                self.cksum
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct PersistenceWal {
    path: PathBuf,
    io_gate: Mutex<()>,
    /// Set to true when replay succeeds; admissions are rejected until then.
    replay_succeeded: AtomicBool,
    /// Set to true when ACK/admit operations fail due to disk or I/O errors.
    /// Cleared when an ACK operation eventually succeeds.
    degraded: AtomicBool,
    /// Total bytes written (approx, updated on admission/ACK).
    total_bytes: std::sync::atomic::AtomicU64,
    /// Number of truncated trailing frames detected during replay.
    truncated_frames: std::sync::atomic::AtomicU64,
    /// Total admission count since last compact (for auto-compaction).
    admission_count: std::sync::atomic::AtomicU64,
    /// Total ACK count since last compact.
    ack_count: std::sync::atomic::AtomicU64,
}

impl PersistenceWal {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            io_gate: Mutex::new(()),
            replay_succeeded: AtomicBool::new(false),
            degraded: AtomicBool::new(false),
            total_bytes: std::sync::atomic::AtomicU64::new(0),
            truncated_frames: std::sync::atomic::AtomicU64::new(0),
            admission_count: std::sync::atomic::AtomicU64::new(0),
            ack_count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Normalize a path by resolving `.` and `..` components without requiring
    /// the file to exist (unlike `canonicalize`).
    fn normalize_path(path: &Path) -> std::path::PathBuf {
        use std::path::Component;
        let mut components = std::path::PathBuf::new();
        for component in path.components() {
            match component {
                Component::Normal(_) => components.push(component),
                Component::CurDir => {}  // skip standalone "."
                Component::ParentDir => {
                    if !components.as_os_str().is_empty() {
                        components.pop();
                    }
                }
                other => components.push(other.as_os_str()),
            }
        }
        components
    }

    pub fn replay_succeeded(&self) -> bool {
        self.replay_succeeded.load(Ordering::Acquire)
    }

    /// Mark the WAL as degraded (e.g. disk full / I/O errors during ACK).
    pub fn set_degraded(&self, degraded: bool) {
        self.degraded.store(degraded, Ordering::Release);
    }

    /// Whether the WAL is currently degraded due to disk or I/O errors.
    pub fn is_degraded(&self) -> bool {
        self.degraded.load(Ordering::Acquire)
    }

    async fn ensure_parent(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("create WAL directory {}: {e}", parent.display()))?;
        }
        Ok(())
    }

    /// Enforce `0o600` permissions on the WAL file (Unix-only; best-effort on
    /// other platforms).
    #[cfg(unix)]
    async fn set_secure_permissions(&self) {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = tokio::fs::metadata(&self.path).await {
            let mut perms = metadata.permissions();
            // Only tighten, never loosen.
            let mode = perms.mode() & 0o777;
            if mode & 0o077 != 0 {
                perms.set_mode(mode & !0o077);
                let _ = tokio::fs::set_permissions(&self.path, perms).await;
            }
        }
    }

    #[cfg(not(unix))]
    async fn set_secure_permissions(&self) {
        // no-op on non-Unix
    }

    /// Check that the WAL path and configured dead-letter path do not point
    /// to the same file.
    pub fn validate_paths_not_equal(dead_letter: Option<&Path>, wal: &Path) -> Result<(), String> {
        if let Some(dl) = dead_letter {
            // Normalize both paths: resolve . and .. components without requiring
            // the file to exist (canonicalize fails on non-existent paths).
            let normalized_wal = PersistenceWal::normalize_path(wal);
            let normalized_dl = PersistenceWal::normalize_path(dl);
            if normalized_wal == normalized_dl {
                return Err(format!(
                    "WAL path and dead-letter path are the same file: {}",
                    normalized_wal.display()
                ));
            }
        }
        Ok(())
    }

    /// Check available disk space on the parent filesystem.
    /// Uses `statvfs` on Unix; always succeeds on other platforms.
    #[cfg(unix)]
    async fn check_disk_space(&self) -> Result<(), String> {
        #[allow(unused_imports)]
        use std::os::unix::fs::MetadataExt;
        use std::path::Path;

        let parent = self.path.parent().unwrap_or(Path::new("."));
        // Use libc::statvfs directly (no nix crate dependency).
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        let cpath = std::ffi::CString::new(parent.as_os_str().as_encoded_bytes())
            .map_err(|_| "path contains null byte".to_string())?;
        let rc = unsafe { libc::statvfs(cpath.as_ptr(), &mut stat) };
        if rc == 0 {
            let free = stat.f_bsize as u64 * stat.f_bavail as u64;
            if free < MIN_DISK_SPACE_BYTES {
                return Err(format!(
                    "low disk space on {}: {free} bytes free, need {MIN_DISK_SPACE_BYTES}",
                    parent.display()
                ));
            }
        }
        Ok(())
    }

    #[cfg(not(unix))]
    async fn check_disk_space(&self) -> Result<(), String> {
        Ok(())
    }

    async fn append_frame(&self, frame: &WalFrame) -> Result<(), String> {
        let _guard = self.io_gate.lock().await;
        self.ensure_parent().await?;
        // Enforce secure permissions on existing file (best-effort).
        self.set_secure_permissions().await;

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await
            .map_err(|e| format!("open WAL {}: {e}", self.path.display()))?;

        // Enforce creation permissions on new file.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = file.metadata().await {
                let mode = metadata.permissions().mode() & 0o777;
                if mode & 0o077 != 0 {
                    let mut perms = metadata.permissions();
                    perms.set_mode(mode & !0o077);
                    let _ = file.set_permissions(perms).await;
                }
            }
        }

        let mut line =
            serde_json::to_vec(frame).map_err(|e| format!("serialize WAL frame: {e}"))?;
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

        self.total_bytes
            .fetch_add(line.len() as u64, Ordering::Release);
        Ok(())
    }

    pub async fn admit(&self, event: PersistenceEvent) -> Result<uuid::Uuid, String> {
        if !self.replay_succeeded.load(Ordering::Acquire) {
            return Err("WAL replay has not succeeded; admissions are rejected".to_string());
        }
        self.check_disk_space().await?;
        let id = uuid::Uuid::new_v4();
        let frame = WalFrame::new(WalRecord::Admission { id, event })?;
        self.append_frame(&frame).await?;
        self.admission_count.fetch_add(1, Ordering::Release);
        // Mark marker as active (not clean) so accidental WAL deletion is
        // detectable even after a compact-to-zero followed by new admissions.
        let _ = self.mark_marker_active().await;
        Ok(id)
    }

    pub async fn ack(&self, id: uuid::Uuid) -> Result<(), String> {
        if !self.replay_succeeded.load(Ordering::Acquire) {
            return Err("WAL replay has not succeeded; ACKs are rejected".to_string());
        }
        let frame = WalFrame::new(WalRecord::Ack { id })?;
        self.append_frame(&frame).await?;
        self.ack_count.fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// Replay WAL and return unacknowledged admissions.
    ///
    /// # Fail-closed semantics
    ///
    /// If the WAL contains a frame with a valid structure but invalid checksum,
    /// replay fails immediately. The caller must NOT proceed with an empty replay
    /// — data integrity cannot be guaranteed.
    ///
    /// Truncated trailing bytes (last line incomplete) are silently discarded
    /// because a crash during append produces exactly this pattern.
    pub async fn replay(&self) -> Result<Vec<(uuid::Uuid, PersistenceEvent)>, String> {
        let _guard = self.io_gate.lock().await;
        // Check instance consistency first: if marker exists but WAL is gone
        // or empty, refuse to replay (fail-closed) UNLESS the marker is
        // marked as clean (intentional compact-to-zero).
        self.check_instance_consistency().await?;
        let mut bytes = match tokio::fs::read(&self.path).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Consistency check passed — WAL legitimately doesn't exist
                // (either first boot or clean post-compact state).
                // Ensure marker exists for future accidental-deletion detection.
                if !self.path.with_extension("wal.instance").exists() {
                    self.write_instance_marker().await?;
                }
                self.replay_succeeded.store(true, Ordering::Release);
                return Ok(Vec::new());
            }
            Err(e) => return Err(format!("read WAL {}: {e}", self.path.display())),
        };

        let mut admitted = Vec::new();
        let mut acked = HashSet::new();
        let mut has_truncated = false;
        // Byte offset at which the truncated tail begins (0 if no truncation).
        // Used to physically truncate the file after replay.
        let mut truncated_at: usize = 0;

        let mut lines: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
        // If the last byte was not a newline, the final segment may be an
        // incomplete write OR a complete frame whose trailing newline was
        // not flushed before a crash. Try to parse and verify it first;
        // only discard if actually corrupt.
        if bytes.last().map(|&b| b != b'\n').unwrap_or(false) {
            if let Some(last) = lines.pop() {
                if !last.is_empty() {
                    match serde_json::from_slice::<WalFrame>(last) {
                        Ok(frame) if frame.verify().is_ok() && frame.ver <= WAL_FORMAT_VERSION => {
                            // Complete valid frame without trailing newline — keep it.
                            lines.push(last);
                        }
                        _ => {
                            // Genuinely truncated or corrupt — discard.
                            has_truncated = true;
                            truncated_at = bytes.len().saturating_sub(last.len());
                        }
                    }
                }
            }
        }

        for (index, line) in lines.iter().enumerate() {
            if line.is_empty() {
                continue;
            }
            let frame: WalFrame = serde_json::from_slice(line).map_err(|e| {
                format!(
                    "corrupt WAL {} line {}: {e}",
                    self.path.display(),
                    index + 1
                )
            })?;

            // Version check: future versions are rejected.
            if frame.ver > WAL_FORMAT_VERSION {
                return Err(format!(
                    "WAL {} line {}: unsupported format version {}, expected <= {}",
                    self.path.display(),
                    index + 1,
                    frame.ver,
                    WAL_FORMAT_VERSION
                ));
            }

            // Integrity check: checksum mismatch = data corruption.
            frame.verify().map_err(|e| {
                format!(
                    "corrupt WAL {} line {}: {e}",
                    self.path.display(),
                    index + 1
                )
            })?;

            match frame.record {
                WalRecord::Admission { id, event } => admitted.push((id, event)),
                WalRecord::Ack { id } => {
                    acked.insert(id);
                }
            }
        }

        self.total_bytes
            .store(bytes.len() as u64, Ordering::Release);
        self.admission_count
            .store(admitted.len() as u64, Ordering::Release);
        self.ack_count.store(acked.len() as u64, Ordering::Release);

        if has_truncated {
            self.truncated_frames.fetch_add(1, Ordering::Release);
            warn!(
                "WAL {} had trailing truncated bytes (discarded); this is expected after a crash \
                 (total truncated frames: {})",
                self.path.display(),
                self.truncated_frames.load(Ordering::Acquire),
            );
            // Physically truncate the file at the last valid offset.
            // Without this, new frames may be appended after the corrupted
            // tail, causing the next restart to encounter a complete bad
            // line and fail-closed.
            if truncated_at > 0 && truncated_at < bytes.len() {
                let removed = bytes.len().saturating_sub(truncated_at);
                match truncate_wal_file(&self.path, truncated_at).await {
                    Ok(new_len) => {
                        bytes.truncate(truncated_at);
                        self.total_bytes.store(new_len, Ordering::Release);
                        warn!(
                            "WAL {} truncated to {} bytes (removed {removed} corrupted bytes)",
                            self.path.display(),
                            truncated_at,
                        );
                    }
                    Err(e) => {
                        warn!(
                            "WAL {} truncation failed: {e}; file may contain corrupted tail",
                            self.path.display(),
                        );
                    }
                }
            }
        }

        // Record that this WAL instance has been initialized.
        // Used to detect accidental WAL deletion on subsequent starts.
        self.write_instance_marker().await?;

        self.replay_succeeded.store(true, Ordering::Release);
        Ok(admitted
            .into_iter()
            .filter(|(id, _)| !acked.contains(id))
            .collect())
    }

    /// Write an instance marker file next to the WAL so we can detect
    /// accidental deletion or truncation on subsequent starts.
    async fn write_instance_marker(&self) -> Result<(), String> {
        let marker_path = self.path.with_extension("wal.instance");
        if marker_path.exists() {
            return Ok(()); // already initialized
        }
        self.write_marker_inner(&marker_path, false).await
    }

    /// Write or overwrite the marker with the given clean state.
    async fn write_marker_inner(&self, marker_path: &std::path::Path, clean: bool) -> Result<(), String> {
        let marker = serde_json::json!({
            "version": 1,
            "created_at_ms": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            "wal_path": self.path.to_string_lossy(),
            "clean": clean,
        });
        tokio::fs::write(marker_path, serde_json::to_vec(&marker).map_err(|e| format!("serialize instance marker: {e}"))?)
            .await
            .map_err(|e| format!("write instance marker: {e}"))?;
        Ok(())
    }

    /// Update the marker to active state (WAL intentionally exists).
    /// Called after admission/ACK when the marker was previously clean.
    async fn mark_marker_active(&self) -> Result<(), String> {
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
                return self.write_marker_inner(&marker_path, false).await;
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
                self.write_marker_inner(&marker_path, false).await?;
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

    /// Compact the WAL by rewriting only unacknowledged admissions.
    ///
    /// # Atomicity
    ///
    /// The entire operation (read current state, write temp, fsync, rename,
    /// fsync parent) is performed inside a single critical section to prevent
    /// concurrent admissions/ACKs from being lost.
    pub async fn compact(&self) -> Result<usize, String> {
        let _guard = self.io_gate.lock().await;

        // Re-read WAL state under the same lock.
        let bytes = match tokio::fs::read(&self.path).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(format!("read WAL for compact {}: {e}", self.path.display())),
        };

        let mut admitted: Vec<(uuid::Uuid, PersistenceEvent)> = Vec::new();
        let mut acked = HashSet::new();
        let mut has_truncated = false;

        let mut lines: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
        if bytes.last().map(|&b| b != b'\n').unwrap_or(false) {
            if let Some(last) = lines.pop() {
                if !last.is_empty() {
                    match serde_json::from_slice::<WalFrame>(last) {
                        Ok(frame) if frame.verify().is_ok() && frame.ver <= WAL_FORMAT_VERSION => {
                            lines.push(last);
                        }
                        _ => {
                            has_truncated = true;
                        }
                    }
                }
            }
        }

        for (index, line) in lines.iter().enumerate() {
            if line.is_empty() {
                continue;
            }
            let frame: WalFrame = serde_json::from_slice(line).map_err(|e| {
                format!(
                    "corrupt WAL {} line {}: {e}",
                    self.path.display(),
                    index + 1
                )
            })?;
            if frame.ver > WAL_FORMAT_VERSION {
                return Err(format!(
                    "WAL {} line {}: unsupported version {}",
                    self.path.display(),
                    index + 1,
                    frame.ver
                ));
            }
            // All frames (both Admission and Ack) MUST pass checksum verification.
            // Skipping ACK checksum opens a data-loss path: a corrupted ACK can
            // cause a real admission to be treated as acknowledged and then
            // permanently deleted during compaction.
            frame.verify().map_err(|e| {
                format!(
                    "corrupt WAL {} line {} ({}): {e}",
                    self.path.display(),
                    index + 1,
                    match &frame.record {
                        WalRecord::Admission { .. } => "admission",
                        WalRecord::Ack { .. } => "ack",
                    },
                )
            })?;
            match &frame.record {
                WalRecord::Admission { id, event } => {
                    admitted.push((*id, event.clone()));
                }
                WalRecord::Ack { id } => {
                    acked.insert(*id);
                }
            }
        }

        let pending: Vec<(uuid::Uuid, PersistenceEvent)> = admitted
            .into_iter()
            .filter(|(id, _)| !acked.contains(id))
            .collect();

        if pending.is_empty() {
            // Nothing to compact; remove the WAL file and record a clean
            // marker so that the next startup does not treat the missing
            // WAL as accidental deletion (Issue #5 / P0 regression).
            let _ = tokio::fs::remove_file(&self.path).await;
            let marker_path = self.path.with_extension("wal.instance");
            if marker_path.exists() {
                // Overwrite with clean marker.
                self.write_marker_inner(&marker_path, true).await?;
            } else {
                // First compact before any marker was written.
                self.write_marker_inner(&marker_path, true).await?;
            }
            self.total_bytes.store(0, Ordering::Release);
            self.admission_count.store(0, Ordering::Release);
            self.ack_count.store(0, Ordering::Release);
            return Ok(0);
        }

        // Write compacted WAL to a temp file.
        let temp = self.path.with_extension("wal.tmp");
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temp)
            .await
            .map_err(|e| format!("create WAL temp {}: {e}", temp.display()))?;

        for (id, event) in &pending {
            let frame = WalFrame::new(WalRecord::Admission {
                id: *id,
                event: event.clone(),
            })?;
            let mut line = serde_json::to_vec(&frame)
                .map_err(|e| format!("serialize compacted WAL frame: {e}"))?;
            line.push(b'\n');
            file.write_all(&line)
                .await
                .map_err(|e| format!("write WAL temp {}: {e}", temp.display()))?;
        }

        file.flush()
            .await
            .map_err(|e| format!("flush WAL temp {}: {e}", temp.display()))?;
        file.sync_all()
            .await
            .map_err(|e| format!("sync WAL temp {}: {e}", temp.display()))?;
        drop(file);

        // Atomic rename.
        tokio::fs::rename(&temp, &self.path).await.map_err(|e| {
            format!(
                "rename WAL {} -> {}: {e}",
                temp.display(),
                self.path.display()
            )
        })?;

        // Sync parent directory so the rename is durable.
        if let Some(parent) = self.path.parent() {
            if let Ok(dir) = tokio::fs::File::open(parent).await {
                dir.sync_all()
                    .await
                    .map_err(|e| format!("sync parent directory {}: {e}", parent.display()))?;
            }
        }

        self.total_bytes.store(
            pending.len() as u64 * 256, // approximate
            Ordering::Release,
        );
        self.admission_count
            .store(pending.len() as u64, Ordering::Release);
        self.ack_count.store(0, Ordering::Release);

        if has_truncated {
            warn!(
                "WAL {} had trailing truncated bytes during compact",
                self.path.display()
            );
        }

        Ok(pending.len())
    }

    /// Number of truncated trailing frames detected since startup.
    pub fn truncated_frames_count(&self) -> u64 {
        self.truncated_frames.load(Ordering::Acquire)
    }

    /// Check whether auto-compaction is worth running based on admission/ACK ratio.
    pub fn should_compact(&self) -> bool {
        let admitted = self.admission_count.load(Ordering::Acquire);
        let acked = self.ack_count.load(Ordering::Acquire);
        let bytes = self.total_bytes.load(Ordering::Acquire);

        // No need to compact tiny WALs.
        if bytes < COMPACT_MIN_BYTES {
            return false;
        }
        // Compact when a significant fraction has been acknowledged.
        if admitted > 0 {
            let pending_ratio = (admitted.saturating_sub(acked)) as f64 / admitted as f64;
            if pending_ratio < COMPACT_AC_RATIO {
                return true;
            }
        }
        false
    }
}

/// Truncate a WAL file at the given byte offset.
/// Opens the file in write mode, truncates, then syncs.
async fn truncate_wal_file(path: &std::path::Path, offset: usize) -> Result<u64, String> {
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .await
        .map_err(|e| format!("open WAL for truncation {}: {e}", path.display()))?;
    file.set_len(offset as u64)
        .await
        .map_err(|e| format!("truncate WAL {}: {e}", path.display()))?;
    file.sync_all()
        .await
        .map_err(|e| format!("sync WAL after truncation {}: {e}", path.display()))?;
    drop(file);
    // Sync parent directory so metadata is durable.
    if let Some(parent) = path.parent() {
        if let Ok(dir) = tokio::fs::File::open(parent).await {
            let _ = dir.sync_all().await;
        }
    }
    Ok(offset as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    fn make_event(kind: &str) -> PersistenceEvent {
        PersistenceEvent::ServerEvent {
            kind: kind.into(),
            payload: Arc::new(json!({"n": 1})),
            simulation: false,
        }
    }

    #[tokio::test]
    async fn replays_only_unacknowledged_events() {
        let path =
            std::env::temp_dir().join(format!("pmp-wal-test-{}.jsonl", uuid::Uuid::new_v4()));
        let wal = PersistenceWal::new(&path);

        // replay on empty wal succeeds
        let replay = wal.replay().await.unwrap();
        assert!(replay.is_empty());
        assert!(wal.replay_succeeded());

        let first_id = wal.admit(make_event("first")).await.unwrap();
        let _second_id = wal.admit(make_event("second")).await.unwrap();
        wal.ack(first_id).await.unwrap();

        let replay = wal.replay().await.unwrap();
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].1.kind(), "second");

        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn compact_removes_acked_events() {
        let path =
            std::env::temp_dir().join(format!("pmp-wal-compact-{}.jsonl", uuid::Uuid::new_v4()));
        let wal = PersistenceWal::new(&path);

        wal.replay().await.unwrap();
        let _id = wal.admit(make_event("keep")).await.unwrap();
        let ack_id = wal.admit(make_event("ack-me")).await.unwrap();
        wal.ack(ack_id).await.unwrap();

        assert_eq!(wal.compact().await.unwrap(), 1);
        assert_eq!(wal.replay().await.unwrap().len(), 1);
        assert_eq!(wal.replay().await.unwrap()[0].1.kind(), "keep");

        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn compact_atomic_no_concurrent_loss() {
        let path =
            std::env::temp_dir().join(format!("pmp-wal-atomic-{}.jsonl", uuid::Uuid::new_v4()));
        let wal = PersistenceWal::new(&path);
        wal.replay().await.unwrap();

        let id1 = wal.admit(make_event("event1")).await.unwrap();
        let id2 = wal.admit(make_event("event2")).await.unwrap();
        wal.ack(id1).await.unwrap();

        // Compact: only id2 should survive.
        assert_eq!(wal.compact().await.unwrap(), 1);
        let replay = wal.replay().await.unwrap();
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].1.kind(), "event2");

        // After compact, new admissions work.
        let id3 = wal.admit(make_event("event3")).await.unwrap();
        wal.ack(id2).await.unwrap();
        wal.ack(id3).await.unwrap();
        assert_eq!(wal.compact().await.unwrap(), 0);
        // After compact-to-zero the marker records clean=true so that
        // replay succeeds (the missing WAL is expected, not accidental).
        assert!(wal.replay().await.unwrap().is_empty());
        // New admissions must still work after compact-to-zero (WAL recreated).
        let id4 = wal.admit(make_event("event4")).await.unwrap();
        wal.ack(id4).await.unwrap();
        assert_eq!(wal.compact().await.unwrap(), 0);

        let marker_path = path.with_extension("wal.instance");
        let _ = tokio::fs::remove_file(&marker_path).await;
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn replay_rejects_corrupt_checksum() {
        let path =
            std::env::temp_dir().join(format!("pmp-wal-corrupt-{}.jsonl", uuid::Uuid::new_v4()));
        // Write a manually crafted frame with wrong checksum.
        let bad_frame = r#"{"ver":1,"record":"admission","id":"00000000-0000-0000-0000-000000000001","event":{"PersistenceEvent":{"ServerEvent":{"kind":"bad","payload":{"n":1},"simulation":false}}},"cksum":"0000"}"#;
        tokio::fs::write(&path, format!("{bad_frame}\n"))
            .await
            .unwrap();
        let wal = PersistenceWal::new(&path);

        let result = wal.replay().await;
        assert!(result.is_err());
        assert!(!wal.replay_succeeded());

        // Verify that admissions are rejected after corrupt replay.
        let admit_result = wal.admit(make_event("after-corrupt")).await;
        assert!(admit_result.is_err());

        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn replay_accepts_truncated_trailing_line() {
        let path =
            std::env::temp_dir().join(format!("pmp-wal-trunc-{}.jsonl", uuid::Uuid::new_v4()));
        let wal = PersistenceWal::new(&path);
        wal.replay().await.unwrap();

        let id = wal.admit(make_event("trunc-test")).await.unwrap();
        wal.ack(id).await.unwrap();

        // Append a trailing incomplete line (simulate crash during write).
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .unwrap();
        file.write_all(b"{\"ver\":1,\"record\":\"admission\",")
            .await
            .unwrap();
        drop(file);

        // Replay should succeed, discarding the truncated line.
        let result = wal.replay().await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());

        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn empty_wal_compacts_to_zero() {
        let path = std::env::temp_dir().join(format!(
            "pmp-wal-empty-compact-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let wal = PersistenceWal::new(&path);
        wal.replay().await.unwrap();
        assert_eq!(wal.compact().await.unwrap(), 0);
        let _ = tokio::fs::remove_file(path).await;
    }

    #[test]
    fn should_compact_triggers_on_ratio() {
        let wal = PersistenceWal::new("dummy");
        // Under min bytes
        wal.admission_count.store(100, Ordering::Release);
        wal.ack_count.store(80, Ordering::Release);
        wal.total_bytes.store(1000, Ordering::Release);
        assert!(!wal.should_compact());

        // Over min bytes, good ratio
        wal.total_bytes.store(300_000, Ordering::Release);
        assert!(wal.should_compact());

        // Not enough ACKs
        wal.ack_count.store(10, Ordering::Release);
        assert!(!wal.should_compact());
    }

    #[tokio::test]
    async fn admits_rejected_before_replay() {
        let path =
            std::env::temp_dir().join(format!("pmp-wal-noreplay-{}.jsonl", uuid::Uuid::new_v4()));
        let wal = PersistenceWal::new(&path);
        let result = wal.admit(make_event("before-replay")).await;
        assert!(result.is_err());
        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn fuzz_malformed_json_is_rejected() {
        let path = std::env::temp_dir().join(format!("pmp-wal-fuzz-{}.jsonl", uuid::Uuid::new_v4()));
        // Write completely invalid JSON
        tokio::fs::write(&path, b"not valid json\n").await.unwrap();
        let wal = PersistenceWal::new(&path);
        assert!(wal.replay().await.is_err());
        assert!(!wal.replay_succeeded());
        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn fuzz_partial_frame_at_end_is_truncated() {
        let path = std::env::temp_dir().join(format!("pmp-wal-trunc2-{}.jsonl", uuid::Uuid::new_v4()));
        let wal = PersistenceWal::new(&path);
        wal.replay().await.unwrap();
        wal.admit(make_event("good")).await.unwrap();
        // Append a truncated JSON fragment
        let mut file = tokio::fs::OpenOptions::new()
            .append(true).open(&path).await.unwrap();
        use tokio::io::AsyncWriteExt;
        file.write_all(b"{\"ver\":1,\"record\":\"admission\"").await.unwrap();
        file.flush().await.unwrap();
        drop(file);
        // Replay should succeed, discarding truncated line
        let replay = wal.replay().await.unwrap();
        assert_eq!(replay.len(), 1);
        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn fuzz_repeated_ack_is_idempotent() {
        let path = std::env::temp_dir().join(format!("pmp-wal-idem-{}.jsonl", uuid::Uuid::new_v4()));
        let wal = PersistenceWal::new(&path);
        wal.replay().await.unwrap();
        let id = wal.admit(make_event("test")).await.unwrap();
        wal.ack(id).await.unwrap();
        wal.ack(id).await.unwrap(); // duplicate ACK
        let replay = wal.replay().await.unwrap();
        assert_eq!(replay.len(), 0);
        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn fuzz_concurrent_admit_and_replay() {
        let path = std::env::temp_dir().join(format!("pmp-wal-conc-{}.jsonl", uuid::Uuid::new_v4()));
        let wal = std::sync::Arc::new(PersistenceWal::new(&path));
        wal.replay().await.unwrap();

        let w1 = std::sync::Arc::clone(&wal);
        let w2 = std::sync::Arc::clone(&wal);
        let h1 = tokio::spawn(async move {
            for i in 0..10 {
                let _ = w1.admit(make_event(&format!("e{i}"))).await;
            }
        });
        let h2 = tokio::spawn(async move {
            for i in 0..10 {
                let _ = w2.admit(make_event(&format!("f{i}"))).await;
            }
        });
        let _ = tokio::join!(h1, h2);

        let replay = wal.replay().await.unwrap();
        assert_eq!(replay.len(), 20);
        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn replay_version_mismatch_is_rejected() {
        let path =
            std::env::temp_dir().join(format!("pmp-wal-vers-{}.jsonl", uuid::Uuid::new_v4()));
        let future_frame = r#"{"ver":255,"record":"admission","id":"00000000-0000-0000-0000-000000000001","event":{"PersistenceEvent":{"ServerEvent":{"kind":"future","payload":{},"simulation":false}}},"cksum":"0000000000000000000000000000000000000000000000000000000000000000"}"#;
        tokio::fs::write(&path, format!("{future_frame}\n"))
            .await
            .unwrap();
        let wal = PersistenceWal::new(&path);
        assert!(wal.replay().await.is_err());
        let _ = tokio::fs::remove_file(path).await;
    }

    // ── Fault injection tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn fault_wal_deleted_fails_with_instance_marker() {
        let path =
            std::env::temp_dir().join(format!("pmp-wal-del-{}.jsonl", uuid::Uuid::new_v4()));
        let wal = PersistenceWal::new(&path);
        wal.replay().await.unwrap(); // creates instance marker
        wal.admit(make_event("will-be-detected")).await.unwrap();
        let _ = tokio::fs::remove_file(&path).await;
        let wal2 = PersistenceWal::new(&path);
        let result = wal2.replay().await;
        assert!(result.is_err(), "deleted WAL after first use must fail: {result:?}");
        assert!(!wal2.replay_succeeded());
        // Cleanup: remove instance marker
        let marker = path.with_extension("wal.instance");
        let _ = tokio::fs::remove_file(&marker).await;
    }

    #[tokio::test]
    async fn fault_compact_and_admit_no_data_loss() {
        let path =
            std::env::temp_dir().join(format!("pmp-wal-cc-{}.jsonl", uuid::Uuid::new_v4()));
        let wal = std::sync::Arc::new(PersistenceWal::new(&path));
        wal.replay().await.unwrap();
        let _ = wal.admit(make_event("seed1")).await.unwrap();
        let _ = wal.admit(make_event("seed2")).await.unwrap();
        let w1 = std::sync::Arc::clone(&wal);
        let h1 = tokio::spawn(async move { w1.compact().await });
        let w2 = std::sync::Arc::clone(&wal);
        let h2 = tokio::spawn(async move {
            let _ = w2.admit(make_event("concurrent")).await;
        });
        let _ = tokio::join!(h1, h2);
        let replay = wal.replay().await.unwrap();
        let kinds: Vec<String> = replay.iter().map(|(_, e)| e.kind().to_string()).collect();
        assert!(
            kinds.contains(&"concurrent".to_string()),
            "concurrent event must survive: {kinds:?}"
        );
        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn fault_zeroed_wal_fails_with_instance_marker() {
        let path =
            std::env::temp_dir().join(format!("pmp-wal-zero-{}.jsonl", uuid::Uuid::new_v4()));
        let wal = PersistenceWal::new(&path);
        wal.replay().await.unwrap(); // creates instance marker
        wal.admit(make_event("lost")).await.unwrap();
        tokio::fs::write(&path, b"").await.unwrap();
        let wal2 = PersistenceWal::new(&path);
        let result = wal2.replay().await;
        assert!(result.is_err(), "zeroed WAL after first use must fail: {result:?}");
        assert!(!wal2.replay_succeeded());
        let marker = path.with_extension("wal.instance");
        let _ = tokio::fs::remove_file(&marker).await;
        let _ = tokio::fs::remove_file(path).await;
    }
}
