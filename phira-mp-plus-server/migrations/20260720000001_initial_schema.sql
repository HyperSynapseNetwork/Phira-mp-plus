-- Initial PMP database schema
-- Migration V1

-- Playtime tracking
CREATE TABLE IF NOT EXISTS playtime (
    user_id INTEGER PRIMARY KEY,
    total_secs BIGINT NOT NULL DEFAULT 0,
    session_start BIGINT
);

-- Room visit history
CREATE TABLE IF NOT EXISTS room_history (
    id SERIAL PRIMARY KEY,
    room_id TEXT NOT NULL,
    room_uuid TEXT NOT NULL,
    user_id INTEGER NOT NULL,
    joined_at BIGINT NOT NULL,
    left_at BIGINT
);

-- User profiles and metadata
CREATE TABLE IF NOT EXISTS mp_users (
    user_id INTEGER PRIMARY KEY,
    user_name TEXT NOT NULL,
    language TEXT NOT NULL DEFAULT '',
    is_monitor BOOLEAN NOT NULL DEFAULT FALSE,
    extra JSONB NOT NULL DEFAULT '{}',
    online BOOLEAN NOT NULL DEFAULT FALSE,
    last_seen_at BIGINT NOT NULL DEFAULT 0,
    created_at BIGINT NOT NULL DEFAULT extract(epoch from now())::bigint * 1000,
    updated_at BIGINT NOT NULL DEFAULT extract(epoch from now())::bigint * 1000
);

-- Room snapshots
CREATE TABLE IF NOT EXISTS mp_room_snapshots (
    sequence BIGSERIAL PRIMARY KEY,
    room_id TEXT NOT NULL,
    snapshot JSONB NOT NULL,
    source TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL DEFAULT extract(epoch from now())::bigint * 1000
);
CREATE INDEX IF NOT EXISTS idx_room_snapshots_room_id ON mp_room_snapshots(room_id);

-- Domain events
CREATE TABLE IF NOT EXISTS mp_events (
    sequence BIGSERIAL PRIMARY KEY,
    event_id TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    scope TEXT NOT NULL DEFAULT '',
    room_id TEXT,
    user_id INTEGER,
    simulation BOOLEAN NOT NULL DEFAULT FALSE,
    payload JSONB NOT NULL,
    created_at BIGINT NOT NULL DEFAULT extract(epoch from now())::bigint * 1000
);
CREATE INDEX IF NOT EXISTS idx_events_kind ON mp_events(kind);
CREATE INDEX IF NOT EXISTS idx_events_room_id ON mp_events(room_id);
CREATE INDEX IF NOT EXISTS idx_events_created_at ON mp_events(created_at);

-- User-room visit history (structured)
CREATE TABLE IF NOT EXISTS mp_user_room_history (
    id BIGSERIAL PRIMARY KEY,
    user_id INTEGER NOT NULL,
    room_id TEXT NOT NULL,
    room_uuid TEXT NOT NULL,
    joined_at BIGINT NOT NULL,
    left_at BIGINT
);
CREATE INDEX IF NOT EXISTS idx_user_room_history_user ON mp_user_room_history(user_id);

-- Game rounds
CREATE TABLE IF NOT EXISTS mp_rounds (
    round_uuid UUID PRIMARY KEY,
    room_id TEXT NOT NULL,
    chart_id INTEGER NOT NULL,
    chart_name TEXT NOT NULL DEFAULT '',
    started_at BIGINT NOT NULL DEFAULT extract(epoch from now())::bigint * 1000,
    finished_at BIGINT
);
CREATE INDEX IF NOT EXISTS idx_rounds_room_id ON mp_rounds(room_id);

-- Touch data batches
CREATE TABLE IF NOT EXISTS mp_round_touch_batches (
    batch_uuid UUID PRIMARY KEY,
    round_uuid UUID NOT NULL REFERENCES mp_rounds(round_uuid),
    player_id INTEGER NOT NULL,
    batch_index INTEGER NOT NULL,
    data JSONB NOT NULL,
    created_at BIGINT NOT NULL DEFAULT extract(epoch from now())::bigint * 1000
);

-- Judge data batches
CREATE TABLE IF NOT EXISTS mp_round_judge_batches (
    batch_uuid UUID PRIMARY KEY,
    round_uuid UUID NOT NULL REFERENCES mp_rounds(round_uuid),
    player_id INTEGER NOT NULL,
    batch_index INTEGER NOT NULL,
    data JSONB NOT NULL,
    created_at BIGINT NOT NULL DEFAULT extract(epoch from now())::bigint * 1000
);

-- Player live data snapshots
CREATE TABLE IF NOT EXISTS mp_round_player_data (
    id BIGSERIAL PRIMARY KEY,
    round_uuid UUID NOT NULL REFERENCES mp_rounds(round_uuid),
    player_id INTEGER NOT NULL,
    snapshot_index INTEGER NOT NULL,
    touches JSONB,
    judges JSONB,
    created_at BIGINT NOT NULL DEFAULT extract(epoch from now())::bigint * 1000
);

