//! Typed result envelope for Runtime v2 room commands.
//!
//! Public callers still receive the legacy `Result<serde_json::Value, String>`
//! shape for now.  Internally, mailbox/actor plumbing should use this typed
//! envelope so delivery path, success/failure, and audit metadata do not have to
//! be inferred from ad-hoc JSON payloads.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    pub fn from_legacy(result: Result<Value, String>, delivery: RoomCommandDelivery) -> Self {
        match result {
            Ok(payload) => Self::Ok { delivery, payload },
            Err(error) => Self::Err { delivery, error },
        }
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

    pub fn into_legacy(self) -> Result<Value, String> {
        match self {
            Self::Ok { payload, .. } => Ok(payload),
            Self::Err { error, .. } => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_round_trips_to_legacy_payload() {
        let payload = serde_json::json!({"ok": true, "room_id": "abc"});
        let result = RoomCommandResult::from_legacy(
            Ok(payload.clone()),
            RoomCommandDelivery::PerRoomMailbox,
        );

        assert!(result.is_ok());
        assert_eq!(result.delivery(), RoomCommandDelivery::PerRoomMailbox);
        assert_eq!(result.payload(), Some(&payload));
        assert_eq!(result.into_legacy().unwrap(), payload);
    }

    #[test]
    fn failure_round_trips_to_legacy_error() {
        let result = RoomCommandResult::from_legacy(
            Err("room not found".to_string()),
            RoomCommandDelivery::FallbackInline,
        );

        assert!(!result.is_ok());
        assert_eq!(result.delivery(), RoomCommandDelivery::FallbackInline);
        assert_eq!(result.error_message().as_deref(), Some("room not found"));
        assert_eq!(result.into_legacy().unwrap_err(), "room not found");
    }
}
