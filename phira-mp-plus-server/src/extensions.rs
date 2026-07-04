//! Phira-mp+ 扩展数据系统
//!
//! 允许插件注册额外的用户数据和房间数据字段。
//! 其他插件和CLI命令可以通过此系统查询这些扩展数据。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// 扩展字段注册信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionField {
    /// 字段键名
    pub key: String,
    /// 默认值
    pub default_value: String,
    /// 注册该字段的插件名
    pub registered_by: String,
    /// 描述
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthCacheEntry {
    pub user_id: i32,
    pub name: String,
    pub language: String,
    pub cached_at: i64,
}

/// 扩展数据存储
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ExtensionDataStore {
    /// 字段注册表
    #[serde(skip)]
    pub fields: HashMap<String, ExtensionField>,
    /// 用户扩展数据: user_id -> (key -> value)
    pub user_data: HashMap<i32, HashMap<String, String>>,
    /// 房间扩展数据: room_id -> (key -> value)
    pub room_data: HashMap<String, HashMap<String, String>>,
    /// 认证缓存 (token_hash -> 用户信息)，持久化以在重启后恢复
    #[serde(default)]
    pub auth_cache: HashMap<String, AuthCacheEntry>,
    /// 全局扩展数据（key -> value），用于 IP 封禁等跨用户/跨房间设置
    #[serde(default)]
    pub global_data: HashMap<String, String>,
}

impl ExtensionDataStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册用户扩展字段
    pub fn register_user_field(
        &mut self,
        key: &str,
        default_value: &str,
        registered_by: &str,
        description: &str,
    ) -> Result<(), String> {
        if self.fields.contains_key(key) {
            return Err(format!("field '{}' already registered", key));
        }
        self.fields.insert(
            key.to_string(),
            ExtensionField {
                key: key.to_string(),
                default_value: default_value.to_string(),
                registered_by: registered_by.to_string(),
                description: description.to_string(),
            },
        );
        Ok(())
    }

    /// 注册房间扩展字段
    pub fn register_room_field(
        &mut self,
        key: &str,
        default_value: &str,
        registered_by: &str,
        description: &str,
    ) -> Result<(), String> {
        // Room fields use the same registry but prefixed
        let room_key = format!("room:{}", key);
        self.register_user_field(&room_key, default_value, registered_by, description)
    }

    /// 获取用户扩展数据
    pub fn get_user_extra(&self, user_id: i32, key: &str) -> Option<&String> {
        self.user_data.get(&user_id)?.get(key)
    }

    /// 设置用户扩展数据
    pub fn set_user_extra(&mut self, user_id: i32, key: &str, value: String) -> Result<(), String> {
        if !self.fields.contains_key(key) {
            return Err(format!("field '{}' is not registered", key));
        }
        self.user_data
            .entry(user_id)
            .or_default()
            .insert(key.to_string(), value);
        Ok(())
    }

    /// 获取房间扩展数据
    pub fn get_room_extra(&self, room_id: &str, key: &str) -> Option<&String> {
        self.room_data.get(room_id)?.get(key)
    }

    /// 设置房间扩展数据
    pub fn set_room_extra(
        &mut self,
        room_id: &str,
        key: &str,
        value: String,
    ) -> Result<(), String> {
        let field_key = format!("room:{}", key);
        if !self.fields.contains_key(&field_key) {
            return Err(format!("room field '{}' is not registered", key));
        }
        self.room_data
            .entry(room_id.to_string())
            .or_default()
            .insert(key.to_string(), value);
        Ok(())
    }

    /// 列出所有注册的用户字段
    pub fn list_registered_fields(&self) -> Vec<String> {
        self.fields
            .keys()
            .filter(|k| !k.starts_with("room:"))
            .cloned()
            .collect()
    }

    /// 列出所有注册的房间字段
    pub fn list_registered_room_fields(&self) -> Vec<String> {
        self.fields
            .keys()
            .filter(|k| k.starts_with("room:"))
            .map(|k| k.trim_start_matches("room:").to_string())
            .collect()
    }

    /// 获取全局扩展数据
    pub fn get_global(&self, key: &str) -> Option<&String> {
        self.global_data.get(key)
    }

    /// 设置全局扩展数据
    pub fn set_global(&mut self, key: &str, value: String) -> Result<(), String> {
        self.global_data.insert(key.to_string(), value);
        Ok(())
    }

    /// 清理用户数据（用户断开时）
    pub fn cleanup_user(&mut self, user_id: i32) {
        self.user_data.remove(&user_id);
    }

    /// 清理房间数据（房间解散时）
    pub fn cleanup_room(&mut self, room_id: &str) {
        self.room_data.remove(room_id);
    }
}

/// 扩展数据管理器（线程安全）
pub struct ExtensionManager {
    store: Arc<RwLock<ExtensionDataStore>>,
    persist_path: Option<String>,
}

impl ExtensionManager {
    pub fn new(persist_path: Option<String>) -> Self {
        let store = if let Some(ref path) = persist_path {
            std::fs::read_to_string(path)
                .ok()
                .and_then(|content| serde_json::from_str(&content).ok())
                .unwrap_or_default()
        } else {
            ExtensionDataStore::new()
        };

        Self {
            store: Arc::new(RwLock::new(store)),
            persist_path,
        }
    }

