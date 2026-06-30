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
