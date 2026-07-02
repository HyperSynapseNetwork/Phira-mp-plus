//! Plugin ABI boundary.
//!
//! The current WASM guest ABI still moves event/API payloads through JSON bytes
//! in guest memory. Runtime v2 centralises that JSON bridge in one small module
//! so the rest of the host no longer treats ad-hoc JSON strings as the plugin ABI.
//! The target state is a typed WIT/component-model ABI; until then this module
//! is the only place where the JSON transport should be encoded/decoded.

use phira_mp_plus_server_api::PluginEvent;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PluginAbiTransport {
    JsonMemoryV1,
    WitTypedV2,
}

impl PluginAbiTransport {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::JsonMemoryV1 => "json_memory_v1",
            Self::WitTypedV2 => "wit_typed_v2",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginAbiPlan {
    pub current_transport: PluginAbiTransport,
    pub target_transport: PluginAbiTransport,
    pub current_version: &'static str,
    pub target_version: &'static str,
    pub risks: Vec<&'static str>,
    pub next_steps: Vec<&'static str>,
}

pub fn plugin_abi_plan() -> PluginAbiPlan {
    PluginAbiPlan {
        current_transport: PluginAbiTransport::JsonMemoryV1,
        target_transport: PluginAbiTransport::WitTypedV2,
        current_version: "abi-json-v1",
        target_version: "abi-wit-v2",
        risks: vec![
            "JSON string transport hides schema drift until runtime",
            "host and guest can disagree on field names without compiler errors",
            "large Touch/Judge payloads pay repeated JSON encode/decode cost",
            "WIT definitions exist in the project direction but are not yet the authoritative plugin ABI",
        ],
        next_steps: vec![
            "centralize every JSON ABI encode/decode call in plugin_abi.rs",
            "add contract tests for every host event and host API method exposed to plugins",
            "write abi-wit-v2 WIT definitions before changing guest-facing behavior",
            "switch WASM host exports from JSON-memory bridge to typed WIT/component bindings after tests cover v1 parity",
        ],
    }
}

/// WIT ABI v2 metadata — references the WIT file at `wit/phira-plugin.wit`.
pub mod wit {
    /// Path to the WIT definition file (relative to workspace root).
    pub const WIT_FILE: &str = "wit/phira-plugin.wit";

    /// The WIT world name defined in the WIT file.
    pub const WIT_WORLD: &str = "phira-plugin-v2";

    /// ABI version string for identification.
    pub const WIT_VERSION: &str = "abi-wit-v2";

    /// Current migration phase.
    /// - 0: WIT files defined, JSON bridge still active
    /// - 1: Host WIT bindings generated, dual-ABI support
    /// - 2: Guest SDK updated, WIT-only mode
    pub const MIGRATION_PHASE: u8 = 0;
}

/// Encode a host event for the current JSON-memory WASM plugin ABI.
///
/// This is intentionally kept as a typed match instead of free-form `json!`
/// calls spread across the host. When a PluginEvent variant changes, this
/// function becomes the ABI review point.
pub fn encode_plugin_event_json(event: &PluginEvent) -> String {
    match event {
        PluginEvent::UserConnect {
            user_id,
            user_name,
            user_ip,
        } => serde_json::json!({
            "type": "user_connect",
            "user_id": user_id,
            "user_name": user_name,
            "user_ip": user_ip,
        }),
        PluginEvent::UserDisconnect { user_id, user_name } => serde_json::json!({
            "type": "user_disconnect",
            "user_id": user_id,
            "user_name": user_name,
        }),
        PluginEvent::RoomCreate { user_id, room_id } => serde_json::json!({
            "type": "room_create",
            "user_id": user_id,
            "room_id": room_id,
        }),
        PluginEvent::RoomJoin {
            user_id,
            room_id,
            is_monitor,
        } => serde_json::json!({
            "type": "room_join",
            "user_id": user_id,
            "room_id": room_id,
            "is_monitor": is_monitor,
        }),
        PluginEvent::RoomLeave { user_id, room_id } => serde_json::json!({
            "type": "room_leave",
            "user_id": user_id,
            "room_id": room_id,
        }),
        PluginEvent::RoomModify {
            user_id,
            room_id,
            data,
        } => serde_json::json!({
            "type": "room_modify",
            "user_id": user_id,
            "room_id": room_id,
            "data": data,
        }),
        PluginEvent::GameStart { user_id, room_id } => serde_json::json!({
            "type": "game_start",
            "user_id": user_id,
            "room_id": room_id,
        }),
        PluginEvent::GameEnd {
            user_id,
            user_name,
            room_id,
            score,
            accuracy,
            perfect,
            good,
            bad,
            miss,
            max_combo,
            full_combo,
        } => serde_json::json!({
            "type": "game_end",
            "user_id": user_id,
            "user_name": user_name,
            "room_id": room_id,
            "score": score,
            "accuracy": accuracy,
            "perfect": perfect,
            "good": good,
            "bad": bad,
            "miss": miss,
            "max_combo": max_combo,
            "full_combo": full_combo,
        }),
        PluginEvent::PlayerTouches {
            user_id,
            room_id,
            data,
        } => serde_json::json!({
            "type": "player_touches",
            "user_id": user_id,
            "room_id": room_id,
            "data": data,
        }),
        PluginEvent::PlayerJudges {
            user_id,
            room_id,
            data,
        } => serde_json::json!({
            "type": "player_judges",
            "user_id": user_id,
            "room_id": room_id,
            "data": data,
        }),
        PluginEvent::RoundComplete {
            room_id,
            chart_id,
            chart_name,
        } => serde_json::json!({
            "type": "round_complete",
            "room_id": room_id,
            "chart_id": chart_id,
            "chart_name": chart_name,
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
    fn abi_plan_tracks_json_bridge_as_current_problem() {
        let plan = plugin_abi_plan();
        assert_eq!(plan.current_transport, PluginAbiTransport::JsonMemoryV1);
        assert_eq!(plan.target_transport, PluginAbiTransport::WitTypedV2);
        assert!(plan.risks.iter().any(|risk| risk.contains("schema drift")));
        assert!(plan
            .next_steps
            .iter()
            .any(|step| step.contains("contract tests")));
    }

    #[test]
    fn event_encoder_preserves_stable_event_type() {
        let encoded = encode_plugin_event_json(&PluginEvent::RoomJoin {
            user_id: 42,
            room_id: "room-a".to_string(),
            is_monitor: false,
        });
        let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(value["type"], "room_join");
        assert_eq!(value["user_id"], 42);
        assert_eq!(value["room_id"], "room-a");
        assert_eq!(value["is_monitor"], false);
    }

    #[test]
    fn api_args_and_result_json_round_trip() {
        let args = vec![
            serde_json::json!({"room_id":"room-a"}),
            serde_json::json!(7),
        ];
        let bytes = encode_plugin_api_args_json(&args).unwrap();
        let decoded: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded, args);

        let result = decode_plugin_api_result_json(br#"{"ok":true}"#).unwrap();
        assert_eq!(result["ok"], true);
        assert!(decode_plugin_api_result_json(b"not json").is_err());
    }
}
