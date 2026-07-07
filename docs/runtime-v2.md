# MP+ Runtime v2

> ⚠️ **历史规范** — 记录 Runtime v2 重构计划。
> 当前架构: actor-model blueprint + typed room command gateway + session dispatch 抽取;
> persistence worker + telemetry batcher + WIT-only plugin ABI (lifecycle wired, all host APIs implemented with capability enforcement)。
> Telemetry 模式: `direct_only` / `worker_preferred`。
> 命令参考: docs/cli.md。

Runtime v2 是 Phira-mp+ 的分阶段重构计划。

This document records the intended direction and the current implementation
state so future patches do not mix old patch baselines with new architecture
work.

## Current status

Runtime v2 has **not** replaced the existing runtime yet.

The early Runtime v2 patches introduced low-risk skeletons:

- `command_registry.rs`
- `simulation.rs`
- `event_bus.rs`
- `persistence_worker.rs`

The existing room/session/CLI/database paths remain the source of truth.

## Plan necessity audit

The current plan is still useful, but not every historical step should keep
driving new work. Treat the old step log as history and use the active
objectives in `runtime_plan.rs` as the current workboard.

Keep as active work:

- `actor-model`: necessary, because `server.rs`, `room.rs`, `session.rs` and
  `cli.rs` are still large and Room/Session actors do not own state yet.
  lock/cycle tracking proved mailbox exclusive-write can work.
- `plugin-abi-v2`: necessary. WIT lifecycle and all host APIs are implemented
  with capability enforcement. Remaining: integration tests and SDK updates.
- `persistence-worker`: necessary, because 7 direct DB write paths still bypass
  the worker (online/offline/disconnect/seen/round_data/ext/room_history).
- `low-overhead-diagnostics`: necessary, but only as an architectural guardrail:
  bounded queues, retained digests and cleanup tasks should remain mandatory for
  every new diagnostic surface.
- `test-coverage`: ongoing. 102 unit tests (up from 97). WIT capability model
  tests, persistence audit tests, simulation isolation tests, and lock
  exclusivity tests added.

Keep as completed guardrails:

- `eventbus`: complete enough for the current stage. Future work should use it,
  not reopen a broad EventBus redesign.
- `touch-judge-persistence`: complete enough for `direct_only` and
  `worker_preferred`; do not add a worker-only mode until worker parity is
  measured under load.
- `web-management-api`: keep blocked. This is a negative plan item that prevents
  accidental privileged Web write APIs.
- `phira-http`: complete. `fetch_phira_user_name` and `fetch_phira_chart`
  migrated to PhiraRetryClient. No bare reqwest outside phira_client/wasm_host.

Defer or merge:

- `tui-v2`: defer until Actor/Persistence signals stabilize. A new TUI before
  ownership changes settle would become another compatibility burden.
- More command-surface-only steps: merge into `cli-actor` work. Do not keep
  adding small CLI split steps unless they remove real coupling.
- Number-only milestone growth: stop adding historical steps without updating
  `runtime_plan.rs`, `actor_runtime.rs` and contract tests in the same patch.

## Rules

1. Do not rewrite room/session state machines in one patch.
2. Do not make real Phira HTTP requests from default simulation mode.
3. Do not let simulation data pollute normal rooms, Web API output, WIT API output, welcome text, rankings or player history.
4. Every Runtime v2 migration step must compile independently.
5. Every Runtime v2 patch must include validation commands and one commit command.

## Target runtime layers

```text
CLI / TUI / in-game admin / WIT
        ↓
Command Registry / Command Tree
        ↓
Actor Runtime facade
        ↓
Server / Session / Room / Persistence / Plugin / Simulation actors
        ↓
EventBus / Metrics / Persistence / TUI diagnostics
```

Web management API is intentionally out of scope.  Web routes may expose read-only
diagnostics such as `/api/runtime`, but Runtime v2 should not add privileged write
operations through Web management endpoints.

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
- Legacy behavior remains unchanged: plugin callbacks, SSE room monitor events, room state transitions and current PostgreSQL direct writes still run on the direct path.

This is intentionally not a state-machine migration. The goal is to make real production events visible to Runtime v2 before any ownership is moved away from the existing Room/Session code.

## Step 5 implementation status

Step 5 connects `EventBus` to `PersistenceWorker` as a diagnostic mirror only:

