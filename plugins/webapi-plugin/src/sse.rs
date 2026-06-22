//! SSE 事件类型定义

use phira_mp_plus_server::server::Record;

/// SSE 事件
#[derive(Debug, Clone)]
pub enum SseEvent {
    CreateRoom { room: String, data: super::api::RoomSnapshot },
    UpdateRoom { room: String, data: serde_json::Value },
    JoinRoom { room: String, user: i32 },
    LeaveRoom { room: String, user: i32 },
    PlayerScore { room: String, record: Record },
    StartRound { room: String },
}
