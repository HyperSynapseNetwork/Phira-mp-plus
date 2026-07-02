use crate::extensions::{ExtensionDataStore, ExtensionManager};
use serde::Serialize;
use std::sync::Arc;
use tracing::{info, warn};

const BAN_STATUS_KEY: &str = "ban-status";
const BAN_REASON_KEY: &str = "ban-reason";
const BANNED_STATUS: &str = "banned";
const DEFAULT_BAN_REASON: &str = "违反服务器规则";

#[derive(Debug, Clone, Serialize)]
pub struct BanEntry {
    pub user_id: i32,
    pub reason: String,
}

pub struct BanManager {
    extensions: Arc<ExtensionManager>,
}

impl BanManager {
    pub fn new(extensions: Arc<ExtensionManager>) -> Self {
        Self { extensions }
    }

    pub async fn register_fields(&self) {
        let _ = self
            .extensions
            .register_user_field(BAN_STATUS_KEY, "", "mp", "封禁状态: banned 或空")
            .await;
        let _ = self
            .extensions
            .register_user_field(BAN_REASON_KEY, "", "mp", "封禁原因")
            .await;
        let _ = self
            .extensions
            .register_room_field("blacklist", "[]", "mp", "房间黑名单用户ID列表 (JSON)")
            .await;

        self.repair_legacy_reasons().await;
        info!("blacklist manager initialized");
    }

    pub async fn ban_user(&self, user_id: i32, reason: &str) -> Result<String, String> {
        let reason = normalize_reason(reason);
        {
            let mut store = self.extensions.store().write().await;
            set_ban_record(&mut store, user_id, BANNED_STATUS, &reason)?;
        }
        self.extensions.persist().await?;
        info!(user = user_id, reason = %reason, "user banned");
        Ok(reason)
    }

    pub async fn unban_user(&self, user_id: i32) -> Result<(), String> {
        {
            let mut store = self.extensions.store().write().await;
            set_ban_record(&mut store, user_id, "", "")?;
        }
        self.extensions.persist().await?;
        info!(user = user_id, "user unbanned");
        Ok(())
    }

    pub async fn ban_reason(&self, user_id: i32) -> Option<String> {
        let store = self.extensions.store().read().await;
        let data = store.user_data.get(&user_id)?;
        if data.get(BAN_STATUS_KEY).map(String::as_str) != Some(BANNED_STATUS) {
            return None;
        }
        Some(normalize_reason(
            data.get(BAN_REASON_KEY)
                .map(String::as_str)
                .unwrap_or_default(),
        ))
    }

    pub async fn is_banned(&self, user_id: i32) -> bool {
        self.ban_reason(user_id).await.is_some()
    }

    pub async fn get_ban_reason(&self, user_id: i32) -> String {
        self.ban_reason(user_id)
            .await
            .unwrap_or_else(|| DEFAULT_BAN_REASON.to_string())
    }

    pub async fn list_banned(&self) -> Vec<BanEntry> {
        let store = self.extensions.store().read().await;
        let mut result = store
            .user_data
            .iter()
            .filter_map(|(&user_id, data)| {
                (data.get(BAN_STATUS_KEY).map(String::as_str) == Some(BANNED_STATUS)).then(|| {
                    BanEntry {
                        user_id,
                        reason: normalize_reason(
                            data.get(BAN_REASON_KEY)
                                .map(String::as_str)
                                .unwrap_or_default(),
                        ),
                    }
                })
            })
            .collect::<Vec<_>>();
        result.sort_unstable_by_key(|entry| entry.user_id);
        result
    }

    pub async fn room_ban_user(&self, room_id: &str, user_id: i32) -> Result<(), String> {
        let mut list = self.get_room_ban_list_raw(room_id).await;
        if list.contains(&user_id) {
            return Err(format!("用户 {} 已在房间 {} 的黑名单中", user_id, room_id));
        }
        list.push(user_id);
        let json =
            serde_json::to_string(&list).map_err(|e| format!("serialize blacklist: {}", e))?;
        self.extensions
            .set_room_extra(room_id, "blacklist", json)
            .await?;
        self.extensions.persist().await?;
        Ok(())
    }

