ALTER TABLE import_runs ADD COLUMN import_mode text NOT NULL DEFAULT 'owned' CHECK (import_mode IN ('owned', 'download'));
