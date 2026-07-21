-- Initial PMP database schema (unified)
-- Migration V1 — replaces legacy hand-written DDL in db.rs
--
-- All statements are idempotent (IF NOT EXISTS / ADD COLUMN IF NOT EXISTS)
-- so this migration is safe for both fresh databases and those already
-- carrying tables created by earlier versions of the hand-written init_tables().

-- ── Sequence ───────────────────────────────────────────────────────────────
CREATE SEQUENCE IF NOT EXISTS mp_persist_sequence;

-- ── Legacy playtime (migrating to mp_ prefix) ──────────────────────────────
CREATE TABLE IF NOT EXISTS playtime (
    user_id INTEGER PRIMARY KEY,
    total_secs BIGINT NOT NULL DEFAULT 0,
    session_start BIGINT
);

-- ── Legacy room_history (migrating to mp_user_room_history) ────────────────
CREATE TABLE IF NOT EXISTS room_history (
    id BIGSERIAL PRIMARY KEY,
    user_id INTEGER NOT NULL,
    room_id TEXT NOT NULL,
    room_uuid TEXT NOT NULL,
    joined_at BIGINT NOT NULL
);

-- ── Users ──────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS mp_users (
    user_id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    language TEXT NOT NULL DEFAULT '',
    ip TEXT,
    first_seen_at BIGINT NOT NULL,
    last_seen_at BIGINT NOT NULL,
    last_connected_at BIGINT,
    last_disconnected_at BIGINT,
    updated_at BIGINT NOT NULL
);

-- ── Room snapshots ─────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS mp_room_snapshots (
    room_id TEXT PRIMARY KEY,
    room_uuid TEXT NOT NULL,
    payload JSONB NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    sequence BIGINT NOT NULL DEFAULT nextval('mp_persist_sequence')
);

-- ── Events ─────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS mp_events (
    sequence BIGINT PRIMARY KEY DEFAULT nextval('mp_persist_sequence'),
    event_id TEXT,
    kind TEXT NOT NULL,
    room_id TEXT,
    user_id INTEGER,
    payload JSONB NOT NULL,
    created_at BIGINT NOT NULL
);

-- ── User-room history (structured) ─────────────────────────────────────────
CREATE TABLE IF NOT EXISTS mp_user_room_history (
    id BIGSERIAL PRIMARY KEY,
    user_id INTEGER NOT NULL,
    room_id TEXT NOT NULL,
    room_uuid TEXT NOT NULL,
    joined_at BIGINT NOT NULL,
    created_at BIGINT NOT NULL
);

-- ── Rounds ─────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS mp_rounds (
    round_uuid TEXT PRIMARY KEY,
    room_id TEXT NOT NULL,
    chart_id INTEGER NOT NULL,
    chart_name TEXT NOT NULL,
    players JSONB NOT NULL DEFAULT '[]'::jsonb,
    started_at BIGINT NOT NULL,
    finished_at BIGINT,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    sequence BIGINT NOT NULL DEFAULT nextval('mp_persist_sequence')
);

-- ── Touch batches ──────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS mp_round_touch_batches (
    sequence BIGINT PRIMARY KEY DEFAULT nextval('mp_persist_sequence'),
    round_uuid TEXT NOT NULL,
    player_id INTEGER NOT NULL,
    count INTEGER NOT NULL,
    first_game_time DOUBLE PRECISION,
    last_game_time DOUBLE PRECISION,
    payload JSONB NOT NULL,
    created_at BIGINT NOT NULL
);

-- ── Judge batches ──────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS mp_round_judge_batches (
    sequence BIGINT PRIMARY KEY DEFAULT nextval('mp_persist_sequence'),
    round_uuid TEXT NOT NULL,
    player_id INTEGER NOT NULL,
    count INTEGER NOT NULL,
    first_game_time DOUBLE PRECISION,
    last_game_time DOUBLE PRECISION,
    payload JSONB NOT NULL,
    created_at BIGINT NOT NULL
);

