# Compatibility Matrix

> Last updated: 2026-07-19

## Server Version

PMP uses SemVer (`major.minor.patch`). Current: `0.4.x` (pre-production).

| Component | Version Scheme | Current |
|-----------|---------------|---------|
| Server | SemVer | `0.4.x` |
| Game Protocol | Integer | `1` |
| WIT ABI | Integer | `2` |
| Config Schema | Integer | `1` |
| DB Schema | Integer | `1` |
| Event Schema | Integer | `1` |

## Upgrade Rules

| Upgrade Type | Rolling Possible | Notes |
|-------------|-----------------|-------|
| Patch (0.4.1 → 0.4.2) | ✅ | Bug fixes only, no schema change |
| Minor (0.4 → 0.5) | ⚠️ | Check changelog for breaking changes |
| Major (0.x → 1.0) | ❌ | Requires full migration |

## Database Compatibility

- DB schema is forward-compatible within the same minor version
- Schema migrations use expand/contract pattern
- Rollback: old server version must be able to read old columns
- Downgrade may require manual revert migration

## Protocol Compatibility

- Game protocol version `1` is stable
- Clients must negotiate protocol version on connect
- Server rejects clients with unsupported protocol version

## Plugin Compatibility

- WIT ABI `v2` is the only supported ABI
- ABI version changes require plugin recompile
- Server validates plugin WIT ABI at load time
- Breaking ABI changes increment the WIT ABI version
