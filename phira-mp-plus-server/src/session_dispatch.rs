//! Session command dispatch.
//!
//! Extracted from session.rs to keep the session lifecycle separate from
//! the ClientCommand match dispatch.

use crate::session::{SessionCategory, User};
use crate::tl;
use anyhow::{anyhow, bail, Result};
use phira_mp_common::{ClientCommand, ServerCommand};
use std::sync::Arc;
use tracing::warn;

pub(crate) async fn process(
    user: Arc<User>,
    category: SessionCategory,
    cmd: ClientCommand,
) -> Option<ServerCommand> {
    #[inline]
    fn err_to_str<T>(result: Result<T>) -> Result<T, String> {
        result.map_err(|it| it.to_string())
    }

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
            if !matches!(&*$d.state.read().await, $($pt)*) {
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
            if !user.server.config.chat_enabled {
                return None;
            }
            let res: Result<()> = async move {
                get_room!(room);
                let content = message.into_inner();
                if let Some(db) = crate::internal_hooks::DB.get() {
                    db.record_room_event_sync(
                        "chat.message",
                        Some(room.id.to_string()),
                        Some(user.id),
                        serde_json::json!({
                            "room_id": room.id.to_string(),
                            "user_id": user.id,
                            "user_name": user.name.clone(),
                            "message": content.clone(),
                        }),
                    );
                }
                room.send_as(&user, content).await;
                user.server
                    .publish_runtime_event(crate::event_bus::MpEvent::ChatMessage {
                        room_id: Some(room.id.clone()),
                        user_id: user.id,
                    });
                Ok(())
            }
            .await;
            Some(ServerCommand::Chat(err_to_str(res)))
        }
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
        ClientCommand::CreateRoom { id } => {
            let res = crate::session_room::create_room(Arc::clone(&user), id).await;
            Some(ServerCommand::CreateRoom(err_to_str(res)))
        }
        ClientCommand::JoinRoom { id, monitor } => {
            let res =
                crate::session_room::join_room(Arc::clone(&user), category, id, monitor).await;
            Some(ServerCommand::JoinRoom(err_to_str(res)))
        }
        ClientCommand::LeaveRoom => {
            let res = crate::session_room::leave_room(Arc::clone(&user), category).await;
            Some(ServerCommand::LeaveRoom(err_to_str(res)))
        }
        ClientCommand::LockRoom { lock } => {
            let res = crate::session_room::lock_room(Arc::clone(&user), lock).await;
            Some(ServerCommand::LockRoom(err_to_str(res)))
        }
        ClientCommand::CycleRoom { cycle } => {
            let res = crate::session_room::cycle_room(Arc::clone(&user), cycle).await;
            Some(ServerCommand::CycleRoom(err_to_str(res)))
        }
        ClientCommand::SelectChart { id } => {
            let res = crate::session_room::select_chart(Arc::clone(&user), id).await;
            Some(ServerCommand::SelectChart(err_to_str(res)))
        }
        ClientCommand::RequestStart => {
            let res = crate::session_room::request_start(Arc::clone(&user)).await;
            Some(ServerCommand::RequestStart(err_to_str(res)))
        }
        ClientCommand::Ready => {
            let res = crate::session_room::ready(Arc::clone(&user)).await;
            Some(ServerCommand::Ready(err_to_str(res)))
        }
        ClientCommand::CancelReady => {
            let res = crate::session_room::cancel_ready(Arc::clone(&user)).await;
            Some(ServerCommand::CancelReady(err_to_str(res)))
        }
        ClientCommand::Played { id } => {
            let res = crate::session_room::played(Arc::clone(&user), id).await;
            Some(ServerCommand::Played(err_to_str(res)))
        }
        ClientCommand::Abort => {
            let res = crate::session_room::abort(Arc::clone(&user)).await;
            Some(ServerCommand::Abort(err_to_str(res)))
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
