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

mod marker;

use std::collections::HashSet;
use serde::{Deserialize, Serialize};
use crate::persistence::message::PersistenceEvent;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::warn;

/// Current frame format version. Increment when the wire format changes.
const WAL_FORMAT_VERSION: u8 = 2;

/// Degraded-reason bit flags (P0-F): each has an independent recovery
/// condition, and an ACK success clears only the ACK bit.
pub(crate) const DEGRADED_ACK: u8 = 1 << 0;
pub(crate) const DEGRADED_CORRUPTION: u8 = 1 << 1;
pub(crate) const DEGRADED_MARKER: u8 = 1 << 2;
pub(crate) const DEGRADED_COMPACT: u8 = 1 << 3;

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
        #[serde(default, skip_serializing_if = "is_zero")]
        sequence: u64,
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

/// Helper for `#[serde(skip_serializing_if)]` on the `sequence` field.
/// When `sequence` is 0 (i.e. a v1 record parsed with the default), omit it
/// from serialization so that v1 checksums continue to verify.
fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[derive(Debug)]
pub struct PersistenceWal {
    path: PathBuf,
    io_gate: Mutex<()>,
    /// Set to true when replay succeeds; admissions are rejected until then.
    replay_succeeded: AtomicBool,
    /// Bitmask of independent WAL degradation reasons (P0-F).  Each reason has
    /// its own recovery condition; an ACK success clears only the ACK bit.
    degraded: std::sync::atomic::AtomicU8,
    /// Monotonically increasing admission sequence counter.
    /// The single counter for all WAL sequences. Restored from max seen
    /// during replay so new admits never conflict with replayed entries.
    admit_sequence: std::sync::atomic::AtomicU64,
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
            degraded: std::sync::atomic::AtomicU8::new(0),
            admit_sequence: std::sync::atomic::AtomicU64::new(0),
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

    /// Mark the WAL as degraded for the given reason bit.  Independent reasons
    /// coexist; clearing one does not clear the others (P0-F).
    pub fn mark_degraded(&self, reason: u8) {
        self.degraded.fetch_or(reason, Ordering::AcqRel);
    }

    /// Clear an ACK-related degraded reason.  Corruption/marker/compact
    /// reasons are NOT cleared by ACK success.
    pub fn clear_ack_degraded(&self) {
        self.degraded.fetch_and(!DEGRADED_ACK, Ordering::AcqRel);
    }

    /// Whether the WAL is currently degraded (any reason set).
    pub fn is_degraded(&self) -> bool {
        self.degraded.load(Ordering::Acquire) != 0
    }

    /// The set of active degradation reason names.
    pub fn degraded_reasons(&self) -> Vec<&'static str> {
        let mask = self.degraded.load(Ordering::Acquire);
        let mut out = Vec::new();
        if mask & DEGRADED_ACK != 0 { out.push("ack"); }
        if mask & DEGRADED_CORRUPTION != 0 { out.push("corruption"); }
        if mask & DEGRADED_MARKER != 0 { out.push("marker"); }
        if mask & DEGRADED_COMPACT != 0 { out.push("compact"); }
        out
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

    /// Append a frame, acquiring the io_gate for mutual exclusion.
    async fn append_frame(&self, frame: &WalFrame) -> Result<(), String> {
        let _guard = self.io_gate.lock().await;
        self.append_frame_inner(frame).await
    }

