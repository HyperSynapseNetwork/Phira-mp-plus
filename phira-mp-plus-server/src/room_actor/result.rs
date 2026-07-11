//! Typed result envelope for Runtime v2 room commands.
//!
//! Public callers still receive the untyped `Result<serde_json::Value, String>`
//! shape for now.  Internally, mailbox/actor plumbing should use this typed
//! envelope so delivery path, success/failure, and audit metadata do not have to
//! be inferred from ad-hoc JSON payloads.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomCommandDelivery {
    Inline,
    PerRoomMailbox,
    FallbackInline,
    MailboxError,
}

impl RoomCommandDelivery {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::PerRoomMailbox => "per_room_mailbox",
            Self::FallbackInline => "fallback_inline",
            Self::MailboxError => "mailbox_error",
        }
    }
}

/// Typed payload for room command results.
///
/// New code should prefer these variants over ad-hoc JSON construction.
/// The `into_json()` method converts to a Value for
/// callers that still expect the untyped JSON bridge.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomCommandPayload {
    Empty,
    LockChanged {
        room_id: String,
        locked: bool,
    },
    CycleChanged {
        room_id: String,
        cycle: bool,
    },
    HostChanged {
        room_id: String,
        host: Option<i32>,
        host_name: String,
        host_is_system: bool,
    },
    HiddenChanged {
        room_id: String,
        hidden: bool,
    },
    EndpointChanged {
        room_id: String,
        endpoint: String,
        endpoint_override: Option<String>,
        using_room_override: bool,
    },
    UserKicked {
        room_id: String,
        user_id: i32,
        user_name: String,
        room_dropped: bool,
    },
    RoomClosed {
        room_id: String,
    },
    RoomStarted {
        room_id: String,
    },
    CancelResult {
        room_id: String,
        canceled: bool,
    },
}

