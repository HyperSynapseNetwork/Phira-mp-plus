# PMP Phase 3 WAL implementation report

## Implemented

- New `persistence/wal.rs` journal with versioned JSONL admission and ACK records.
- `PersistenceEvent` is now losslessly serializable, including `Arc<Value>` payloads.
- Queue admission is conditional on a durable WAL append (`flush + sync_data`).
- Startup replay runs before newly queued work is consumed.
- Processing writes ACK records; flush and shutdown compact the journal.
- Configuration adds a mandatory non-empty `runtime_v2.persistence_wal_path`.
- Corrupt WAL records are rejected and reported to Supervisor as critical failures.
- Unit coverage verifies replay, ACK filtering and compaction.

## Guarantee boundary

This implementation materially improves crash recovery for ordinary PersistenceWorker events. It does not yet provide a complete telemetry commit protocol: Touch/Judge are staged into TelemetryBatcher and the worker currently ACKs after staging, not after PostgreSQL transaction commit. The next step is to return per-batch commit/dead-letter acknowledgements from TelemetryBatcher and bind WAL ACK to those results.

## Validation limitation

The current execution environment does not contain Rust/Cargo. Structural checks and configuration/document consistency checks were performed, but `cargo check`, `cargo test`, Clippy and rustfmt must run in CI before release.
