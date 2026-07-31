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
) -> Option<ServerCommand> {
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
            crate::session_actor::route_chat(user, category, message.into_inner()).await
        }
        ClientCommand::LockRoom { lock } => crate::session_actor::route_lock(user, lock).await,
        ClientCommand::CycleRoom { cycle } => crate::session_actor::route_cycle(user, cycle).await,
        ClientCommand::LeaveRoom => crate::session_actor::route_leave(user, category).await,
        ClientCommand::CreateRoom { id } => crate::session_actor::route_create(user, id).await,
        ClientCommand::JoinRoom { id, monitor } => {
            crate::session_actor::route_join(user, category, id, monitor).await
        }
        ClientCommand::SelectChart { id } => {
            crate::session_actor::route_select_chart(user, id).await
        }
        ClientCommand::RequestStart => crate::session_actor::route_request_start(user).await,
        ClientCommand::Ready => crate::session_actor::route_ready(user).await,
        ClientCommand::CancelReady => crate::session_actor::route_cancel_ready(user).await,
        ClientCommand::Played { id } => crate::session_actor::route_played(user, id).await,
        ClientCommand::Abort => crate::session_actor::route_abort(user).await,
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
