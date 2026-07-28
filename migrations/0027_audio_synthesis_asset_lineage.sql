-- Sequential synthesis phases create new versions of the same file asset.
-- Preserve unique result-version ownership without treating the asset itself
-- as the operation identity.
--
-- sqlx wraps migrations in a transaction. SQLite ignores the foreign-key and
-- legacy-alter-table PRAGMAs inside a transaction, so exit the wrapper before
-- rebuilding the table and begin a new transaction for migrator bookkeeping.
COMMIT;
PRAGMA foreign_keys = OFF;
PRAGMA legacy_alter_table = ON;

ALTER TABLE audio_synthesis_operations RENAME TO audio_synthesis_operations_old;

CREATE TABLE audio_synthesis_operations (
    id                          INTEGER PRIMARY KEY,
    operation_key               TEXT NOT NULL UNIQUE CHECK (length(operation_key) > 0),
    planned_operation_id        TEXT NOT NULL CHECK (length(planned_operation_id) > 0),
    source_file_version_id      INTEGER NOT NULL
        REFERENCES file_versions(id) ON DELETE RESTRICT,
    source_media_snapshot_id    INTEGER NOT NULL
        REFERENCES media_snapshots(id) ON DELETE RESTRICT,
    target_codec                TEXT NOT NULL CHECK (length(target_codec) > 0),
    target_channels             INTEGER NOT NULL CHECK (target_channels > 0),
    container                   TEXT NOT NULL CHECK (length(container) > 0),
    target_path                 TEXT NOT NULL UNIQUE CHECK (length(target_path) > 0),
    state                       TEXT NOT NULL
        CHECK (state IN ('planned','staged','committed')),
    dispatch_generation         INTEGER NOT NULL DEFAULT 0
        CHECK (dispatch_generation >= 0),
    claim_lease_id              INTEGER REFERENCES leases(id) ON DELETE RESTRICT,
    claim_token                 TEXT,
    claim_expires_at            TEXT,
    staging_path                TEXT UNIQUE,
    expected_size_bytes         INTEGER CHECK (expected_size_bytes >= 0),
    expected_checksum           TEXT,
    worker_result               TEXT CHECK (worker_result IS NULL OR json_valid(worker_result)),
    artifact_handle_id          INTEGER UNIQUE
        REFERENCES artifact_handles(id) ON DELETE RESTRICT,
    artifact_location_id        INTEGER UNIQUE
        REFERENCES artifact_locations(id) ON DELETE RESTRICT,
    verification_id             INTEGER UNIQUE
        REFERENCES artifact_verifications(id) ON DELETE RESTRICT,
    commit_record_id            INTEGER UNIQUE
        REFERENCES artifact_commit_records(id) ON DELETE RESTRICT,
    probe_worker_id             INTEGER REFERENCES workers(id) ON DELETE RESTRICT,
    probe_payload               TEXT CHECK (probe_payload IS NULL OR json_valid(probe_payload)),
    result_file_asset_id        INTEGER
        REFERENCES file_assets(id) ON DELETE RESTRICT,
    result_file_version_id      INTEGER UNIQUE
        REFERENCES file_versions(id) ON DELETE RESTRICT,
    result_file_location_id     INTEGER UNIQUE
        REFERENCES file_locations(id) ON DELETE RESTRICT,
    result_media_snapshot_id    INTEGER UNIQUE
        REFERENCES media_snapshots(id) ON DELETE RESTRICT,
    created_at                  TEXT NOT NULL,
    finished_at                 TEXT,
    CHECK (
        (claim_lease_id IS NULL AND claim_token IS NULL AND claim_expires_at IS NULL)
        OR
        (claim_lease_id IS NOT NULL AND length(claim_token) > 0
         AND claim_expires_at IS NOT NULL)
    ),
    CHECK (
        (expected_size_bytes IS NULL AND expected_checksum IS NULL)
        OR
        (expected_size_bytes IS NOT NULL AND expected_checksum IS NOT NULL)
    ),
    CHECK (
        (artifact_handle_id IS NULL AND artifact_location_id IS NULL)
        OR
        (artifact_handle_id IS NOT NULL AND artifact_location_id IS NOT NULL)
    ),
    CHECK (
        verification_id IS NULL
        OR (artifact_handle_id IS NOT NULL AND artifact_location_id IS NOT NULL)
    ),
    CHECK (commit_record_id IS NULL OR verification_id IS NOT NULL),
    CHECK (
        (probe_worker_id IS NULL AND probe_payload IS NULL)
        OR
        (probe_worker_id IS NOT NULL AND probe_payload IS NOT NULL)
    ),
    CHECK (
        (result_file_asset_id IS NULL
         AND result_file_version_id IS NULL
         AND result_file_location_id IS NULL
         AND result_media_snapshot_id IS NULL)
        OR
        (result_file_asset_id IS NOT NULL
         AND result_file_version_id IS NOT NULL
         AND result_file_location_id IS NOT NULL
         AND result_media_snapshot_id IS NOT NULL)
    ),
    CHECK (
        (state = 'committed' AND finished_at IS NOT NULL)
        OR
        (state != 'committed' AND finished_at IS NULL)
    )
) STRICT;

INSERT INTO audio_synthesis_operations (
    id,
    operation_key,
    planned_operation_id,
    source_file_version_id,
    source_media_snapshot_id,
    target_codec,
    target_channels,
    container,
    target_path,
    state,
    dispatch_generation,
    claim_lease_id,
    claim_token,
    claim_expires_at,
    staging_path,
    expected_size_bytes,
    expected_checksum,
    worker_result,
    artifact_handle_id,
    artifact_location_id,
    verification_id,
    commit_record_id,
    probe_worker_id,
    probe_payload,
    result_file_asset_id,
    result_file_version_id,
    result_file_location_id,
    result_media_snapshot_id,
    created_at,
    finished_at
)
SELECT
    id,
    operation_key,
    planned_operation_id,
    source_file_version_id,
    source_media_snapshot_id,
    target_codec,
    target_channels,
    container,
    target_path,
    state,
    dispatch_generation,
    claim_lease_id,
    claim_token,
    claim_expires_at,
    staging_path,
    expected_size_bytes,
    expected_checksum,
    worker_result,
    artifact_handle_id,
    artifact_location_id,
    verification_id,
    commit_record_id,
    probe_worker_id,
    probe_payload,
    result_file_asset_id,
    result_file_version_id,
    result_file_location_id,
    result_media_snapshot_id,
    created_at,
    finished_at
FROM audio_synthesis_operations_old;

DROP TABLE audio_synthesis_operations_old;

CREATE INDEX audio_synthesis_operations_by_source
    ON audio_synthesis_operations (source_file_version_id, id);
CREATE INDEX audio_synthesis_operations_by_state
    ON audio_synthesis_operations (state, id);

PRAGMA legacy_alter_table = OFF;
PRAGMA foreign_keys = ON;

PRAGMA foreign_key_check;

BEGIN;
