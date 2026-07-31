-- Add server_instance_id to playtime table so crash recovery can
-- distinguish sessions from the current server instance vs stale sessions
-- from previous (crashed) instances.
ALTER TABLE playtime ADD COLUMN IF NOT EXISTS server_instance_id TEXT;
