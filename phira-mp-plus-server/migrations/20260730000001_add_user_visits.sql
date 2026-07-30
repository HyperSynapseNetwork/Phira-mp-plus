CREATE TABLE IF NOT EXISTS mp_user_visits (
    visit_id BIGSERIAL PRIMARY KEY,
    event_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    user_id INTEGER NOT NULL,
    connected_at BIGINT NOT NULL,
    created_at BIGINT NOT NULL,
    UNIQUE(event_id)
);
CREATE INDEX IF NOT EXISTS idx_mp_user_visits_user ON mp_user_visits(user_id);
CREATE INDEX IF NOT EXISTS idx_mp_user_visits_session ON mp_user_visits(session_id);
