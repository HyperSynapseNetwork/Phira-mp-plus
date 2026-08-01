//! Golden packet wire-format tests for the official Phira MP protocol.
//!
//! STABILITY WARNING (PMP44 §27 / §33):
//! These tests PIN the exact byte layout of every packet on the wire. They are
//! the compatibility contract with the official Phira protocol. Any change to
//! an enum discriminant, field order, or primitive encoding (uleb vs fixed,
//! LE vs BE, bool width, string framing) will break these tests — that is
//! intentional. Do NOT "fix" a failing golden test by editing the expected
//! bytes; if the layout truly must change, run a full protocol-compat audit
//! against the official Phira client/server first (PMP44 §27) and update the
//! pins here with an explicit record of the compat decision (PMP44 §33).
//!
//! Discriminant rule: the `BinaryData` derive macro in phira-mp-macros assigns
//! enum discriminants as the VARIANT INDEX AS u8 (0,1,2,... in declaration
//! order), read/written as a single little-endian byte — independent of any
//! `#[repr(u8)]` attribute. This is why the numbers below are simply the
//! declaration order of each enum in command.rs.
//!
//! Primitive encodings (bin.rs):
//!   u8 / i8 / bool           -> 1 byte (bool: 0 or 1)
//!   u16/i16 (f16 bits)       -> 2 bytes LE
//!   u32 / i32 / f32          -> 4 bytes LE
//!   u64 / i64 / f64          -> 8 bytes LE
//!   String / Varchar<N> / RoomId -> uleb length prefix + raw UTF-8 bytes
//!   Vec<T>                   -> uleb length + each element
//!   Option<T>                -> bool (1=Some) + value if Some
//!   Result<A,B>              -> bool (1=Ok) + value
//!   HashMap<K,V>             -> uleb length + each (K,V) pair
//!   enum                     -> 1 byte u8 discriminant (variant index)

use phira_mp_common::*;
use std::collections::HashMap;
use std::sync::Arc;

// ── helpers ────────────────────────────────────────────────────────────────────

fn encode_bin(payload: &impl BinaryData) -> Vec<u8> {
    let mut out = Vec::new();
    encode_packet(payload, &mut out).expect("encode_packet should succeed");
    out
}

fn decode_bin<T: BinaryData>(bytes: &[u8]) -> T {
    decode_packet(bytes).expect("decode_packet should succeed")
}

/// First byte of the encoded packet = enum discriminant (u8).
fn disc(payload: &impl BinaryData) -> u8 {
    encode_bin(payload)[0]
}

fn varc<const N: usize>(s: &str) -> Varchar<N> {
    Varchar::try_from(s.to_string()).unwrap()
}

fn room(s: &str) -> RoomId {
    RoomId::try_from(s.to_string()).unwrap()
}

fn user(id: i32, name: &str, monitor: bool) -> UserInfo {
    UserInfo {
        id,
        name: name.to_string(),
        monitor,
    }
}

fn touch_frame() -> TouchFrame {
    TouchFrame {
        time: 1.5,
        points: vec![(1i8, CompactPos::new(2.0, 3.0))],
    }
}

fn judge_event() -> JudgeEvent {
    JudgeEvent {
        time: 1.0,
        line_id: 2,
        note_id: 3,
        judgement: Judgement::Good,
    }
}

fn client_room_state() -> ClientRoomState {
    ClientRoomState {
        id: room("abc"),
        state: RoomState::SelectChart(Some(3)),
        live: true,
        locked: false,
        cycle: true,
        is_host: true,
        is_ready: false,
        users: HashMap::new(),
    }
}

/// Round-trip encode -> decode -> assert Debug output is identical.
fn round_trip_ok<T: BinaryData + std::fmt::Debug>(val: &T) {
    let bytes = encode_bin(val);
    let decoded: T = decode_bin(&bytes);
    assert_eq!(
        format!("{:?}", val),
        format!("{:?}", decoded),
        "round-trip Debug mismatch; encoded bytes: {bytes:?}"
    );
}