- The existing `db.rs` direct PostgreSQL write paths remain the source of truth.
- Low-frequency production events are converted into `PersistenceEvent` values and queued through the worker.
- Touches/Judges are intentionally skipped for now because they need a real batching and simulation-isolation policy before migration.
- `runtime persistence` now reports queue capacity, pending count, mirrored/skipped EventBus counts, lag counters, per-kind queue counts and recent worker trace entries.
- `/api/runtime` exposes the same worker stats for external observability.

Step 5 validated the Worker queue and backpressure behavior without changing persisted production data. Step 20 now starts the dual-write guard for low-frequency production events while keeping old direct writes alive.

## Step 6 implementation status

Step 6 adds the first concrete simulation-only persistence sink while still keeping production data safe:

- Runtime v2 no longer keeps old top-level aliases; use canonical namespaces such as `room host` and `runtime status`.
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


## Step 11 implementation status

Step 11 fixes a real production data-path issue and records the Actor-model destination more explicitly:

- Production `Touches` and `Judges` are now persisted/cached even when there is no active game monitor.
- Active monitors now only control live monitor broadcast, not whether gameplay telemetry is stored.
- Player live telemetry caches and `RoundStore` appends run whenever the room has a current round.
- Plugin `PlayerTouches` / `PlayerJudges` and Runtime v2 `touches.received` / `judges.received` signals are emitted regardless of monitor presence.
- The old warning for non-live Touch/Judge receipt is replaced with trace/debug diagnostics because this is now an expected unattended-game path.
- `actor_runtime.rs` introduces the first explicit Actor-model blueprint for splitting responsibilities currently concentrated in `server.rs`, `session.rs`, `room.rs` and `cli.rs`.
- `runtime actors` prints the migration boundaries and next step for each actor boundary.
- Web management API remains out of scope; the project keeps CLI/TUI/in-game admin/WIT as write-capable control surfaces.

This is the first Step that intentionally fixes a production gameplay data problem while still avoiding a risky Room state-machine ownership migration.

## Step 12 implementation status

Step 12 adds a persistent in-memory report layer for Simulation suites:

- `simulation suite <name>` now records a `SimulationSuiteReport` when the suite finishes or aborts.
- Reports keep per-step counters for ticks, chat messages, ready events, touch batches, judge batches and round results.
- Reports also compute total workload events and workload events per second, so suite runs can be compared without manually reading raw EventBus traces.
- `simulation report` prints the latest suite report.
- `simulation report list [limit]` lists recent suite summaries.
- `simulation report clear` clears the bounded report history.
- The report history is intentionally in-memory and bounded to the most recent 32 suite reports. It is for operator feedback and regression comparison, not long-term analytics storage.
- Suite completion also publishes a `simulation.suite_report` Runtime v2 event so the EventBus/PersistenceWorker simulation path can observe the same summary.

Useful examples:

```text
simulation suite smoke
simulation report
simulation report list 8
simulation report clear
```

This step improves the usability of the Simulation runner without changing production Room/Session ownership. It also keeps Web management API out of scope: reports are exposed through CLI/TUI-style command surfaces, not through privileged Web write endpoints.

## Step 13 implementation status

Step 13 starts the first production-facing Actor migration seam by adding a
`RoomCommandGateway`:

- CLI/admin room write commands for kick, close, start, cancel, host transfer,
  lock and cycle now route through one gateway.
- Legacy `server_state_query` room write methods for kick, host transfer, lock
  and close call the same gateway instead of carrying duplicate implementations
  in `server.rs`.
- The gateway is still an inline facade; it does not own room state and it does
  not replace the existing `Room` state machine yet.
- `runtime rooms` shows gateway counters and the current phase.
- `/api/runtime` includes `room_command_gateway` read-only diagnostics.
- At startup, the Actor blueprint marks `room-actor` as `write_routed` because a
  first set of admin/StateQuery write paths now pass through an actor-shaped
  boundary.

This step is intentionally more than documentation: it removes duplicate room
write logic from the large command surfaces and creates the seam that a future
per-room mailbox actor can implement without changing CLI/WIT/admin semantics.

## Step 14 implementation status

Step 14 begins the Room Actor migration with the first mailbox-backed production
write path:

- `RoomCommandGateway` now owns a bounded async mailbox.
- `set_lock` and `set_cycle` cross that mailbox before touching the existing
  `Room` state machine.
- The old inline implementation is retained as a fallback if the mailbox is not
  available, which keeps this step reversible during testing.
