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
- route admin/StateQuery room writes through `RoomCommandGateway` first
- replace the inline gateway implementation with per-room mailbox actors after counters and suite reports are stable

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

## Near-term priority after Step 13

1. PersistenceWorker production low-frequency dual-write path.
2. Touch/Judge production batching policy in the worker.
3. Replace `RoomCommandGateway` inline methods with a mailbox-backed per-room actor for one low-risk command.
4. Session command handler extraction.
5. Plugin dispatch isolation so slow plugins cannot block Room/Session hot paths.

## Step 14 update: first mailbox-backed room command path

Step 14 starts turning `RoomCommandGateway` from an inline facade into an
actor-shaped command boundary.

Implemented now:

- `room set <id> lock <bool>` / `room.set_lock` still use the same public gateway
  method, but the actual write crosses a bounded mailbox first.
- `room set <id> cycle <bool>` uses the same mailbox path.
- The mailbox worker falls back to the inline path if the queue is unavailable,
  so this migration step does not brick admin commands during early testing.
- `runtime rooms` reports mailbox counters: enabled, enqueued, completed,
  failed, fallback and closed.

Still intentionally not done:

- no per-room mailbox ownership yet;
- no full `Room` state-machine rewrite;
- no Web management API;
- no migration of start/cancel/host/kick/close into mailbox yet.

Next cut:

1. Move one more command family, probably host transfer or close, through the
   mailbox after `set_lock/set_cycle` pass Actions and manual room tests.
2. Split the gateway worker into per-room mailboxes.
3. Only after that, move selected state-machine transitions out of `room.rs`.

## Step 15 update: per-room mailbox registry

Step 15 moves the gateway from a single mailbox-backed command path toward real
per-room actor ownership.  The gateway still delegates to the current `Room`
implementation internally, but selected writes are serialized by `room_id` before
running.

Commands now routed through the per-room mailbox registry:

- `room set <id> lock <bool>`
- `room set <id> cycle <bool>`
- `room host <id> <user|?>` / host transfer
- `room close <id>`

Still inline inside the gateway:

- `kick`
- `start`
- `cancel`

Operational counters added or expanded:

- active per-room mailbox count
- mailbox created count
- registry hit/miss count
- enqueue/complete/fallback/closed counters

Next cut:

1. Move `kick` into the per-room mailbox path after close/host tests pass.
2. Move `start` and `cancel` only after auditing `WaitForReady` state locking and
   send-before/after-lock behavior.
3. Add command latency and command-id tracing to the gateway before moving more
   state-machine transitions.
4. Start extracting actual room-owned state into actor-local ownership only after
   the mailbox path has stable counters and test coverage.

## Step 16 update: kick mailbox path and command audit

Step 16 adds the next real command family to the per-room mailbox registry and
adds command-level observability.

Commands now routed through the per-room mailbox registry:

- `room set <id> lock <bool>`
- `room set <id> cycle <bool>`
- `room host <id> <user|?>` / host transfer
- `room close <id>`
- `room kick <id> <user_id>` / `room.kick`

Command audit telemetry now records:

- monotonically increasing command id;
- room id;
- action;
- success/failure;
- latency in microseconds;
- recent error message when a command fails.

Next cut:

1. Audit `start` and `cancel` carefully before moving them into the mailbox,
   because they touch `WaitForReady` state and send game-control messages.
2. Add tests around the gateway fallback behavior.
3. Begin moving selected room-owned state into actor-local ownership once the
   command boundary has stable failure and latency metrics.

## Step 17 update: start/cancel mailbox path

Step 17 completes the first admin room-write sweep through the per-room mailbox
registry by moving the higher-risk start/cancel operations behind the same
command boundary.

Commands now routed through the per-room mailbox registry:

- `room set <id> lock <bool>`
- `room set <id> cycle <bool>`
- `room host <id> <user|?>` / host transfer
- `room close <id>`
- `room kick <id> <user_id>` / `room.kick`
- `room start <id>` / `room-start`
- `room cancel <id>` / `room-cancel`

The cancel path was also tightened so it no longer awaits client sends while
holding the room-state write lock.  It performs the state transition in the
critical section, releases the lock, then sends `CancelGame` and publishes the
state update.

Next cut:

1. Extract `RoomCommandGateway` handlers into smaller command modules so
   `room_actor.rs` does not become the next large file.
2. Introduce typed `RoomCommand`/`RoomCommandResult` enums instead of passing
   ad-hoc JSON values through the actor boundary.
3. Start moving selected room-owned state into an actor-owned struct after the
   mailbox route remains stable under real and simulation suite tests.

## Step 18 update: split RoomCommandGateway module

Step 18 prevents the new Actor seam from becoming a replacement monolith.
The previous `phira-mp-plus-server/src/room_actor.rs` file is now split into a
module directory:

```text
room_actor/
  mod.rs
  command.rs
  gateway.rs
  stats.rs
```

No runtime behavior is intentionally changed in this cut.  The same admin room
write commands still flow through `RoomCommandGateway` and the same per-room
mailbox registry.  This is a maintenance move that creates room for the next
real Actor steps.

Next cut:

1. Replace ad-hoc `serde_json::Value` room command results with typed
   `RoomCommandResult` values.
2. Split compatibility inline handlers by command family so `gateway.rs` does
   not become the next large file.
3. Start moving selected room-owned state into actor-local structs once typed
   commands and command results are in place.
