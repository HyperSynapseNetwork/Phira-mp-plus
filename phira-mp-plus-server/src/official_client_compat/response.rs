//! Official response contract mapping (P0-A).
//!
//! The official Phira client installs a single response callback per request
//! command and waits roughly 7 seconds. PMP must therefore answer every
//! request-type command with the matching `ServerCommand` variant — never a
//! silent drop. This module is the single mapping table used by the rate
//! limiter, permission checks and actor mailbox error paths.

use phira_mp_common::{ClientCommand, ServerCommand};

/// Commands that legitimately produce no response.
///
/// The official protocol treats Touches/Judges as fire-and-forget telemetry:
/// no `ServerCommand` is ever emitted for them. Every other request-type
/// command MUST receive a response. Dispatch code keeps this marker explicit so
/// a bare `None` can never be mistaken for a legitimate no-response path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoResponseExpected {
    /// Touch frames — telemetry only, no reply.
    Touches,
    /// Judge events — telemetry only, no reply.
    Judges,
}

/// Return the `NoResponseExpected` marker for commands whose dispatch path is
/// allowed to produce no `ServerCommand`. `None` means a response is required.
pub(crate) fn no_response_expected(cmd: &ClientCommand) -> Option<NoResponseExpected> {
    match cmd {
        ClientCommand::Touches { .. } => Some(NoResponseExpected::Touches),
        ClientCommand::Judges { .. } => Some(NoResponseExpected::Judges),
        _ => None,
    }
}

