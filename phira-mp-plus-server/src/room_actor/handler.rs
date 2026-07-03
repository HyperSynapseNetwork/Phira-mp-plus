//! Runtime v2 room command handler boundary.
//!
//! The mailbox worker should not keep growing command execution logic.  This
//! handler is the seam where the current adapter-based implementation will be
//! swapped for real room-owned actor state over time.

use super::{
    command::RoomActorCommand, context::RoomCommandContext, RoomCommandDelivery, RoomCommandResult,
};
use serde_json::Value;

pub(super) struct RoomCommandHandler;

impl RoomCommandHandler {
    pub(super) async fn execute(
        ctx: RoomCommandContext<'_>,
        command: &RoomActorCommand,
    ) -> RoomCommandResult {
        let gateway = ctx.gateway;
        let state = ctx.state;
        match command {
            RoomActorCommand::SetLock {
                room_id, locked, ..
            } => RoomCommandResult::from_untyped(
                gateway.set_lock_inline(state, room_id, *locked).await,
                RoomCommandDelivery::PerRoomMailbox,
            ),
            RoomActorCommand::SetCycle { room_id, cycle, .. } => RoomCommandResult::from_untyped(
                gateway.set_cycle_inline(state, room_id, *cycle).await,
                RoomCommandDelivery::PerRoomMailbox,
            ),
            RoomActorCommand::SetHost {
                room_id, target_id, ..
            } => RoomCommandResult::from_untyped(
                gateway.set_host_inline(state, room_id, *target_id).await,
                RoomCommandDelivery::PerRoomMailbox,
            ),
            RoomActorCommand::CloseRoom { room_id, .. } => RoomCommandResult::from_untyped(
                gateway.close_room_inline(state, room_id).await,
                RoomCommandDelivery::PerRoomMailbox,
            ),
            RoomActorCommand::KickUser {
                room_id, target_id, ..
            } => RoomCommandResult::from_untyped(
                gateway.kick_user_inline(state, room_id, *target_id).await,
                RoomCommandDelivery::PerRoomMailbox,
            ),
            RoomActorCommand::StartRoom { room_id, .. } => {
                let result = gateway.start_room_inline(state, room_id).await;
                match result {
                    Ok(payload) => RoomCommandResult::from_payload(payload, RoomCommandDelivery::PerRoomMailbox),
                    Err(error) => RoomCommandResult::Err { delivery: RoomCommandDelivery::PerRoomMailbox, error },
                }
            }
            RoomActorCommand::CancelStart { room_id, .. } => {
                let result = gateway.cancel_start_inline(state, room_id).await;
                match result {
                    Ok(payload) => RoomCommandResult::from_payload(payload, RoomCommandDelivery::PerRoomMailbox),
                    Err(error) => RoomCommandResult::Err { delivery: RoomCommandDelivery::PerRoomMailbox, error },
                }
            }
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
}