// ── 1. discriminant golden values ──────────────────────────────────────────────
// Official-protocol wire-format pins (PMP44 §27/§33). These MUST NOT change
// without a protocol-compat audit.

#[test]
fn client_command_discriminants() {
    assert_eq!(disc(&ClientCommand::Ping), 0);
    assert_eq!(disc(&ClientCommand::Authenticate { token: varc("x") }), 1);
    assert_eq!(disc(&ClientCommand::Chat { message: varc("hi") }), 2);
    assert_eq!(disc(&ClientCommand::Touches { frames: Arc::new(vec![touch_frame()]) }), 3);
    assert_eq!(disc(&ClientCommand::Judges { judges: Arc::new(vec![judge_event()]) }), 4);
    assert_eq!(disc(&ClientCommand::CreateRoom { id: room("abc") }), 5);
    assert_eq!(disc(&ClientCommand::JoinRoom { id: room("abc"), monitor: true }), 6);
    assert_eq!(disc(&ClientCommand::LeaveRoom), 7);
    assert_eq!(disc(&ClientCommand::LockRoom { lock: true }), 8);
    assert_eq!(disc(&ClientCommand::CycleRoom { cycle: true }), 9);
    assert_eq!(disc(&ClientCommand::SelectChart { id: 1 }), 10);
    assert_eq!(disc(&ClientCommand::RequestStart), 11);
    assert_eq!(disc(&ClientCommand::Ready), 12);
    assert_eq!(disc(&ClientCommand::CancelReady), 13);
    assert_eq!(disc(&ClientCommand::Played { id: 2 }), 14);
    assert_eq!(disc(&ClientCommand::Abort), 15);
    assert_eq!(disc(&ClientCommand::ConsoleAuthenticate { token: varc("y") }), 16);
    assert_eq!(disc(&ClientCommand::RoomMonitorAuthenticate { key: vec![1, 2, 3] }), 17);
    assert_eq!(disc(&ClientCommand::QueryRoomInfo), 18);
    assert_eq!(disc(&ClientCommand::GameMonitorAuthenticate { token: varc("z") }), 19);
}

#[test]
fn message_discriminants() {
    assert_eq!(disc(&Message::Chat { user: 7, content: "hi".into() }), 0);
    assert_eq!(disc(&Message::CreateRoom { user: 1 }), 1);
    assert_eq!(disc(&Message::JoinRoom { user: 1, name: "alice".into() }), 2);
    assert_eq!(disc(&Message::LeaveRoom { user: 1, name: "alice".into() }), 3);
    assert_eq!(disc(&Message::NewHost { user: 1 }), 4);
    assert_eq!(disc(&Message::SelectChart { user: 1, name: "c".into(), id: 5 }), 5);
    assert_eq!(disc(&Message::GameStart { user: 1 }), 6);
    assert_eq!(disc(&Message::Ready { user: 1 }), 7);
    assert_eq!(disc(&Message::CancelReady { user: 1 }), 8);
    assert_eq!(disc(&Message::CancelGame { user: 1 }), 9);
    assert_eq!(disc(&Message::StartPlaying), 10);
    assert_eq!(
        disc(&Message::Played {
            user: 1,
            score: 2,
            accuracy: 3.5,
            full_combo: true,
            perfect: 4,
            good: 5,
            bad: 6,
            miss: 7,
            max_combo: 8,
        }),
        11
    );
    assert_eq!(disc(&Message::GameEnd), 12);
    assert_eq!(disc(&Message::Abort { user: 1 }), 13);
    assert_eq!(disc(&Message::LockRoom { lock: true }), 14);
    assert_eq!(disc(&Message::CycleRoom { cycle: true }), 15);
}

