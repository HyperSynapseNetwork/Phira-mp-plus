//! Simulation event persistence — raw event writes for simulation runs.
//!
//! Extracted from db.rs to keep the simulation INSERT logic separate from
//! general-purpose database helpers.

use crate::db::DbManager;
use serde_json::Value;

impl DbManager {
    /// Record a simulation event to the mp_sim_events table.
    pub async fn record_sim_event(
        &self,
        run_id: Option<String>,
        kind: &str,
        payload: Value,
    ) -> bool {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            return sqlx::query(
                "INSERT INTO mp_sim_events (run_id, kind, payload, created_at)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(run_id.as_deref())
            .bind(kind)
            .bind(payload)
            .bind(now)
            .execute(pool)
            .await
            .is_ok();
        }
        #[cfg(not(feature = "postgres"))]
        let _ = (run_id, kind, payload);
        false
    }

    /// Synchronous (spawn-and-forget) variant of record_sim_event.
    pub fn record_sim_event_sync(&self, run_id: Option<String>, kind: &str, payload: Value) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let pool = pool.clone();
            let kind = kind.to_string();
            tokio::spawn(async move {
                let db = DbManager::Pg(pool);
                let _ = db.record_sim_event(run_id, &kind, payload).await;
            });
        }
        #[cfg(not(feature = "postgres"))]
        let _ = (run_id, kind, payload);
    }
}
