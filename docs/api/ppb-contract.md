# PPB ↔ PMP Internal API Contract

## Architecture

```
PPB (Public Edge) ──→ PMP (Game Runtime)
    │                      │
    │  Internal HTTP       │  TCP 12346 (game protocol)
    │  (port 12347)        │  WS  /api/ws
    │  SSE /api/events     │  SSE /newapi/rooms/listen
    │  REST /api/*         │
```

## Network Boundary

- PMP binds to `127.0.0.1` by default (production)
- PPB must be on the same host or a trusted network
- No TLS between PPB and PMP (assume private network)
- All public TLS termination happens at PPB

## Authentication

- (TODO) PPB → PMP: service token via `admin_token` config or `PM_ADMIN_TOKEN` env
- PMP → PPB: no outbound calls by default

## Endpoints

### Health

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/health/live` | GET | Process alive (200) |
| `/health/ready` | GET | Subsystems ready (200) / degraded (503) |

### Event Stream

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/events` | GET | SSE stream of all runtime events |
| `/api/ws` | GET | WebSocket for real-time updates |

### Plugin Routes

Plugins register routes at init time via `http.register_route`. These follow the pattern:

- `/api/auth/visited/count` — Visitor count
- `/api/rooms/info` — Room listing
- `/api/rooms/info/:name` — Room detail by name
- `/rankapi/playtime_leaderboard` — Playtime ranking
- `/newapi/rooms/listen` — SSE room events

## SSE Event Format

```json
{
    "event_type": "RoomCreate | RoomJoin | RoomLeave | RoomModify | GameEnd | RoundComplete",
    "data": { }
}
```

Plugins translate these via `on_api("sse:translate", ...)`.

## Versioning

- PMP SemVer in `--version` output
- PPB should check compatibility via version endpoint
- Breaking protocol changes increment the game protocol version
