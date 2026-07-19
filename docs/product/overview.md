# Phira-mp+ Product Overview

## What it is

Phira-mp+ (PMP) is the real-time multiplayer game runtime for the Phira+
architecture. It handles TCP game protocol, session management, room state
machines, game rounds, trusted WASM plugin execution, and reliable event
persistence.

## What it is not

PMP is **not** a public-facing web gateway. The following responsibilities
belong to Phira+ Backend (PPB):

- Public user accounts and OAuth
- Web API gateway
- TLS termination and rate limiting
- Admin dashboard frontend
- CDN and WAF

## Architecture context

```
Internet → PPB (auth/gateway/TLS) → PMP (game runtime) → PostgreSQL
                                        ↑
                                    WASM plugins
```

PMP operates on a trusted internal network behind PPB. All public traffic
flows through PPB; PMP only serves internal, authenticated requests.

## Target audience

- Self-hosted Phira server operators
- Phira+ service deployers
- Plugin developers (trusted ecosystem)

## Current status

Pre-production hardening candidate (v0.4.x). Suitable for controlled staging
and internal grayscale testing, not for general production release.
