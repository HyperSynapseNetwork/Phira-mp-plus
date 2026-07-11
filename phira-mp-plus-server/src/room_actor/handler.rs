//! Runtime v2 room command handler boundary.
//!
//! The mailbox worker should not keep growing command execution logic.  This
//! handler is the seam where the current adapter-based implementation will be
//! swapped for real room-owned actor state over time.

use super::{
    command::RoomActorCommand, context::RoomCommandContext, RoomCommandDelivery,
    RoomCommandPayload, RoomCommandResult,
};
use serde_json::Value;

fn typed_or_err(
    result: Result<RoomCommandPayload, String>,
    delivery: RoomCommandDelivery,
) -> RoomCommandResult {
    match result {
        Ok(payload) => RoomCommandResult::from_payload(payload, delivery),
        Err(error) => RoomCommandResult::Err { delivery, error },
    }
}

pub(super) struct RoomCommandHandler;

impl RoomCommandHandler {
    pub(super) async fn execute(
        ctx: RoomCommandContext<'_>,
        command: &RoomActorCommand,
    ) -> RoomCommandResult {
        let gateway = ctx.gateway;
        let state = ctx.state;
        let room = ctx.room.clone();
        match command {
            RoomActorCommand::SetLock {
                room_id,
                locked,
                actor_user_id,
                ..
            } => typed_or_err(
                gateway
                    .set_lock_in_actor(state, room_id, *locked, *actor_user_id, room.clone())
                    .await,
                RoomCommandDelivery::PerRoomMailbox,
            ),
            RoomActorCommand::SetCycle {
                room_id,
                cycle,
                actor_user_id,
                ..
            } => typed_or_err(
                gateway
                    .set_cycle_in_actor(state, room_id, *cycle, *actor_user_id, room.clone())
                    .await,
                RoomCommandDelivery::PerRoomMailbox,
            ),
            RoomActorCommand::SetHost {
                room_id, target_id, ..
            } => typed_or_err(
                gateway
                    .set_host_in_actor(state, room_id, *target_id, room.clone())
                    .await,
                RoomCommandDelivery::PerRoomMailbox,
            ),
            RoomActorCommand::SetHidden {
                room_id, hidden, ..
            } => typed_or_err(
                gateway
                    .set_hidden_in_actor(state, room_id, *hidden, room.clone())
                    .await,
                RoomCommandDelivery::PerRoomMailbox,
            ),
            RoomActorCommand::SetEndpoint {
                room_id, endpoint, ..
            } => typed_or_err(
                gateway
                    .set_endpoint_in_actor(state, room_id, endpoint.clone(), room.clone())
                    .await,
                RoomCommandDelivery::PerRoomMailbox,
            ),
            RoomActorCommand::CloseRoom { room_id, .. } => typed_or_err(
                gateway
                    .close_room_in_actor(state, room_id, room.clone())
                    .await,
                RoomCommandDelivery::PerRoomMailbox,
            ),
            RoomActorCommand::KickUser {
                room_id, target_id, ..
            } => typed_or_err(
                gateway
                    .kick_user_in_actor(state, room_id, *target_id, room.clone())
                    .await,
                RoomCommandDelivery::PerRoomMailbox,
            ),
            RoomActorCommand::StartRoom { room_id, .. } => typed_or_err(
                gateway
                    .start_room_in_actor(state, room_id, room.clone())
                    .await,
                RoomCommandDelivery::PerRoomMailbox,
            ),
            RoomActorCommand::CancelStart { room_id, .. } => typed_or_err(
                gateway.cancel_start_in_actor(state, room_id, room).await,
                RoomCommandDelivery::PerRoomMailbox,
            ),
        }
    }