#[test]
fn server_command_discriminants() {
    assert_eq!(disc(&ServerCommand::Pong), 0);
    assert_eq!(
        disc(&ServerCommand::Authenticate(Ok((user(1, "alice", false), None)))),
        1
    );
    assert_eq!(disc(&ServerCommand::Chat(Ok(()))), 2);
    assert_eq!(
        disc(&ServerCommand::Touches {
            player: 1,
            frames: Arc::new(vec![touch_frame()])
        }),
        3
    );
    assert_eq!(
        disc(&ServerCommand::Judges {
            player: 1,
            judges: Arc::new(vec![judge_event()])
        }),
        4
    );
    assert_eq!(disc(&ServerCommand::Message(Message::Chat { user: 7, content: "hi".into() })), 5);
    assert_eq!(disc(&ServerCommand::ChangeState(RoomState::WaitingForReady)), 6);
    assert_eq!(disc(&ServerCommand::ChangeHost(true)), 7);
    assert_eq!(disc(&ServerCommand::CreateRoom(Ok(()))), 8);
    assert_eq!(
        disc(&ServerCommand::JoinRoom(Ok(JoinRoomResponse {
            state: RoomState::WaitingForReady,
            users: vec![],
            live: true,
        }))),
        9
    );
    assert_eq!(disc(&ServerCommand::OnJoinRoom(user(1, "alice", false))), 10);
    assert_eq!(disc(&ServerCommand::LeaveRoom(Ok(()))), 11);
    assert_eq!(disc(&ServerCommand::LockRoom(Ok(()))), 12);
    assert_eq!(disc(&ServerCommand::CycleRoom(Ok(()))), 13);
    assert_eq!(disc(&ServerCommand::SelectChart(Ok(()))), 14);
    assert_eq!(disc(&ServerCommand::RequestStart(Ok(()))), 15);
    assert_eq!(disc(&ServerCommand::Ready(Ok(()))), 16);
    assert_eq!(disc(&ServerCommand::CancelReady(Ok(()))), 17);
    assert_eq!(disc(&ServerCommand::Played(Ok(()))), 18);
    assert_eq!(disc(&ServerCommand::Abort(Ok(()))), 19);
    assert_eq!(
        disc(&ServerCommand::RoomResponse(Ok((HashMap::new(), HashMap::new())))),
        20
    );
    assert_eq!(disc(&ServerCommand::RoomEvent(RoomEvent::LeaveRoom { room: room("abc"), user: 1 })), 21);
    assert_eq!(disc(&ServerCommand::UserVisit(1)), 22);
}

// ── 2. round-trip encode/decode ────────────────────────────────────────────────
// These also serve as wire-format pins: if a decode ever reads a byte stream
// that its own encoder cannot produce, or an encoder produces bytes that the
// decoder rejects, the round-trip fails.

#[test]
fn round_trip_all_client_commands() {
    round_trip_ok(&ClientCommand::Ping);
    round_trip_ok(&ClientCommand::Authenticate { token: varc("token") });
    round_trip_ok(&ClientCommand::Chat { message: varc("hello world") });
    round_trip_ok(&ClientCommand::Touches {
        frames: Arc::new(vec![touch_frame(), touch_frame()]),
    });
    round_trip_ok(&ClientCommand::Judges {
        judges: Arc::new(vec![judge_event()]),
    });
    round_trip_ok(&ClientCommand::CreateRoom { id: room("abc") });
    round_trip_ok(&ClientCommand::JoinRoom {
        id: room("abc"),
        monitor: true,
    });
    round_trip_ok(&ClientCommand::LeaveRoom);
    round_trip_ok(&ClientCommand::LockRoom { lock: false });
    round_trip_ok(&ClientCommand::CycleRoom { cycle: true });
    round_trip_ok(&ClientCommand::SelectChart { id: -42 });
    round_trip_ok(&ClientCommand::RequestStart);
    round_trip_ok(&ClientCommand::Ready);
    round_trip_ok(&ClientCommand::CancelReady);
    round_trip_ok(&ClientCommand::Played { id: 999 });
    round_trip_ok(&ClientCommand::Abort);
    round_trip_ok(&ClientCommand::ConsoleAuthenticate { token: varc("console") });
    round_trip_ok(&ClientCommand::RoomMonitorAuthenticate {
        key: vec![0, 255, 128, 7],
    });
    round_trip_ok(&ClientCommand::QueryRoomInfo);
    round_trip_ok(&ClientCommand::GameMonitorAuthenticate { token: varc("gm") });
}

