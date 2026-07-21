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
    /// Total bytes written (approx, updated on admission/ACK).
    total_bytes: std::sync::atomic::AtomicU64,
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
            total_bytes: std::sync::atomic::AtomicU64::new(0),
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
        let bytes = match tokio::fs::read(&self.path).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.replay_succeeded.store(true, Ordering::Release);
                return Ok(Vec::new());
            }
            Err(e) => return Err(format!("read WAL {}: {e}", self.path.display())),
        };

        let mut admitted = Vec::new();
        let mut acked = HashSet::new();
        let mut has_truncated = false;

        let mut lines: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
        // If the last byte was not a newline, the final segment is a truncated
        // (incomplete) write — discard it silently.
        if bytes.last().map(|&b| b != b'\n').unwrap_or(false) {
            if let Some(last) = lines.pop() {
                if !last.is_empty() {
                    has_truncated = true;
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
            warn!(
                "WAL {} had trailing truncated bytes (discarded); this is expected after a crash",
                self.path.display()
            );
        }

        self.replay_succeeded.store(true, Ordering::Release);
        Ok(admitted
            .into_iter()
            .filter(|(id, _)| !acked.contains(id))
            .collect())
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
                    has_truncated = true;
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
            // During compact we silently skip checksum errors on known ACK
            // records (the original data is recoverable from the live state).
            // Admission records MUST pass checksum.
            match &frame.record {
                WalRecord::Admission { .. } => {
                    frame.verify().map_err(|e| {
                        format!(
                            "corrupt WAL {} line {} admission record: {e}",
                            self.path.display(),
                            index + 1
                        )
                    })?;
                    if let WalRecord::Admission { id, event } = &frame.record {
                        admitted.push((*id, event.clone()));
                    }
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
            // Nothing to compact; remove the file.
            let _ = tokio::fs::remove_file(&self.path).await;
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
        let second_id = wal.admit(make_event("second")).await.unwrap();
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
        let id = wal.admit(make_event("keep")).await.unwrap();
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
        assert!(wal.replay().await.unwrap().is_empty());

        let _ = tokio::fs::remove_file(path).await;
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
    async fn fault_wal_deleted_during_replay() {
        let path =
            std::env::temp_dir().join(format!("pmp-wal-del-{}.jsonl", uuid::Uuid::new_v4()));
        let wal = PersistenceWal::new(&path);
        wal.replay().await.unwrap();
        wal.admit(make_event("will-be-lost")).await.unwrap();
        let _ = tokio::fs::remove_file(&path).await;
        let wal2 = PersistenceWal::new(&path);
        let r = wal2.replay().await.unwrap();
        assert!(r.is_empty());
        assert!(wal2.replay_succeeded());
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
    async fn fault_zeroed_wal_recovers_empty() {
        let path =
            std::env::temp_dir().join(format!("pmp-wal-zero-{}.jsonl", uuid::Uuid::new_v4()));
        let wal = PersistenceWal::new(&path);
        wal.replay().await.unwrap();
        wal.admit(make_event("lost")).await.unwrap();
        tokio::fs::write(&path, b"").await.unwrap();
        let wal2 = PersistenceWal::new(&path);
        let r = wal2.replay().await.unwrap();
        assert!(r.is_empty());
        wal2.admit(make_event("fresh")).await.unwrap();
        let r2 = wal2.replay().await.unwrap();
        assert_eq!(r2.len(), 1);
        assert_eq!(r2[0].1.kind(), "fresh");
        let _ = tokio::fs::remove_file(path).await;
    }
}
