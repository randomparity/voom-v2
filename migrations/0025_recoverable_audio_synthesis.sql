-- Issue #333: recoverable audio-synthesis publication and stream lineage.

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
        CHECK (state IN ('planned','staged','prepared','recovery_required','committed')),
    dispatch_generation         INTEGER NOT NULL DEFAULT 0
        CHECK (dispatch_generation >= 0),
    claim_lease_id              INTEGER REFERENCES leases(id) ON DELETE RESTRICT,
    claim_token                 TEXT,
    claim_expires_at            TEXT,
    staging_path                TEXT UNIQUE,
    temp_path                   TEXT UNIQUE,
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
    result_file_asset_id        INTEGER UNIQUE
        REFERENCES file_assets(id) ON DELETE RESTRICT,
    result_file_version_id      INTEGER UNIQUE
        REFERENCES file_versions(id) ON DELETE RESTRICT,
    result_file_location_id     INTEGER UNIQUE
        REFERENCES file_locations(id) ON DELETE RESTRICT,
    result_media_snapshot_id    INTEGER UNIQUE
        REFERENCES media_snapshots(id) ON DELETE RESTRICT,
    recovery_failure_class      TEXT,
    recovery_error_code         TEXT,
    recovery_message            TEXT,
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
    ),
    CHECK (
        (state = 'recovery_required'
         AND recovery_failure_class IS NOT NULL
         AND recovery_error_code IS NOT NULL
         AND recovery_message IS NOT NULL)
        OR
        (state != 'recovery_required'
         AND recovery_failure_class IS NULL
         AND recovery_error_code IS NULL
         AND recovery_message IS NULL)
    )
) STRICT;

CREATE INDEX audio_synthesis_operations_by_source
    ON audio_synthesis_operations (source_file_version_id, id);
CREATE INDEX audio_synthesis_operations_by_state
    ON audio_synthesis_operations (state, id);

CREATE TABLE audio_synthesis_companions (
    id                           INTEGER PRIMARY KEY,
    operation_id                 INTEGER NOT NULL
        REFERENCES audio_synthesis_operations(id) ON DELETE CASCADE,
    ordinal                      INTEGER NOT NULL CHECK (ordinal >= 0),
    companion_id                 TEXT NOT NULL CHECK (length(companion_id) > 0),
    source_snapshot_stream_id    TEXT NOT NULL
        CHECK (length(source_snapshot_stream_id) > 0),
    source_provider_stream_index INTEGER NOT NULL
        CHECK (source_provider_stream_index >= 0),
    result_snapshot_stream_id    TEXT NOT NULL
        CHECK (length(result_snapshot_stream_id) > 0),
    result_provider_stream_index INTEGER CHECK (result_provider_stream_index >= 0),
    codec                        TEXT,
    channels                     INTEGER CHECK (channels > 0),
    language                     TEXT,
    title                        TEXT,
    disposition_default          INTEGER CHECK (disposition_default IN (0, 1)),
    disposition_forced           INTEGER CHECK (disposition_forced IN (0, 1)),
    disposition_commentary       INTEGER CHECK (disposition_commentary IN (0, 1)),
    result_facts                 TEXT CHECK (result_facts IS NULL OR json_valid(result_facts)),
    UNIQUE (operation_id, ordinal),
    UNIQUE (operation_id, companion_id),
    UNIQUE (operation_id, source_snapshot_stream_id),
    UNIQUE (operation_id, source_provider_stream_index),
    UNIQUE (operation_id, result_snapshot_stream_id),
    UNIQUE (operation_id, result_provider_stream_index),
    CHECK (companion_id = result_snapshot_stream_id)
) STRICT;

CREATE TABLE audio_synthesis_dispatch_attempts (
    id                     INTEGER PRIMARY KEY,
    operation_id           INTEGER NOT NULL
        REFERENCES audio_synthesis_operations(id) ON DELETE CASCADE,
    generation             INTEGER NOT NULL CHECK (generation >= 0),
    worker_id              INTEGER NOT NULL REFERENCES workers(id) ON DELETE RESTRICT,
    worker_epoch           INTEGER NOT NULL CHECK (worker_epoch >= 0),
    idempotency_key        TEXT NOT NULL UNIQUE CHECK (length(idempotency_key) > 0),
    attempt_directory      TEXT NOT NULL UNIQUE CHECK (length(attempt_directory) > 0),
    staging_path           TEXT NOT NULL UNIQUE CHECK (length(staging_path) > 0),
    status                 TEXT NOT NULL
        CHECK (status IN ('active','terminal','quarantined','quiesced')),
    evidence_kind          TEXT
        CHECK (evidence_kind IS NULL OR evidence_kind IN
               ('terminal_response','operator_acknowledgement')),
    evidence_at            TEXT,
    acknowledged_by        TEXT,
    created_at             TEXT NOT NULL,
    CHECK (
        (status IN ('active','quarantined')
         AND evidence_kind IS NULL AND evidence_at IS NULL
         AND acknowledged_by IS NULL)
        OR
        (status = 'terminal'
         AND evidence_kind = 'terminal_response'
         AND evidence_at IS NOT NULL
         AND acknowledged_by IS NULL)
        OR
        (status = 'quiesced'
         AND evidence_kind = 'operator_acknowledgement'
         AND evidence_at IS NOT NULL
         AND length(acknowledged_by) > 0)
    ),
    UNIQUE (operation_id, generation)
) STRICT;

CREATE TABLE audio_synthesis_stream_lineage (
    id                           INTEGER PRIMARY KEY,
    companion_id                 INTEGER NOT NULL UNIQUE
        REFERENCES audio_synthesis_companions(id) ON DELETE RESTRICT,
    source_file_version_id       INTEGER NOT NULL
        REFERENCES file_versions(id) ON DELETE RESTRICT,
    source_media_snapshot_id     INTEGER NOT NULL
        REFERENCES media_snapshots(id) ON DELETE RESTRICT,
    source_snapshot_stream_id    TEXT NOT NULL
        CHECK (length(source_snapshot_stream_id) > 0),
    source_provider_stream_index INTEGER NOT NULL
        CHECK (source_provider_stream_index >= 0),
    result_file_version_id       INTEGER NOT NULL
        REFERENCES file_versions(id) ON DELETE RESTRICT,
    result_media_snapshot_id     INTEGER NOT NULL
        REFERENCES media_snapshots(id) ON DELETE RESTRICT,
    result_snapshot_stream_id    TEXT NOT NULL
        CHECK (length(result_snapshot_stream_id) > 0),
    result_provider_stream_index INTEGER NOT NULL
        CHECK (result_provider_stream_index >= 0),
    codec                        TEXT NOT NULL CHECK (length(codec) > 0),
    channels                     INTEGER NOT NULL CHECK (channels > 0),
    language                     TEXT,
    title                        TEXT,
    disposition_default          INTEGER NOT NULL CHECK (disposition_default IN (0, 1)),
    disposition_forced           INTEGER NOT NULL CHECK (disposition_forced IN (0, 1)),
    disposition_commentary       INTEGER NOT NULL
        CHECK (disposition_commentary IN (0, 1)),
    recorded_at                  TEXT NOT NULL,
    UNIQUE (result_media_snapshot_id, result_snapshot_stream_id),
    UNIQUE (result_media_snapshot_id, result_provider_stream_index)
) STRICT;

CREATE INDEX audio_synthesis_lineage_by_source
    ON audio_synthesis_stream_lineage (
        source_file_version_id,
        source_media_snapshot_id,
        source_provider_stream_index
    );
