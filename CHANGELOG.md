# Changelog

## 0.4.x (pre-production hardening)

### Phase A — Production blocking fixes

- **CI gate**: Removed global `-A clippy::all`; clippy now uses `-D warnings`
- **WAL**: Atomic compaction, versioned frames with SHA-256 checksum, fail-closed replay
- **DB fail-fast**: Configured database must connect or server rejects startup; `allow_database_degraded_mode` to bypass
- **HTTP security**: Default bind to `127.0.0.1`; configurable via `http_bind_address`
- **Health endpoints**: `/health/live`, `/health/ready` with subsystem checks
- **Secrets from env**: `PM_DATABASE_URL`, `PM_ADMIN_TOKEN` and `*_FILE` variants
- **Config**: `config_version: 1`, `max_rooms: null` warning
- **Deployment**: Multi-stage Dockerfile with non-root user; systemd service with sandboxing
- **Toolchain**: `rust-toolchain.toml` pinned to 1.96.0
- **Docs**: Archived superseded reports; added `docs/guarantees.md`, `docs/product/overview.md`