    /// Append a frame.  Caller MUST hold `io_gate`.
    async fn append_frame_inner(&self, frame: &WalFrame) -> Result<(), String> {
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

        // Record the file length before writing so a partial frame can be
        // truncated back on failure (P0-B).
        let original_len = file
            .metadata()
            .await
            .map(|m| m.len())
            .unwrap_or(0);

        let write_result = async {
            file.write_all(&line).await
                .map_err(|e| format!("append WAL {}: {e}", self.path.display()))?;
            file.flush().await
                .map_err(|e| format!("flush WAL {}: {e}", self.path.display()))?;
            file.sync_data().await
                .map_err(|e| format!("sync WAL {}: {e}", self.path.display()))?;
            Ok::<(), String>(())
        }
        .await;
        if let Err(e) = write_result {
            // Roll back the partial frame if possible.  If truncation fails,
            // the WAL is corrupt — mark fatal degraded.
            let trunc = file
                .set_len(original_len)
                .await
                .map_err(|te| format!("truncate WAL after failed append: {te}"));
            match trunc {
                Ok(()) => {
                    return Err(format!(
                        "append WAL {} failed (rolled back {original_len} bytes): {e}",
                        self.path.display()
                    ));
                }
                Err(te) => {
                    self.mark_degraded(DEGRADED_CORRUPTION);
                    return Err(format!(
                        "append WAL {} failed AND truncate-back failed — WAL corrupt: {te}"
                    ));
                }
            }
        }

        self.total_bytes
            .fetch_add(line.len() as u64, Ordering::Release);
        Ok(())
    }

