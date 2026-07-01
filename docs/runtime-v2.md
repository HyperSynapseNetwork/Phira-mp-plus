# MP+ Runtime v2

Runtime v2 is a staged refactor plan for Phira-mp+.

This document records the intended direction and the current implementation
state so future patches do not mix old patch baselines with new architecture
work.

## Current status

Runtime v2 has **not** replaced the existing runtime yet.

The first Runtime v2 patch only introduces low-risk skeletons:

- `command_registry.rs`
- `simulation.rs`
- `event_bus.rs`
- `persistence_worker.rs`

The existing room/session/CLI/database paths remain the source of truth.

## Rules

1. Do not rewrite room/session state machines in one patch.
2. Do not make real Phira HTTP requests from default simulation mode.
3. Do not let simulation data pollute normal rooms, Web API output, WIT API output, welcome text, rankings or player history.
4. Every Runtime v2 migration step must compile independently.
5. Every Runtime v2 patch must include validation commands and one commit command.

## Target runtime layers

```text
CLI / TUI / in-game admin / WIT / future Web admin
        ↓
Command Registry / Command Tree
        ↓
Runtime Services
        ↓
EventBus
        ↓
Plugin / Web API / Metrics / Persistence / TUI / Simulation
```

## Simulation modes

- `simulation`: default, deterministic, no real Phira access.
- `benchmark hybrid`: optional partial real Phira access.
- `benchmark real`: explicit real TCP/auth compatibility test.

The existing `benchmark` command currently represents the real-network path and
must not be treated as the default Runtime v2 stress test.

## First migration candidates

1. Command metadata and help formatting.
2. Simulation status/seed/cleanup commands.
3. EventBus publication for admin and simulation events.
4. Persistence Worker for room snapshots and low-frequency events.
5. Dedicated simulation data isolation.

## Step 2 implementation status

Step 2 connects the skeletons to low-risk operational surfaces:

- `help` can now render detailed metadata from `CommandRegistry`.
- TUI tab completion reads the same registry instead of hard-coded arrays.
- `runtime status|commands|events|persistence` exposes skeleton health.
- `simulation run|stop` manages in-memory lifecycle counters and broadcasts start/stop notices to real players.
- Simulation still does **not** create virtual users, virtual rooms, database rows, Web API records or WIT-visible records.
- Existing `benchmark` remains the explicit real-network compatibility path.

Step 2 deliberately does not migrate Room, Session, PostgreSQL writes or plugin events to Runtime v2. The next safe migration target is adding a simulation data isolation design before any real virtual room/player objects are created.

## Step 3 implementation status

Step 3 turns Simulation from a pure counter skeleton into an isolated shadow world:

- `simulation run ...` materializes deterministic shadow users, rooms and sample rounds inside `SimulationManager`.
- Shadow rooms are marked hidden and are **not** inserted into the production `rooms` map.
- Shadow users are negative-ID simulation users and are **not** inserted into the production `users` map.
- `/api/rooms` still returns only real, non-hidden production rooms.
- New diagnostic Web API routes are available:
  - `/api/runtime`
  - `/api/simulation`
  - `/api/simulation/world`
- New CLI commands are available:
  - `simulation tick [count]`
  - `simulation inspect [limit]`
- `EventBus` now keeps basic publish/delivery counters and a bounded recent-event trace for diagnostics.

Step 3 still deliberately avoids moving real Room/Session state-machine ownership into Runtime v2. The next safe cut is to let Simulation publish more structured Runtime v2 events and to add a simulation-only persistence path; after that, selected low-risk Room events can be mirrored into EventBus.

## Step 4 implementation status

Step 4 starts connecting the production runtime to `EventBus` as an observation-only mirror:

- `publish_room_event` now mirrors low-risk `RoomEvent` values into typed Runtime v2 events.
- Normal client authentication/reconnect publishes `user.connected`.
- Normal disconnect/kick paths publish `user.disconnected`.
- Chat, ready/cancel-ready, touches, judges, game start and round completion now have EventBus signals.
- `EventBus` diagnostics include per-kind counters in addition to the recent trace.
- Legacy behavior remains unchanged: plugin callbacks, SSE room monitor events, room state transitions and current PostgreSQL direct writes still run on the old path.

This is intentionally not a state-machine migration. The goal is to make real production events visible to Runtime v2 before any ownership is moved away from the existing Room/Session code.

## Step 5 implementation status

Step 5 connects `EventBus` to `PersistenceWorker` as a diagnostic mirror only:

- The existing `db.rs` direct PostgreSQL write paths remain the source of truth.
- Low-frequency production events are converted into `PersistenceEvent` values and queued through the worker.
- Touches/Judges are intentionally skipped for now because they need a real batching and simulation-isolation policy before migration.
- `runtime persistence` now reports queue capacity, pending count, mirrored/skipped EventBus counts, lag counters, per-kind queue counts and recent worker trace entries.
- `/api/runtime` exposes the same worker stats for external observability.

This step validates the Worker queue and backpressure behavior without changing persisted production data. The next safe step is to add explicit simulation persistence isolation metadata/table planning, then migrate one low-frequency database write path behind a feature flag or dual-write guard.

## Step 6 implementation status

Step 6 adds the first concrete simulation-only persistence sink while still keeping production data safe:

