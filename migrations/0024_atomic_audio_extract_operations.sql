-- Issue #337: durable ordered audio-extraction publication and lineage.

CREATE TABLE audio_extract_operations (
    id                          INTEGER PRIMARY KEY,
    operation_key               TEXT NOT NULL UNIQUE CHECK (length(operation_key) > 0),
    operation_id                TEXT CHECK (operation_id IS NULL OR length(operation_id) > 0),
    source_file_version_id      INTEGER NOT NULL
        REFERENCES file_versions(id) ON DELETE RESTRICT,
    source_bundle_id            INTEGER NOT NULL
        REFERENCES asset_bundles(id) ON DELETE RESTRICT,
    source_media_snapshot_id    INTEGER NOT NULL
        REFERENCES media_snapshots(id) ON DELETE RESTRICT,
    state                       TEXT NOT NULL
        CHECK (state IN ('planned','staged','prepared','recovery_required','committed')),
    dispatch_generation         INTEGER NOT NULL DEFAULT 0
        CHECK (dispatch_generation >= 0),
    claim_lease_id              INTEGER REFERENCES leases(id) ON DELETE RESTRICT,
    claim_token                 TEXT,
    claim_expires_at            TEXT,
    recovery_failure_class      TEXT,
    recovery_error_code         TEXT,
    recovery_message            TEXT,
    worker_result               TEXT CHECK (worker_result IS NULL OR json_valid(worker_result)),
    created_at                  TEXT NOT NULL,
    finished_at                 TEXT,
    CHECK (
        (claim_lease_id IS NULL AND claim_token IS NULL AND claim_expires_at IS NULL)
        OR
        (claim_lease_id IS NOT NULL AND length(claim_token) > 0
         AND claim_expires_at IS NOT NULL)
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

CREATE INDEX audio_extract_operations_by_source
    ON audio_extract_operations (source_file_version_id, id);
CREATE INDEX audio_extract_operations_by_state
    ON audio_extract_operations (state, id);

CREATE TABLE audio_extract_operation_outputs (
    id                           INTEGER PRIMARY KEY,
    operation_id                 INTEGER NOT NULL
        REFERENCES audio_extract_operations(id) ON DELETE CASCADE,
    ordinal                      INTEGER NOT NULL CHECK (ordinal >= 0),
    output_id                    TEXT,
    source_snapshot_stream_id    TEXT NOT NULL
        CHECK (length(source_snapshot_stream_id) > 0),
    source_provider_stream_index INTEGER NOT NULL
        CHECK (source_provider_stream_index >= 0),
    bundle_role                  TEXT NOT NULL
        CHECK (bundle_role IN ('commentary_audio','external_audio')),
    target_path                  TEXT NOT NULL UNIQUE CHECK (length(target_path) > 0),
    staging_path                 TEXT,
    temp_path                    TEXT,
    expected_size_bytes          INTEGER CHECK (expected_size_bytes >= 0),
    expected_checksum            TEXT,
    artifact_handle_id           INTEGER UNIQUE
        REFERENCES artifact_handles(id) ON DELETE RESTRICT,
    artifact_location_id         INTEGER UNIQUE
        REFERENCES artifact_locations(id) ON DELETE RESTRICT,
    verification_id              INTEGER UNIQUE
        REFERENCES artifact_verifications(id) ON DELETE RESTRICT,
    commit_record_id             INTEGER UNIQUE
        REFERENCES artifact_commit_records(id) ON DELETE RESTRICT,
    probe_worker_id              INTEGER REFERENCES workers(id) ON DELETE RESTRICT,
    probe_payload                TEXT CHECK (probe_payload IS NULL OR json_valid(probe_payload)),
    result_file_asset_id         INTEGER UNIQUE
        REFERENCES file_assets(id) ON DELETE RESTRICT,
    result_file_version_id       INTEGER UNIQUE
        REFERENCES file_versions(id) ON DELETE RESTRICT,
    result_file_location_id      INTEGER UNIQUE
        REFERENCES file_locations(id) ON DELETE RESTRICT,
    result_media_snapshot_id     INTEGER UNIQUE
        REFERENCES media_snapshots(id) ON DELETE RESTRICT,
    bundle_member_id             INTEGER UNIQUE
        REFERENCES asset_bundle_members(id) ON DELETE RESTRICT,
    result_facts                 TEXT CHECK (result_facts IS NULL OR json_valid(result_facts)),
    UNIQUE (operation_id, ordinal),
    UNIQUE (operation_id, output_id),
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
    CHECK (
        (probe_worker_id IS NULL AND probe_payload IS NULL)
        OR
        (probe_worker_id IS NOT NULL AND probe_payload IS NOT NULL)
    ),
    CHECK (commit_record_id IS NULL OR verification_id IS NOT NULL),
    CHECK (
        (result_file_asset_id IS NULL
         AND result_file_version_id IS NULL
         AND result_file_location_id IS NULL
         AND result_media_snapshot_id IS NULL
         AND bundle_member_id IS NULL)
        OR
        (result_file_asset_id IS NOT NULL
         AND result_file_version_id IS NOT NULL
         AND result_file_location_id IS NOT NULL
         AND result_media_snapshot_id IS NOT NULL
         AND bundle_member_id IS NOT NULL)
    )
) STRICT;

CREATE INDEX audio_extract_outputs_by_operation
    ON audio_extract_operation_outputs (operation_id, ordinal);

CREATE TABLE audio_extract_dispatch_attempts (
    id                     INTEGER PRIMARY KEY,
    operation_id           INTEGER NOT NULL
        REFERENCES audio_extract_operations(id) ON DELETE CASCADE,
    generation             INTEGER NOT NULL CHECK (generation >= 0),
    worker_id              INTEGER NOT NULL REFERENCES workers(id) ON DELETE RESTRICT,
    worker_epoch           INTEGER NOT NULL CHECK (worker_epoch >= 0),
    idempotency_key        TEXT NOT NULL UNIQUE CHECK (length(idempotency_key) > 0),
    attempt_directory      TEXT NOT NULL CHECK (length(attempt_directory) > 0),
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

CREATE TABLE audio_extract_dispatch_attempt_paths (
    attempt_id   INTEGER NOT NULL
        REFERENCES audio_extract_dispatch_attempts(id) ON DELETE CASCADE,
    ordinal      INTEGER NOT NULL CHECK (ordinal >= 0),
    path         TEXT NOT NULL UNIQUE CHECK (length(path) > 0),
    PRIMARY KEY (attempt_id, ordinal)
) STRICT;

CREATE TABLE audio_extract_output_lineage (
    id                           INTEGER PRIMARY KEY,
    operation_output_id          INTEGER NOT NULL UNIQUE
        REFERENCES audio_extract_operation_outputs(id) ON DELETE RESTRICT,
    source_file_version_id       INTEGER NOT NULL
        REFERENCES file_versions(id) ON DELETE RESTRICT,
    source_media_snapshot_id     INTEGER NOT NULL
        REFERENCES media_snapshots(id) ON DELETE RESTRICT,
    source_snapshot_stream_id    TEXT NOT NULL
        CHECK (length(source_snapshot_stream_id) > 0),
    source_provider_stream_index INTEGER NOT NULL
        CHECK (source_provider_stream_index >= 0),
    result_file_version_id       INTEGER NOT NULL UNIQUE
        REFERENCES file_versions(id) ON DELETE RESTRICT,
    recorded_at                  TEXT NOT NULL
) STRICT;

CREATE INDEX audio_extract_lineage_by_source
    ON audio_extract_output_lineage (
        source_file_version_id,
        source_media_snapshot_id,
        source_provider_stream_index
    );
