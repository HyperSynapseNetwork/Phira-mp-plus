//! Typed result envelope for Runtime room commands.
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
    HostStarted {
        room_id: String,
    },
    CancelResult {
        room_id: String,
        canceled: bool,
    },
    ChartSelected {
        room_id: String,
        chart_id: i32,
    },
    ChartDurationSet,
    UserReady {
        room_id: String,
        user_id: i32,
    },
    UserNotReady {
        room_id: String,
        user_id: i32,
    },
    RoundResultSubmitted {
        room_id: String,
        user_id: i32,
        score: i32,
    },
    RoundAborted {
        room_id: String,
        user_id: i32,
    },
    UserAdded {
        room_id: String,
        user_id: i32,
        monitor: bool,
        room_full: bool,
    },
    UserRemoved {
        room_id: String,
        user_id: i32,
        room_dropped: bool,
    },
    LiveChanged {
        room_id: String,
        live: bool,
    },
    TouchesCached {
        room_id: String,
        user_id: i32,
    },
    JudgesCached {
        room_id: String,
        user_id: i32,
    },
    DisplayNameSet {
        room_id: String,
        user_id: i32,
        name: String,
    },
    PersistentEmptyChanged {
        room_id: String,
        persistent_empty: bool,
    },
    /// PMP45 P0-F: 原子认证快照（`BindAndSnapshot` 命令的 payload）。
    BindAndSnapshot(BindAndSnapshotData),
}

/// PMP45 P0-F: `BindAndSnapshot` 原子快照的可序列化负载。与
/// `phira_mp_common::ClientRoomState` 同构（`ClientRoomState` 本身不实现
/// serde，因此以本结构体经 room mailbox 的 JSON 桥回传，认证路径再重建为
/// `ClientRoomState`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BindAndSnapshotData {
    pub room_id: String,
    pub state: phira_mp_common::StrippedRoomState,
    pub chart: Option<i32>,
    pub live: bool,
    pub locked: bool,
    pub cycle: bool,
    pub is_host: bool,
    pub is_ready: bool,
    pub users: Vec<BindAndSnapshotUser>,
    /// Room Actor 构建快照时的网关 command_seq（actor 排序点）。供认证路径
    /// 观测与 cutover 对齐参考。
    pub token: u64,
    /// PMP46 Blocker 2: 快照时刻的 Room Actor 权威状态事件序号。认证路径以它
    /// 调用 `gate.begin_room_cutover(snapshot_seq)`，激活时只剔除
    /// `room_seq <= snapshot_seq` 的缓冲事件——Room Actor 序号与 Gate 自身
    /// 序号是两个无关数字，绝不能用 Gate 序号对齐快照（audit §7.5）。
    pub snapshot_seq: u64,
}

/// `BindAndSnapshotData.users` 的单个成员。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BindAndSnapshotUser {
    pub id: i32,
    pub name: String,
    pub monitor: bool,
}

impl BindAndSnapshotData {
    /// 重建 `phira_mp_common::ClientRoomState`（认证响应的快照负载）。
    pub fn into_client_room_state(self) -> phira_mp_common::ClientRoomState {
        let state = match self.state {
            phira_mp_common::StrippedRoomState::SelectingChart => {
                phira_mp_common::RoomState::SelectChart(self.chart)
            }
            phira_mp_common::StrippedRoomState::WaitingForReady => {
                phira_mp_common::RoomState::WaitingForReady
            }
            phira_mp_common::StrippedRoomState::Playing => phira_mp_common::RoomState::Playing,
        };
        // room_id 来自 Room Actor 权威房间，必定合法；`_` 仅作防御性兜底。
        let id = self.room_id.clone().try_into().unwrap_or_else(|_| {
            phira_mp_common::RoomId::try_from("_".to_string())
                .expect("`_` is always a valid room id")
        });
        phira_mp_common::ClientRoomState {
            id,
            state,
            live: self.live,
            locked: self.locked,
            cycle: self.cycle,
            is_host: self.is_host,
            is_ready: self.is_ready,
            users: self
                .users
                .into_iter()
                .map(|u| {
                    (
                        u.id,
                        phira_mp_common::UserInfo {
                            id: u.id,
                            name: u.name,
                            monitor: u.monitor,
                        },
                    )
                })
                .collect(),
        }
    }
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
            Self::HostStarted { room_id } => json!({
                "ok": true, "room_id": room_id,
            }),
            Self::CancelResult { room_id, canceled } => json!({
                "ok": true, "room_id": room_id, "canceled": canceled,
            }),
            Self::ChartSelected { room_id, chart_id } => json!({
                "ok": true, "room_id": room_id, "chart_id": chart_id,
            }),
            Self::ChartDurationSet => json!({ "ok": true }),
            Self::UserReady { room_id, user_id } => json!({
                "ok": true, "room_id": room_id, "user_id": user_id,
            }),
            Self::UserNotReady { room_id, user_id } => json!({
                "ok": true, "room_id": room_id, "user_id": user_id,
            }),
            Self::RoundResultSubmitted { room_id, user_id, score } => json!({
                "ok": true, "room_id": room_id, "user_id": user_id, "score": score,
            }),
            Self::RoundAborted { room_id, user_id } => json!({
                "ok": true, "room_id": room_id, "user_id": user_id,
            }),
            Self::UserAdded { room_id, user_id, monitor, room_full } => json!({
                "ok": true, "room_id": room_id, "user_id": user_id,
                "monitor": monitor, "room_full": room_full,
            }),
            Self::UserRemoved { room_id, user_id, room_dropped } => json!({
                "ok": true, "room_id": room_id, "user_id": user_id,
                "room_dropped": room_dropped,
            }),
            Self::LiveChanged { room_id, live } => json!({
                "ok": true, "room_id": room_id, "live": live,
            }),
            Self::TouchesCached { room_id, user_id } => json!({
                "ok": true, "room_id": room_id, "user_id": user_id,
            }),
            Self::JudgesCached { room_id, user_id } => json!({
                "ok": true, "room_id": room_id, "user_id": user_id,
            }),
            Self::DisplayNameSet { room_id, user_id, name } => json!({
                "ok": true, "room_id": room_id, "user_id": user_id, "name": name,
            }),
            Self::PersistentEmptyChanged { room_id, persistent_empty } => json!({
                "ok": true, "room_id": room_id, "persistent_empty": persistent_empty,
            }),
            Self::BindAndSnapshot(data) => json!({
                "ok": true, "snapshot": data,
            }),
        }
    }
}