#[test]
fn round_trip_all_messages() {
    round_trip_ok(&Message::Chat {
        user: -1,
        content: "hi".into(),
    });
    round_trip_ok(&Message::CreateRoom { user: 2 });
    round_trip_ok(&Message::JoinRoom {
        user: 3,
        name: "bob".into(),
    });
    round_trip_ok(&Message::LeaveRoom {
        user: 4,
        name: "carol".into(),
    });
    round_trip_ok(&Message::NewHost { user: 5 });
    round_trip_ok(&Message::SelectChart {
        user: 6,
        name: "chart".into(),
        id: 7,
    });
    round_trip_ok(&Message::GameStart { user: 8 });
    round_trip_ok(&Message::Ready { user: 9 });
    round_trip_ok(&Message::CancelReady { user: 10 });
    round_trip_ok(&Message::CancelGame { user: 11 });
    round_trip_ok(&Message::StartPlaying);
    round_trip_ok(&Message::Played {
        user: 12,
        score: 1234567,
        accuracy: 99.97,
        full_combo: true,
        perfect: 111,
        good: 22,
        bad: 3,
        miss: 4,
        max_combo: 555,
    });
    round_trip_ok(&Message::GameEnd);
    round_trip_ok(&Message::Abort { user: 13 });
    round_trip_ok(&Message::LockRoom { lock: true });
    round_trip_ok(&Message::CycleRoom { cycle: false });
}

#[test]
fn round_trip_all_server_commands() {
    round_trip_ok(&ServerCommand::Pong);
    round_trip_ok(&ServerCommand::Authenticate(Ok((user(1, "alice", true), Some(client_room_state())))));
    round_trip_ok(&ServerCommand::Authenticate(Err("bad token".to_string())));
    round_trip_ok(&ServerCommand::Chat(Ok(())));
    round_trip_ok(&ServerCommand::Chat(Err("no permission".to_string())));
    round_trip_ok(&ServerCommand::Touches {
        player: 1,
        frames: Arc::new(vec![touch_frame()]),
    });
    round_trip_ok(&ServerCommand::Judges {
        player: 2,
        judges: Arc::new(vec![judge_event()]),
    });
    round_trip_ok(&ServerCommand::Message(Message::Chat {
        user: 7,
        content: "hi".into(),
    }));
    round_trip_ok(&ServerCommand::ChangeState(RoomState::SelectChart(None)));
    round_trip_ok(&ServerCommand::ChangeState(RoomState::Playing));
    round_trip_ok(&ServerCommand::ChangeHost(false));
    round_trip_ok(&ServerCommand::CreateRoom(Ok(())));
    round_trip_ok(&ServerCommand::CreateRoom(Err("room exists".to_string())));
    round_trip_ok(&ServerCommand::JoinRoom(Ok(JoinRoomResponse {
        state: RoomState::SelectChart(Some(3)),
        users: vec![user(1, "alice", false), user(2, "bob", true)],
        live: true,
    })));
    round_trip_ok(&ServerCommand::JoinRoom(Err("full".to_string())));
    round_trip_ok(&ServerCommand::OnJoinRoom(user(3, "carol", false)));
    round_trip_ok(&ServerCommand::LeaveRoom(Ok(())));
    round_trip_ok(&ServerCommand::LockRoom(Ok(())));
    round_trip_ok(&ServerCommand::CycleRoom(Ok(())));
    round_trip_ok(&ServerCommand::SelectChart(Ok(())));
    round_trip_ok(&ServerCommand::RequestStart(Ok(())));
    round_trip_ok(&ServerCommand::Ready(Ok(())));
    round_trip_ok(&ServerCommand::CancelReady(Ok(())));
    round_trip_ok(&ServerCommand::Played(Ok(())));
    round_trip_ok(&ServerCommand::Abort(Ok(())));
    round_trip_ok(&ServerCommand::RoomResponse(Ok(({
        let mut rooms = HashMap::new();
        let mut data = RoomData {
            host: 1,
            users: vec![1, 2],
            lock: false,
            cycle: true,
            chart: Some(5),
            state: StrippedRoomState::WaitingForReady,
            rounds: vec![],
        };
        data.rounds.push(RoundData {
            chart: 5,
            records: vec![Record {
                id: 1,
                player: 1,
                score: 100,
                perfect: 10,
                good: 2,
                bad: 0,
                miss: 1,
                max_combo: 12,
                accuracy: 98.5,
                full_combo: true,
                std: 1.0,
                std_score: 2.5,
            }],
        });
        rooms.insert(room("abc"), data);
        rooms
    }, HashMap::new()))));
    round_trip_ok(&ServerCommand::RoomEvent(RoomEvent::CreateRoom {
        room: room("abc"),
        data: RoomData {
            host: 1,
            users: vec![1],
            lock: false,
            cycle: false,
            chart: None,
            state: StrippedRoomState::SelectingChart,
            rounds: vec![],
        },
    }));
    round_trip_ok(&ServerCommand::RoomEvent(RoomEvent::LeaveRoom {
        room: room("abc"),
        user: 9,
    }));
    round_trip_ok(&ServerCommand::UserVisit(77));
}

