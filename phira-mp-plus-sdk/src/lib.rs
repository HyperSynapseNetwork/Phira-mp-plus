//! Phira-mp+ 插件开发工具包 (SDK)
//!
//! 此 SDK 提供开发 Phira-mp+ 插件所需的类型定义和辅助函数。
//! 插件可以通过实现 `PhiraPlugin` 特征来开发。
//!
//! # 快速开始
//!
//! ```ignore
//! use phira_mp_plus_sdk::*;
//!
//! struct MyPlugin;
//!
//! impl PhiraPlugin for MyPlugin {
//!     fn info(&self) -> PluginInfo {
//!         PluginInfo {
//!             name: "my-plugin".into(),
//!             version: "0.1.0".into(),
//!             author: "me".into(),
//!             description: "我的第一个 Phira-mp+ 插件".into(),
//!         }
//!     }
//!
//!     fn on_connect(&self, ctx: &PluginContext, user_id: i32, user_name: &str) {
//!         ctx.log("info", &format!("用户 {}({}) 连接了服务器", user_name, user_id));
//!     }
//! }
//! ```

use serde::{Deserialize, Serialize};

/// 插件信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
}

/// 插件事件类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PluginEvent {
    UserConnect { user_id: i32, user_name: String },
    UserDisconnect { user_id: i32, user_name: String },
    RoomCreate { user_id: i32, room_id: String },
    RoomJoin { user_id: i32, room_id: String, is_monitor: bool },
    RoomLeave { user_id: i32, room_id: String },
    RoomModify { user_id: i32, room_id: String, data: String },
    GameStart { user_id: i32, room_id: String },
    GameEnd { user_id: i32, room_id: String, score: i32, accuracy: f32 },
    /// 玩家触摸事件
    PlayerTouches { user_id: i32, room_id: String, data: Vec<TouchEventPoint> },
    /// 玩家判定事件
    PlayerJudges { user_id: i32, room_id: String, data: Vec<JudgeEventItem> },
}

/// 触摸事件中的一个触点
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TouchEventPoint {
    pub time: f32,
    pub finger: i8,
    pub x: f32,
    pub y: f32,
}

/// 判定事件中的一个判定
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeEventItem {
    pub time: f32,
    pub line_id: u32,
    pub note_id: u32,
    pub judgement: String,
}

/// 用户数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserData {
    pub id: i32,
    pub name: String,
    pub language: String,
    pub is_monitor: bool,
}

/// 房间数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomData {
    pub id: String,
    pub host_id: i32,
    pub host_name: String,
    pub player_count: u32,
    pub monitor_count: u32,
    pub state: String,
    pub locked: bool,
    pub cycling: bool,
}

/// 插件上下文 - 提供插件可调用的服务器 API
#[derive(Debug, Clone)]
pub struct PluginContext {
    /// 服务器 API 调用接口
    api: ServerApiRef,
}

impl PluginContext {
    /// 创建新的上下文
    pub fn new(api: ServerApiRef) -> Self {
        Self { api }
    }

    /// 记录日志
    pub fn log(&self, level: &str, message: &str) {
        self.api.call("log", &serde_json::json!({ "level": level, "message": message }));
    }

    /// 获取用户信息
    pub fn get_user(&self, user_id: i32) -> Option<UserData> {
        self.api
            .call("get_user", &serde_json::json!({ "user_id": user_id }))
            .and_then(|r| serde_json::from_value(r).ok())
    }

    /// 获取用户扩展数据
    pub fn get_user_extra(&self, user_id: i32, key: &str) -> Option<String> {
        self.api
            .call("get_user_extra", &serde_json::json!({ "user_id": user_id, "key": key }))
            .and_then(|r| r.as_str().map(String::from))
    }

    /// 设置用户扩展数据
    pub fn set_user_extra(&self, user_id: i32, key: &str, value: &str) -> Result<(), String> {
        self.api
            .call("set_user_extra", &serde_json::json!({ "user_id": user_id, "key": key, "value": value }));
        Ok(())
    }

    /// 获取房间信息
    pub fn get_room(&self, room_id: &str) -> Option<RoomData> {
        self.api
            .call("get_room", &serde_json::json!({ "room_id": room_id }))
            .and_then(|r| serde_json::from_value(r).ok())
    }

    /// 发送消息给指定用户
    pub fn send_to_user(&self, user_id: i32, message: &str) -> Result<(), String> {
        self.api
            .call("send_to_user", &serde_json::json!({ "user_id": user_id, "message": message }));
        Ok(())
    }

    /// 广播消息到房间
    pub fn send_to_room(&self, room_id: &str, message: &str) -> Result<(), String> {
        self.api
            .call("send_to_room", &serde_json::json!({ "room_id": room_id, "message": message }));
        Ok(())
    }