-- Round results
CREATE TABLE IF NOT EXISTS mp_round_results (
    id BIGSERIAL PRIMARY KEY,
    round_uuid UUID NOT NULL REFERENCES mp_rounds(round_uuid),
    player_id INTEGER NOT NULL,
    score INTEGER NOT NULL DEFAULT 0,
    accuracy REAL NOT NULL DEFAULT 0,
    perfect INTEGER NOT NULL DEFAULT 0,
    good INTEGER NOT NULL DEFAULT 0,
    bad INTEGER NOT NULL DEFAULT 0,
    miss INTEGER NOT NULL DEFAULT 0,
    max_combo INTEGER NOT NULL DEFAULT 0,
    full_combo BOOLEAN NOT NULL DEFAULT FALSE,
    aborted BOOLEAN NOT NULL DEFAULT FALSE,
    std_score REAL NOT NULL DEFAULT 0,
    created_at BIGINT NOT NULL DEFAULT extract(epoch from now())::bigint * 1000
);

-- Telemetry batch metadata
CREATE TABLE IF NOT EXISTS mp_runtime_telemetry_batches (
    batch_uuid UUID PRIMARY KEY,
    run_id TEXT,
    scope TEXT NOT NULL DEFAULT '',
    pipeline TEXT NOT NULL DEFAULT '',
    source TEXT NOT NULL DEFAULT '',
    flush_reason TEXT NOT NULL DEFAULT '',
    schema_version INTEGER NOT NULL DEFAULT 1,
    dual_write BOOLEAN NOT NULL DEFAULT FALSE,
    item_count INTEGER NOT NULL DEFAULT 0,
    created_at BIGINT NOT NULL DEFAULT extract(epoch from now())::bigint * 1000
);

-- Telemetry items
CREATE TABLE IF NOT EXISTS mp_runtime_telemetry_items (
    id BIGSERIAL PRIMARY KEY,
    batch_uuid UUID NOT NULL REFERENCES mp_runtime_telemetry_batches(batch_uuid),
    event_id TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT '',
    room_id TEXT,
    round_uuid TEXT,
    player_id INTEGER NOT NULL DEFAULT 0,
    item_count INTEGER NOT NULL DEFAULT 0,
    payload JSONB NOT NULL,
    created_at BIGINT NOT NULL DEFAULT extract(epoch from now())::bigint * 1000
);

-- Persistence metadata / runtime config snapshots
CREATE TABLE IF NOT EXISTS mp_runtime_persistence_meta (
    id BIGSERIAL PRIMARY KEY,
    config_key TEXT NOT NULL,
    config_value JSONB NOT NULL,
    created_at BIGINT NOT NULL DEFAULT extract(epoch from now())::bigint * 1000
);

-- Retention policy tracking
CREATE TABLE IF NOT EXISTS mp_runtime_retention_policies (
    id BIGSERIAL PRIMARY KEY,
    policy_name TEXT NOT NULL,
    retention_days INTEGER NOT NULL DEFAULT 30,
    last_cleaned_at BIGINT,
    created_at BIGINT NOT NULL DEFAULT extract(epoch from now())::bigint * 1000
);

-- Benchmark reports
CREATE TABLE IF NOT EXISTS mp_runtime_benchmark_reports (
    sequence BIGSERIAL PRIMARY KEY,
    mode TEXT NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    duration_secs BIGINT NOT NULL DEFAULT 0,
    is_simulation BOOLEAN NOT NULL DEFAULT FALSE,
    operations INTEGER NOT NULL DEFAULT 0,
    failed_operations INTEGER NOT NULL DEFAULT 0,
    probes_failed INTEGER NOT NULL DEFAULT 0,
    report JSONB NOT NULL,
    summary JSONB NOT NULL DEFAULT '{}',
    created_at BIGINT NOT NULL DEFAULT extract(epoch from now())::bigint * 1000
);

-- Simulation events (test harness)
CREATE TABLE IF NOT EXISTS mp_sim_events (
    sequence BIGSERIAL PRIMARY KEY,
    run_id UUID NOT NULL,
    tick BIGINT NOT NULL,
    kind TEXT NOT NULL,
    payload JSONB NOT NULL,
    created_at BIGINT NOT NULL DEFAULT extract(epoch from now())::bigint * 1000
);

-- Key-value settings store
CREATE TABLE IF NOT EXISTS mp_settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at BIGINT NOT NULL DEFAULT extract(epoch from now())::bigint * 1000
);

-- Schema version tracking
CREATE TABLE IF NOT EXISTS _pmp_schema_version (
    version INTEGER PRIMARY KEY,
    applied_at BIGINT NOT NULL DEFAULT extract(epoch from now())::bigint * 1000,
    description TEXT NOT NULL DEFAULT ''
);

INSERT INTO _pmp_schema_version (version, description)
VALUES (1, 'Initial schema: all production tables')
ON CONFLICT (version) DO NOTHING;