- `runtime rooms`, `runtime actors` and `/api/runtime` expose mailbox counters.
- This is still not a full per-room actor. Room state ownership remains in the
  existing `Room` type while the command boundary is being proven.

This step is the first production-facing actor-shaped write path. It is designed
specifically to reduce future growth in `server.rs`, `cli.rs` and `room.rs`
without rewriting the whole room lifecycle in one patch.

## Step 15 implementation status

Step 15 expands the Room Actor migration from a single mailbox-backed path into
an explicit per-room mailbox registry:

- `RoomCommandGateway` keeps mailbox senders keyed by `room_id`.
- `room set <id> lock <bool>` and `room set <id> cycle <bool>` continue through
  the mailbox path, but now use the per-room registry.
- `room host <id> <user|?>` / host transfer now crosses the same per-room
  mailbox boundary.
- `room close <id>` now crosses the same per-room mailbox boundary and removes
  the mailbox after close completes.
- The old inline implementations remain as fallback paths if mailbox setup,
  send, or reply fails.
- `runtime rooms`, `runtime actors`, and `/api/runtime` expose active room
  mailbox count, created mailbox count, registry hits, registry misses and the
  existing enqueue/complete/fallback counters.

Still intentionally not done:

- `kick`, `start`, and `cancel` remain inline gateway calls.
- Room state ownership still lives inside the existing `Room` type.
- There is still no privileged Web management API.

This step is the first real movement from a gateway-wide worker toward true
per-room actors while keeping the current protocol and room state machine
compatible.

## Step 16 implementation status

Step 16 continues the Room Actor migration by moving `kick` into the per-room
mailbox path and adding command-level audit telemetry:

- `room kick <room_id> <user_id>` and the corresponding internal `room.kick`
  path now cross the per-room mailbox registry.
- If kicking the target causes the room to be dropped, the mailbox worker exits
  and removes that room mailbox from the registry.
- `RoomCommandGateway` now records command id, room id, action, success/failure,
  latency and recent error text for room write commands.
- Each audited command also publishes a Runtime v2 `room.command` custom event.
- `runtime rooms`, `runtime actors` and `/api/runtime` expose command audit
  counters and recent command samples.

Still intentionally not done:

- `start` and `cancel` remain inline gateway calls until their `WaitForReady`
  locking and send behavior are audited separately.
- Room state ownership still lives in the existing `Room` type.
- There is still no privileged Web management API.

This step gives the Actor migration measurable command latency and failure
telemetry before moving higher-risk state-machine transitions.

## Step 17 implementation status

Step 17 moves the remaining higher-risk admin start/cancel room commands behind
`RoomCommandGateway`'s per-room mailbox registry:

- `room start <room_id>` now crosses the per-room mailbox.
- `room cancel <room_id>` now crosses the per-room mailbox.
- `Room::begin_admin_start` still owns the actual protocol behavior; the gateway
  only serializes the command with other per-room admin writes.
- `cancel_start` no longer sends `CancelGame` while holding the room-state write
  lock. It changes `WaitForReady -> SelectChart` inside the critical section,
  drops the lock, then sends control messages and publishes the state update.
- All admin room writes currently routed through the gateway are audited via
  `room.command` events and `runtime rooms` latency/error counters.

Still intentionally not done:

- The existing `Room` type still owns the real room state machine.
- The gateway is not yet a full actor-owned room-state implementation.
- There is still no privileged Web management API.

The next migration step should split the gateway handlers into smaller
actor-owned command modules and start moving state ownership out of `room.rs`
only after the mailbox path stays stable under suite/real tests.

## Step 18 implementation status

Step 18 is intentionally a structural refactor rather than a behavior change.
After Steps 13-17, `room_actor.rs` had started to become the next large file,
which would defeat the Runtime v2 goal of removing feature piles from
`server.rs`, `session.rs`, `cli.rs`, and `room.rs`.

The old single file has been split into a module directory:

```text
phira-mp-plus-server/src/room_actor/
  mod.rs       public module boundary and migration notes
  command.rs   mailbox command envelope and reply handling
  gateway.rs   RoomCommandGateway routing and compatibility handlers
  stats.rs     diagnostics and recent command audit data
```

Runtime behavior is intentionally unchanged:

- the same room write commands still route through `RoomCommandGateway`;
- the same per-room mailbox registry remains in place;
- the same inline compatibility handlers and fallback behavior remain;
- the existing `Room` type still owns the real room state machine;
- there is still no privileged Web management API.

