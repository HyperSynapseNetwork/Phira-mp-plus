//! JSON-memory bridge encode/decode for the current plugin ABI.

use phira_mp_plus_server_api::PluginEvent;

/// Encode a host event for the JSON-memory WASM plugin ABI.
pub fn encode_plugin_event_json(event: &PluginEvent) -> String {
    match event {
        PluginEvent::UserConnect { user_id, user_name, user_ip } => serde_json::json!({
            "type": "user_connect", "user_id": user_id, "user_name": user_name, "user_ip": user_ip,
        }),
        PluginEvent::UserDisconnect { user_id, user_name } => serde_json::json!({
            "type": "user_disconnect", "user_id": user_id, "user_name": user_name,
        }),
        PluginEvent::RoomCreate { user_id, room_id } => serde_json::json!({
            "type": "room_create", "user_id": user_id, "room_id": room_id,
        }),
        PluginEvent::RoomJoin { user_id, room_id, is_monitor } => serde_json::json!({
            "type": "room_join", "user_id": user_id, "room_id": room_id, "is_monitor": is_monitor,
        }),
        PluginEvent::RoomLeave { user_id, room_id } => serde_json::json!({
            "type": "room_leave", "user_id": user_id, "room_id": room_id,
        }),
        PluginEvent::RoomModify { user_id, room_id, data } => serde_json::json!({
            "type": "room_modify", "user_id": user_id, "room_id": room_id, "data": data,
        }),
        PluginEvent::GameStart { user_id, room_id } => serde_json::json!({
            "type": "game_start", "user_id": user_id, "room_id": room_id,
        }),
        PluginEvent::GameEnd { user_id, user_name, room_id, score, accuracy, perfect, good, bad, miss, max_combo, full_combo } => serde_json::json!({
            "type": "game_end", "user_id": user_id, "user_name": user_name, "room_id": room_id,
            "score": score, "accuracy": accuracy, "perfect": perfect, "good": good,
            "bad": bad, "miss": miss, "max_combo": max_combo, "full_combo": full_combo,
        }),
        PluginEvent::PlayerTouches { user_id, room_id, data } => serde_json::json!({
            "type": "player_touches", "user_id": user_id, "room_id": room_id, "data": data,
        }),
        PluginEvent::PlayerJudges { user_id, room_id, data } => serde_json::json!({
            "type": "player_judges", "user_id": user_id, "room_id": room_id, "data": data,
        }),
        PluginEvent::RoundComplete { room_id, chart_id, chart_name } => serde_json::json!({
            "type": "round_complete", "room_id": room_id, "chart_id": chart_id, "chart_name": chart_name,
        }),
    }
    .to_string()
}

pub fn encode_plugin_api_args_json(args: &[serde_json::Value]) -> Result<Vec<u8>, String> {
    serde_json::to_vec(args).map_err(|e| format!("encode plugin API args: {e}"))
}

pub fn decode_plugin_api_result_json(bytes: &[u8]) -> Result<serde_json::Value, String> {
    serde_json::from_slice(bytes).map_err(|e| format!("invalid plugin API JSON: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_encoder_preserves_stable_event_type() {
        let encoded = encode_plugin_event_json(&PluginEvent::RoomJoin {
            user_id: 42, room_id: "room-a".to_string(), is_monitor: false,
        });
        let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(value["type"], "room_join");
        assert_eq!(value["user_id"], 42);
    }

    #[test]
    fn api_args_and_result_json_round_trip() {
        let args = vec![serde_json::json!({"room_id":"room-a"}), serde_json::json!(7)];
        let bytes = encode_plugin_api_args_json(&args).unwrap();
        let decoded: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded, args);
        let result = decode_plugin_api_result_json(br#"{"ok":true}"#).unwrap();
        assert_eq!(result["ok"], true);
        assert!(decode_plugin_api_result_json(b"not json").is_err());
    }
}
