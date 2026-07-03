//! Server query helpers extracted from server.rs.
//!
//! Admin ID management functions.

use crate::server::PlusServerState;
use tracing::warn;

impl PlusServerState {
    pub async fn admin_id_list(&self) -> Vec<i32> {
        let mut ids: Vec<i32> = self.admin_ids.read().await.iter().copied().collect();
        ids.sort_unstable();
        ids
    }

    pub async fn is_admin_id(&self, user_id: i32) -> bool {
        self.admin_ids.read().await.contains(&user_id)
    }

    async fn persist_admin_ids(&self) {
        let ids = self.admin_id_list().await;
        if let Some(db) = crate::internal_hooks::DB.get() {
            if let Err(err) = db.set_admin_ids(&ids).await {
                warn!("persist admin ids failed: {err}");
            }
        }
        let _ = std::fs::create_dir_all("data");
        if let Ok(json) = serde_json::to_string_pretty(&ids) {
            let _ = std::fs::write("data/admin-phira-ids.json", json);
        }
    }

    pub async fn set_admin_ids(&self, ids: Vec<i32>) -> Vec<i32> {
        {
            let mut guard = self.admin_ids.write().await;
            guard.clear();
            guard.extend(ids.into_iter().filter(|id| *id > 0));
        }
        self.persist_admin_ids().await;
        self.admin_id_list().await
    }

    pub async fn add_admin_id(&self, user_id: i32) -> Vec<i32> {
        if user_id > 0 {
            self.admin_ids.write().await.insert(user_id);
        }
        self.persist_admin_ids().await;
        self.admin_id_list().await
    }

    pub async fn remove_admin_id(&self, user_id: i32) -> Vec<i32> {
        self.admin_ids.write().await.remove(&user_id);
        self.persist_admin_ids().await;
        self.admin_id_list().await
    }
}
