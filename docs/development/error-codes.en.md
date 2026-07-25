# Error Codes & CLI Reference

## Exit Codes

| Code | Meaning | When |
|------|---------|------|
| `0` | Success | Normal exit |
| `1` | Runtime error | Startup failure, config error |
| `2` | Database error | PostgreSQL init failure |
| `3` | Config validation error | Invalid server_config.yml |
| `101` | Panic | Unrecoverable internal error |

## CLI Command Reference

### Core Commands

| Command | Description |
|---------|-------------|
| `help [cmd]` | Show help for command or group |
| `exit` | Shut down server |
| `status` | Show server version, port, room count |
| `check-config` | Validate loaded config with redacted output |
| `doctor` | Run system diagnostics (DB/rooms/sessions) |

### Room Management

| Command | Description |
|---------|-------------|
| `room list` | List active rooms |
| `room info <id>` | Show room details |
| `room close <id>` | Force-close a room |
| `room kick <id> <user>` | Kick user from room |
| `room lock <id>` | Lock/unlock room |
| `room cycle <id>` | Toggle auto-cycle |

### Plugin Management

| Command | Description |
|---------|-------------|
| `plugin list` | List loaded plugins |
| `plugin info <name>` | Show plugin metadata |
| `plugin enable <name>` | Enable a disabled plugin |
| `plugin disable <name>` | Disable a plugin |
| `plugin reload [name]` | Reload all or specific plugin |
| `plugin remove <name>` | Unregister plugin (keep files) |
| `plugin purge <name>` | Delete plugin files + data |

### Runtime Diagnostics

| Command | Description |
|---------|-------------|
| `runtime status` | Runtime diagnostics summary |
| `runtime events` | EventBus stats |
| `runtime persistence` | WAL + DB stats |
| `runtime commands` | Command registry stats |

### Operations

| Command | Description |
|---------|-------------|
| `wal inspect` | WAL path and size |
| `dead-letter list [n]` | Show recent dead-letter entries |
| `dead-letter replay` | Re-queue dead-letter events |
| `backup create [path]` | Archive config + data + WAL |
| `restore verify <path>` | Verify backup integrity |
| `config reload` | Hot-reload YAML config |

### Security

| Command | Description |
|---------|-------------|
| `ban <user_id> <reason>` | Ban a user |
| `unban <user_id>` | Unban a user |
| `banlist` | List banned users |

## Error Message Format

```
  ✗ <error description>
  ◆ <info message>
  ✓ <success message>
  ○ <neutral/status message>
```
