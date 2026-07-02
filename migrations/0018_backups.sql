-- Sprint 17 -- durable backup records (T9: real backup worker, records, report, CLI).
--
-- A backup is written `pending` before the copy and transitioned to `verified`
-- or `failed` after, so a crash mid-backup leaves a recoverable `pending` row
-- (finished_at IS NULL). The both-or-neither CHECK keeps the terminal columns
-- consistent with `status`. The partial-unique index enforces at most one
-- verified backup per (ticket, source version) so a retried mutating operation
-- reuses the existing copy instead of writing a duplicate.

CREATE TABLE backups (
    id                     INTEGER PRIMARY KEY,
    source_file_version_id INTEGER NOT NULL REFERENCES file_versions(id) ON DELETE RESTRICT,
    job_id                 INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    ticket_id              INTEGER NOT NULL REFERENCES tickets(id) ON DELETE RESTRICT,
    provider               TEXT NOT NULL,
    destination_path       TEXT NOT NULL,
    size_bytes             INTEGER CHECK (size_bytes IS NULL OR size_bytes >= 0),
    checksum               TEXT,
    status                 TEXT NOT NULL CHECK (status IN ('pending', 'verified', 'failed')),
    failure_class          TEXT,
    error_code             TEXT,
    message                TEXT,
    started_at             TEXT NOT NULL,
    finished_at            TEXT,
    created_at             TEXT NOT NULL,
    CHECK (
           (status = 'pending'  AND size_bytes IS NULL AND checksum IS NULL
                AND failure_class IS NULL AND error_code IS NULL AND message IS NULL
                AND finished_at IS NULL)
        OR (status = 'verified' AND size_bytes IS NOT NULL AND checksum IS NOT NULL
                AND failure_class IS NULL AND error_code IS NULL AND message IS NULL
                AND finished_at IS NOT NULL)
        OR (status = 'failed'   AND failure_class IS NOT NULL AND error_code IS NOT NULL
                AND message IS NOT NULL AND finished_at IS NOT NULL)
    )
) STRICT;

CREATE INDEX backups_by_file_version ON backups (source_file_version_id, id DESC);
CREATE INDEX backups_by_job ON backups (job_id, id DESC);
CREATE UNIQUE INDEX backups_verified_key
    ON backups (ticket_id, source_file_version_id) WHERE status = 'verified';
