-- Track server instance lifecycle for accurate playtime crash recovery.
--
-- Each server instance registers itself at startup and periodically updates
-- `last_alive_at` (heartbeat).  When a crash leaves stale playtime sessions
-- behind, recovery can accrue playtime only up to the old instance's last
-- known alive time instead of the startup time — so server downtime is not
-- counted as player playtime.
CREATE TABLE IF NOT EXISTS mp_server_instances (
    instance_id TEXT PRIMARY KEY,
    created_at BIGINT NOT NULL,
    last_alive_at BIGINT NOT NULL
);
