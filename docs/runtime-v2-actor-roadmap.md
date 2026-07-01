# Runtime v2 Actor Roadmap

This roadmap is the long-term target for preventing more code from piling into
`server.rs`, `session.rs`, `room.rs` and `cli.rs`.

The project is still allowed to ship useful patches before the actor migration is
complete.  The rule is not "stop feature work until actors exist".  The rule is
"new cross-cutting features should move toward actor boundaries instead of adding
more direct calls between large files".

## Explicit non-goal

Do **not** implement a privileged Web management API for Runtime v2.

Allowed Web/API work:

- read-only diagnostics
- public room/status APIs that already exist
- observability endpoints that do not mutate server state

Write-capable control surfaces should remain:

- CLI
- TUI
- in-game admin `_` command
- WIT/plugin APIs with explicit capabilities

## Migration rule

Every actor migration should follow this order:

1. Mirror existing events without changing behavior.
2. Add read-side diagnostics and tests.
3. Route read paths through the actor/facade.
4. Route one write path through the actor/facade.
5. Keep dual-write or compatibility shim while testing.
6. Remove the old direct call only after simulation suites and Actions pass.

## Target actor boundaries

### Server supervisor

Owns process lifecycle, listener startup, shutdown, task supervision and global
runtime handles.

Current source pressure:

- `server.rs`

Near-term cut:

- keep `PlusServerState` as compatibility facade
- move new runtime handles behind actor-owned services

### Session actor

Owns one client connection, authentication state, inbound command decoding and
outbound send queue.

Current source pressure:

- `session.rs`

Near-term cut:

- extract command side effects into message handlers
- ensure Touches/Judges are persisted independent of monitor presence
- avoid business logic waiting on Phira HTTP directly

### Room actor

Owns one room state machine: membership, host transfer, ready/start/play/result,
room metadata and telemetry fan-in.

Current source pressure:

- `room.rs`

Near-term cut:

- keep mirroring RoomEvent into EventBus
- add simulation regression coverage before moving write ownership
- move one low-risk field update behind a room command facade

### Persistence actor

Owns database batching, backpressure, retry, simulation isolation, flush on
shutdown and high-frequency Touch/Judge persistence.

Current source pressure:

- `db.rs`
- `round_store.rs`
- `persistence_worker.rs`

Near-term cut:

- migrate low-frequency production events from direct writes into worker
- then migrate production Touch/Judge batches using bounded batching
- keep `mp_sim_*` or simulation flags isolated from real data

### Simulation actor

Owns shadow users, shadow rooms, deterministic replay, suites and synthetic
workload generation.

Current source pressure:

- `simulation.rs`

Near-term cut:

- add suite report summaries
- add deterministic regression snapshots
- use simulation to test actor migration before real Room writes move

### Plugin actor

Owns plugin dispatch, capability checks, slow-plugin isolation and event fanout.

Current source pressure:

- `plugin.rs`
- `wasm_host.rs`
- `plugin_http.rs`

Near-term cut:

- route EventBus events to plugin dispatcher
- prevent slow plugin callbacks from blocking Room/Session hot paths

### CLI actor

Owns CLI/TUI/in-game admin command execution through Command Registry.

Current source pressure:

- `cli.rs`
- `cli_tui.rs`
- `command_registry.rs`

Near-term cut:

- move command handlers into registry-backed modules
- keep aliases stable
- keep help and completion generated from the same registry

## Near-term priority after Step 11

1. Suite report / summary output.
2. PersistenceWorker production low-frequency dual-write path.
3. Touch/Judge production batching policy in the worker.
4. Room command facade for one low-risk metadata update.
5. Session command handler extraction.