    /// 广播消息到所有用户
    pub fn send_to_all(&self, message: &str) -> Result<(), String> {
        self.api
            .call("send_to_all", &serde_json::json!({ "message": message }));
        Ok(())
    }

    /// 生成 UUID
    pub fn generate_uuid(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// 获取当前时间
    pub fn current_time(&self) -> String {
        chrono::Utc::now().to_rfc3339()
    }
}

/// 服务器 API 引用
#[derive(Debug, Clone)]
pub struct ServerApiRef {
    caller: fn(method: &str, params: &serde_json::Value) -> Option<serde_json::Value>,
}

impl ServerApiRef {
    pub fn new(
        caller: fn(method: &str, params: &serde_json::Value) -> Option<serde_json::Value>,
    ) -> Self {
        Self { caller }
    }

    pub fn call(
        &self,
        method: &str,
        params: &serde_json::Value,
    ) -> Option<serde_json::Value> {
        (self.caller)(method, params)
    }
}

/// 插件特征 - 所有 Phira-mp+ 插件需要实现此特征
pub trait PhiraPlugin: Send + Sync {
    /// 获取插件信息
    fn info(&self) -> PluginInfo;

    /// 插件初始化
    fn init(&mut self, _ctx: &PluginContext) -> Result<(), String> {
        Ok(())
    }

    /// 插件清理
    fn cleanup(&self) {}

    // === 事件处理方法（可选重写） ===

    /// 用户连接时触发
    fn on_connect(&self, _ctx: &PluginContext, _user_id: i32, _user_name: &str) {}

    /// 用户断开时触发
    fn on_disconnect(&self, _ctx: &PluginContext, _user_id: i32, _user_name: &str) {}

    /// 创建房间时触发
    fn on_room_create(&self, _ctx: &PluginContext, _user_id: i32, _room_id: &str) {}

    /// 加入房间时触发
    fn on_room_join(
        &self,
        _ctx: &PluginContext,
        _user_id: i32,
        _room_id: &str,
        _is_monitor: bool,
    ) {
    }

    /// 离开房间时触发
    fn on_room_leave(&self, _ctx: &PluginContext, _user_id: i32, _room_id: &str) {}

    /// 房间数据修改时触发
    fn on_room_modify(
        &self,
        _ctx: &PluginContext,
        _user_id: i32,
        _room_id: &str,
        _data: &str,
    ) {
    }

    /// 游戏开始时触发
    fn on_game_start(&self, _ctx: &PluginContext, _user_id: i32, _room_id: &str) {}

    /// 游戏结束时触发
    fn on_game_end(
        &self,
        _ctx: &PluginContext,
        _user_id: i32,
        _room_id: &str,
        _score: i32,
        _accuracy: f32,
    ) {
    }
}

/// 示例插件 - 记录所有连接
pub struct ExampleLogger;

impl PhiraPlugin for ExampleLogger {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "example-logger".to_string(),
            version: "0.1.0".to_string(),
            author: "Phira-mp+".to_string(),
            description: "示例插件 - 记录所有用户事件".to_string(),
        }
    }

    fn on_connect(&self, ctx: &PluginContext, user_id: i32, user_name: &str) {
        ctx.log("info", &format!("[ExampleLogger] 用户 {}({}) 连接了服务器", user_name, user_id));
    }

    fn on_disconnect(&self, ctx: &PluginContext, user_id: i32, user_name: &str) {
        ctx.log("info", &format!("[ExampleLogger] 用户 {}({}) 断开了连接", user_name, user_id));
    }

    fn on_room_create(&self, ctx: &PluginContext, user_id: i32, room_id: &str) {
        ctx.log("info", &format!("[ExampleLogger] 用户 {} 创建了房间 {}", user_id, room_id));
    }

    fn on_room_join(&self, ctx: &PluginContext, user_id: i32, room_id: &str, is_monitor: bool) {
        ctx.log("info", &format!(
            "[ExampleLogger] 用户 {} 加入了房间 {} (旁观: {})",
            user_id, room_id, is_monitor
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_info() {
        let plugin = ExampleLogger;
        let info = plugin.info();
        assert_eq!(info.name, "example-logger");
        assert_eq!(info.version, "0.1.0");
    }

    #[test]
    fn test_user_data_serialization() {
        let data = UserData {
            id: 1,
            name: "test".to_string(),
            language: "zh-CN".to_string(),
            is_monitor: false,
        };
        let json = serde_json::to_string(&data).unwrap();
        assert!(json.contains("test"));
        assert!(json.contains("zh-CN"));
    }
}
