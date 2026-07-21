# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.4.x | ✅ Active development |
| < 0.4 | ❌ |

## Reporting a Vulnerability

To report a security vulnerability, please contact the maintainers directly:

- Open a GitHub Security Advisory at: https://github.com/HyperSynapseNetwork/Phira-mp-plus/security/advisories
- Or email: security@phira-mp-plus.dev

You should receive a response within 48 hours. If you don't, please follow up.

## Disclosure Policy

1. Report received → acknowledged within 48h
2. Investigation → fix developed (timeline depends on severity)
3. Fix released → coordinated disclosure

## Scope

- PMP game server (this repository)
- Official WASM plugin SDK
- Phira+ protocol implementations

Out of scope: third-party plugins, custom deployments, the Phira web frontend.

## Data Handling

PMP processes Phira user IDs, gameplay data (touches, judges, scores),
and connection metadata (IP addresses, timestamps). Data retention is
configured via `persistence_retention_days` and `round_data_retention_days`
in `server_config.yml`. See [docs/configuration.md](docs/configuration.md)
for configuration details.
