//! Runtime v2 room command handler boundary.
//!
//! The mailbox worker should not keep growing command execution logic.  This
//! handler is the seam where the current adapter-based implementation will be
//! swapped for real room-owned actor state over time.
//!
//! Migration:
//! - `execute()` dispatches via the gateway (Room object path, legacy)
//! - `execute_with_actor()` dispatches via actor-owned state (new path)
//!   For commands that support direct state modification (SetLock, SetCycle,
//!   SetHidden), the actor state is modified directly.  Other commands still
//!   fall back to the legacy gateway path but receive the actor reference so
//!   the gateway can also write to actor state.

use super::{
    command::RoomActorCommand, context::RoomCommandContext, RoomCommandDelivery,
    RoomCommandPayload, RoomCommandResult,
};
use crate::plugin::PluginEvent;
use phira_mp_common::{Message, PartialRoomData};
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
    /// Execute a command using the existing gateway path (legacy).
    /// Commands go through `resolve_room` → `Room` object methods.
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
            RoomActorCommand::SetChart {
                room_id,
                chart_id,
                chart_name,
                ..
            } => typed_or_err(
                gateway
                    .set_chart_in_actor(state, room_id, *chart_id, chart_name, room.clone())
                    .await,
                RoomCommandDelivery::PerRoomMailbox,
            ),
            RoomActorCommand::SetReady { room_id, user_id, .. } => typed_or_err(
                gateway
                    .set_ready_in_actor(state, room_id, *user_id, room.clone())
                    .await,
                RoomCommandDelivery::PerRoomMailbox,
            ),
            RoomActorCommand::CancelReady { room_id, user_id, .. } => typed_or_err(
                gateway
                    .cancel_ready_in_actor(state, room_id, *user_id, room.clone())
                    .await,
                RoomCommandDelivery::PerRoomMailbox,
            ),
            RoomActorCommand::SubmitResult {
                room_id,
                user_id,
                score,
                accuracy,
                perfect,
                good,
                bad,
                miss,
                max_combo,
                full_combo,
                ..
            } => typed_or_err(
                gateway
                    .submit_result_in_actor(
                        state, room_id, *user_id, *score, *accuracy,
                        *perfect, *good, *bad, *miss, *max_combo, *full_combo,
                        room.clone(),
                    )
                    .await,
                RoomCommandDelivery::PerRoomMailbox,
            ),
            RoomActorCommand::AbortRound { room_id, user_id, .. } => typed_or_err(
                gateway
                    .abort_round_in_actor(state, room_id, *user_id, room.clone())
                    .await,
                RoomCommandDelivery::PerRoomMailbox,
            ),
            RoomActorCommand::AddUser {
                room_id,
                user_id,
                user_name,
                monitor,
                ..
            } => typed_or_err(
                gateway
                    .add_user_in_actor(state, room_id, *user_id, user_name, *monitor, room.clone())
                    .await,
                RoomCommandDelivery::PerRoomMailbox,
            ),
            RoomActorCommand::HostStart {
                room_id, user_id, ..
            } => typed_or_err(
                gateway
                    .host_start_in_actor(state, room_id, *user_id, room.clone())
                    .await,
                RoomCommandDelivery::PerRoomMailbox,
            ),
            RoomActorCommand::RemoveUser { room_id, user_id, .. } => typed_or_err(
                gateway
                    .remove_user_in_actor(state, room_id, *user_id, room.clone())
                    .await,
                RoomCommandDelivery::PerRoomMailbox,
            ),
        }
    }

    /// Execute a command using actor-owned state (migration path).
    /// Simple control commands (SetLock, SetCycle, SetHidden) modify
    /// actor_state directly.  Other commands still delegate to the
    /// gateway `_in_actor` methods, but the gateway also receives the
    /// actor reference so it can sync state.
    pub(super) async fn execute_with_actor(
        mut ctx: RoomCommandContext<'_>,
        command: &RoomActorCommand,
    ) -> RoomCommandResult {
        let room = ctx.room.clone();
        let state = ctx.state;
        let gateway = ctx.gateway;

        match command {
            // ── Direct state modification (migration target) ─────────────
            RoomActorCommand::SetLock {
                room_id,
                locked,
                actor_user_id,
                ..
            } => {
                let actor_state = ctx.expect_actor_state();
                actor_state.state.set_locked(*locked);
                // Side effects via room for backward compat
                if let Some(ref room) = room {
                    room.set_locked(*locked);
                    room.send(Message::LockRoom { lock: *locked }).await;
                    room.publish_update(PartialRoomData {
                        lock: Some(*locked),
                        ..Default::default()
                    })
                    .await;
                }
                state
                    .dispatch_plugin_event(PluginEvent::RoomModify {
                        user_id: *actor_user_id,
                        room_id: room_id.clone(),
                        data: serde_json::json!({"action":"lock","value":locked}).to_string(),
                    })
                    .await;
                RoomCommandResult::ok(
                    RoomCommandPayload::LockChanged {
                        room_id: room_id.clone(),
                        locked: *locked,
                    },
                    RoomCommandDelivery::PerRoomMailbox,
                )
            }

            RoomActorCommand::SetCycle {
                room_id,
                cycle,
                actor_user_id,
                ..
            } => {
                let actor_state = ctx.expect_actor_state();
                actor_state.state.set_cycle(*cycle);
                if let Some(ref room) = room {
                    room.set_cycle(*cycle);
                    room.send(Message::CycleRoom { cycle: *cycle }).await;
                    room.publish_update(PartialRoomData {
                        cycle: Some(*cycle),
                        ..Default::default()
                    })
                    .await;
                }
                state
                    .dispatch_plugin_event(PluginEvent::RoomModify {
                        user_id: *actor_user_id,
                        room_id: room_id.clone(),
                        data: serde_json::json!({"action":"cycle","value":cycle}).to_string(),
                    })
                    .await;
                RoomCommandResult::ok(
                    RoomCommandPayload::CycleChanged {
                        room_id: room_id.clone(),
                        cycle: *cycle,
                    },
                    RoomCommandDelivery::PerRoomMailbox,
                )
            }

            RoomActorCommand::SetHidden {
                room_id, hidden, ..
            } => {
                let actor_state = ctx.expect_actor_state();
                actor_state.state.set_hidden(*hidden);
                if let Some(ref room) = room {
                    room.set_hidden(*hidden);
                }
                state
                    .dispatch_plugin_event(PluginEvent::RoomModify {
                        user_id: 0,
                        room_id: room_id.clone(),
                        data: serde_json::json!({"action":"hidden","value":hidden}).to_string(),
                    })
                    .await;
                RoomCommandResult::ok(
                    RoomCommandPayload::HiddenChanged {
                        room_id: room_id.clone(),
                        hidden: *hidden,
                    },
                    RoomCommandDelivery::PerRoomMailbox,
                )
            }

            // ── Delegated to gateway (actor state synced afterward) ──────
            _ => {
                let result = Self::execute(
                    RoomCommandContext::with_room(gateway, state, room.clone().unwrap()),
                    command,
                )
                .await;
                // After gateway execution, sync actor state from room
                if result.is_ok() {
                    if let Some(actor) = ctx.actor.as_mut() {
                        if let Some(ref room) = room {
                            actor.actor_state = Some(
                                crate::room_actor::actor::RoomActorState::from_room(room).await,
                            );
                        }
                    }
                }
                result
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