/// Map a request-type `ClientCommand` to the matching official `ServerCommand`
/// error variant. This is the compatibility contract: any PMP-internal failure
/// (rate limit, permission, missing room, mailbox error, shutdown) must surface
/// as the corresponding response instead of a silent `None`.
///
/// Returns `None` only for:
/// - Touches/Judges (`NoResponseExpected` — see [`no_response_expected`]);
/// - Authenticate-family replays, which have dedicated semantics handled before
///   dispatch and cannot legally be answered with this mapping.
pub(crate) fn official_error_response(
    cmd: &ClientCommand,
    error: String,
) -> Option<ServerCommand> {
    match cmd {
        ClientCommand::Chat { .. } => Some(ServerCommand::Chat(Err(error))),
        ClientCommand::CreateRoom { .. } => Some(ServerCommand::CreateRoom(Err(error))),
        ClientCommand::JoinRoom { .. } => Some(ServerCommand::JoinRoom(Err(error))),
        ClientCommand::SelectChart { .. } => Some(ServerCommand::SelectChart(Err(error))),
        ClientCommand::LockRoom { .. } => Some(ServerCommand::LockRoom(Err(error))),
        ClientCommand::CycleRoom { .. } => Some(ServerCommand::CycleRoom(Err(error))),
        ClientCommand::LeaveRoom => Some(ServerCommand::LeaveRoom(Err(error))),
        ClientCommand::RequestStart => Some(ServerCommand::RequestStart(Err(error))),
        ClientCommand::Ready => Some(ServerCommand::Ready(Err(error))),
        ClientCommand::CancelReady => Some(ServerCommand::CancelReady(Err(error))),
        ClientCommand::Played { .. } => Some(ServerCommand::Played(Err(error))),
        ClientCommand::Abort => Some(ServerCommand::Abort(Err(error))),
        // QueryRoomInfo is a PMP monitor extension; its error response is a
        // RoomResponse(Err), produced here and handled by session_dispatch.
        ClientCommand::QueryRoomInfo => Some(ServerCommand::RoomResponse(Err(error))),
        // Fire-and-forget telemetry — no response expected.
        ClientCommand::Touches { .. } | ClientCommand::Judges { .. } => None,
        ClientCommand::Ping => Some(ServerCommand::Pong),
        // Authenticate-family commands are handled before/independently of
        // dispatch; a replay has dedicated semantics.
        ClientCommand::Authenticate { .. }
        | ClientCommand::ConsoleAuthenticate { .. }
        | ClientCommand::RoomMonitorAuthenticate { .. }
        | ClientCommand::GameMonitorAuthenticate { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phira_mp_common::RoomId;

    fn room_id(id: &str) -> RoomId {
        id.to_string().try_into().unwrap()
    }

    fn chat(msg: &str) -> ClientCommand {
        ClientCommand::Chat {
            message: msg.to_string().try_into().unwrap(),
        }
    }

    #[test]
    fn error_response_covers_every_request_command() {
        let cases: &[(ClientCommand, fn(Option<ServerCommand>) -> String)] = &[
            (chat("hi"), |r| match r {
                Some(ServerCommand::Chat(Err(e))) => e,
                _ => panic!("Chat mapping broken"),
            }),
            (ClientCommand::CreateRoom { id: room_id("r1") }, |r| match r {
                Some(ServerCommand::CreateRoom(Err(e))) => e,
                _ => panic!("CreateRoom mapping broken"),
            }),
            (
                ClientCommand::JoinRoom {
                    id: room_id("r1"),
                    monitor: false,
                },
                |r| match r {
                    Some(ServerCommand::JoinRoom(Err(e))) => e,
                    _ => panic!("JoinRoom mapping broken"),
                },
            ),
            (ClientCommand::SelectChart { id: 1 }, |r| match r {
                Some(ServerCommand::SelectChart(Err(e))) => e,
                _ => panic!("SelectChart mapping broken"),
            }),
            (ClientCommand::LockRoom { lock: true }, |r| match r {
                Some(ServerCommand::LockRoom(Err(e))) => e,
                _ => panic!("LockRoom mapping broken"),
            }),
            (ClientCommand::CycleRoom { cycle: true }, |r| match r {
                Some(ServerCommand::CycleRoom(Err(e))) => e,
                _ => panic!("CycleRoom mapping broken"),
            }),
            (ClientCommand::LeaveRoom, |r| match r {
                Some(ServerCommand::LeaveRoom(Err(e))) => e,
                _ => panic!("LeaveRoom mapping broken"),
            }),
            (ClientCommand::RequestStart, |r| match r {
                Some(ServerCommand::RequestStart(Err(e))) => e,
                _ => panic!("RequestStart mapping broken"),
            }),
            (ClientCommand::Ready, |r| match r {
                Some(ServerCommand::Ready(Err(e))) => e,
                _ => panic!("Ready mapping broken"),
            }),
            (ClientCommand::CancelReady, |r| match r {
                Some(ServerCommand::CancelReady(Err(e))) => e,
                _ => panic!("CancelReady mapping broken"),
            }),
            (ClientCommand::Played { id: 1 }, |r| match r {
                Some(ServerCommand::Played(Err(e))) => e,
                _ => panic!("Played mapping broken"),
            }),
            (ClientCommand::Abort, |r| match r {
                Some(ServerCommand::Abort(Err(e))) => e,
                _ => panic!("Abort mapping broken"),
            }),
            (ClientCommand::QueryRoomInfo, |r| match r {
                Some(ServerCommand::RoomResponse(Err(e))) => e,
                _ => panic!("QueryRoomInfo mapping broken"),
            }),
        ];
        for (cmd, check) in cases {
            let resp = official_error_response(cmd, "boom".to_string());
            assert_eq!(check(resp), "boom");
        }
    }

    #[test]
    fn touches_and_judges_are_no_response() {
        let touches = ClientCommand::Touches {
            frames: Arc::new(vec![]),
        };
        let judges = ClientCommand::Judges {
            judges: Arc::new(vec![]),
        };
        assert_eq!(
            no_response_expected(&touches),
            Some(NoResponseExpected::Touches)
        );
        assert_eq!(
            no_response_expected(&judges),
            Some(NoResponseExpected::Judges)
        );
        assert!(official_error_response(&touches, "boom".into()).is_none());
        assert!(official_error_response(&judges, "boom".into()).is_none());
    }

    #[test]
    fn request_commands_require_a_response() {
        assert_eq!(no_response_expected(&chat("hi")), None);
        assert_eq!(no_response_expected(&ClientCommand::Ready), None);
        assert_eq!(
            no_response_expected(&ClientCommand::CreateRoom {
                id: room_id("r1")
            }),
            None
        );
    }

    #[test]
    fn replay_authenticate_has_dedicated_semantics() {
        let resp = official_error_response(
            &ClientCommand::Authenticate {
                token: "t".to_string().try_into().unwrap(),
            },
            "boom".into(),
        );
        assert!(resp.is_none());
    }

    // ── P1 protocol gate: lock the official enum discriminants ─────────────
    // The BinaryData macro serializes enum discriminants by declaration order.
    // These tests pin that order so an accidental insertion/reorder is caught.
    use std::sync::Arc;

    fn first_byte<T: phira_mp_common::BinaryData>(value: &T) -> u8 {
        let mut buf = Vec::new();
        phira_mp_common::encode_packet(value, &mut buf).unwrap();
        buf[0]
    }

    #[test]
    fn client_command_discriminants_match_official() {
        assert_eq!(first_byte(&ClientCommand::Ping), 0);
        assert_eq!(
            first_byte(&ClientCommand::Authenticate {
                token: "t".to_string().try_into().unwrap()
            }),
            1
        );
        assert_eq!(first_byte(&chat("hi")), 2);
        assert_eq!(first_byte(&ClientCommand::Touches { frames: Arc::new(vec![]) }), 3);
        assert_eq!(first_byte(&ClientCommand::Judges { judges: Arc::new(vec![]) }), 4);
        assert_eq!(first_byte(&ClientCommand::CreateRoom { id: room_id("r1") }), 5);
        assert_eq!(
            first_byte(&ClientCommand::JoinRoom { id: room_id("r1"), monitor: false }),
            6
        );
        assert_eq!(first_byte(&ClientCommand::LeaveRoom), 7);
        assert_eq!(first_byte(&ClientCommand::LockRoom { lock: true }), 8);
        assert_eq!(first_byte(&ClientCommand::CycleRoom { cycle: true }), 9);
        assert_eq!(first_byte(&ClientCommand::SelectChart { id: 1 }), 10);
        assert_eq!(first_byte(&ClientCommand::RequestStart), 11);
        assert_eq!(first_byte(&ClientCommand::Ready), 12);
        assert_eq!(first_byte(&ClientCommand::CancelReady), 13);
        assert_eq!(first_byte(&ClientCommand::Played { id: 1 }), 14);
        assert_eq!(first_byte(&ClientCommand::Abort), 15);
    }

    #[test]
    fn server_command_discriminants_match_official() {
        assert_eq!(first_byte(&ServerCommand::Pong), 0);
        assert_eq!(
            first_byte(&ServerCommand::Authenticate(Err("e".into()))),
            1
        );
        assert_eq!(first_byte(&ServerCommand::Chat(Err("e".into()))), 2);
        assert_eq!(
            first_byte(&ServerCommand::Message(phira_mp_common::Message::Ready {
                user: 1
            })),
            5
        );
        assert_eq!(first_byte(&ServerCommand::CreateRoom(Err("e".into()))), 8);
        assert_eq!(
            first_byte(&ServerCommand::JoinRoom(Err("e".into()))),
            9
        );
        assert_eq!(first_byte(&ServerCommand::LeaveRoom(Err("e".into()))), 11);
        assert_eq!(first_byte(&ServerCommand::LockRoom(Err("e".into()))), 12);
        assert_eq!(first_byte(&ServerCommand::CycleRoom(Err("e".into()))), 13);
        assert_eq!(first_byte(&ServerCommand::SelectChart(Err("e".into()))), 14);
        assert_eq!(first_byte(&ServerCommand::RequestStart(Err("e".into()))), 15);
        assert_eq!(first_byte(&ServerCommand::Ready(Err("e".into()))), 16);
        assert_eq!(first_byte(&ServerCommand::CancelReady(Err("e".into()))), 17);
        assert_eq!(first_byte(&ServerCommand::Played(Err("e".into()))), 18);
        assert_eq!(first_byte(&ServerCommand::Abort(Err("e".into()))), 19);
    }
}