    pub async fn room_unban_user(&self, room_id: &str, user_id: i32) -> Result<(), String> {
        let mut list = self.get_room_ban_list_raw(room_id).await;
        let before = list.len();
        list.retain(|&id| id != user_id);
        if list.len() == before {
            return Err(format!("用户 {} 不在房间 {} 的黑名单中", user_id, room_id));
        }
        let json =
            serde_json::to_string(&list).map_err(|e| format!("serialize blacklist: {}", e))?;
        self.extensions
            .set_room_extra(room_id, "blacklist", json)
            .await?;
        self.extensions.persist().await?;
        Ok(())
    }

    pub async fn is_room_banned(&self, room_id: &str, user_id: i32) -> bool {
        self.get_room_ban_list_raw(room_id).await.contains(&user_id)
    }

    pub async fn list_room_bans(&self, room_id: &str) -> Vec<i32> {
        self.get_room_ban_list_raw(room_id).await
    }

    async fn get_room_ban_list_raw(&self, room_id: &str) -> Vec<i32> {
        self.extensions
            .get_room_extra(room_id, "blacklist")
            .await
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    async fn repair_legacy_reasons(&self) {
        let repaired = {
            let mut store = self.extensions.store().write().await;
            let mut repaired = 0usize;
            for data in store.user_data.values_mut() {
                if data.get(BAN_STATUS_KEY).map(String::as_str) != Some(BANNED_STATUS) {
                    continue;
                }
                let reason = normalize_reason(
                    data.get(BAN_REASON_KEY)
                        .map(String::as_str)
                        .unwrap_or_default(),
                );
                if data.get(BAN_REASON_KEY).map(String::as_str) != Some(reason.as_str()) {
                    data.insert(BAN_REASON_KEY.to_string(), reason);
                    repaired += 1;
                }
            }
            repaired
        };

        if repaired > 0 {
            if let Err(err) = self.extensions.persist().await {
                warn!(repaired, %err, "failed to persist repaired ban reasons");
            } else {
                info!(repaired, "repaired legacy ban reasons");
            }
        }
    }
}

fn set_ban_record(
    store: &mut ExtensionDataStore,
    user_id: i32,
    status: &str,
    reason: &str,
) -> Result<(), String> {
    for key in [BAN_STATUS_KEY, BAN_REASON_KEY] {
        if !store.fields.contains_key(key) {
            return Err(format!("field '{}' is not registered", key));
        }
    }

    let data = store.user_data.entry(user_id).or_default();
    data.insert(BAN_REASON_KEY.to_string(), reason.to_string());
    data.insert(BAN_STATUS_KEY.to_string(), status.to_string());
    Ok(())
}

fn normalize_reason(reason: &str) -> String {
    let mut normalized = String::with_capacity(reason.len());
    let mut pending_space = false;

    for ch in reason.trim().chars() {
        if ch.is_control() {
            pending_space = true;
            continue;
        }
        if pending_space
            && !normalized.is_empty()
            && !normalized.chars().last().is_some_and(char::is_whitespace)
        {
            normalized.push(' ');
        }
        pending_space = false;
        normalized.push(ch);
    }

    let normalized = normalized.trim();
    if normalized.is_empty() {
        DEFAULT_BAN_REASON.to_string()
    } else {
        normalized.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_reason, DEFAULT_BAN_REASON};

    #[test]
    fn normalizes_empty_reason() {
        assert_eq!(normalize_reason(" \t\n "), DEFAULT_BAN_REASON);
    }

    #[test]
    fn strips_control_characters_without_hiding_text() {
        assert_eq!(normalize_reason("  spam\nlinks\u{7}  "), "spam links");
    }
}
