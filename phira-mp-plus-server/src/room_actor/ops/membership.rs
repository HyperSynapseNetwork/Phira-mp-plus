use super::super::{
    command::{RoomActorCommand, RoomCommandKind, RoomOrigin},
    RoomCommandGateway,
};
use crate::server::PlusServerState;
use serde_json::Value;
use std::time::Instant;

impl RoomCommandGateway {
    /// Kick a user/monitor from a room.
    pub async fn kick_user(
        &self,
        state: &PlusServerState,
        room_id: &str,
        target_id: i32,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, None, |reply| RoomActorCommand::KickUser {
                room_id: rid.clone(),
                target_id,
                reply,
            })
            .await;
        self.finish_command(
            state,
            RoomCommandKind::KickUser.action(),
            room_id,
            started,
            result,
        )
        .into_untyped()
    }


    /// Close and remove a room.
    pub async fn close_room(
        &self,
        state: &PlusServerState,
        room_id: &str,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let result = self
            .room_mailbox(room_id, None, |reply| RoomActorCommand::CloseRoom {
                room_id: room_id.to_string(),
                reply,
            })
            .await;
        self.finish_command(
            state,
            RoomCommandKind::CloseRoom.action(),
            room_id,
            started,
            result,
        )
        .into_untyped()
    }


    // ── AddUser ────────────────────────────────────────────────────────────

    pub async fn add_user(
        &self,
        state: &PlusServerState,
        room_id: &str,
        user_id: i32,
        user_name: &str,
        monitor: bool,
        deadline: Instant,
        origin: RoomOrigin,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let uname = user_name.to_string();
        let result = self
            .room_mailbox(&rid, Some(deadline), |reply| RoomActorCommand::AddUser {
                room_id: rid.clone(),
                user_id,
                user_name: uname,
                monitor,
                deadline,
                origin,
                reply,
            })
            .await;
        self.finish_command(state, RoomCommandKind::AddUser.action(), room_id, started, result)
            .into_untyped()
    }


    // ── RemoveUser ──────────────────────────────────────────────────────────

    pub async fn remove_user(
        &self,
        state: &PlusServerState,
        room_id: &str,
        user_id: i32,
        deadline: Option<Instant>,
        origin: RoomOrigin,
    ) -> Result<Value, String> {
        let deadline = deadline.unwrap_or_else(|| Instant::now() + std::time::Duration::from_secs(30));
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, Some(deadline), |reply| RoomActorCommand::RemoveUser {
                room_id: rid.clone(),
                user_id,
                deadline,
                origin,
                reply,
            })
            .await;
        self.finish_command(state, RoomCommandKind::RemoveUser.action(), room_id, started, result)
            .into_untyped()
    }


    // ── SetLive ─────────────────────────────────────────────────────────────

    /// Set the live flag for the room.
    pub async fn set_live(
        &self,
        state: &PlusServerState,
        room_id: &str,
        live: bool,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, None, |reply| RoomActorCommand::SetLive {
                room_id: rid.clone(),
                live,
                reply,
            })
            .await;
        self.finish_command(state, RoomCommandKind::SetLive.action(), room_id, started, result)
            .into_untyped()
    }


    // ── SetDegraded ─────────────────────────────────────────────────────────

    /// PMP45 P0-K: 设置房间 degraded 标志。Join 补偿失败后置 true 以阻塞后续
    /// Join（Ghost member 清理延后）；操作员 / 未来的 reconcile 可置 false 恢复。
    pub async fn set_degraded(
        &self,
        state: &PlusServerState,
        room_id: &str,
        degraded: bool,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, None, |reply| RoomActorCommand::SetDegraded {
                room_id: rid.clone(),
                degraded,
                reply,
            })
            .await;
        self.finish_command(state, RoomCommandKind::SetDegraded.action(), room_id, started, result)
            .into_untyped()
    }

    /// 赛事模式房间（房间级配置）。开启后禁用 PMP 默认交互行为，交 PPB 编排。
    pub async fn set_tournament(
        &self,
        state: &PlusServerState,
        room_id: &str,
        tournament: bool,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, None, |reply| RoomActorCommand::SetTournament {
                room_id: rid.clone(),
                tournament,
                reply,
            })
            .await;
        self.finish_command(state, RoomCommandKind::SetTournament.action(), room_id, started, result)
            .into_untyped()
    }

    // ── CheckAllReady (fire-and-forget) ────────────────────────────────────

    /// PMP45 P0-O: 响应后 fire-and-forget 的 `check_all_ready` 重入。
    ///
    /// 暂未使用（RemoveUser 当前同步执行 check_all_ready 以避免 E0391 类型
    /// 循环）；保留供未来在 Actor 排序点异步重入检查使用。
    #[allow(dead_code)]
    pub async fn fire_check_all_ready(
        &self,
        state: &PlusServerState,
        room_id: &str,
        deadline: Instant,
    ) {
        let Some(tx) = self.room_mailbox_sender(room_id).await else {
            return;
        };
        let (reply, _rx) = tokio::sync::oneshot::channel::<()>();
        let cmd = RoomActorCommand::CheckAllReady {
            room_id: room_id.to_string(),
            deadline,
            reply,
        };
        let _ = tx.send(cmd).await;
        let _ = state; // 与其它 gateway 方法保持签名一致（room 校验用 state）
    }

}