    pub fn new_in_memory() -> Self {
        Self::new(None)
    }

    /// 获取底层存储的引用
    pub fn store(&self) -> &Arc<RwLock<ExtensionDataStore>> {
        &self.store
    }

    /// 持久化数据到磁盘，并在启用 PostgreSQL 时同步写入统一事件流。
    pub async fn persist(&self) -> Result<(), String> {
        let data = self.store.read().await;
        let value = serde_json::to_value(&*data).map_err(|e| format!("serialize: {}", e))?;
        if let Some(ref path) = self.persist_path {
            let json =
                serde_json::to_string_pretty(&value).map_err(|e| format!("serialize: {}", e))?;
            std::fs::write(path, json).map_err(|e| format!("write: {}", e))?;
        }
        if let Some(db) = crate::internal_hooks::DB.get() {
            db.record_room_event_sync("extensions.snapshot", None, None, value);
        }
        Ok(())
    }

    // === 便捷方法 ===

    pub async fn register_user_field(
        &self,
        key: &str,
        default_value: &str,
        registered_by: &str,
        description: &str,
    ) -> Result<(), String> {
        let result = self.store.write().await.register_user_field(
            key,
            default_value,
            registered_by,
            description,
        );
        if result.is_ok() {
            if let Some(db) = crate::internal_hooks::DB.get() {
                db.record_room_event_sync(
                    "extensions.user_field.register",
                    None,
                    None,
                    serde_json::json!({
                        "key": key,
                        "default_value": default_value,
                        "registered_by": registered_by,
                        "description": description,
                    }),
                );
            }
        }
        result
    }

    pub async fn register_room_field(
        &self,
        key: &str,
        default_value: &str,
        registered_by: &str,
        description: &str,
    ) -> Result<(), String> {
        let result = self.store.write().await.register_room_field(
            key,
            default_value,
            registered_by,
            description,
        );
        if result.is_ok() {
            if let Some(db) = crate::internal_hooks::DB.get() {
                db.record_room_event_sync(
                    "extensions.room_field.register",
                    None,
                    None,
                    serde_json::json!({
                        "key": key,
                        "default_value": default_value,
                        "registered_by": registered_by,
                        "description": description,
                    }),
                );
            }
        }
        result
    }

    pub async fn get_user_extra(&self, user_id: i32, key: &str) -> Option<String> {
        self.store
            .read()
            .await
            .get_user_extra(user_id, key)
            .cloned()
    }

    pub async fn set_user_extra(
        &self,
        user_id: i32,
        key: &str,
        value: String,
    ) -> Result<(), String> {
        let key_owned = key.to_string();
        let result = self
            .store
            .write()
            .await
            .set_user_extra(user_id, key, value.clone());
        if result.is_ok() {
            if let Some(db) = crate::internal_hooks::DB.get() {
                db.record_room_event_sync(
                    "extensions.user.set",
                    None,
                    Some(user_id),
                    serde_json::json!({
                        "user_id": user_id,
                        "key": key_owned,
                        "value": value,
                    }),
                );
            }
        }
        result
    }

    pub async fn get_room_extra(&self, room_id: &str, key: &str) -> Option<String> {
        self.store
            .read()
            .await
            .get_room_extra(room_id, key)
            .cloned()
    }

    pub async fn set_room_extra(
        &self,
        room_id: &str,
        key: &str,
        value: String,
    ) -> Result<(), String> {
        let room_owned = room_id.to_string();
        let key_owned = key.to_string();
        let result = self
            .store
            .write()
            .await
            .set_room_extra(room_id, key, value.clone());
        if result.is_ok() {
            if let Some(db) = crate::internal_hooks::DB.get() {
                db.record_room_event_sync(
                    "extensions.room.set",
                    Some(room_owned.clone()),
                    None,
                    serde_json::json!({
                        "room_id": room_owned,
                        "key": key_owned,
                        "value": value,
                    }),
                );
            }
        }
        result
    }

    pub async fn get_global(&self, key: &str) -> Option<String> {
        self.store.read().await.get_global(key).cloned()
    }

    pub async fn set_global(&self, key: &str, value: String) -> Result<(), String> {
        self.store.write().await.set_global(key, value)
    }

    pub async fn list_user_fields(&self) -> Vec<String> {
        self.store.read().await.list_registered_fields()
    }

    pub async fn list_room_fields(&self) -> Vec<String> {
        self.store.read().await.list_registered_room_fields()
    }

    // ── 认证缓存持久化 ──

    pub async fn get_auth_cache(&self) -> HashMap<String, AuthCacheEntry> {
        self.store.read().await.auth_cache.clone()
    }

    pub async fn set_auth_cache(&self, cache: HashMap<String, AuthCacheEntry>) {
        self.store.write().await.auth_cache = cache;
    }

    /// 更新认证缓存（不会立即写盘 — 由外部定期 persist）
    /// SHA256 hex hash of auth token, used as cache key.
    pub fn token_hash(token: &str) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(token.as_bytes()))
    }

    pub async fn update_auth_cache(&self, token_hash: String, entry: AuthCacheEntry) {
        self.store
            .write()
            .await
            .auth_cache
            .insert(token_hash, entry);
    }
}