-- ── Round player data ──────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS mp_round_player_data (
    round_uuid TEXT NOT NULL,
    player_id INTEGER NOT NULL,
    touches JSONB NOT NULL DEFAULT '[]'::jsonb,
    judges JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    sequence BIGINT NOT NULL DEFAULT nextval('mp_persist_sequence'),
    PRIMARY KEY (round_uuid, player_id)
);

-- ── Round results ──────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS mp_round_results (
    round_uuid TEXT NOT NULL,
    user_id INTEGER NOT NULL,
    room_id TEXT NOT NULL,
    score INTEGER NOT NULL,
    accuracy DOUBLE PRECISION NOT NULL,
    payload JSONB NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    sequence BIGINT NOT NULL DEFAULT nextval('mp_persist_sequence'),
    PRIMARY KEY (round_uuid, user_id)
);

-- ── Runtime telemetry batches ──────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS mp_runtime_telemetry_batches (
    sequence BIGINT PRIMARY KEY DEFAULT nextval('mp_persist_sequence'),
    event_id TEXT UNIQUE,
    batch_uuid TEXT NOT NULL,
    run_id TEXT,
    scope TEXT NOT NULL DEFAULT 'production',
    pipeline TEXT NOT NULL DEFAULT 'runtime_v2.telemetry_batcher',
    kind TEXT NOT NULL,
    room_id TEXT,
    round_uuid TEXT,
    player_id INTEGER NOT NULL,
    item_count INTEGER NOT NULL,
    payload JSONB NOT NULL,
    created_at BIGINT NOT NULL,
    source TEXT NOT NULL DEFAULT 'telemetry_batcher',
    dual_write BOOLEAN NOT NULL DEFAULT TRUE,
    schema_version INTEGER NOT NULL DEFAULT 3,
    flush_reason TEXT NOT NULL DEFAULT 'unknown'
);

-- ── Runtime telemetry items ────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS mp_runtime_telemetry_items (
    sequence BIGINT PRIMARY KEY DEFAULT nextval('mp_persist_sequence'),
    event_id TEXT,
    batch_uuid TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    kind TEXT NOT NULL,
    room_id TEXT,
    round_uuid TEXT,
    player_id INTEGER NOT NULL,
    payload JSONB NOT NULL,
    created_at BIGINT NOT NULL,
    schema_version INTEGER NOT NULL DEFAULT 3
);

-- ── Persistence metadata / runtime config snapshots ────────────────────────
CREATE TABLE IF NOT EXISTS mp_runtime_persistence_meta (
    key TEXT PRIMARY KEY,
    value JSONB NOT NULL,
    updated_at BIGINT NOT NULL
);

-- ── Retention policies ─────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS mp_runtime_retention_policies (
    scope TEXT PRIMARY KEY,
    retain_seconds BIGINT NOT NULL,
    cleanup_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at BIGINT NOT NULL
);

-- ── Benchmark reports ──────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS mp_runtime_benchmark_reports (
    sequence BIGINT PRIMARY KEY DEFAULT nextval('mp_persist_sequence'),
    report_id TEXT,
    mode TEXT NOT NULL,
    title TEXT NOT NULL,
    duration_secs BIGINT NOT NULL,
    is_simulation BOOLEAN NOT NULL DEFAULT FALSE,
    operations BIGINT,
    failed_operations BIGINT,
    probes_attempted BIGINT NOT NULL DEFAULT 0,
    probes_succeeded BIGINT NOT NULL DEFAULT 0,
    probes_failed BIGINT NOT NULL DEFAULT 0,
    probes_blocked BIGINT NOT NULL DEFAULT 0,
    probes_skipped BIGINT NOT NULL DEFAULT 0,
    failure_samples INTEGER NOT NULL DEFAULT 0,
    notes INTEGER NOT NULL DEFAULT 0,
    report JSONB NOT NULL,
    created_at BIGINT NOT NULL,
    source TEXT NOT NULL DEFAULT 'persistence_worker',
    schema_version INTEGER NOT NULL DEFAULT 1
);

