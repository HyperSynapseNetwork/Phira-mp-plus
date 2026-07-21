# Upgrade & Rollback

## Version Scheme

PMP follows SemVer (`major.minor.patch`).

| Component | Version Location | Breaking Change |
|-----------|----------------|-----------------|
| Server | `Cargo.toml` | Major bump = possible breaking |
| Config schema | `config_version` field | Increment on backward-incompatible config change |
| DB schema | `_pmp_schema_version` table | Increment on migration |
| Event schema | `MP_EVENT_SCHEMA_VERSION` | Increment on event format change |
| WIT ABI | `phira-plugin-v2` world name | World rename = plugin rebuild required |
| Game protocol | `1` (stable) | Will not change |

## Upgrade Types

| Type | Example | Rollback Possible | Notes |
|------|---------|------------------|-------|
| Patch | `0.4.1` → `0.4.2` | ✅ | Bug fixes, no schema change |
| Minor | `0.4.x` → `0.5.x` | ⚠️ | Check DB migrations for backward compat |
| Major | `0.x` → `1.0.0` | ❌ | Breaking change, plan migration |

## Database Migrations

- Migrations are applied automatically at startup
- Each migration is a numbered SQL file in `migrations/`
- Applied migrations are recorded in `_pmp_schema_version`
- Forward-only: rollback requires a new migration (not revert)
- Old server may run on migrated DB as long as it ignores new columns

## Rollback Procedure

1. Stop the server
2. Restore the old binary
3. Restore the old config file
4. Start the server
5. Verify: `check-config` + `doctor` commands
6. WAL from the new version is compatible with old version (append-only format)
7. DB schema is backward-compatible within patch version

## Pre-Upgrade Checklist

- [ ] Read CHANGELOG for breaking changes
- [ ] Run `backup create` to archive current state
- [ ] Verify backup: `restore verify <path>`
- [ ] Test upgrade on staging first
- [ ] Plan rollback if migration is not backward-compatible
