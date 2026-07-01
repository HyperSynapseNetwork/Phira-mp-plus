//! Room command audit trail, metrics, and diagnostic snapshots.

use super::{
    RoomCommandAuditEntry, RoomCommandGateway, RoomCommandGatewayStats, RoomCommandResult,
    MAX_ROOM_COMMAND_AUDIT,
};
use crate::{event_bus::MpEvent, server::PlusServerState};
use std::{sync::atomic::Ordering, time::Instant};

impl RoomCommandGateway {
    pub fn stats(&self) -> RoomCommandGatewayStats {
        let mailbox_enabled = self.mailbox_enabled();
        let recent_commands = self
            .recent_commands
            .read()
            .map(|items| items.iter().rev().take(12).cloned().collect())
            .unwrap_or_default();
        RoomCommandGatewayStats {
            routed: self.routed.load(Ordering::Relaxed),
            succeeded: self.succeeded.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            audited: self.audited.load(Ordering::Relaxed),
            latency_total_us: self.latency_total_us.load(Ordering::Relaxed),
            latency_max_us: self.latency_max_us.load(Ordering::Relaxed),
            mailbox_enabled,
            mailbox_enqueued: self.mailbox_enqueued.load(Ordering::Relaxed),
            mailbox_completed: self.mailbox_completed.load(Ordering::Relaxed),
            mailbox_failed: self.mailbox_failed.load(Ordering::Relaxed),
            mailbox_fallback: self.mailbox_fallback.load(Ordering::Relaxed),
            mailbox_closed: self.mailbox_closed.load(Ordering::Relaxed),
            room_mailboxes: self.room_mailboxes.read().map(|m| m.len()).unwrap_or(0),
            mailbox_created: self.mailbox_created.load(Ordering::Relaxed),
            mailbox_registry_hit: self.mailbox_registry_hit.load(Ordering::Relaxed),
            mailbox_registry_miss: self.mailbox_registry_miss.load(Ordering::Relaxed),
            recent_commands,
            phase: if mailbox_enabled { "per_room_mailbox_partial" } else { "inline_facade" }.to_string(),
            note: "set_lock/set_cycle/set_host/close/kick/start/cancel now cross a per-room mailbox registry with typed RoomCommandResult audit metadata".to_string(),
        }
    }

    pub(super) fn finish_command(
        &self,
        state: &PlusServerState,
        action: &str,
        room_id: &str,
        started: Instant,
        result: RoomCommandResult,
    ) -> RoomCommandResult {
        self.routed.fetch_add(1, Ordering::Relaxed);
        let ok = result.is_ok();
        if ok {
            self.succeeded.fetch_add(1, Ordering::Relaxed);
        } else {
            self.failed.fetch_add(1, Ordering::Relaxed);
        }

        let latency_us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        self.audited.fetch_add(1, Ordering::Relaxed);
        self.latency_total_us.fetch_add(latency_us, Ordering::Relaxed);
        self.observe_max_latency(latency_us);

        let command_id = self.command_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let error = result.error_message();
        let audit = RoomCommandAuditEntry {
            command_id,
            room_id: room_id.to_string(),
            action: action.to_string(),
            ok,
            latency_us,
            error,
            delivery: result.delivery().as_str().to_string(),
        };
        self.push_audit(audit.clone());
        state.event_bus.publish(MpEvent::Custom {
            kind: "room.command".to_string(),
            payload: serde_json::json!({
                "command_id": audit.command_id,
                "room_id": audit.room_id,
                "action": audit.action,
                "ok": audit.ok,
                "latency_us": audit.latency_us,
                "error": audit.error,
                "delivery": audit.delivery,
            }),
        });
        result
    }

    fn observe_max_latency(&self, latency_us: u64) {
        let mut current = self.latency_max_us.load(Ordering::Relaxed);
        while latency_us > current {
            match self.latency_max_us.compare_exchange_weak(
                current,
                latency_us,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(next) => current = next,
            }
        }
    }

    fn push_audit(&self, audit: RoomCommandAuditEntry) {
        if let Ok(mut recent) = self.recent_commands.write() {
            if recent.len() >= MAX_ROOM_COMMAND_AUDIT {
                recent.pop_front();
            }
            recent.push_back(audit);
        }
}
}
