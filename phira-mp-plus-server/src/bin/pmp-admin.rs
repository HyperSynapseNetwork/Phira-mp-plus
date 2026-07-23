//! Standalone admin tool for Phira-mp+ server operations.
//!
//! Provides backup and restore functionality that was previously embedded
//! in the server runtime's interactive CLI. Keeping it in a separate binary
//! removes backup code from the server hot path.
//!
//! Usage:
//!   pmp-admin backup create [output_dir]
//!   pmp-admin backup verify <path>

use std::path::Path;

/// Include the shared backup module from src/backup.rs.
///
/// This module is NOT compiled as part of the server library (lib.rs does
/// not declare `pub mod backup`). It is only compiled when building this
/// binary or running its tests.
#[path = "../backup.rs"]
mod backup;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage:");
        eprintln!("  pmp-admin backup create [output_dir]");
        eprintln!("  pmp-admin backup verify <path>");
        eprintln!();
        eprintln!("Examples:");
        eprintln!("  pmp-admin backup create");
        eprintln!("  pmp-admin backup create /tmp/my-backups");
        eprintln!("  pmp-admin backup verify /tmp/my-backups/pmp-backup-1715000000");
        std::process::exit(1);
    }

    match args[1].as_str() {
        "backup" => {
            if args.len() < 3 {
                eprintln!("Usage: pmp-admin backup create [output_dir]");
                eprintln!("       pmp-admin backup verify <path>");
                std::process::exit(1);
            }
            match args[2].as_str() {
                "create" => {
                    let output_dir = args.get(3).map(|s| s.as_str()).unwrap_or(".");
                    let config_path = default_config_path();
                    match backup::create_backup(&config_path, output_dir) {
                        Ok(path) => {
                            println!("Backup created: {path}");
                        }
                        Err(e) => {
                            eprintln!("Backup failed: {e}");
                            std::process::exit(1);
                        }
                    }
                }
                "verify" => {
                    if args.len() < 4 {
                        eprintln!("Usage: pmp-admin backup verify <path>");
                        std::process::exit(1);
                    }
                    let path = &args[3];
                    match backup::verify_backup(path) {
                        Ok(report) => {
                            println!(
                                "Backup valid: {} files, {} bytes, {} manifest entries",
                                report.file_count,
                                report.total_size,
                                report.manifest_entries,
                            );
                        }
                        Err(e) => {
                            eprintln!("Verification failed: {e}");
                            std::process::exit(1);
                        }
                    }
                }
                other => {
                    eprintln!("Unknown backup subcommand: {other}");
                    eprintln!("Usage: pmp-admin backup create [output_dir]");
                    eprintln!("       pmp-admin backup verify <path>");
                    std::process::exit(1);
                }
            }
        }
        other => {
            eprintln!("Unknown command: {other}");
            eprintln!("Usage:");
            eprintln!("  pmp-admin backup create [output_dir]");
            eprintln!("  pmp-admin backup verify <path>");
            std::process::exit(1);
        }
    }
}

/// Find the server configuration file to include in backups.
///
/// Checks the default path used by the server; if not found, returns the
/// path anyway so `create_backup` can report a clearer error.
fn default_config_path() -> String {
    let candidates = ["server_config.yml", "../server_config.yml"];
    for c in &candidates {
        if Path::new(c).exists() {
            return c.to_string();
        }
    }
    // Return the most likely default; `create_backup` handles missing gracefully.
    "server_config.yml".to_string()
}
