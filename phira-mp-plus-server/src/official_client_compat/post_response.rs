//! ProtocolHack: centralized post-response compensation scheduling (PMP42 P1).
//!
//! The official Phira server's core command sequence never includes PMP
//! extension packets: `ChangeHost`/`ChangeState` corrections, Persistent Room
//! extra state, and replay simulation are all PMP-only additions. When PMP needs
//! such a correction, it must arrive **after** the official response has been
//! flushed, in a fixed order, and must never block the Room Actor.
//!
//! This module is the single place that schedules those compensation messages.
//! Each item records its reason for observability (`tracing::debug`). The delay
//! defaults to `minimum_response_latency_ms` (10ms) and can be set to 0 via
//! `compatibility.protocol_hack_delay_ms` for differential testing against the
//! official server.
//!
//! Invariants:
//! - Compensation is scheduled via `tokio::spawn` — the Room Actor never blocks.
//! - Fixed emission order: [`PostResponseKind`] ascending (ChangeHost first).
//! - The caller MUST invoke `schedule_post_response` only after the official
//!   response has been flushed to the socket.

use crate::server::config::PlusConfig;
use crate::session::User;
use phira_mp_common::ServerCommand;
use std::future::Future;
use std::pin::Pin;
use std::sync::Weak;
use tracing::debug;

/// Fixed emission order for PMP extension compensation messages.
///
/// Lower discriminants are emitted first. This order is part of the
/// compatibility contract — the official client expects host corrections before
/// state corrections, and state before replay simulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PostResponseKind {
    /// Host correction (`ChangeHost(true/false)`).
    ChangeHost,
    /// Room state correction (`ChangeState` / `GameStart` / `StartPlaying`).
    ChangeState,
    /// Persistent Room extra state beyond the official snapshot.
    PersistentRoom,
    /// Replay simulation messages.
    Replay,
}

/// A single post-response compensation message.
pub(crate) struct PostResponseItem {
    kind: PostResponseKind,
    /// Why this compensation is needed (observability/audit).
    reason: &'static str,
    send: Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>,
}

impl PostResponseItem {
    /// Schedule a compensation that delivers `command` to `user`'s session.
    pub(crate) fn to_user(
        user: Weak<User>,
        kind: PostResponseKind,
        command: ServerCommand,
        reason: &'static str,
    ) -> Self {
        Self {
            kind,
            reason,
            send: Box::new(move || {
                Box::pin(async move {
                    if let Some(user) = user.upgrade() {
                        user.try_send(command).await;
                    }
                })
            }),
        }
    }
}

/// The configured ProtocolHack delay in milliseconds.
fn protocol_hack_delay_ms(config: &PlusConfig) -> u64 {
    config
        .compatibility
        .protocol_hack_delay_ms
        .unwrap_or(config.compatibility.minimum_response_latency_ms)
}

/// Schedule post-response compensation messages.
///
/// The items are sorted into the fixed [`PostResponseKind`] order and delivered
/// after `protocol_hack_delay_ms` (default `minimum_response_latency_ms`). The
/// scheduling itself is fire-and-forget: a spawned task owns the sleep and the
/// delivery, so the caller — typically the Room Actor or the join path — never
/// waits on compensation work.
pub(crate) fn schedule_post_response(config: &PlusConfig, items: Vec<PostResponseItem>) {
    if items.is_empty() {
        return;
    }
    let delay_ms = protocol_hack_delay_ms(config);
    tokio::spawn(async move {
        if delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }
        // Fixed order: ChangeHost → ChangeState → PersistentRoom → Replay.
        // sort_by_key is stable, so items of the same kind keep insertion order.
        let mut items = items;
        items.sort_by_key(|it| it.kind);
        for item in items {
            debug!(
                kind = ?item.kind,
                reason = item.reason,
                "post-response compensation dispatch"
            );
            (item.send)().await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_hack_kind_order_is_fixed() {
        // ChangeHost must precede ChangeState must precede PersistentRoom/Replay.
        assert!(PostResponseKind::ChangeHost < PostResponseKind::ChangeState);
        assert!(PostResponseKind::ChangeState < PostResponseKind::PersistentRoom);
        assert!(PostResponseKind::PersistentRoom < PostResponseKind::Replay);
    }

    #[test]
    fn delay_falls_back_to_minimum_response_latency() {
        let config = PlusConfig {
            compatibility: crate::CompatibilityConfig {
                minimum_response_latency_ms: 10,
                protocol_hack_delay_ms: None,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(protocol_hack_delay_ms(&config), 10);
    }

    #[test]
    fn delay_can_be_zero_for_differential_tests() {
        let config = PlusConfig {
            compatibility: crate::CompatibilityConfig {
                protocol_hack_delay_ms: Some(0),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(protocol_hack_delay_ms(&config), 0);
    }

    #[test]
    fn delay_respects_explicit_override() {
        let config = PlusConfig {
            compatibility: crate::CompatibilityConfig {
                minimum_response_latency_ms: 10,
                protocol_hack_delay_ms: Some(25),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(protocol_hack_delay_ms(&config), 25);
    }

    #[tokio::test]
    async fn empty_items_is_a_no_op() {
        let config = PlusConfig::default();
        // Should not panic and not spawn a sleeping task.
        schedule_post_response(&config, Vec::new());
    }

    #[tokio::test]
    async fn items_are_delivered_in_fixed_kind_order_after_delay() {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::Arc;
        use std::time::Duration;

        let config = PlusConfig {
            compatibility: crate::CompatibilityConfig {
                protocol_hack_delay_ms: Some(1),
                ..Default::default()
            },
            ..Default::default()
        };

        let order = Arc::new(AtomicU64::new(0));
        let mk = |kind: PostResponseKind, expected: u64| PostResponseItem {
            kind,
            reason: "order test",
            send: {
                let order = Arc::clone(&order);
                Box::new(move || {
                    Box::pin(async move {
                        let prev = order.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(prev, expected, "compensation emitted out of order");
                    })
                })
            },
        };

        // Submit in reverse order; schedule_post_response must re-sort.
        let items = vec![
            mk(PostResponseKind::Replay, 3),
            mk(PostResponseKind::ChangeHost, 0),
            mk(PostResponseKind::PersistentRoom, 2),
            mk(PostResponseKind::ChangeState, 1),
        ];
        schedule_post_response(&config, items);

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(order.load(Ordering::SeqCst), 4, "all compensations must fire");
    }

    #[tokio::test]
    async fn zero_delay_dispatches_without_waiting() {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::Arc;

        let config = PlusConfig {
            compatibility: crate::CompatibilityConfig {
                protocol_hack_delay_ms: Some(0),
                ..Default::default()
            },
            ..Default::default()
        };
        let fired = Arc::new(AtomicU64::new(0));
        let item = PostResponseItem {
            kind: PostResponseKind::ChangeState,
            reason: "zero-delay test",
            send: {
                let fired = Arc::clone(&fired);
                Box::new(move || {
                    Box::pin(async move {
                        fired.fetch_add(1, Ordering::SeqCst);
                    })
                })
            },
        };
        schedule_post_response(&config, vec![item]);

        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(fired.load(Ordering::SeqCst), 1);
    }
}
