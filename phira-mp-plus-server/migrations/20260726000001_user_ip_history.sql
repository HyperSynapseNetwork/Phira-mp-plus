-- ── User IP history ────────────────────────────────────────────────────────
-- Tracks every IP a user has connected from, with usage frequency.
CREATE TABLE IF NOT EXISTS user_ip_history (
    user_id INTEGER NOT NULL,
    ip TEXT NOT NULL,
    first_seen_at BIGINT NOT NULL,
    last_seen_at BIGINT NOT NULL,
    use_count INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (user_id, ip)
);