    /// Append a trailing newline if the WAL does not end with one.  Called
    /// during replay when a complete valid frame is found without a trailing
    /// newline (crash between write and newline).  Without normalization the
    /// next append would write immediately after the frame's `}`, merging two
    /// frames into one corrupt line (PMP38 P0-B).  Caller MUST hold `io_gate`.
    async fn append_missing_newline(&self) -> Result<(), String> {
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .await
            .map_err(|e| format!("open WAL {}: {e}", self.path.display()))?;
        file.write_all(b"\n")
            .await
            .map_err(|e| format!("append newline to WAL {}: {e}", self.path.display()))?;
        file.flush()
            .await
            .map_err(|e| format!("flush WAL {}: {e}", self.path.display()))?;
        file.sync_data()
            .await
            .map_err(|e| format!("sync WAL {}: {e}", self.path.display()))?;
        self.total_bytes.fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// Return the current admission sequence counter value.
    /// The next admit will receive this value (post-increment).
    pub fn current_sequence(&self) -> u64 {
        self.admit_sequence.load(Ordering::Acquire)
    }

    pub async fn admit(&self, event: PersistenceEvent) -> Result<(uuid::Uuid, u64), String> {
        if !self.replay_succeeded.load(Ordering::Acquire) {
            return Err("WAL replay has not succeeded; admissions are rejected".to_string());
        }
        self.check_disk_space().await?;
        let id = uuid::Uuid::new_v4();

        // Allocate the sequence number INSIDE the io_gate critical section,
        // serialized with the append+fsync.  Previously the sequence was
        // allocated with fetch_add BEFORE append_frame; if the append failed
        // (disk full, fsync error) the sequence was consumed but no frame was
        // written, creating a permanent gap that could deadlock the worker's
        // sequence gating (it would wait forever for a sequence that never
        // appears).  With load+store inside the gate, a failed append leaves
        // the counter unchanged — the next admission retries the same sequence.
        let _guard = self.io_gate.lock().await;
        let seq = self.admit_sequence.load(Ordering::Acquire) + 1;
        let frame = WalFrame::new(WalRecord::Admission { id, event, sequence: seq })?;
        self.append_frame_inner(&frame).await?;
        self.admit_sequence.store(seq, Ordering::Release);
        self.admission_count.fetch_add(1, Ordering::Release);
        // Mark marker as active (not clean) so accidental WAL deletion is
        // detectable even after a compact-to-zero followed by new admissions.
        // The WAL frame is already durably fsync'd, so a marker failure must
        // NOT reject the admission (the event is safe).  Instead mark the
        // deletion guard degraded — the caller sees AdmittedDegraded (P0-A).
        if let Err(e) = self.mark_marker_active().await {
            self.mark_degraded(DEGRADED_MARKER);
            tracing::warn!(
                wal_id = %id, error = %e,
                "WAL frame durable but instance marker update failed (guard degraded)"
            );
        }
        Ok((id, seq))
    }

    /// Whether the deletion-guard marker is degraded (admissions are durable
    /// but accidental-deletion detection is not working).
    pub fn marker_degraded(&self) -> bool {
        self.degraded.load(Ordering::Acquire) & DEGRADED_MARKER != 0
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

    /// Replay WAL and return unacknowledged admissions with their sequence
    /// numbers.
    ///
    /// # Fail-closed semantics
    ///
    /// If the WAL contains a frame with a valid structure but invalid checksum,
    /// replay fails immediately. The caller must NOT proceed with an empty replay
    /// — data integrity cannot be guaranteed.
    ///
    /// Truncated trailing bytes (last line incomplete) are silently discarded
    /// because a crash during append produces exactly this pattern.
    pub async fn replay(&self) -> Result<Vec<(uuid::Uuid, PersistenceEvent, u64)>, String> {
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
                } else {
                    // Restore admit_sequence from the marker's high-water mark
                    // so sequence numbers do not regress after a clean
                    // compact-to-zero (the WAL file is gone but the marker
                    // records the last assigned sequence).
                    let marker_max = self.read_marker_max_sequence().await;
                    self.admit_sequence.store(marker_max, Ordering::Release);
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
        let mut needs_upgrade = false;
        let mut parsed_records: Vec<WalRecord> = Vec::new();

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
                            // Complete valid frame without trailing newline — keep it
                            // and normalize by appending the missing newline so the
                            // next append does not corrupt it (PMP38 P0-B).
                            lines.push(last);
                            self.append_missing_newline().await?;
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

            // Track records for potential v1→v2 WAL upgrade.
            if frame.ver == 1 {
                needs_upgrade = true;
            }
            parsed_records.push(frame.record.clone());

            match frame.record {
                WalRecord::Admission { id, event, sequence } => {
                    admitted.push((id, event, sequence));
                }
                WalRecord::Ack { id } => {
                    acked.insert(id);
                }
            }
        }

        // Assign sequential sequence numbers to v1 admission records (which
        // have sequence=0 from #[serde(default)]), starting one past the
        // highest existing v2 sequence so we never collide.
        let v1_start = if needs_upgrade {
            admitted.iter().map(|(_, _, seq)| *seq).max().unwrap_or(0) + 1
        } else {
            0
        };
        if needs_upgrade {
            let mut next_seq = v1_start;
            for entry in admitted.iter_mut() {
                if entry.2 == 0 {
                    entry.2 = next_seq;
                    next_seq += 1;
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
            //
            // `truncated_at` may be 0 when the file consists entirely of a
            // single truncated line — we still truncate to 0 bytes.
            if truncated_at < bytes.len() {
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
                        // Truncation is required for data safety — fail-closed.
                        return Err(format!(
                            "WAL {} truncation failed after detecting corrupted tail: {e}",
                            self.path.display(),
                        ));
                    }
                }
            }
        }

        // Record that this WAL instance has been initialized.
        // Used to detect accidental WAL deletion on subsequent starts.
        self.write_instance_marker().await?;

        // Capture the max sequence present in this WAL file BEFORE admitted is
        // consumed into unacked below.
        let wal_max = admitted.iter().map(|(_, _, seq)| *seq).max().unwrap_or(0);

        // Build un-ACKed list — sequences come directly from the WAL record.
        let unacked: Vec<(uuid::Uuid, PersistenceEvent, u64)> = admitted
            .into_iter()
            .filter(|(id, _, _)| !acked.contains(id))
            .collect();

        // Upgrade WAL from v1 to v2 if any v1 frames were encountered.
        // This must happen before admit_sequence is restored so that the
        // rewritten records carry their newly assigned sequences.
        if needs_upgrade {
            self.upgrade_wal_from_v1(&parsed_records, v1_start).await?;
        }

        // Restore admit_sequence from max seen so future admit() calls do not
        // conflict with replayed entry sequences.  Take the higher of:
        //   - the marker's recorded high-water mark (persisted across
        //     compactions so sequences never regress after a compact-to-zero)
        //   - the max sequence present in this WAL file
        let marker_max = self.read_marker_max_sequence().await;
        let restore_seq = wal_max.max(marker_max);
        self.admit_sequence.store(restore_seq, Ordering::Release);
        // Refresh the marker so its max_sequence reflects the restored
        // high-water mark even on first boot with an existing WAL.  Propagate
        // the error — an unpersisted high-water mark could allow sequence
        // regression after a later compact (P0-E).
        let marker_path = self.path.with_extension("wal.instance");
        self.write_marker_inner(&marker_path, false, restore_seq).await?;

        self.replay_succeeded.store(true, Ordering::Release);
        Ok(unacked)
    }


    /// Upgrade a v1-format WAL to v2, assigning sequential sequence numbers
    /// to v1 admission records that lack the `sequence` field.
    ///
    /// Called during `replay()` when v1 frames are detected.  This is an
    /// idempotent migration — the WAL is rewritten atomically (write temp,
    /// fsync, rename, fsync-parent) so a crash during upgrade leaves the
    /// original v1 file intact.
    async fn upgrade_wal_from_v1(&self, records: &[WalRecord], v1_start: u64) -> Result<(), String> {
        let temp = self.path.with_extension("wal.tmp");
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temp)
            .await
            .map_err(|e| format!("create WAL temp for upgrade {}: {e}", temp.display()))?;

        let mut next_seq = v1_start;
        for record in records {
            let upgraded = match record {
                WalRecord::Admission { id, event, sequence } if *sequence == 0 => {
                    let seq = next_seq;
                    next_seq += 1;
                    WalRecord::Admission {
                        id: *id,
                        event: event.clone(),
                        sequence: seq,
                    }
                }
                _ => record.clone(),
            };
            let frame = WalFrame::new(upgraded)?;
            let mut line = serde_json::to_vec(&frame)
                .map_err(|e| format!("serialize upgraded WAL frame: {e}"))?;
            line.push(b'\n');
            file.write_all(&line)
                .await
                .map_err(|e| format!("write upgraded WAL: {e}"))?;
        }

        file.flush()
            .await
            .map_err(|e| format!("flush upgraded WAL: {e}"))?;
        file.sync_all()
            .await
            .map_err(|e| format!("sync upgraded WAL: {e}"))?;
        drop(file);

        tokio::fs::rename(&temp, &self.path)
            .await
            .map_err(|e| format!("rename upgraded WAL: {e}"))?;

        if let Some(parent) = self.path.parent() {
            if let Ok(dir) = tokio::fs::File::open(parent).await {
                dir.sync_all()
                    .await
                    .map_err(|e| format!("sync parent after WAL upgrade: {e}"))?;
            }
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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // No WAL file to compact.  This is only safe when the marker
                // says CLEAN (compact-to-zero or no WAL yet).  An ACTIVE
                // marker with a missing WAL means the WAL was abnormally lost —
                // do NOT rewrite it to clean (that would mask the loss); return
                // Err and mark degraded (PMP38 P0-A).
                let marker_path = self.path.with_extension("wal.instance");
                let is_clean = match tokio::fs::read_to_string(&marker_path).await {
                    Ok(c) => serde_json::from_str::<serde_json::Value>(&c)
                        .ok()
                        .and_then(|v| v.get("clean").and_then(|x| x.as_bool())),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        // No marker at all — never-initialized/fresh instance.
                        // With no WAL this is the first-boot state: clean.
                        return Ok(0);
                    }
                    Err(e) => {
                        // Marker exists but is unreadable — fail-closed (P0-G):
                        // cannot confirm the WAL absence is intentional.
                        self.mark_degraded(DEGRADED_MARKER);
                        return Err(format!(
                            "WAL marker {} is unreadable during compact: {e}",
                            marker_path.display()
                        ));
                    }
                };
                match is_clean {
                    // Marker exists and is clean.
                    Some(true) => return Ok(0),
                    // Marker active, or present but corrupt/parsing failed —
                    // abnormal — fail-closed instead of assuming clean (P0-G).
                    _ => {
                        self.mark_degraded(DEGRADED_COMPACT);
                        return Err(format!(
                            "WAL marker is not clean ({is_clean:?}) but WAL file {} is \
                             missing (abnormal loss) during compact",
                            self.path.display()
                        ));
                    }
                }
            }
            Err(e) => return Err(format!("read WAL for compact {}: {e}", self.path.display())),
        };

        let mut admitted: Vec<(uuid::Uuid, PersistenceEvent, u64)> = Vec::new();
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
                WalRecord::Admission { id, event, sequence } => {
                    admitted.push((*id, event.clone(), *sequence));
                }
                WalRecord::Ack { id } => {
                    acked.insert(*id);
                }
            }
        }

        let pending: Vec<(uuid::Uuid, PersistenceEvent, u64)> = admitted
            .into_iter()
            .filter(|(id, _, _)| !acked.contains(id))
            .collect();

        if pending.is_empty() {
            // Nothing to compact; remove the WAL file and record a clean
            // marker so that the next startup does not treat the missing
            // WAL as accidental deletion (Issue #5 / P0 regression).
            // Record the current high-water mark so sequence numbers do not
            // regress after a compact-to-zero (P1).
            let max_sequence = self.admit_sequence.load(Ordering::Acquire);
            let _ = tokio::fs::remove_file(&self.path).await;
            let marker_path = self.path.with_extension("wal.instance");
            if marker_path.exists() {
                // Overwrite with clean marker.
                self.write_marker_inner(&marker_path, true, max_sequence).await?;
            } else {
                // First compact before any marker was written.
                self.write_marker_inner(&marker_path, true, max_sequence).await?;
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

        for (id, event, sequence) in &pending {
            let frame = WalFrame::new(WalRecord::Admission {
                id: *id,
                event: event.clone(),
                sequence: *sequence,
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

    /// Estimated total bytes in the WAL file (updated on admission/ACK/compact).
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes.load(Ordering::Acquire)
    }

    /// List unacknowledged admissions with their sequence numbers.
    ///
    /// Unlike `replay()`, this is a pure read — it does not set
    /// `replay_succeeded`, truncate trailing garbage, write instance
    /// markers, or mutate any WAL state.  It is safe to call at any
    /// time after a successful `replay()` (or on an empty/never-used
    /// WAL).
    ///
    /// Returns the set of (id, event, seq) whose ACK has not yet
    /// been observed, in file order.  Sequence numbers come directly
    /// from the stored Admission record in the WAL file.
    /// Returns an empty vec when the WAL file does not exist or when
    /// the WAL is in an un-replayed state.
    pub async fn list_pending(&self) -> Result<Vec<(uuid::Uuid, PersistenceEvent, u64)>, String> {
        if !self.replay_succeeded.load(Ordering::Acquire) {
            return Ok(Vec::new());
        }
        let bytes = match tokio::fs::read(&self.path).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("read WAL {}: {e}", self.path.display())),
        };

        let mut admitted: Vec<(uuid::Uuid, PersistenceEvent, u64)> = Vec::new();
        let mut acked = std::collections::HashSet::new();

        // Collect all segments first so we can distinguish a trailing
        // truncated tail (expected after a crash during append) from genuine
        // mid-file corruption.
        let mut segments: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();

        // If the file does not end with a newline, the final segment may be a
        // complete-but-unflushed frame OR a truncated tail.  Try to parse it;
        // only discard if it is genuinely corrupt/truncated.
        let has_trailing_truncation = bytes.last().map(|&b| b != b'\n').unwrap_or(false);
        if has_trailing_truncation {
            if let Some(last) = segments.last() {
                if !last.is_empty() {
                    let is_valid = serde_json::from_slice::<WalFrame>(last)
                        .map(|f| f.verify().is_ok() && f.ver <= WAL_FORMAT_VERSION)
                        .unwrap_or(false);
                    if !is_valid {
                        // Genuinely truncated tail — pop it and move on.
                        segments.pop();
                    }
                }
            }
        }

        // Process all remaining segments strictly.  Any corruption here is a
        // fail-closed condition: the WAL is degraded and the caller must not
        // proceed as if there were no pending entries (a corrupt admission
        // silently skipped would let Flush/Shutdown report success while an
        // uncommitted event is lost).
        for line in segments {
            if line.is_empty() {
                continue;
            }
            let frame: WalFrame = serde_json::from_slice(line).map_err(|e| {
                self.mark_degraded(DEGRADED_CORRUPTION);
                format!(
                    "corrupt WAL {} during list_pending: {e}",
                    self.path.display()
                )
            })?;
            if frame.ver > WAL_FORMAT_VERSION {
                self.mark_degraded(DEGRADED_CORRUPTION);
                return Err(format!(
                    "WAL {} during list_pending: unsupported format version {}",
                    self.path.display(),
                    frame.ver
                ));
            }
            if let Err(e) = frame.verify() {
                self.mark_degraded(DEGRADED_CORRUPTION);
                return Err(format!(
                    "corrupt WAL {} during list_pending: {e}",
                    self.path.display()
                ));
            }
            match frame.record {
                WalRecord::Admission { id, event, sequence } => {
                    admitted.push((id, event, sequence));
                }
                WalRecord::Ack { id } => {
                    acked.insert(id);
                }
            }
        }

        Ok(admitted
            .into_iter()
            .filter(|(id, _, _)| !acked.contains(id))
            .collect())
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

        let (first_id, _) = wal.admit(make_event("first")).await.unwrap();
        let (_, _) = wal.admit(make_event("second")).await.unwrap();
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
        let (_, _) = wal.admit(make_event("keep")).await.unwrap();
        let (ack_id, _) = wal.admit(make_event("ack-me")).await.unwrap();
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

        let (id1, _) = wal.admit(make_event("event1")).await.unwrap();
        let (id2, _) = wal.admit(make_event("event2")).await.unwrap();
        wal.ack(id1).await.unwrap();

        // Compact: only id2 should survive.
        assert_eq!(wal.compact().await.unwrap(), 1);
        let replay = wal.replay().await.unwrap();
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].1.kind(), "event2");

        // After compact, new admissions work.
        let (id3, _) = wal.admit(make_event("event3")).await.unwrap();
        wal.ack(id2).await.unwrap();
        wal.ack(id3).await.unwrap();
        assert_eq!(wal.compact().await.unwrap(), 0);
        // After compact-to-zero the marker records clean=true so that
        // replay succeeds (the missing WAL is expected, not accidental).
        assert!(wal.replay().await.unwrap().is_empty());
        // New admissions must still work after compact-to-zero (WAL recreated).
        let (id4, _) = wal.admit(make_event("event4")).await.unwrap();
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
        let bad_frame = r#"{"ver":1,"record":"admission","id":"00000000-0000-0000-0000-000000000001","event":{"PersistenceEvent":{"ServerEvent":{"kind":"bad","payload":{"n":1}}}},"cksum":"0000"}"#;
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

        let (id, _) = wal.admit(make_event("trunc-test")).await.unwrap();
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

    #[tokio::test]
    async fn sequence_does_not_regress_after_compact_to_zero() {
        let path = std::env::temp_dir().join(format!(
            "pmp-wal-seq-monotonic-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let wal = PersistenceWal::new(&path);
        wal.replay().await.unwrap();

        let (id1, seq1) = wal.admit(make_event("first")).await.unwrap();
        let (id2, seq2) = wal.admit(make_event("second")).await.unwrap();
        assert!(seq2 > seq1);
        wal.ack(id1).await.unwrap();
        wal.ack(id2).await.unwrap();

        // Compact to zero: WAL removed, clean marker records max_sequence.
        assert_eq!(wal.compact().await.unwrap(), 0);

        // Fresh instance replays the (now empty) WAL.  admit_sequence must be
        // restored from the marker's high-water mark, not reset to 0.
        let wal2 = PersistenceWal::new(&path);
        wal2.replay().await.unwrap();
        let (_, seq3) = wal2.admit(make_event("third")).await.unwrap();
        assert!(
            seq3 > seq2,
            "sequence must not regress after compact-to-zero: seq3={seq3}, seq2={seq2}"
        );

        let _ = tokio::fs::remove_file(&path).await;
        let _ = tokio::fs::remove_file(path.with_extension("wal.instance")).await;
    }

    #[tokio::test]
    async fn marker_clean_allows_replay_after_compact_to_zero() {
        // After a clean compact-to-zero the marker is marked clean, so a
        // missing WAL file is expected (not accidental deletion) and replay
        // must succeed.
        let path = std::env::temp_dir().join(format!(
            "pmp-wal-marker-clean-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let wal = PersistenceWal::new(&path);
        wal.replay().await.unwrap();
        let (id, _) = wal.admit(make_event("ack-me")).await.unwrap();
        wal.ack(id).await.unwrap();
        assert_eq!(wal.compact().await.unwrap(), 0); // removes WAL, writes clean marker

        let wal2 = PersistenceWal::new(&path);
        let replay = wal2.replay().await;
        assert!(replay.is_ok(), "clean marker must allow replay with no WAL: {replay:?}");

        let _ = tokio::fs::remove_file(path.with_extension("wal.instance")).await;
    }

    #[tokio::test]
    async fn list_pending_fails_closed_on_mid_file_corruption() {
        let path = std::env::temp_dir().join(format!(
            "pmp-wal-runtime-corrupt-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let wal = PersistenceWal::new(&path);
        wal.replay().await.unwrap();

        let (id1, _) = wal.admit(make_event("first")).await.unwrap();
        let (id2, _) = wal.admit(make_event("second")).await.unwrap();
        wal.ack(id1).await.unwrap();
        wal.ack(id2).await.unwrap();

        // Append a corrupt frame after the valid frames.  The trailing newline
        // means it is NOT treated as a truncated tail — it is a genuine
        // corrupt frame that list_pending must fail on.
        let mut content = tokio::fs::read(&path).await.unwrap();
        let corrupt = br#"{"ver":2,"record":"admission","id":"00000000-0000-0000-0000-0000000000ff","event":{"ServerEvent":{"kind":"corrupt","payload":{"n":1}}},"sequence":99,"cksum":"0000"}"#;
        content.extend_from_slice(b"\n");
        content.extend_from_slice(corrupt);
        content.extend_from_slice(b"\n");
        tokio::fs::write(&path, &content).await.unwrap();

        // list_pending must fail closed on corruption (not skip silently).
        let result = wal.list_pending().await;
        assert!(result.is_err(), "list_pending must fail on corruption");
        assert!(wal.is_degraded(), "WAL must be marked degraded on corruption");

        let _ = tokio::fs::remove_file(&path).await;
        let _ = tokio::fs::remove_file(path.with_extension("wal.instance")).await;
    }

    #[tokio::test]
    async fn list_pending_tolerates_trailing_truncated_line() {
        let path = std::env::temp_dir().join(format!(
            "pmp-wal-runtime-trunc-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let wal = PersistenceWal::new(&path);
        wal.replay().await.unwrap();

        let (id1, _) = wal.admit(make_event("ok")).await.unwrap();
        wal.ack(id1).await.unwrap();

        // Append a trailing truncated line (crash during append).
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .unwrap();
        use tokio::io::AsyncWriteExt;
        file.write_all(b"{\"ver\":2,\"record\":\"admission\",\"id\":\"").await.unwrap();
        drop(file);

        // A trailing truncated line is tolerated; list_pending still succeeds.
        let result = wal.list_pending().await;
        assert!(result.is_ok(), "trailing truncation must be tolerated: {result:?}");
        assert!(!wal.is_degraded());

        let _ = tokio::fs::remove_file(&path).await;
        let _ = tokio::fs::remove_file(path.with_extension("wal.instance")).await;
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
    async fn failed_admit_does_not_advance_sequence() {
        // Simulate an append failure by making the WAL path a directory
        // (open for append will fail with EISDIR).  The sequence counter
        // must NOT advance, so a subsequent successful admit reuses the
        // same sequence — no gap.
        let path = std::env::temp_dir().join(format!(
            "pmp-wal-gap-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let wal = PersistenceWal::new(&path);
        wal.replay().await.unwrap();

        // Create a directory at the WAL path to force append failure.
        tokio::fs::create_dir_all(&path).await.unwrap();
        let result = wal.admit(make_event("will-fail")).await;
        assert!(result.is_err(), "admit against a directory must fail");

        // Remove the directory so the next admit succeeds.
        tokio::fs::remove_dir(&path).await.unwrap();

        // First successful admit gets seq 1 (not 2) — proving the failed
        // admit did not consume a sequence number.
        let (_, seq) = wal.admit(make_event("ok")).await.unwrap();
        assert_eq!(seq, 1, "failed admit must not consume a sequence number");

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
        let (id, _) = wal.admit(make_event("test")).await.unwrap();
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
    async fn replay_sequence_numbers_are_monotonic() {
        let path =
            std::env::temp_dir().join(format!("pmp-wal-seq-{}.jsonl", uuid::Uuid::new_v4()));
        let wal = PersistenceWal::new(&path);
        wal.replay().await.unwrap();

        let (id1, seq1) = wal.admit(make_event("first")).await.unwrap();
        let (id2, seq2) = wal.admit(make_event("second")).await.unwrap();
        let (id3, seq3) = wal.admit(make_event("third")).await.unwrap();

        // Sequences must be strictly increasing.
        assert!(seq1 < seq2);
        assert!(seq2 < seq3);

        // list_pending should return the same sequences.
        let pending = wal.list_pending().await.unwrap();
        for (pid, _pe, pseq) in &pending {
            if *pid == id1 { assert_eq!(*pseq, seq1); }
            if *pid == id2 { assert_eq!(*pseq, seq2); }
            if *pid == id3 { assert_eq!(*pseq, seq3); }
        }

        // After ACK, the entry is removed from the sequences map.
        wal.ack(id2).await.unwrap();
        let pending_after = wal.list_pending().await.unwrap();
        assert_eq!(pending_after.len(), 2);
        // Remaining entries should still have their original sequences.
        for (pid, _pe, pseq) in &pending_after {
            if *pid == id1 { assert_eq!(*pseq, seq1); }
            if *pid == id3 { assert_eq!(*pseq, seq3); }
        }

        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn replay_version_mismatch_is_rejected() {
        let path =
            std::env::temp_dir().join(format!("pmp-wal-vers-{}.jsonl", uuid::Uuid::new_v4()));
        let future_frame = r#"{"ver":255,"record":"admission","id":"00000000-0000-0000-0000-000000000001","event":{"PersistenceEvent":{"ServerEvent":{"kind":"future","payload":{}}}},"cksum":"0000000000000000000000000000000000000000000000000000000000000000"}"#;
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
        let kinds: Vec<String> = replay.iter().map(|(_, e, _)| e.kind().to_string()).collect();
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
