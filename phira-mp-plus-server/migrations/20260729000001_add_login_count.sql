-- Add login_count column to mp_users for total_visit_count tracking
ALTER TABLE mp_users ADD COLUMN IF NOT EXISTS login_count BIGINT NOT NULL DEFAULT 0;