// ── 3. field layout golden (exact byte arrays) ─────────────────────────────────
// Official-protocol wire-format pins (PMP44 §27/§33). Each byte is load-bearing.

#[test]
fn golden_simple_packets() {
    // Unit variants: just the discriminant byte.
    assert_eq!(encode_bin(&ClientCommand::Ping), vec![0x00]);
    assert_eq!(encode_bin(&ClientCommand::Ready), vec![0x0C]);
    assert_eq!(encode_bin(&ClientCommand::LeaveRoom), vec![0x07]);
    assert_eq!(encode_bin(&ClientCommand::Abort), vec![0x0F]);
    assert_eq!(encode_bin(&ServerCommand::Pong), vec![0x00]);
    assert_eq!(encode_bin(&Message::StartPlaying), vec![0x0A]);
    assert_eq!(encode_bin(&Message::GameEnd), vec![0x0C]);

    // LockRoom { lock: true }: discriminant 8, then bool true.
    assert_eq!(encode_bin(&ClientCommand::LockRoom { lock: true }), vec![0x08, 0x01]);
    // bool false is a distinct byte value.
    assert_eq!(encode_bin(&ClientCommand::LockRoom { lock: false }), vec![0x08, 0x00]);

    // Authenticate { token: "x" }: disc 1, Varchar<32> = uleb(1) + 'x'.
    assert_eq!(
        encode_bin(&ClientCommand::Authenticate { token: varc("x") }),
        vec![0x01, 0x01, 0x78]
    );

    // Chat { message: "hi" }: disc 2, Varchar<200> = uleb(2) + "hi".
    assert_eq!(
        encode_bin(&ClientCommand::Chat { message: varc("hi") }),
        vec![0x02, 0x02, 0x68, 0x69]
    );

    // ServerCommand::Chat(Err("nope")): disc 2, Result Err (bool 0), String "nope".
    assert_eq!(
        encode_bin(&ServerCommand::Chat(Err("nope".to_string()))),
        vec![0x02, 0x00, 0x04, 0x6E, 0x6F, 0x70, 0x65]
    );

    // ChangeState(WaitingForReady): disc 6, RoomState::WaitingForReady = 1.
    assert_eq!(
        encode_bin(&ServerCommand::ChangeState(RoomState::WaitingForReady)),
        vec![0x06, 0x01]
    );
    // ChangeState(SelectChart(Some(3))): disc 6, RoomState=0, Option Some (1), i32 3 LE.
    assert_eq!(
        encode_bin(&ServerCommand::ChangeState(RoomState::SelectChart(Some(3)))),
        vec![0x06, 0x00, 0x01, 0x03, 0x00, 0x00, 0x00]
    );
    // ChangeState(SelectChart(None)): Option None = bool 0, no payload follows.
    assert_eq!(
        encode_bin(&ServerCommand::ChangeState(RoomState::SelectChart(None))),
        vec![0x06, 0x00, 0x00]
    );
}

