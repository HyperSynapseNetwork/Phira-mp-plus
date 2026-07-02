# WIT ABI Specification

## Current State

| Property | Value |
|----------|-------|
| **Runtime ABI** | `abi-json-v1` (JSON-memory bridge) |
| **Target ABI** | `abi-wit-v2` (WIT / Component Model) |
| **Canonical WIT** | `wit/phira-plugin.wit` (workspace root) |
| **Legacy WIT** | `phira-mp-plus-server/wit/phira/mpplus.wit` (deprecated pointer only) |
| **MIGRATION_PHASE** | `0` |
| **Host bindings** | Not yet generated — `wasm_host.rs` still uses JSON-memory bridge |

> ⚠️ **Important**: The WIT file is currently a **contract target only**. Plugins are still
> loaded via the `abi-json-v1` JSON-memory bridge. Do not assume that the typed WIT
> exports/imports are executable at runtime.

## Canonical WIT Interfaces

The canonical WIT (`wit/phira-plugin.wit`) defines:

- `phira-types` — Core data types (touch/judge events, plugin info, HTTP response, JSON value)
- `phira-host` — Host functions available to plugins (log, UUID, time, API call, chat, HTTP)
- `phira-events` — Plugin event types (connect/disconnect, room lifecycle, game events)
- `phira-query` — User and room data queries
- `phira-room-mgmt` — Room management operations
- `phira-user-mgmt` — User moderation (kick, ban, etc.)
- `phira-messaging` — Message broadcasting
- `phira-persistence` — Event/snapshot queries
- `phira-admin` — Admin ID management
- `phira-simulation` — Simulation control
- `phira-runtime` — Runtime diagnostics

World: `phira-plugin-v2`

## Migration Plan

1. ✅ WIT interfaces defined matching the current JSON ABI
2. ❌ Generate host bindings with `wasmtime::component::bindgen!`
3. ❌ Implement host side (`wasm_host.rs` → typed WIT imports)
4. ❌ Update guest SDK (`phira-mp-plus-sdk`) to use WIT exports
5. ❌ Dual-support both ABIs during transition
6. ❌ Remove JSON bridge after all plugins migrate

## Legacy WIT

The legacy WIT (`phira-mp-plus-server/wit/phira/mpplus.wit`) is a **deprecated pointer only**.
It is kept as a migration reference for `abi-json-v1` era plugins. New plugins should target
the canonical WIT (`wit/phira-plugin.wit`), though typed host bindings are not yet available.
