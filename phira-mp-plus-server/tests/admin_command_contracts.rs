use phira_mp_plus_server::session_room::decode_admin_room_command;
// Admin `_` command parsing contracts.
//
// Tests the rules:
// - First `_` = admin command entry
// - Subsequent single `_` = space
// - Double `__` = literal underscore
// - `--` continuation concatenation

use phira_mp_plus_server::cli::collect_cli_continuation;

/// Uses production decode_admin_room_command from session_room.rs
#[test]
fn admin_id_list_decodes_from_underscore() {
    let decoded = decode_admin_room_command("_admin-id_list");
    assert_eq!(decoded, "admin-id list");
}

#[test]
fn room_list_decodes_from_underscore() {
    let decoded = decode_admin_room_command("_room_list");
    assert_eq!(decoded, "room list");
}

#[test]
fn double_underscore_becomes_literal() {
    let decoded = decode_admin_room_command("_room_info_my__room");
    assert_eq!(decoded, "room info my_room");
}

#[test]
fn triple_underscore_is_space_then_literal() {
    let decoded = decode_admin_room_command("_room_set___test");
    // Greedy parse: __ → literal _, _ → space, so "set_ test" not "set _ test"
    assert_eq!(decoded, "room set_ test");
}

#[test]
fn no_leading_underscore_is_passthrough() {
    let decoded = decode_admin_room_command("room list");
    assert_eq!(decoded, "room list");
}

#[test]
fn continuation_joins_lines() {
    let mut pending: Option<String> = None;
    let r1 = collect_cli_continuation(&mut pending, "room set a--".to_string()).unwrap();
    assert!(r1.is_none(), "first line ending with -- should be pending");
    assert!(pending.is_some(), "should have pending content");

    let r2 = collect_cli_continuation(&mut pending, "-- b".to_string()).unwrap();
    assert_eq!(r2, Some("room set a b".to_string()));
    assert!(pending.is_none(), "pending should be consumed");
}

#[test]
fn continuation_error_clears_pending() {
    let mut pending = Some("room set".to_string());
    // Next line must start with --
    let result = collect_cli_continuation(&mut pending, "invalid".to_string());
    assert!(result.is_err(), "should reject continuation without --");
    assert_eq!(pending, None, "pending should be cleared on error");
}

#[test]
fn empty_line_returns_none() {
    let mut pending: Option<String> = None;
    let r = collect_cli_continuation(&mut pending, "  ".to_string()).unwrap();
    assert!(r.is_none());
}