This step matters because the Actor migration now has a maintainable place to
grow.  Future cuts should add typed command/result modules and eventually move
room-owned state out of `room.rs` without turning `room_actor` into another
monolith.

## Step 19 implementation status

Step 19 intentionally broadens the work again after several Room Actor-focused
patches.  Runtime v2 is not only the Room Actor migration; the master goals are:

- Simulation as the default benchmark path, with hybrid and real modes explicit;
- Actor-model migration for Room, Session, Persistence, Simulation, Plugin and CLI;
- Touches/Judges persistence even when no active monitor is connected;
- a unified Phira HTTP client with timeout, retry, backoff and circuit breaker;
- Persistence Worker ownership of database writes, first by dual-write and then by
  replacing direct writes after validation;
- EventBus as the runtime spine for observability and low-coupling fanout;
- TUI v2 panels based on stable runtime signals;
- no privileged Web management API.

This step adds `runtime_plan.rs`, a code-level workboard used by CLI and
`/api/runtime`.  This is deliberately not only documentation: long-running
maintenance work can query the current roadmap from the running server with:

```text
runtime roadmap
runtime status
```

Step 19 also introduces `phira_client.rs`, the first unified Phira HTTP
RetryClient.  Authentication, chart lookup and record lookup in `session.rs` now
route through this client.  It keeps the existing retry notice behavior while
centralizing timeout/backoff/retry statistics for future hybrid/real benchmark
modes:

```text
runtime phira
```

Step 27 turns that client into a config-driven policy surface under
`runtime_v2.phira_http` and adds a lightweight circuit breaker.  After a
configurable number of consecutive retryable failures, the breaker opens for a
short period and rejects new Phira HTTP work immediately.  This keeps fragile
Phira upstream failures from accumulating inside Session/Room hot paths.  The
breaker is visible through `runtime phira` and `/api/runtime`, but is configured
through `server_config.yml` rather than another new management command.

## Step 20 implementation status

Step 20 starts the first production-facing PersistenceWorker dual-write path:

- Low-frequency production `EventBus` events are still mirrored into `PersistenceWorker`.
- The worker now writes those low-frequency production events into `mp_events` through the existing `DbManager::record_room_event_sync` helper.
- Existing `db.rs` direct writes remain active and remain the compatibility source of truth for now.
- Worker-written production events include `runtime_v2_source = "persistence_worker"` and `runtime_v2_worker_preferred = true` in the JSON payload so duplicated direct-path writes are easy to identify during testing.
- Production Touch/Judge batches are **not** migrated to the worker yet. They were already fixed to persist without active monitors, but their high-frequency write path needs explicit batching/backpressure/flush semantics before worker ownership.
- `runtime persistence` now reports `prod_db_req` and `prod_skip` counters in addition to simulation persistence counters.

This is a deliberate dual-write stage, not a cutover. The next safe step is to move one low-frequency direct caller to worker-only mode after comparing `mp_events` output from the direct path and Runtime v2 worker path.


## Step 21 implementation status

Step 21 adds the high-frequency telemetry batching staging layer without yet
cutting production Touch/Judge persistence over to the worker.  The production
truth path is still the Step 11 direct persistence fix, which stores
Touches/Judges even when no active monitor is connected.

New in this step:

- `telemetry_batcher.rs` provides a bounded queue, batch sizing, interval flushes,
  dry-run mode, drop counters and recent trace entries.
- Production `touches.received` and `judges.received` EventBus events are now
  mirrored into `PersistenceWorker` as count-only `TouchBatch` / `JudgeBatch`
  staging events.
- `PersistenceWorker` routes those production telemetry staging events into the
  dry-run `TelemetryBatcher` instead of writing them to production tables.
- `runtime persistence` now shows telemetry staged count, accepted/dropped counts,
  pending queue size, flush count and touch/judge item totals.

This is intentionally a measurement and backpressure step.  It lets the server
observe real Touch/Judge throughput before moving production high-frequency data
into worker-owned batch database writes.  Step 22 should add a guarded batch-write
mode and keep the direct persistence path as the compatibility fallback until the
new path has been validated under simulation and real clients.

## Step 22 implementation status

Step 22 switches high-frequency production telemetry from dry-run staging to
**guarded batch-write dual-write** mode.  The project is still in test stage, so
schema evolution is allowed and this step adds a Runtime v2-owned telemetry table
instead of treating the old schema as immutable.

New in this step:

- `TelemetryBatcherPolicy::default()` now uses `dry_run = false`, so accepted
  production Touch/Judge telemetry is flushed to database by default.