impl RoomCommandPayload {
    pub fn into_json(self) -> Value {
        match self {
            Self::Empty => json!({"ok": true}),
            Self::LockChanged { room_id, locked } => json!({
                "ok": true, "room_id": room_id, "locked": locked,
            }),
            Self::CycleChanged { room_id, cycle } => json!({
                "ok": true, "room_id": room_id, "cycle": cycle,
            }),
            Self::HostChanged {
                room_id,
                host,
                host_name,
                host_is_system,
            } => json!({
                "ok": true, "room_id": room_id,
                "host": host, "host_name": host_name,
                "host_is_system": host_is_system,
            }),
            Self::HiddenChanged { room_id, hidden } => json!({
                "ok": true, "room_id": room_id, "hidden": hidden,
            }),
            Self::EndpointChanged {
                room_id,
                endpoint,
                endpoint_override,
                using_room_override,
            } => json!({
                "ok": true,
                "room_id": room_id,
                "phira_api_endpoint": endpoint,
                "phira_api_endpoint_override": endpoint_override,
                "using_room_override": using_room_override,
            }),
            Self::UserKicked {
                room_id,
                user_id,
                user_name,
                room_dropped,
            } => json!({
                "ok": true, "room_id": room_id, "user_id": user_id,
                "user_name": user_name, "room_dropped": room_dropped,
            }),
            Self::RoomClosed { room_id } => json!({
                "ok": true, "room_id": room_id,
            }),
            Self::RoomStarted { room_id } => json!({
                "ok": true, "room_id": room_id,
            }),
            Self::CancelResult { room_id, canceled } => json!({
                "ok": true, "room_id": room_id, "canceled": canceled,
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RoomCommandResult {
    Ok {
        delivery: RoomCommandDelivery,
        payload: Value,
    },
    Err {
        delivery: RoomCommandDelivery,
        error: String,
    },
}

impl RoomCommandResult {
    pub fn from_untyped(result: Result<Value, String>, delivery: RoomCommandDelivery) -> Self {
        match result {
            Ok(payload) => Self::Ok { delivery, payload },
            Err(error) => Self::Err { delivery, error },
        }
    }

    /// Construct from a typed payload, converting to the JSON bridge shape.
    pub fn from_payload(payload: RoomCommandPayload, delivery: RoomCommandDelivery) -> Self {
        Self::Ok {
            delivery,
            payload: payload.into_json(),
        }
    }

    /// Construct from a typed payload and wrap in Ok.
    pub fn ok(payload: RoomCommandPayload, delivery: RoomCommandDelivery) -> Self {
        Self::from_payload(payload, delivery)
    }

    pub fn mailbox_error(error: impl Into<String>) -> Self {
        Self::Err {
            delivery: RoomCommandDelivery::MailboxError,
            error: error.into(),
        }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }

    pub fn delivery(&self) -> RoomCommandDelivery {
        match self {
            Self::Ok { delivery, .. } | Self::Err { delivery, .. } => *delivery,
        }
    }

    pub fn payload(&self) -> Option<&Value> {
        match self {
            Self::Ok { payload, .. } => Some(payload),
            Self::Err { .. } => None,
        }
    }

    pub fn error_message(&self) -> Option<String> {
        match self {
            Self::Ok { .. } => None,
            Self::Err { error, .. } => Some(error.clone()),
        }
    }

    pub fn into_untyped(self) -> Result<Value, String> {
        match self {
            Self::Ok { payload, .. } => Ok(payload),
            Self::Err { error, .. } => Err(error),
        }
    }

    /// Extract the JSON payload, if present.
    pub fn into_payload(self) -> Option<Value> {
        match self {
            Self::Ok { payload, .. } => Some(payload),
            Self::Err { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_round_trips_to_untyped_payload() {
        let payload = serde_json::json!({"ok": true, "room_id": "abc"});
        let result = RoomCommandResult::from_untyped(
            Ok(payload.clone()),
            RoomCommandDelivery::PerRoomMailbox,
        );

        assert!(result.is_ok());
        assert_eq!(result.delivery(), RoomCommandDelivery::PerRoomMailbox);
        assert_eq!(result.payload(), Some(&payload));
        assert_eq!(result.into_untyped().unwrap(), payload);
    }

    #[test]
    fn failure_round_trips_to_untyped_error() {
        let result = RoomCommandResult::from_untyped(
            Err("room not found".to_string()),
            RoomCommandDelivery::FallbackInline,
        );

        assert!(!result.is_ok());
        assert_eq!(result.delivery(), RoomCommandDelivery::FallbackInline);
        assert_eq!(result.error_message().as_deref(), Some("room not found"));
        assert_eq!(result.into_untyped().unwrap_err(), "room not found");
    }
    #[test]
    fn delivery_names_are_stable_contract() {
        assert_eq!(RoomCommandDelivery::Inline.as_str(), "inline");
        assert_eq!(
            RoomCommandDelivery::PerRoomMailbox.as_str(),
            "per_room_mailbox"
        );
        assert_eq!(
            RoomCommandDelivery::FallbackInline.as_str(),
            "fallback_inline"
        );
        assert_eq!(RoomCommandDelivery::MailboxError.as_str(), "mailbox_error");
    }

    #[test]
    fn mailbox_error_keeps_typed_delivery_and_untyped_error() {
        let result = RoomCommandResult::mailbox_error("reply lost");

        assert!(!result.is_ok());
        assert_eq!(result.delivery(), RoomCommandDelivery::MailboxError);
        assert_eq!(result.error_message().as_deref(), Some("reply lost"));
        assert_eq!(result.into_untyped().unwrap_err(), "reply lost");
    }

    #[test]
    fn typed_payload_empty_converts_to_json() {
        let payload = RoomCommandPayload::Empty;
        let json = payload.into_json();
        assert_eq!(json, serde_json::json!({"ok": true}));
    }

    #[test]
    fn typed_payload_lock_changed_converts_to_json() {
        let payload = RoomCommandPayload::LockChanged {
            room_id: "room-a".into(),
            locked: true,
        };
        let json = payload.into_json();
        assert_eq!(json["ok"], true);
        assert_eq!(json["room_id"], "room-a");
        assert_eq!(json["locked"], true);
    }

    #[test]
    fn typed_payload_host_changed_converts_to_json() {
        let payload = RoomCommandPayload::HostChanged {
            room_id: "room-b".into(),
            host: Some(42),
            host_name: "player1".into(),
            host_is_system: false,
        };
        let json = payload.into_json();
        assert_eq!(json["ok"], true);
        assert_eq!(json["room_id"], "room-b");
        assert_eq!(json["host"], 42);
    }

    #[test]
    fn typed_payload_user_kicked_includes_room_dropped() {
        let payload = RoomCommandPayload::UserKicked {
            room_id: "room-c".into(),
            user_id: 7,
            user_name: "tester".into(),
            room_dropped: true,
        };
        let json = payload.into_json();
        assert_eq!(json["room_dropped"], true);
        assert_eq!(json["user_name"], "tester");
    }

    #[test]
    fn from_payload_wraps_in_ok() {
        let result = RoomCommandResult::from_payload(
            RoomCommandPayload::Empty,
            RoomCommandDelivery::PerRoomMailbox,
        );
        assert!(result.is_ok());
        assert_eq!(result.delivery(), RoomCommandDelivery::PerRoomMailbox);
        let json = result.into_untyped().unwrap();
        assert_eq!(json["ok"], true);
    }

    #[test]
    fn ok_convenience_creates_typed_result() {
        let result = RoomCommandResult::ok(
            RoomCommandPayload::RoomClosed {
                room_id: "room-x".into(),
            },
            RoomCommandDelivery::Inline,
        );
        assert!(result.is_ok());
        let payload = result.into_payload().unwrap();
        assert_eq!(payload["room_id"], "room-x");
    }
}
