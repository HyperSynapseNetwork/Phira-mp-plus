# WIT ABI Specification

## Current State

| Property | Value |
|----------|-------|
| **Runtime ABI** | `abi-json-v1` (JSON-memory bridge) |
| **Target ABI** | `abi-wit-v2` (WIT / Component Model) |
| **Canonical WIT** | `wit/phira-plugin.wit` (workspace root) |
| **MIGRATION_PHASE** | `0` (default, JSON bridge active; enable `wit-bindgen` feature for phase 1) |
| **Host bindings** | Generated via `wasmtime::component::bindgen!` behind `wit-bindgen` feature |
| **Host traits** | `WitPluginHost` skeleton implements `phira_host::Host` (behind `wit-bindgen`) |
| **Plugin ABI module** | Split into `plugin_abi/{mod,plan,json,dto}.rs` with typed DTOs |

> ⚠️ Default builds use `abi-json-v1`. Enable `--features wit-bindgen` to compile
> the WIT component-model bindings and typed host trait implementations.

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
- `phira-config` — Plugin configuration (key-value, JSON, per-plugin config.json)
- `phira-simulation` — Simulation control
- `phira-runtime` — Runtime diagnostics

World: `phira-plugin-v2`

## Migration Plan

1. ✅ WIT interfaces defined matching the current JSON ABI
2. ✅ Host bindings generated (`wasmtime::component::bindgen!` behind `wit-bindgen` feature)
3. ✅ Host trait skeleton (`WitPluginHost` in `wit_host.rs`, behind `wit-bindgen`)
4. ❌ Update guest SDK (`phira-mp-plus-sdk`) to use WIT exports
5. ❌ Dual-support both ABIs during transition
6. ❌ Remove JSON bridge after all plugins migrate

## Related files

- WIT definition: `wit/phira-plugin.wit`
- Host bindings: `plugin_abi/mod.rs` → `wit_abi` module (behind `wit-bindgen`)
- Host trait impls: `wit_host.rs` (behind `wit-bindgen`)
- JSON bridge: `plugin_abi/json.rs` (default path)
