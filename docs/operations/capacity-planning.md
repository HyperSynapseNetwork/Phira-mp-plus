# Capacity Planning

## Target Capacity (Single Instance)

| Metric | Target | Notes |
|--------|--------|-------|
| Concurrent connections | 5,000 | Authenticated sessions |
| Active rooms | 500 | At any given time |
| Hot-spot room | 100 | Simultaneous players in one room |
| Control command p99 | < 100 ms | Without plugin/DB failures |
| Slow consumer penalty | < 20% | p99 degradation when slow clients present |
| 24h RSS growth | < 256 MB | With jemalloc |

## Resource Estimates

| Component | Memory | CPU |
|-----------|--------|-----|
| Base server | ~50 MB RSS | 1 core |
| Per 1,000 sessions | ~100 MB | 0.5 core |
| Per 100 rooms | ~50 MB | 0.5 core |
| Per WASM plugin | ~64 MB max | per-call fuel-limited |

## Bottlenecks

1. **PostgreSQL connection pool**: Each query consumes a pool slot. Ensure `max_connections` is set appropriately.
2. **WAL I/O**: WAL fsync before queue admission. Disk latency directly affects event throughput.
3. **Plugin execution**: Plugins run synchronously on the blocking pool. Each plugin call holds a pool thread.
4. **Broadcast fan-out**: Room state changes are broadcast to all members. O(n) per event.

## Monitoring Thresholds

| Alert | Threshold | Action |
|-------|-----------|--------|
| Session count high | > 80% of max_sessions | Scale out or investigate leak |
| WAL pending age | > 60 seconds | Check persistence worker |
| DB error rate | > 1% in 5 min | Check database connectivity |
| Slow consumer ratio | > 10% of sessions | Tweak send buffer / disconnect policy |
| Supervisor degraded | any | Check critical task failures |