    pub(super) fn should_stop_room_mailbox(
        command: &RoomActorCommand,
        result: &RoomCommandResult,
    ) -> bool {
        if command.kind().stops_room_mailbox_after_execution() {
            return true;
        }

        matches!(command, RoomActorCommand::KickUser { .. })
            && result
                .payload()
                .and_then(|value| value.get("room_dropped"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    fn dummy_reply() -> oneshot::Sender<RoomCommandResult> {
        let (tx, _rx) = oneshot::channel();
        tx
    }

    #[test]
    fn close_stops_room_mailbox_contract() {
        let command = RoomActorCommand::CloseRoom {
            room_id: "room-a".to_string(),
            reply: dummy_reply(),
        };
        let result = RoomCommandResult::from_untyped(
            Ok(serde_json::json!({"ok": true})),
            RoomCommandDelivery::PerRoomMailbox,
        );

        assert!(RoomCommandHandler::should_stop_room_mailbox(
            &command, &result
        ));
    }

    #[test]
    fn kick_only_stops_room_mailbox_when_room_dropped() {
        let keep_command = RoomActorCommand::KickUser {
            room_id: "room-a".to_string(),
            target_id: 42,
            reply: dummy_reply(),
        };
        let keep_result = RoomCommandResult::from_untyped(
            Ok(serde_json::json!({"ok": true, "room_dropped": false})),
            RoomCommandDelivery::PerRoomMailbox,
        );
        assert!(!RoomCommandHandler::should_stop_room_mailbox(
            &keep_command,
            &keep_result
        ));

        let drop_command = RoomActorCommand::KickUser {
            room_id: "room-a".to_string(),
            target_id: 42,
            reply: dummy_reply(),
        };
        let drop_result = RoomCommandResult::from_untyped(
            Ok(serde_json::json!({"ok": true, "room_dropped": true})),
            RoomCommandDelivery::PerRoomMailbox,
        );
        assert!(RoomCommandHandler::should_stop_room_mailbox(
            &drop_command,
            &drop_result
        ));
    }

    #[test]
    fn start_does_not_stop_room_mailbox_contract() {
        let command = RoomActorCommand::StartRoom {
            room_id: "room-a".to_string(),
            reply: dummy_reply(),
        };
        let result = RoomCommandResult::from_untyped(
            Ok(serde_json::json!({"ok": true})),
            RoomCommandDelivery::PerRoomMailbox,
        );

        assert!(!RoomCommandHandler::should_stop_room_mailbox(
            &command, &result
        ));
    }

    #[test]
    fn cancel_does_not_stop_room_mailbox_contract() {
        let command = RoomActorCommand::CancelStart {
            room_id: "room-b".to_string(),
            reply: dummy_reply(),
        };
        let result = RoomCommandResult::from_untyped(
            Ok(serde_json::json!({"canceled": true})),
            RoomCommandDelivery::PerRoomMailbox,
        );
        assert!(!RoomCommandHandler::should_stop_room_mailbox(
            &command, &result
        ));
    }

    #[test]
    fn set_lock_does_not_stop_room_mailbox() {
        let command = RoomActorCommand::SetLock {
            room_id: "room-c".to_string(),
            locked: true,
            actor_user_id: 0,
            reply: dummy_reply(),
        };
        let result = RoomCommandResult::from_untyped(
            Ok(serde_json::json!({"locked": true})),
            RoomCommandDelivery::PerRoomMailbox,
        );
        assert!(!RoomCommandHandler::should_stop_room_mailbox(
            &command, &result
        ));
    }

    #[test]
    fn set_cycle_does_not_stop_room_mailbox() {
        let command = RoomActorCommand::SetCycle {
            room_id: "room-d".to_string(),
            cycle: true,
            actor_user_id: 0,
            reply: dummy_reply(),
        };
        let result = RoomCommandResult::from_untyped(
            Ok(serde_json::json!({"cycle": true})),
            RoomCommandDelivery::PerRoomMailbox,
        );
        assert!(!RoomCommandHandler::should_stop_room_mailbox(
            &command, &result
        ));
    }

    #[test]
    fn set_host_does_not_stop_room_mailbox() {
        let command = RoomActorCommand::SetHost {
            room_id: "room-e".to_string(),
            target_id: Some(42),
            reply: dummy_reply(),
        };
        let result = RoomCommandResult::from_untyped(
            Ok(serde_json::json!({"host": 42})),
            RoomCommandDelivery::PerRoomMailbox,
        );
        assert!(!RoomCommandHandler::should_stop_room_mailbox(
            &command, &result
        ));
    }
}