- `mp_runtime_telemetry_batches` stores Runtime v2 worker-owned telemetry rows
  with `kind`, `room_id`, `round_uuid`, `player_id`, `item_count`, full JSON
  payload, source marker and dual-write marker.
- `session.rs` now stages the actual Touch/Judge payload and current round id
  directly into `PersistenceWorker`.  EventBus `touches.received` /
  `judges.received` remain count-only observability events and are no longer
  persisted by the worker mirror to avoid duplicate count-only rows.
- The direct `RoundStore` / `db.rs` path still writes the production
  touch/judge data.  Runtime v2 writes a second copy into the dedicated
  batch-write table so test runs can compare both paths before cutover.
- `runtime persistence` now reports `db_write_batches`, `db_write_items` and
  `db_write_errors` for the telemetry batcher.

The next step should compare `mp_round_touch_batches` / `mp_round_judge_batches`
with `mp_runtime_telemetry_batches` under simulation and real clients.  Once the
worker path is verified, a later step can remove the direct hot-path write or
turn it into fallback-only mode.

## Step 23 implementation status

Step 23 makes the Runtime v2 telemetry persistence structure more complete.
Because Phira-mp-plus is still in test stage, this step intentionally treats the
Runtime v2 schema as evolvable instead of freezing a temporary dual-write table.

New in this step:

- `mp_runtime_telemetry_batches` is upgraded from a flat batch table to a richer
  batch-header table with `batch_uuid`, `run_id`, `scope`, `pipeline`, `source`,
  `worker_preferred`, `schema_version` and `flush_reason` columns.
- `mp_runtime_telemetry_items` stores raw per-item Touch/Judge payload rows
  exploded from the batch payload `data` array.  This gives later analysis and
  replay tools a normalized item stream without having to repeatedly parse whole
  batch JSON payloads.
- `mp_runtime_persistence_meta` records the current Runtime v2 persistence schema
  manifest.
- `mp_runtime_retention_policies` records test-stage retention policy defaults
  for production telemetry, simulation data and runtime events.  Cleanup is still
  not automatically enforced; the table exists so the policy is explicit before a
  later cleanup worker is introduced.
- `TelemetryBatcherStats` now reports schema version, last batch uuid and raw
  item rows written.
- `runtime schema` / `runtime storage` / `runtime telemetry` explains the current
  Runtime v2 persistence layout from the CLI.

Current production Touch/Judge mode is still dual-write:

```text
direct RoundStore/db.rs path
+ Runtime v2 TelemetryBatcher -> mp_runtime_telemetry_batches/items
```

The next persistence step can introduce an explicit cutover switch such as
`direct_only` or `worker_preferred`.  Since the project is still in
active testing, schema and data files may continue to change aggressively as long
as the commands and migration notes stay clear.

## Step 24 implementation status

Step 24 adds an explicit production Touch/Judge persistence cutover switch.  The
project is still in test stage, so the switch is intentionally operational rather
than hidden behind a compatibility promise.

Available modes (simplified from 4 to 2 in Phase-0):

```text
direct_only    only the direct RoundStore/db.rs path writes Touch/Judge data
worker_preferred    Runtime v2 TelemetryBatcher batch-write only
```

New in this step:

- `TelemetryCutoverMode` centralizes the mode names, parsing and behavior rules.
- `PersistenceWorker` stores the current mode and exposes `runtime cutover` /
  `runtime telemetry-mode` CLI controls.
- `session.rs` now decides whether to call the direct `RoundStore` append path,
  enqueue the Runtime v2 worker path, or both based on the current cutover mode.
- `mp_runtime_persistence_meta` is updated whenever the mode changes so the
  active persistence behavior is visible from the database.
- `runtime persistence`, `runtime schema` and `runtime status` now show the
  active cutover mode.
- Step 24 also fixes two Step 23 mechanical issues: the duplicated `payload` token
  in `raw_item_count()` and a duplicated runtime persistence match trace.

Recommended test order:

```text
runtime cutover direct_only   # verify direct RoundStore/db.rs path
runtime cutover worker_preferred    # compare direct tables with Runtime v2 batch/item tables
runtime cutover worker_preferred   # verify the Session hot path can skip direct writes
```

Because this branch is not production yet, database schema and persistence
behavior can continue to evolve.  The important invariant is that the active mode
must always be visible through CLI diagnostics and database meta rows.