-- ── Simulation events ──────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS mp_sim_events (
    sequence BIGINT PRIMARY KEY DEFAULT nextval('mp_persist_sequence'),
    event_id TEXT,
    run_id TEXT,
    kind TEXT NOT NULL,
    payload JSONB NOT NULL,
    created_at BIGINT NOT NULL
);

-- ── Key-value settings store ───────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS mp_settings (
    key TEXT PRIMARY KEY,
    value JSONB NOT NULL,
    updated_at BIGINT NOT NULL
);

-- ── Schema version tracking (legacy) ───────────────────────────────────────
CREATE TABLE IF NOT EXISTS _pmp_schema_version (
    version INTEGER PRIMARY KEY,
    applied_at BIGINT NOT NULL DEFAULT extract(epoch from now())::bigint * 1000,
    description TEXT NOT NULL DEFAULT ''
);

INSERT INTO _pmp_schema_version (version, description)
VALUES (1, 'migrations/20260721000001_initial_schema.sql')
ON CONFLICT (version) DO NOTHING;

-- ── Backwards-compat column additions ─────────────────────────────────────
-- These are no-ops on fresh tables but cover databases created by earlier
-- hand-written init_tables() that may be missing some columns.

ALTER TABLE mp_users ADD COLUMN IF NOT EXISTS ip TEXT;
ALTER TABLE mp_events ADD COLUMN IF NOT EXISTS event_id TEXT;
ALTER TABLE mp_sim_events ADD COLUMN IF NOT EXISTS event_id TEXT;
ALTER TABLE mp_round_player_data ADD COLUMN IF NOT EXISTS sequence BIGINT NOT NULL DEFAULT nextval('mp_persist_sequence');
ALTER TABLE mp_runtime_telemetry_batches ADD COLUMN IF NOT EXISTS event_id TEXT;
ALTER TABLE mp_runtime_benchmark_reports ADD COLUMN IF NOT EXISTS report_id TEXT;
ALTER TABLE mp_runtime_telemetry_batches ADD COLUMN IF NOT EXISTS dual_write BOOLEAN NOT NULL DEFAULT TRUE;
ALTER TABLE mp_runtime_telemetry_batches ADD COLUMN IF NOT EXISTS source TEXT NOT NULL DEFAULT 'telemetry_batcher';
ALTER TABLE mp_runtime_telemetry_items ADD COLUMN IF NOT EXISTS event_id TEXT;
ALTER TABLE mp_runtime_telemetry_batches ADD COLUMN IF NOT EXISTS batch_uuid TEXT;
ALTER TABLE mp_runtime_telemetry_batches ADD COLUMN IF NOT EXISTS run_id TEXT;
ALTER TABLE mp_runtime_telemetry_batches ADD COLUMN IF NOT EXISTS scope TEXT NOT NULL DEFAULT 'production';
ALTER TABLE mp_runtime_telemetry_batches ADD COLUMN IF NOT EXISTS pipeline TEXT NOT NULL DEFAULT 'runtime_v2.telemetry_batcher';
ALTER TABLE mp_runtime_telemetry_batches ADD COLUMN IF NOT EXISTS schema_version INTEGER NOT NULL DEFAULT 3;
ALTER TABLE mp_runtime_telemetry_batches ALTER COLUMN schema_version SET DEFAULT 3;
ALTER TABLE mp_runtime_telemetry_items ALTER COLUMN schema_version SET DEFAULT 3;
ALTER TABLE mp_runtime_telemetry_batches ADD COLUMN IF NOT EXISTS flush_reason TEXT NOT NULL DEFAULT 'unknown';

