//! Admin persistence — admin IDs stored in mp_settings.
//!
//! Extracted from db.rs.

use crate::db::DbManager;

impl DbManager {
    /// Set the admin Phira ID list.
    pub async fn set_admin_ids(&self, ids: &[i32]) -> std::result::Result<(), String> {
        let Self::Pg(pool) = self;
            let value = serde_json::to_string(ids).map_err(|e| e.to_string())?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            sqlx::query(
                "INSERT INTO mp_settings (key, value, updated_at) VALUES ('admin_phira_ids', $1::jsonb, $2)
                 ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at"
            )
            .bind(value)
            .bind(now)
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Get the admin Phira ID list.
    pub async fn get_admin_ids(&self) -> Option<Vec<i32>> {
        let Self::Pg(pool) = self;
        use sqlx::Row;
        let row = sqlx::query(
            "SELECT value::text AS value FROM mp_settings WHERE key = 'admin_phira_ids'",
        )
        .fetch_optional(pool)
        .await
        .ok()??;
        let raw = row.try_get::<String, _>("value").ok()?;
        serde_json::from_str(&raw).ok()
    }

    /// Get the list of persistent empty room IDs from mp_settings.
    ///
    /// Returns `Ok(None)` when the key is not set, `Ok(Some(ids))` on success,
    /// and `Err(sqlx::Error)` on database failures — allowing callers to
    /// distinguish "no config" from "database error".
    pub async fn get_persistent_rooms(&self) -> Result<Option<Vec<String>>, sqlx::Error> {
        let Self::Pg(pool) = self;
        use sqlx::Row;
        let row = sqlx::query(
            "SELECT value::text AS value FROM mp_settings WHERE key = 'persistent_rooms'",
        )
        .fetch_optional(pool)
        .await?;
        match row {
            Some(r) => {
                let raw: String = r.try_get("value")?;
                let ids: Vec<String> = serde_json::from_str(&raw).map_err(|e| {
                    sqlx::Error::Decode(Box::new(e))
                })?;
                Ok(Some(ids))
            }
            None => Ok(None),
        }
    }
}