## Step 25: Runtime telemetry read path

Step 24 made `worker_preferred` possible for production Touch/Judge writes.  Step 25
makes that mode usable by adding the corresponding read path:

- `DbManager::read_round_player_data()` now reads direct `mp_round_player_data`
  first and falls back to normalized `mp_runtime_telemetry_items` when direct
  data is absent.  This keeps existing `round.data` / WIT / plugin read callers
  working after `runtime cutover worker_preferred`.
- `persist.touches` and `persist.judges` keep their direct batch query shape but
  fall back to `mp_runtime_telemetry_batches` when direct batch rows are absent.
- Runtime v2 batch rows include `runtime_v2_read_path=true` in the returned JSON
  so callers can distinguish the new storage path during testing.
- This step also fixes two mechanical schema-v2 patch hazards in `db.rs`: a
  duplicated `schema_version` bind in telemetry batch insertion and a duplicated
  `.await?` in table initialization.

Recommended worker-only validation:

```text
runtime cutover worker_preferred
# play one real round
round.data <round_uuid> <player_id>
persist.touches 0 20 <round_uuid> <player_id>
persist.judges 0 20 <round_uuid> <player_id>
runtime persistence
```

Because the project is still in test stage, this read path intentionally favors
clarity and debuggability over backward-compatible schema minimalism.  The next
step can cut over more consumers from direct batch tables to the normalized item
stream once the worker-only path is verified.


## Step 26: Config-driven runtime policy, fewer CLI knobs

Step 26 deliberately returns to a calmer development rhythm.  No new user-facing
management command is added in this step.  Runtime v2 persistence policy is now
configured from `server_config.yml` through the `runtime_v2` block:

```yaml
runtime_v2:
  persistence_queue_capacity: 4096
  telemetry_cutover_mode: worker_preferred
  telemetry_batcher:
    enabled: true
    dry_run: false
    queue_capacity: 8192
    max_items_per_batch: 256
    flush_interval_ms: 1000
```

This keeps the operational command surface from growing while the project is
still in heavy test-stage iteration.  Runtime diagnostics remain available, but
new defaults and pipeline policy should prefer configuration over more tiny CLI
subcommands.

Implementation notes:

- `PlusConfig::runtime_v2` owns startup defaults for PersistenceWorker and
  TelemetryBatcher.
- `PersistenceWorker::spawn_with_policy()` accepts queue capacity, batcher
  policy and initial Touch/Judge cutover mode.
- Internal hooks record the active policy into `mp_runtime_persistence_meta`
  after database initialization, so the running behavior is visible without
  adding another management command.
- Existing `runtime cutover` remains for quick testing, but future work should
  avoid adding one-off commands unless they are truly needed.

## Step 27: Config-driven Phira HTTP policy and circuit breaker

Step 27 continues the calmer post-Step-26 rhythm: no new management command is
added.  The existing `runtime phira` diagnostic output is reused, while the
actual policy moves into `server_config.yml`:

```yaml
runtime_v2:
  phira_http:
    timeout_ms: 5000
    max_retries: 3
    base_backoff_ms: 200
    max_backoff_ms: 3000
    circuit_breaker:
      enabled: true
      failure_threshold: 8
      open_duration_ms: 20000
```

The unified `PhiraRetryClient` now owns timeout, retry, backoff and a lightweight
circuit breaker.  When the upstream Phira service repeatedly returns retryable
failures such as 502, 5xx, 429, timeout or connect errors, the breaker opens for
a short configured window and rejects new Phira HTTP work immediately.  This is
intended to prevent authentication, chart lookup and record lookup from piling
up inside Session/Room hot paths while the official Phira service is unhealthy.

The breaker is intentionally simple for this stage:

- consecutive final request failures increase the breaker counter;
- a successful request closes the breaker and clears the failure counter;
- once the threshold is reached, requests are rejected until the open window
  expires;
- retry notices still use the existing client chat message during actual retry
  attempts;
- Simulation remains offline by default and does not call Phira.

This is not the final Phira subsystem.  Later work can add endpoint-level health,
metadata-worker queues and hybrid benchmark controls, but the hot path now has a
single place to enforce upstream-failure policy.

## Step 28: Session hot-path module split

Step 28 keeps the quieter development rhythm from Steps 26-27: no new runtime
management command is added.  Instead it starts reducing `session.rs` pressure by
moving two high-churn responsibilities into focused modules:

- `session_auth.rs` owns remote `/me` authentication helpers, ban rejection text
  normalization and delayed auth failure delivery.
