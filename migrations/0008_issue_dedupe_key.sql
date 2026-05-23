ALTER TABLE issues ADD COLUMN dedupe_key TEXT;

CREATE UNIQUE INDEX issues_dedupe_key_unique
    ON issues (dedupe_key)
    WHERE dedupe_key IS NOT NULL;