-- Ensure mp_events.sequence uses the global sequence.
ALTER TABLE mp_events ALTER COLUMN sequence SET DEFAULT nextval('mp_persist_sequence');

-- ── Indexes ────────────────────────────────────────────────────────────────
CREATE INDEX IF NOT EXISTS idx_mp_events_created ON mp_events(created_at);
CREATE INDEX IF NOT EXISTS idx_mp_events_kind ON mp_events(kind);
CREATE INDEX IF NOT EXISTS idx_mp_events_room ON mp_events(room_id);
CREATE INDEX IF NOT EXISTS idx_mp_events_user ON mp_events(user_id);

CREATE INDEX IF NOT EXISTS idx_mp_rounds_started ON mp_rounds(started_at);

CREATE INDEX IF NOT EXISTS idx_mp_touch_batches_round_player_seq ON mp_round_touch_batches(round_uuid, player_id, sequence);
CREATE INDEX IF NOT EXISTS idx_mp_touch_batches_created ON mp_round_touch_batches(created_at);

CREATE INDEX IF NOT EXISTS idx_mp_judge_batches_round_player_seq ON mp_round_judge_batches(round_uuid, player_id, sequence);
CREATE INDEX IF NOT EXISTS idx_mp_judge_batches_created ON mp_round_judge_batches(created_at);

CREATE INDEX IF NOT EXISTS idx_mp_user_room_history_user ON mp_user_room_history(user_id);

CREATE UNIQUE INDEX IF NOT EXISTS idx_room_history_join_identity ON room_history(user_id, room_uuid, joined_at);
CREATE UNIQUE INDEX IF NOT EXISTS idx_mp_user_room_history_join_identity ON mp_user_room_history(user_id, room_uuid, joined_at);

CREATE UNIQUE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_event_id ON mp_runtime_telemetry_batches(event_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_item_event_ordinal ON mp_runtime_telemetry_items(event_id, ordinal);
CREATE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_batch_uuid ON mp_runtime_telemetry_batches(batch_uuid);
CREATE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_created ON mp_runtime_telemetry_batches(created_at);
CREATE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_kind ON mp_runtime_telemetry_batches(kind);
CREATE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_scope_created ON mp_runtime_telemetry_batches(scope, created_at);
CREATE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_run ON mp_runtime_telemetry_batches(run_id);
CREATE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_round_player ON mp_runtime_telemetry_batches(round_uuid, player_id, sequence);
CREATE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_room ON mp_runtime_telemetry_batches(room_id);
CREATE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_items_batch ON mp_runtime_telemetry_items(batch_uuid, ordinal);
CREATE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_items_round_player ON mp_runtime_telemetry_items(round_uuid, player_id, sequence);
CREATE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_items_kind_created ON mp_runtime_telemetry_items(kind, created_at);

CREATE UNIQUE INDEX IF NOT EXISTS uq_mp_runtime_benchmark_report_id ON mp_runtime_benchmark_reports(report_id) WHERE report_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_mp_runtime_benchmark_reports_mode_created ON mp_runtime_benchmark_reports(mode, created_at);
CREATE INDEX IF NOT EXISTS idx_mp_runtime_benchmark_reports_created ON mp_runtime_benchmark_reports(created_at);
CREATE INDEX IF NOT EXISTS idx_mp_runtime_benchmark_reports_sim ON mp_runtime_benchmark_reports(is_simulation, created_at);

CREATE UNIQUE INDEX IF NOT EXISTS uq_mp_events_event_id ON mp_events(event_id) WHERE event_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS uq_mp_sim_events_event_id ON mp_sim_events(event_id) WHERE event_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_mp_sim_events_run ON mp_sim_events(run_id);
CREATE INDEX IF NOT EXISTS idx_mp_sim_events_kind ON mp_sim_events(kind);
CREATE INDEX IF NOT EXISTS idx_mp_sim_events_created ON mp_sim_events(created_at);