- `session_telemetry.rs` owns Touch/Judge hot-path handling, including Runtime v2
  telemetry cutover, direct fallback, EventBus publishing, plugin dispatch and
  monitor broadcast.

This is intentionally behavior-preserving.  Touches/Judges are still persisted
without requiring an active monitor, and the current cutover modes
`direct_only` and `worker_preferred` are preserved.  The
point of this step is to stop the Session module from absorbing more unrelated
Runtime v2 logic before deeper Session Actor work begins.

Future Session decomposition should follow the same pattern: extract cohesive
hot-path responsibilities behind small internal modules first, then route the
remaining command handling through actor-shaped boundaries once behavior is easy
to test and compare.

## Step 29: Session room-command module split

Step 29 continues the same restrained development rhythm: it does not add new
runtime commands or database objects.  It moves client-side room lifecycle and
room gameplay command handling out of `session.rs` into `session_room.rs`.

The extracted module owns the behavior for:

- in-game admin CLI room-name shortcut decoding;
- create/join/leave room;
- client lock/cycle room toggles;
- select chart and request start;
- ready/cancel-ready;
- played/abort;
- query-room-info response assembly.

This is still a behavior-preserving split.  It does not replace the existing
Room state machine, and it does not move Session ownership to an actor yet.  The
purpose is to make the next Session Actor pass safer: `session.rs` can remain the
socket/authentication dispatcher, while room command semantics live behind a
single internal module boundary.

## Step 30 - Command surface policy

Step 30 stops treating every new operational detail as a first-class command.
The command system now has an explicit command surface policy:

- `primary` commands are shown in the default `help` overview and should be the
  recommended day-to-day command surface.
- `advanced` commands remain supported but are hidden from the default overview;
  use `help all` or `help group <group> all` when operating Runtime v2 internals.
Compatibility command spellings are no longer retained.  The project is still a
test branch, so Runtime v2 now favors canonical namespaces over compatibility
layers.

This is intentionally not a new Web management API and not another pile of
`runtime xxx` tools.  The goal is to keep CLI/TUI/in-game-admin commands usable
while Runtime v2 continues to grow.  Future command additions should first answer:

1. Is this a primary day-to-day operation or an advanced diagnostic?
2. Can this be configuration-driven instead of a new command?
3. Does the command belong under an existing namespace such as `room`, `plugin`,
   `simulation`, `benchmark`, or `runtime`?
4. If it is a temporary migration command, what is the removal plan?

Step 31 removes old command spellings instead of keeping a hidden compatibility
layer.  Examples: `plug-enable`, `room-start`, `room-transfer`, `bench`, `sim`,
`bc`, and one-letter shortcuts are gone. Use the canonical namespaces such as
`plugin enable`, `room start`, `room host`, `benchmark`, `simulation`, and
`broadcast all|room|user`.

## Step 31 - Canonical command surface

The project is still a test branch, so Runtime v2 no longer carries a CLI
compatibility layer.  Step 31 removes old aliases and one-off top-level room
commands instead of hiding them as compat commands.

Canonical examples:

- `plugin list`, `plugin enable`, `plugin disable`, `plugin reload`
- `room start`, `room cancel`, `room host`, `room close`, `room kick`
- `benchmark`, `benchmark modes`, `benchmark run real`, `benchmark run hybrid`
- `simulation run`, `simulation suite`, `simulation report`
- `broadcast all`, `broadcast room`, `broadcast user`

This keeps the command system small enough to continue building Runtime v2
without turning CLI compatibility into a permanent maintenance burden.

## Step 32 - skipped

No standalone Step 32 is retained. The work originally planned around command
surface cleanup was folded into Step 30 and Step 31.

## Step 33 - skipped

No standalone Step 33 is retained. The next meaningful change was the explicit
Plugin ABI / test-coverage workstream recorded in Step 34. Keep these skipped
markers so future audits do not mistake the numbering gap for missing
implementation.

### Step 34：Plugin ABI v2 完成主机 API / 测试目标收敛

Plugin ABI v2 已完成从骨架到完整实现的过渡，当前状态：

