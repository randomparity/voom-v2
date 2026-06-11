-- Sprint 11 -- staged artifact verification and host-owned add-only commit.

-- sqlx 0.8's SQLite migrator wraps every migration in a transaction. SQLite
-- rewrites existing child-table foreign keys to file_versions_old when
-- foreign_keys is ON, so this migration explicitly exits the wrapper
-- transaction for the table rebuild, temporarily disables FK enforcement, then
-- starts a transaction again for sqlx migration bookkeeping.
--
-- Why the explicit COMMIT / PRAGMA / BEGIN pattern:
--   SQLite session PRAGMAs (foreign_keys, legacy_alter_table) are documented
--   as no-ops when issued inside an open transaction — the setting is silently
--   ignored if a transaction is already active.  Because sqlx starts a
--   transaction before running this file, we must COMMIT that transaction
--   first, set the PRAGMAs while no transaction is open, then BEGIN a new
--   transaction so sqlx can record the migration checksum on commit.
COMMIT;
PRAGMA foreign_keys = OFF;
PRAGMA legacy_alter_table = ON;

ALTER TABLE file_versions RENAME TO file_versions_old;

CREATE TABLE file_versions (
    id                          INTEGER PRIMARY KEY,
    file_asset_id               INTEGER NOT NULL REFERENCES file_assets(id) ON DELETE RESTRICT,
    content_hash                TEXT NOT NULL,
    size_bytes                  INTEGER NOT NULL CHECK (size_bytes >= 0),
    produced_by                 TEXT NOT NULL
        CHECK (produced_by IN ('ingest','transcode','remux','restore','external_observed','staged_commit')),
    produced_from_version_id    INTEGER REFERENCES file_versions(id) ON DELETE RESTRICT,
    created_at                  TEXT NOT NULL,
    retired_at                  TEXT,
    epoch                       INTEGER NOT NULL DEFAULT 0,
    CHECK (
        (produced_by IN ('ingest','external_observed'))
        OR produced_from_version_id IS NOT NULL
    )
) STRICT;

INSERT INTO file_versions
SELECT id, file_asset_id, content_hash, size_bytes, produced_by,
       produced_from_version_id, created_at, retired_at, epoch
FROM file_versions_old;

DROP TABLE file_versions_old;

CREATE INDEX file_versions_by_asset ON file_versions (file_asset_id);
CREATE INDEX file_versions_by_hash  ON file_versions (content_hash);

PRAGMA legacy_alter_table = OFF;
PRAGMA foreign_keys = ON;

CREATE TABLE artifact_verifications (
    id                    INTEGER PRIMARY KEY,
    artifact_handle_id    INTEGER NOT NULL REFERENCES artifact_handles(id) ON DELETE RESTRICT,
    artifact_location_id  INTEGER NOT NULL REFERENCES artifact_locations(id) ON DELETE RESTRICT,
    path                  TEXT NOT NULL,
    worker_id             INTEGER NOT NULL REFERENCES workers(id) ON DELETE RESTRICT,
    status                TEXT NOT NULL CHECK (status IN ('succeeded','failed')),
    expected_size_bytes   INTEGER NOT NULL CHECK (expected_size_bytes >= 0),
    expected_checksum     TEXT NOT NULL,
    observed_size_bytes   INTEGER CHECK (observed_size_bytes IS NULL OR observed_size_bytes >= 0),
    observed_checksum     TEXT,
    failure_class         TEXT,
    error_code            TEXT,
    message               TEXT,
    report                TEXT NOT NULL CHECK (json_valid(report)),
    started_at            TEXT NOT NULL,
    finished_at           TEXT NOT NULL,
    CHECK (
           (status = 'succeeded' AND observed_size_bytes IS NOT NULL AND observed_checksum IS NOT NULL
            AND failure_class IS NULL AND error_code IS NULL AND message IS NULL)
        OR (status = 'failed' AND failure_class IS NOT NULL AND error_code IS NOT NULL AND message IS NOT NULL)
    )
) STRICT;

CREATE INDEX artifact_verifications_by_artifact
    ON artifact_verifications (artifact_handle_id, id DESC);
CREATE INDEX artifact_verifications_success_by_location
    ON artifact_verifications (artifact_handle_id, artifact_location_id, id DESC)
    WHERE status = 'succeeded';

CREATE TABLE artifact_commit_records (
    id                       INTEGER PRIMARY KEY,
    artifact_handle_id       INTEGER NOT NULL REFERENCES artifact_handles(id) ON DELETE RESTRICT,
    source_file_version_id   INTEGER NOT NULL REFERENCES file_versions(id) ON DELETE RESTRICT,
    verification_id          INTEGER NOT NULL REFERENCES artifact_verifications(id) ON DELETE RESTRICT,
    target_path              TEXT NOT NULL,
    result_file_version_id   INTEGER REFERENCES file_versions(id) ON DELETE RESTRICT,
    result_file_location_id  INTEGER REFERENCES file_locations(id) ON DELETE RESTRICT,
    state                    TEXT NOT NULL CHECK (state IN ('pending','committed','failed','recovery_required')),
    failure_class            TEXT,
    error_code               TEXT,
    message                  TEXT,
    recovery_reason          TEXT,
    temp_path                TEXT,
    report                   TEXT NOT NULL CHECK (json_valid(report)),
    started_at               TEXT NOT NULL,
    promotion_started_at     TEXT,
    finished_at              TEXT,
    CHECK (
           (state = 'pending' AND result_file_version_id IS NULL AND result_file_location_id IS NULL
            AND failure_class IS NULL AND error_code IS NULL AND message IS NULL AND recovery_reason IS NULL
            AND finished_at IS NULL)
        OR (state = 'committed' AND result_file_version_id IS NOT NULL AND result_file_location_id IS NOT NULL
            AND failure_class IS NULL AND error_code IS NULL AND message IS NULL AND recovery_reason IS NULL
            AND finished_at IS NOT NULL)
        OR (state = 'failed' AND failure_class IS NOT NULL AND error_code IS NOT NULL AND message IS NOT NULL
            AND recovery_reason IS NULL AND finished_at IS NOT NULL)
        OR (state = 'recovery_required' AND failure_class IS NOT NULL AND error_code IS NOT NULL
            AND message IS NOT NULL AND recovery_reason IS NOT NULL AND finished_at IS NOT NULL)
    )
) STRICT;

CREATE UNIQUE INDEX artifact_commit_records_one_owner_per_artifact
    ON artifact_commit_records (artifact_handle_id)
    WHERE state IN ('pending','committed','recovery_required');

CREATE UNIQUE INDEX artifact_commit_records_one_owner_per_target
    ON artifact_commit_records (target_path)
    WHERE state IN ('pending','committed','recovery_required');

CREATE INDEX artifact_commit_records_by_state
    ON artifact_commit_records (state, started_at DESC);

-- Keep this statement at the end of the migration. The seeded-upgrade test
-- must also query it explicitly because SQLite returns one row per violation.
PRAGMA foreign_key_check;

BEGIN;
