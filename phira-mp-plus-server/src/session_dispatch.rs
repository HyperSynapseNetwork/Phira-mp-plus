//! Session command dispatch.
//!
//! Extracted from session.rs to keep the session lifecycle separate from
//! the ClientCommand match dispatch.

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
        ($d:ident, $($pt:tt)*) => {
            let $d = user
                .room
                .read()
                .await
                .as_ref()
                .map(Arc::clone)
                .ok_or_else(|| anyhow!("{}", tl!("no-room")))?;
            if !matches!(&*$d.cached_state.read().await, $($pt)*) {
                bail!("{}", tl!("invalid-state"));
            }
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
        return None;
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
            get_room!(~ room);
            crate::session_telemetry::handle_touches(Arc::clone(&user), room, frames).await;
            None
        }
        ClientCommand::Judges { judges } => {
            get_room!(~ room);
            crate::session_telemetry::handle_judges(Arc::clone(&user), room, judges).await;
            None
        }
        ClientCommand::QueryRoomInfo => {
            match crate::session_room::query_room_info(Arc::clone(&user)).await {
                Ok(cmd) => Some(cmd),
                Err(_) => None,
            }
        }
        ClientCommand::RoomMonitorAuthenticate { .. }
        | ClientCommand::GameMonitorAuthenticate { .. }
        | ClientCommand::ConsoleAuthenticate { .. } => Some(ServerCommand::Authenticate(Err(
            "already authenticated".into(),
        ))),
    }
}