#[test]
fn golden_message_chat() {
    // ServerCommand::Message(Message::Chat { user: 7, content: "hi" })
    //   [0x05] ServerCommand::Message
    //   [0x00] Message::Chat
    //   [0x07 00 00 00] user i32 LE = 7
    //   [0x02 'h' 'i'] String "hi" = uleb(2) + bytes
    assert_eq!(
        encode_bin(&ServerCommand::Message(Message::Chat {
            user: 7,
            content: "hi".into()
        })),
        vec![0x05, 0x00, 0x07, 0x00, 0x00, 0x00, 0x02, 0x68, 0x69]
    );
}

#[test]
fn golden_touches_frames() {
    // ClientCommand::Touches { frames: [ TouchFrame { time: 1.5, points: [(1, CompactPos(2.0, 3.0))] } ] }
    //   [0x03] ClientCommand::Touches
    //   [0x01] Vec length 1
    //   [0x00 00 C0 3F] TouchFrame.time f32 1.5 LE
    //   [0x01] points Vec length 1
    //   [0x01] tuple.0 i8 = 1
    //   [0x00 40] CompactPos.x f16 2.0 LE
    //   [0x00 42] CompactPos.y f16 3.0 LE
    assert_eq!(
        encode_bin(&ClientCommand::Touches {
            frames: Arc::new(vec![touch_frame()])
        }),
        vec![0x03, 0x01, 0x00, 0x00, 0xC0, 0x3F, 0x01, 0x01, 0x00, 0x40, 0x00, 0x42]
    );
}

#[test]
fn golden_authenticate_nested() {
    // The most complex client-facing packet: ServerCommand::Authenticate wrapping
    // a full ClientRoomState. Pins nested enum + Option + RoomId + RoomState.
    //
    // ServerCommand::Authenticate(Ok((UserInfo, Some(ClientRoomState))))
    //
    // [0x01]                     ServerCommand::Authenticate (disc 1)
    // [0x01]                     Result::Ok (bool 1)
    //   [0x01 00 00 00]          UserInfo.id i32 LE = 1
    //   [0x05 'a' 'l' 'i' 'c' 'e'] UserInfo.name = "alice"
    //   [0x01]                   UserInfo.monitor bool = true
    //   [0x01]                   Option::Some (bool 1)
    //     [0x03 'a' 'b' 'c']     RoomId "abc" (Varchar<20> framing)
    //     [0x00]                 RoomState::SelectChart (disc 0)
    //     [0x01]                 Option::Some (bool 1)
    //     [0x03 00 00 00]        chart i32 LE = 3
    //     [0x01]                 live = true
    //     [0x00]                 locked = false
    //     [0x01]                 cycle = true
    //     [0x01]                 is_host = true
    //     [0x00]                 is_ready = false
    //     [0x00]                 users HashMap len 0
    let packet = ServerCommand::Authenticate(Ok((user(1, "alice", true), Some(client_room_state()))));
    assert_eq!(
        encode_bin(&packet),
        vec![
            0x01,             // ServerCommand::Authenticate
            0x01,             // Result::Ok
            0x01, 0x00, 0x00, 0x00, // UserInfo.id = 1
            0x05, 0x61, 0x6C, 0x69, 0x63, 0x65, // "alice"
            0x01,             // monitor = true
            0x01,             // Option::Some
            0x03, 0x61, 0x62, 0x63, // RoomId "abc"
            0x00,             // RoomState::SelectChart
            0x01,             // Option::Some
            0x03, 0x00, 0x00, 0x00, // chart = 3
            0x01,             // live
            0x00,             // locked
            0x01,             // cycle
            0x01,             // is_host
            0x00,             // is_ready
            0x00,             // users (empty HashMap)
        ]
    );
}
