-- Track the connection session_id owning each open playtime session so that
-- an old UserOffline/UserDisconnect replay cannot close a NEWER session that
-- reconnected on the same instance (session generation protection).
ALTER TABLE playtime ADD COLUMN IF NOT EXISTS session_id TEXT;
