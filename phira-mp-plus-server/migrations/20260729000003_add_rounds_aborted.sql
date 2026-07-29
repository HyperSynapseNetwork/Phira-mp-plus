-- Add aborted column to mp_rounds for crash-recovery marking
ALTER TABLE mp_rounds ADD COLUMN IF NOT EXISTS aborted BOOLEAN NOT NULL DEFAULT FALSE;
