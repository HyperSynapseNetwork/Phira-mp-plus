# PMP Guarantees

> Last updated: 2026-07-22
> Applies to: Phira-mp+ v0.4.x (pre-production hardening candidate)

This document lists the guarantees PMP provides, the preconditions required,
the failure semantics, and the test evidence supporting each claim.

---

## 1. Event Persistence

| Guarantee | Precondition | Failure Semantics | Evidence |
|-----------|-------------|-------------------|----------|
| Admitted events survive process crash | WAL fsync succeeds | Event rejected before admission; no data loss | WAL replay + ACK compaction tests |
| Acknowledged events are durable | DB commit OR dead-letter write succeeds | Retry until durable; admission retained on dual failure | `worker.rs` state machine |
| Telemetry batcher events survive crash | Batch not yet committed | WAL replay restores uncommitted events | Phase 3 WAL integration test |

## 2. Room State

| Guarantee | Precondition | Failure Semantics | Evidence |
|-----------|-------------|-------------------|----------|
| Room commands are serialized | All commands go through mailbox | Mailbox reject preserves queue order | Room actor tests |
| No duplicate room generations | UUID + generation fencing | Late commands rejected | Room mailbox fencing tests |

## 3. Plugin Isolation

| Guarantee | Precondition | Failure Semantics | Evidence |
|-----------|-------------|-------------------|----------|
| Fuel-limited execution | Wasmtime fuel enabled | Trap on exhaustion | Plugin fuel tests |
| Memory-bounded allocation | Store limiter set | Allocation failure | Wasm runtime integration tests |

## 4. Shutdown

| Guarantee | Precondition | Failure Semantics | Evidence |
|-----------|-------------|-------------------|----------|
| Events accepted before shutdown are flushed | Flush timeout respected | Excess events returned to caller | Shutdown sequence tests |
| WAL compaction on shutdown | Compaction succeeds | WAL grows on next start | `shutdown()` + compact tests |

## 5. Known Non-Guarantees

- Cluster-level HA (single-process only)
- Cross-process plugin isolation (in-process Wasmtime only)
- Multi-region replication