- ✅ **WIT lifecycle 已接线**：`call_init` / `call_cleanup` / `call_on_event` / `call_api` 均完整实现。`PluginEvent` → WIT typed event 的 11 个 variant 转换已完成。
- ✅ **Host API 已完整覆盖**：全部 12 个 WIT interface 的方法均有真实实现或明确 capability error。再无 `not yet implemented` 遗留。
- ✅ **Capability enforcement**：room-mgmt 写需 `room.manage`，admin 写需 `admin`，config 写需 `config`，simulation 控制需 `admin`。持久化查询返回 `persist.read` capability error。
- ✅ **http_request**：沙箱化 HTTP 客户端，集成 `validate_http_url` SSRF 保护、超时、重定向限制。
- ❌ **集成测试**：每个 host API 方法缺 running-server 上下文测试。
- ❌ **phira-plugin-sdk 示例**：需更新使 WIT/component model 成为唯一当前 ABI 路径。

### Step 35: CLI dispatch split

Step 35 keeps the faster development rhythm without lowering quality: it moves
only the top-level command routing table out of `cli.rs` into
`cli/dispatch.rs`.  No command semantics are changed in this step.

The purpose is to stop `cli.rs` from combining console lifecycle, line
continuation, help rendering, and every command route in the same file.  The new
boundary is intentionally narrow:

- `cli.rs` owns console lifecycle, input continuation, output helpers, and the
  concrete command implementation methods that already existed.
- `cli/dispatch.rs` owns the canonical top-level command dispatch table.

This is a fast structural split, not a compatibility pass and not a new command
surface.  Follow-up work can split `cli/dispatch.rs` further into
`room/plugin/runtime/simulation/benchmark` modules, but only after the first
boundary compiles cleanly in Actions.

### Step 35: CLI dispatch split

Step 35 keeps the faster development rhythm without lowering quality: it moves
only the top-level command routing table out of `cli.rs` into
`cli/dispatch.rs`.  No command semantics are changed in this step.

The purpose is to stop `cli.rs` from combining console lifecycle, line
continuation, help rendering, and every command route in the same file.  The new
boundary is intentionally narrow:

- `cli.rs` owns console lifecycle, input continuation, output helpers, and the
  concrete command implementation methods that already existed.
- `cli/dispatch.rs` owns the canonical top-level command dispatch table.

This is a fast structural split, not a compatibility pass and not a new command
surface.  Follow-up work can split `cli/dispatch.rs` further into
`room/plugin/runtime/simulation/benchmark` modules, but only after the first
boundary compiles cleanly in Actions.

### Step 36: CLI command-family split

Step 36 continues the CLI separation without adding new commands or changing
command semantics.  The `cli/dispatch.rs` top-level router now delegates
argument validation and command-family routing to `cli/commands/` modules:

- `cli/commands/plugin.rs`
- `cli/commands/room.rs`
- `cli/commands/broadcast.rs`
- `cli/commands/admin.rs`

The goal is to stop the dispatch layer from becoming another monolith after it
was split out of `cli.rs`.  Concrete command implementation methods still remain
on `CliHandler` for now; this is intentional so the first family split stays
low-risk.  Future steps can move the implementation bodies themselves into the
same command-family modules after Actions validates this dispatch-only split.

### Step 37: Plan consolidation gate

Step 37 is not a feature step. It is a gate for deciding which plans remain
necessary before Runtime v2 adds more surface area.

Required cleanup (resolved):

- ✅ Runtime status output matches `plugin_abi::wit::MIGRATION_PHASE` (phase 2).
- ✅ All plan docs describe the same current state.
- ✅ All TODO/FIXME markers removed from production code.
- ✅ JSON ABI removed from `phira-plugin-sdk` docs; WIT-only.
- ✅ Plan drift corrected: phira-http done, debt-triage done, MIGRATION_PHASE 2.

### Step 38: Runtime v2 closure gate

Step 38 tracks the final closure of all active Runtime v2 work items.
See `runtime_plan.rs` objective `step-38-closure-gate` for current blockers.

Remaining closure requirements:

- **plugin-abi-v2**: WIT integration tests (init/cleanup/on-event/on-api via real WASM).
- **actor-model**: lock/cycle Owned, remaining state WriteRouted → Owned.
- **persistence-worker**: round_store bypass remains (high-frequency data, intentionally deferred).
- **test-coverage**: 130 unit tests, WIT capability contract tests included.
- **simulation/benchmark-modes**: verified no-Phira default, cleanup tested.
3. Move the next Room state slice into the mailbox worker instead of adding
   more RoomCommandGateway facade methods.
4. Move legacy Phira metadata helpers behind `PhiraRetryClient` or a dedicated
   metadata worker.
5. Only then revisit TUI v2 panels.
