//! Session permission checks extracted from session_dispatch.
//!
//! Keeps permission rules in one place so the dispatch match doesn't need
//! to carry inline category checks.

use crate::session::SessionCategory;
use phira_mp_common::ClientCommand;

/// Determine whether a command is permitted for the given session category.
pub fn is_command_permitted(category: SessionCategory, cmd: &ClientCommand) -> bool {
    match category {
        SessionCategory::Normal => !matches!(cmd, ClientCommand::QueryRoomInfo),
        SessionCategory::GameMonitor => matches!(
            cmd,
            ClientCommand::JoinRoom { monitor: true, .. }
                | ClientCommand::LeaveRoom
                | ClientCommand::Ready
                | ClientCommand::CancelReady
                | ClientCommand::Authenticate { .. }
                | ClientCommand::ConsoleAuthenticate { .. }
                | ClientCommand::RoomMonitorAuthenticate { .. }
                | ClientCommand::GameMonitorAuthenticate { .. }
        ),
        SessionCategory::RoomMonitor => matches!(
            cmd,
            ClientCommand::QueryRoomInfo
                | ClientCommand::Authenticate { .. }
                | ClientCommand::ConsoleAuthenticate { .. }
                | ClientCommand::RoomMonitorAuthenticate { .. }
                | ClientCommand::GameMonitorAuthenticate { .. }
        ),
        SessionCategory::Console => matches!(
            cmd,
            ClientCommand::Authenticate { .. }
                | ClientCommand::ConsoleAuthenticate { .. }
                | ClientCommand::RoomMonitorAuthenticate { .. }
                | ClientCommand::GameMonitorAuthenticate { .. }
        ),
    }
}

/// Whether the command is an authentication-type command that should only
/// be processed once per session.
pub fn is_auth_command(cmd: &ClientCommand) -> bool {
    matches!(
        cmd,
        ClientCommand::Authenticate { .. }
            | ClientCommand::ConsoleAuthenticate { .. }
            | ClientCommand::RoomMonitorAuthenticate { .. }
            | ClientCommand::GameMonitorAuthenticate { .. }
    )
}
