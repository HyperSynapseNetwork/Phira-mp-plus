//! Existing room operation adapters behind the Runtime gateway.

use super::{
    command::{RoomActorCommand, RoomCommandKind},
    RoomCommandGateway,
};
use serde_json::Value;
use std::time::Instant;

mod control;
mod membership;
mod session;
mod settings;
mod telemetry;

impl RoomCommandGateway {
    /// 写入或清空当前谱面时长（秒）。选谱解析后异步调用写入；结算时传
    /// `None` 释放（PMP48：选谱解析、结算释放，不长期缓存）。
    pub async fn set_chart_duration(
        &self,
        state: &crate::server::PlusServerState,
        room_id: &str,
        duration: Option<f64>,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, None, |reply| RoomActorCommand::SetChartDuration {
                room_id: rid.clone(),
                duration,
                reply,
            })
            .await;
        self.finish_command(
            state,
            RoomCommandKind::SetChartDuration.action(),
            room_id,
            started,
            result,
        )
        .into_untyped()
    }
}
