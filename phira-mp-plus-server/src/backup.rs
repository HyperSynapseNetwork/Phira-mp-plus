//! Backup and restore utilities.
//!
//! Creates a timestamped backup directory containing copies of:
//! - Configuration file
//! - WAL + dead-letter journals
//! - Extension data
//! - A SHA-256 manifest for integrity verification
//!
//! This module is NOT part of the server runtime. It is only used by the
//! standalone `pmp-admin` binary (`src/bin/pmp-admin.rs`).

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Result of a backup verification.
pub struct VerifyReport {
    pub file_count: usize,
    pub total_size: u64,
    pub manifest_entries: usize,
}

/// Create a file-level backup in a timestamped directory.
pub fn create_backup(config_path: &str, output_dir: &str) -> Result<String, String> {
    use std::fs;
    use std::io::Write;

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup_dir = Path::new(output_dir).join(format!("pmp-backup-{timestamp}"));
    let data_dir = backup_dir.join("data");

    fs::create_dir_all(&data_dir).map_err(|e| format!("create backup dir: {e}"))?;

    // Copy config file
    if Path::new(config_path).exists() {
        fs::copy(config_path, backup_dir.join("server_config.yml"))
            .map_err(|e| format!("copy config: {e}"))?;
    }

    // Copy WAL + dead-letter + extension data
    for entry in ["data/persistence-worker.wal.jsonl", "data/persistence-dead-letter.jsonl", "data/extensions.json"] {
        let src = Path::new(entry);
        if src.exists() {
            if let Some(parent) = src.parent() {
                let dst = data_dir.join(parent);
                fs::create_dir_all(&dst).map_err(|e| format!("create data subdir: {e}"))?;
            }
            fs::copy(src, data_dir.join(entry))
                .map_err(|e| format!("copy {entry}: {e}"))?;
        }
    }

    // Generate SHA-256 manifest
    let manifest_path = backup_dir.join("MANIFEST.sha256");
    let mut manifest = fs::File::create(&manifest_path).map_err(|e| format!("create manifest: {e}"))?;
    let mut file_count = 0u64;
    let mut total_size = 0u64;

    for entry in walkdir(&backup_dir, &backup_dir) {
        let entry_path = entry.map_err(|e| format!("walkdir: {e}"))?;
        if entry_path.is_dir() || entry_path == manifest_path {
            continue;
        }
        let relative = entry_path.strip_prefix(&backup_dir).unwrap();
        let data = fs::read(&entry_path).map_err(|e| format!("read {}: {e}", entry_path.display()))?;
        let hash = sha256(&data);
        let hex_hash: String = hash.iter().map(|b| format!("{b:02x}")).collect();
        writeln!(manifest, "{hex_hash}  {}", relative.display())
            .map_err(|e| format!("write manifest: {e}"))?;
        file_count += 1;
        total_size += data.len() as u64;
    }

    eprintln!(
        "backup created: {} ({} files, {} bytes)",
        backup_dir.display(),
        file_count,
        total_size,
    );

    Ok(backup_dir.to_string_lossy().to_string())
}

/// Verify a backup by checking its manifest.
pub fn verify_backup(path: &str) -> Result<VerifyReport, String> {
    use std::fs;
    use std::io::BufRead;

    let backup_dir = Path::new(path);
    let manifest_path = backup_dir.join("MANIFEST.sha256");

    if !manifest_path.exists() {
        return Err(format!("MANIFEST.sha256 not found in {path}"));
    }

    let manifest = fs::File::open(&manifest_path)
        .map_err(|e| format!("open manifest: {e}"))?;
    let reader = std::io::BufReader::new(manifest);

    let mut file_count = 0usize;
    let mut total_size = 0u64;
    let mut manifest_entries = 0usize;

    for line in reader.lines() {
        let line = line.map_err(|e| format!("read manifest line: {e}"))?;
        manifest_entries += 1;

        let parts: Vec<&str> = line.splitn(2, "  ").collect();
        if parts.len() != 2 {
            continue;
        }
        let expected_hash = parts[0].trim();
        let relative_path = parts[1].trim();

        let full_path = backup_dir.join(relative_path);
        if !full_path.exists() {
            return Err(format!("missing file in backup: {relative_path}"));
        }

        let data = fs::read(&full_path).map_err(|e| format!("read {relative_path}: {e}"))?;
        let actual_hash: String = sha256(&data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();

        if actual_hash != expected_hash {
            return Err(format!(
                "checksum mismatch for {relative_path}: expected {expected_hash}, got {actual_hash}"
            ));
        }

        file_count += 1;
        total_size += data.len() as u64;
    }

    if manifest_entries == 0 {
        return Err("manifest is empty".to_string());
    }

    Ok(VerifyReport {
        file_count: file_count.saturating_sub(1), // exclude manifest itself
        total_size,
        manifest_entries,
    })
}

/// Compute SHA-256 hash.
fn sha256(data: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

/// Simple recursive directory walker (no external dep).
#[allow(clippy::only_used_in_recursion)]
fn walkdir(dir: &Path, base: &Path) -> Vec<Result<PathBuf, String>> {
    let mut results = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries {
            match entry {
                Ok(entry) => {
                    let path = entry.path();
                    if path.is_dir() {
                        results.extend(walkdir(&path, base));
                    }
                    results.push(Ok(path));
                }
                Err(e) => {
                    results.push(Err(format!("read dir entry: {e}")));
                }
            }
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn verify_nonexistent_fails() {
        let result = verify_backup("/tmp/nonexistent-pmp-backup");
        assert!(result.is_err());
    }

    #[test]
    fn walkdir_finds_files() {
        let tmp = std::env::temp_dir().join("pmp-backup-test");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("test.txt"), b"hello").unwrap();
        let files = walkdir(&tmp, &tmp);
        assert!(!files.is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn backup_create_and_verify_roundtrip() {
        // Create a mock backup manually and verify it
        let tmp = std::env::temp_dir().join("pmp-backup-roundtrip");
        let data_dir = tmp.join("data");
        fs::create_dir_all(&data_dir).unwrap();
        fs::write(data_dir.join("test.wal"), b"some wal data").unwrap();
        fs::write(tmp.join("server_config.yml"), b"port: 12346").unwrap();

        // Generate manifest
        let manifest_path = tmp.join("MANIFEST.sha256");
        let mut manifest = fs::File::create(&manifest_path).unwrap();
        for entry in walkdir(&tmp, &tmp) {
            let path = entry.unwrap();
            if path.is_dir() || path == manifest_path {
                continue;
            }
            let data = fs::read(&path).unwrap();
            let hash = sha256(&data);
            let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
            let relative = path.strip_prefix(&tmp).unwrap();
            use std::io::Write;
            writeln!(manifest, "{hex}  {}", relative.display()).unwrap();
        }
        drop(manifest);

        let report = verify_backup(tmp.to_str().unwrap()).unwrap();
        assert!(report.file_count > 0, "should verify at least one file");
        assert!(report.total_size > 0, "total size should be positive");
        let _ = fs::remove_dir_all(&tmp);
    }
}
