//! Session command dispatch.
//!
//! Extracted from session.rs to keep the session lifecycle separate from
//! the ClientCommand match dispatch.
//!
//! PMP42 P0-A: the official Phira client is an immutable compatibility target.
//! Every official request-type command MUST receive a matching `ServerCommand`
//! response. This module therefore maps permission / room / query failures to
//! the corresponding error response instead of returning a silent `None`.

#![allow(clippy::too_many_arguments)]

use crate::official_client_compat::response::official_error_response;
use crate::session::{SessionCategory, User};
use crate::tl;
use phira_mp_common::{ClientCommand, ServerCommand};
use std::sync::Arc;
use tracing::warn;

pub(crate) async fn process(
    user: Arc<User>,
    category: SessionCategory,
    cmd: ClientCommand,
    origin: crate::session::CommandOrigin,
    received_at: std::time::Instant,
    deadline: std::time::Instant,
) -> Option<ServerCommand> {
    // P0-F: JoinRoom(Ok) is delivered internally by join_room, so the minimum
    // response latency must be enforced there. `received_at` is the network
    // receive point captured in session.rs; `deadline` is derived from it at the
    // same boundary (P0-J) — never re-created here.
    macro_rules! get_room {
        (~ $d:ident) => {
            // Used by Touches/Judges: these are NoResponseExpected, so a
            // missing room simply drops the telemetry — no response is sent.
            let $d = match user.room.read().await.as_ref().map(Arc::clone) {
                Some(room) => room,
                None => {
                    warn!("no room");
                    return None;
                }
            };
        };
        ($d:ident) => {
            let $d = user
                .room
                .read()
                .await
                .as_ref()
                .map(Arc::clone)
                .ok_or_else(|| anyhow!("{}", tl!("no-room")))?;
        };
    }
    let permitted = crate::session_permissions::is_command_permitted(category, &cmd);
    if !permitted {
        warn!(
            user = user.id,
            ?category,
            ?cmd,
            "command rejected for session category"
        );
        // P0-A: a permission rejection must produce the official error response
        // for the command, never a silent drop.
        return official_error_response(&cmd, "permission denied".to_string());
    }

    match cmd {
        ClientCommand::Ping => unreachable!(),
        ClientCommand::Authenticate { .. } => Some(ServerCommand::Authenticate(Err(tl!(
            "repeated-authenticate"
        )))),
        ClientCommand::Chat { message } => {
            crate::session_actor::route_chat(
                user,
                category,
                message.into_inner(),
                origin,
                deadline,
            )
            .await
        }
        ClientCommand::LockRoom { lock } => {
            crate::session_actor::route_lock(user, lock, origin, deadline).await
        }
        ClientCommand::CycleRoom { cycle } => {
            crate::session_actor::route_cycle(user, cycle, origin, deadline).await
        }
        ClientCommand::LeaveRoom => {
            crate::session_actor::route_leave(user, category, origin, deadline).await
        }
        ClientCommand::CreateRoom { id } => {
            crate::session_actor::route_create(user, id, origin, deadline).await
        }
        ClientCommand::JoinRoom { id, monitor } => {
            crate::session_actor::route_join(
                user,
                category,
                id,
                monitor,
                received_at,
                origin,
                deadline,
            )
            .await
        }
        ClientCommand::SelectChart { id } => {
            crate::session_actor::route_select_chart(user, id, origin, deadline).await
        }
        ClientCommand::RequestStart => {
            crate::session_actor::route_request_start(user, origin, deadline).await
        }
        ClientCommand::Ready => crate::session_actor::route_ready(user, origin, deadline).await,
        ClientCommand::CancelReady => {
            crate::session_actor::route_cancel_ready(user, origin, deadline).await
        }
        ClientCommand::Played { id } => {
            crate::session_actor::route_played(user, id, origin, deadline).await
        }
        ClientCommand::Abort => crate::session_actor::route_abort(user, origin, deadline).await,
        ClientCommand::Touches { frames } => {
            // NoResponseExpected::Touches — fire-and-forget telemetry, the
            // official protocol never replies.
            get_room!(~ room);
            crate::session_telemetry::handle_touches(Arc::clone(&user), room, frames).await;
            None
        }
        ClientCommand::Judges { judges } => {
            // NoResponseExpected::Judges — fire-and-forget telemetry, the
            // official protocol never replies.
            get_room!(~ room);
            crate::session_telemetry::handle_judges(Arc::clone(&user), room, judges).await;
            None
        }
        ClientCommand::QueryRoomInfo => {
            match crate::session_room::query_room_info(Arc::clone(&user)).await {
                Ok(cmd) => Some(cmd),
                Err(err) => Some(ServerCommand::RoomResponse(Err(err.to_string()))),
            }
        }
        ClientCommand::RoomMonitorAuthenticate { .. }
        | ClientCommand::GameMonitorAuthenticate { .. }
        | ClientCommand::ConsoleAuthenticate { .. } => Some(ServerCommand::Authenticate(Err(
            tl!("already-authenticated"),
        ))),
    }
}
