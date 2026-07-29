//! Server startup state recovery — crash recovery for unfinished rounds,
//! schema validation, and database diagnostics.
//!
//! After a server restart (planned or crash), in-memory state is empty while
//! PostgreSQL still holds data from the previous run.  This module re-discovers
//! that data and reconciles the server state so that:
//!
//! 1. Unfinished rounds are marked as aborted (so plugins/telemetry see a
//!    terminal state and don't wait for a round that will never finish).
//! 2. Database health is logged (schema version, user / playtime counts).
//! 3. Future expansion: persistent empty room restoration.

use std::sync::Arc;

use crate::db::DbManager;
use tracing::{error, info, warn};

use super::state::PlusServerState;

/// Run all startup recovery steps.
///
/// Must be called **after** the PostgreSQL connection is established and
/// migrations have run, but **before** accepting network connections or
/// initialising plugins that depend on a consistent state.
///
/// Failures are **non-fatal** — the server logs warnings and continues.
/// A broken database would have been caught by `DbManager::new()` earlier.
pub async fn recover_state(_state: &Arc<PlusServerState>, db: &DbManager) {
    // ── 1. Crash recovery: abort unfinished rounds ──────────────────────
    let unfinished = db.find_unfinished_rounds().await;
    let count = unfinished.len();
    if count > 0 {
        warn!(
            "startup recovery: found {count} unfinished round(s) from \
             previous server session — marking as aborted"
        );
        for round in &unfinished {
            warn!(
                "crash recovery: aborting unfinished round {} (room={}, \
                 chart_id={}, started_at={})",
                round.round_uuid, round.room_id, round.chart_id, round.started_at,
            );
            if db.abort_round(&round.round_uuid).await {
                info!(
                    "crash recovery: successfully aborted round {}",
                    round.round_uuid
                );
            } else {
                error!(
                    "crash recovery: failed to abort round {}",
                    round.round_uuid
                );
            }
        }
        info!(
            "startup recovery: aborted {count} unfinished round(s) from \
             previous server session"
        );
    } else {
        info!("startup recovery: no unfinished rounds to recover");
    }

    // ── 2. Schema version validation ────────────────────────────────────
    let schema_version = db.get_schema_version().await;
    match schema_version {
        Some(ver) => info!("startup recovery: schema version = {ver}"),
        None => warn!(
            "startup recovery: _pmp_schema_version table is empty or \
             inaccessible — this is normal on a fresh database"
        ),
    }

    // ── 3. Database diagnostics ─────────────────────────────────────────
    let user_count = db.count_users().await;
    let playtime_count = db.count_playtime().await;
    info!(
        "startup recovery: database state — {} user record(s), \
         {} playtime entry(ies)",
        user_count, playtime_count,
    );

    // ── 4. Persistent empty rooms (placeholder) ─────────────────────────
    // Future: read persistent_empty_rooms from mp_settings and recreate
    // them via state.create_empty_room().  Skipped for P0 — the rooms that
    // existed before the restart will be recreated by the normal plugin /
    // admin flow when the server is back online.
    info!("startup recovery: persistent empty room restoration is not yet implemented");
}