- The `rt` top-level alias was removed from `runtime` because it conflicted with the existing `room-transfer` alias and produced an unreachable-pattern warning.
- `simulation persist` / `simulation snapshot` publishes the current shadow-world snapshot as a `simulation.snapshot` Runtime v2 event.
- `PersistenceWorker` now requests PostgreSQL writes only for events marked as simulation data.
- Simulation diagnostics are written to the dedicated `mp_sim_events` table when PostgreSQL is enabled and initialized.
- Production events mirrored through `PersistenceWorker` are still diagnostic-only and do not replace existing `db.rs` direct production writes.

This is intentionally a simulation-only write path. It gives Runtime v2 a real persistence target for test data without contaminating the normal `mp_users`, `mp_room_snapshots`, `mp_events` or round result tables.

## Step 7 implementation status

Step 7 makes Simulation runnable without manually typing `simulation tick`:

- `simulation run ...` now starts a background runner by default (`auto=true`).
- The runner advances the isolated shadow world every `tick_ms` milliseconds.
- The runner automatically stops the same run when `duration` seconds is reached.
- Manual mode is still available with `simulation run ... auto=false`, then `simulation tick [count]`.
- Optional periodic simulation snapshots can be emitted with `persist_every=N`; `0` disables periodic snapshots.
- Status output now includes elapsed/remaining seconds and runner settings.
- The runner broadcasts the normal start/end notices to real players, but still does not insert simulation users/rooms into production state.

Useful examples:

```text
simulation run baseline
simulation run small duration=30 tick_ms=500
simulation run custom users=500 rooms=50 duration=300 tick_ms=1000 persist_every=30
simulation run baseline auto=false
simulation tick 10
simulation stop
simulation cleanup
```

This is still not a Room/Session state-machine migration. It is a controllable Runtime v2 load generator running entirely inside the simulation shadow world.


## Step 8 implementation status

Step 8 makes Simulation events more realistic while keeping the shadow world isolated:

- Each Simulation tick still publishes the summary `simulation.tick` event.
- The same tick now also emits bounded aggregate events:
  - `simulation.chat`
  - `simulation.ready`
  - `simulation.touch`
  - `simulation.judge`
  - `simulation.round`
- These are aggregate-per-tick events, not one event per virtual user or note, so the EventBus and PersistenceWorker are not flooded during early Runtime v2 testing.
- `simulation.touch` and `simulation.judge` are mapped to the worker's simulation-only `TouchBatch` / `JudgeBatch` paths, then stored through `mp_sim_events` when PostgreSQL is enabled.
- Production Touches/Judges are still not migrated to the worker path. The current production `db.rs` writes remain unchanged.

Useful checks:

```text
simulation run baseline duration=10 tick_ms=500 persist_every=5
runtime events
runtime persistence
simulation inspect 20
```

This gives Runtime v2 a safer high-frequency rehearsal path before touching real Room/Session hot paths.

## Step 9 implementation status

Step 9 adds Simulation workload scenarios/profiles on top of the Step 8 aggregate-event generator:

- `SimulationConfig` now carries a `scenario` field.
- Available scenarios are:
  - `balanced`
  - `chat_storm`
  - `ready_storm`
  - `round_storm`
  - `touch_judge_burst`
  - `idle`
- `simulation scenarios` lists the available scenarios and their intended pressure shape.
- `simulation run ... scenario=<name>` changes the deterministic per-tick aggregate workload without changing the requested preset size.
- Generated `simulation.tick`, `simulation.chat`, `simulation.ready`, `simulation.touch`, `simulation.judge` and `simulation.round` payloads include the scenario name.
- The scenario system still runs inside the isolated shadow world and still does not insert virtual users/rooms into production maps.

Useful examples:

```text
simulation scenarios
simulation run baseline scenario=chat_storm duration=30 tick_ms=500
simulation run medium scenario=touch_judge_burst persist_every=10
simulation run custom users=200 rooms=20 scenario=idle auto=false
simulation tick 5
runtime events
runtime persistence
```

This gives Runtime v2 a repeatable way to test different pressure shapes before migrating any production Room/Session hot path into the new runtime services.

## Step 10 implementation status

Step 10 adds Simulation suites/batch runs so one command can exercise several workload shapes in sequence:

- `SimulationSuite` defines repeatable suite presets:
  - `smoke`: short sanity check for CI/manual smoke testing.
  - `mixed`: balanced sweep across chat, ready, round and touch/judge scenarios.
  - `stress`: heavier sweep for sustained EventBus/PersistenceWorker pressure.
- `simulation suite` lists available suites and their planned steps.
- `simulation suite <smoke|mixed|stress>` starts a background suite runner.
- Each suite step is still a normal isolated shadow-world run with its own `run_id`.
- The suite runner publishes:
  - `simulation.suite_started`
  - `simulation.suite_step_started`
  - `simulation.suite_step_completed`
  - `simulation.suite_completed`
- Suite events flow through EventBus and the simulation-only PersistenceWorker path, so they can be observed via `runtime events`, `runtime persistence` and `mp_sim_events`.
- A suite never inserts virtual users/rooms into production `users` or `rooms` maps.

Useful examples:

```text
simulation suite
simulation suite smoke
simulation suite mixed duration=15 tick_ms=500 persist_every=5
simulation suite stress users=800 rooms=80 duration=30
runtime events
runtime persistence
```

This step makes Simulation usable as a repeatable regression/load workflow instead of a single one-off scenario run.
