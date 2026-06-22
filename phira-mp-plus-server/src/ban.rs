//! Phira-mp+ 黑名单管理系统
//!
//! 提供全局玩家封禁、自定义拒绝提示信息、房间黑名单等功能。
//! 所有数据存储在 ExtensionManager 中，作为额外用户/房间信息的一部分。

use crate::extensions::ExtensionManager;
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

/// 封禁条目
#[derive(Debug, Clone, Serialize)]
pub struct BanEntry {
    pub user_id: i32,
    pub reason: String,
    pub custom_message: String,
}

/// 系统默认拒绝提示
const DEFAULT_BAN_MESSAGE: &str = "你的账号已被封禁";

/// 黑名单管理器
pub struct BanManager {
    extensions: Arc<ExtensionManager>,
    default_message: RwLock<String>,
}

impl BanManager {
    /// 创建黑名单管理器，并注册扩展字段
    pub fn new(extensions: Arc<ExtensionManager>) -> Self {
        Self {
            extensions,
            default_message: RwLock::new(DEFAULT_BAN_MESSAGE.to_string()),
        }
    }

    /// 注册扩展字段（启动时调用）
    pub async fn register_fields(&self) {
        // 用户扩展字段
        let _ = self
            .extensions
            .register_user_field("ban-status", "", "mp", "封禁状态: banned 或空")
            .await;
        let _ = self
            .extensions
            .register_user_field("ban-reason", "", "mp", "封禁原因")
            .await;
        let _ = self
            .extensions
            .register_user_field("ban-message", "", "mp", "自定义拒绝提示信息")
            .await;
        // 房间扩展字段
        let _ = self
            .extensions
            .register_room_field("blacklist", "[]", "mp", "房间黑名单用户ID列表 (JSON数组)")
            .await;
        // 读取默认拒绝信息
        if let Some(msg) = self.extensions.get_user_extra(0, "ban-default-message").await {
            *self.default_message.write().await = msg;
        }
        info!("blacklist manager initialized");
    }

    // ── 全局封禁 ──

    /// 封禁用户
    pub async fn ban_user(&self, user_id: i32, reason: &str) -> Result<(), String> {
        self.extensions
            .set_user_extra(user_id, "ban-status", "banned".to_string())
            .await?;
        self.extensions
            .set_user_extra(user_id, "ban-reason", reason.to_string())
            .await?;
        info!(user = user_id, reason = %reason, "user banned");
        Ok(())
    }

    /// 解封用户
    pub async fn unban_user(&self, user_id: i32) -> Result<(), String> {
        self.extensions
            .set_user_extra(user_id, "ban-status", String::new())
            .await?;
        self.extensions
            .set_user_extra(user_id, "ban-reason", String::new())
            .await?;
        info!(user = user_id, "user unbanned");
        Ok(())
    }

    /// 检查用户是否被封禁
    pub async fn is_banned(&self, user_id: i32) -> bool {
        self.extensions
            .get_user_extra(user_id, "ban-status")
            .await
            .as_deref()
            == Some("banned")
    }

    /// 获取封禁原因
    pub async fn get_ban_reason(&self, user_id: i32) -> Option<String> {
        if self.is_banned(user_id).await {
            self.extensions.get_user_extra(user_id, "ban-reason").await
        } else {
            None
        }
    }

    /// 列出所有被封禁的用户
    pub async fn list_banned(&self) -> Vec<BanEntry> {
        // 遍历所有有 ban-status 数据的用户
        let store = self.extensions.store().read().await;
        let mut result = Vec::new();
        for (&uid, data) in &store.user_data {
            if data.get("ban-status").map(|s| s.as_str()) == Some("banned") {
                result.push(BanEntry {
                    user_id: uid,
                    reason: data
                        .get("ban-reason")
                        .cloned()
                        .unwrap_or_default(),
                    custom_message: data
                        .get("ban-message")
                        .cloned()
                        .unwrap_or_default(),
                });
            }
        }
        result
    }

    // ── 拒绝提示信息 ──

    /// 设置默认拒绝提示信息
    pub async fn set_default_ban_message(&self, msg: &str) {
        *self.default_message.write().await = msg.to_string();
        // 通过虚拟用户 0 存储
        let _ = self
            .extensions
            .register_user_field("ban-default-message", msg, "mp", "默认拒绝提示")
            .await;
        let _ = self
            .extensions
            .set_user_extra(0, "ban-default-message", msg.to_string())
            .await;
        info!("default ban message set to: {}", msg);
    }

    /// 获取默认拒绝提示信息
    pub async fn get_default_ban_message(&self) -> String {
        self.default_message.read().await.clone()
    }

    /// 设置用户的特殊拒绝提示信息
    pub async fn set_user_ban_message(&self, user_id: i32, msg: &str) -> Result<(), String> {
        self.extensions
            .set_user_extra(user_id, "ban-message", msg.to_string())
            .await
    }

    /// 获取用户最终拒绝提示信息（优先使用自定义，否则使用默认）
    pub async fn get_effective_ban_message(&self, user_id: i32) -> String {
        if let Some(msg) = self.extensions.get_user_extra(user_id, "ban-message").await {
            if !msg.is_empty() {
                return msg;
            }
        }
        self.default_message.read().await.clone()
    }

    // ── 房间黑名单 ──

    /// 将用户加入房间黑名单
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
            .await
    }

    /// 将用户移出房间黑名单
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
            .await
    }

    /// 检查用户是否在房间黑名单中
    pub async fn is_room_banned(&self, room_id: &str, user_id: i32) -> bool {
        self.get_room_ban_list_raw(room_id)
            .await
            .contains(&user_id)
    }

    /// 获取房间黑名单列表
    pub async fn list_room_bans(&self, room_id: &str) -> Vec<i32> {
        self.get_room_ban_list_raw(room_id).await
    }

    /// 获取房间黑名单原始数据
    async fn get_room_ban_list_raw(&self, room_id: &str) -> Vec<i32> {
        self.extensions
            .get_room_extra(room_id, "blacklist")
            .await
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }
}