/// PMP45 P0-J: 房间命令结果的终态分类（terminal classification，audit §16）。
///
/// 权威提交是不可逆的——即使后续响应 flush 失败，已提交状态也成立。调用方
/// 必须依据该分类决定：
/// - `Committed`：进入不确定/断连终端（关闭 origin 传输，客户端 reconnect 恢复
///   权威状态），绝不能假装命令从未发生（否则客户端重试会导致状态重复提交）。
/// - `Rejected`/`Stale`：未提交任何权威状态，可安全发送普通错误响应。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomCommandTerminal {
    /// 结果为 `Ok` —— 权威状态已提交（即使响应失败状态也成立）。
    Committed,
    /// 结果为 `Err`（deadline 拒绝 / 校验失败）—— 未提交任何权威状态。
    Rejected,
    /// `origin_stale` 拒绝（`stale session origin`）—— 未提交任何权威状态。
    Stale,
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

    /// PMP45 P0-J: 把结果分类为终态。`Ok` → `Committed`；`Err` 的
    /// `stale session origin`（`refuse_stale_origin` 返回的错误串）→ `Stale`；
    /// 其余 `Err`（deadline 拒绝 `command deadline elapsed` / 校验失败 /
    /// mailbox 错误）→ `Rejected`。错误串是本 crate 内部稳定的契约
    /// （`room_actor/handler.rs` 的 `refuse_stale_origin`），据此区分 stale
    /// 与普通拒绝——两者都未提交状态，但来源不同。
    pub fn terminal(&self) -> RoomCommandTerminal {
        match self {
            Self::Ok { .. } => RoomCommandTerminal::Committed,
            Self::Err { error, .. } => {
                if error.contains("stale session origin") {
                    RoomCommandTerminal::Stale
                } else {
                    RoomCommandTerminal::Rejected
                }
            }
        }
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
    fn terminal_classification_pins_p0j_semantics() {
        // PMP45 P0-J: Ok → Committed（权威状态已提交，即使响应失败状态也成立）。
        let committed = RoomCommandResult::from_untyped(
            Ok(serde_json::json!({"ok": true})),
            RoomCommandDelivery::PerRoomMailbox,
        );
        assert_eq!(committed.terminal(), RoomCommandTerminal::Committed);
        // stale session origin（refuse_stale_origin 的错误串）→ Stale，未提交。
        let stale = RoomCommandResult::from_untyped(
            Err("stale session origin; command refused".to_string()),
            RoomCommandDelivery::PerRoomMailbox,
        );
        assert_eq!(stale.terminal(), RoomCommandTerminal::Stale);
        // 其余 Err（deadline 拒绝 / 校验失败 / mailbox 错误）→ Rejected，未提交。
        let rejected = RoomCommandResult::from_untyped(
            Err("command deadline elapsed".to_string()),
            RoomCommandDelivery::PerRoomMailbox,
        );
        assert_eq!(rejected.terminal(), RoomCommandTerminal::Rejected);
        let mailbox_err = RoomCommandResult::mailbox_error("room mailbox unavailable");
        assert_eq!(mailbox_err.terminal(), RoomCommandTerminal::Rejected);
    }

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
            RoomCommandDelivery::MailboxError,
        );

        assert!(!result.is_ok());
        // FallbackInline removed in PMP25
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
