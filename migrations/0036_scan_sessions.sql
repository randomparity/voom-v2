CREATE UNIQUE INDEX file_locations_scan_watermark_root
ON file_locations(id, storage_root_id);

CREATE UNIQUE INDEX node_incarnations_owner_binding
ON node_incarnations(incarnation_id, node_id);

CREATE TABLE scan_sessions (
    id                          INTEGER PRIMARY KEY,
    storage_root_id             INTEGER NOT NULL
        REFERENCES library_roots(id) ON DELETE RESTRICT,
    root_epoch                  INTEGER NOT NULL CHECK (root_epoch >= 0),
    owner_node_id               INTEGER NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
    owner_incarnation_id        TEXT,
    status                      TEXT NOT NULL
        CHECK (status IN ('requested', 'running', 'succeeded', 'failed', 'cancelled', 'stale')),
    next_sequence               INTEGER NOT NULL DEFAULT 0 CHECK (next_sequence >= 0),
    batch_count                 INTEGER NOT NULL DEFAULT 0 CHECK (batch_count >= 0),
    observation_count           INTEGER NOT NULL DEFAULT 0
        CHECK (observation_count BETWEEN 0 AND 100000),
    idle_timeout_seconds        INTEGER NOT NULL CHECK (idle_timeout_seconds BETWEEN 1 AND 86400),
    progress_deadline_at        TEXT NOT NULL,
    location_high_watermark_id  INTEGER,
    requested_at                TEXT NOT NULL,
    started_at                  TEXT,
    terminal_at                 TEXT,
    terminal_reason             TEXT CHECK (
        terminal_reason IS NULL
        OR (
            length(CAST(terminal_reason AS BLOB)) BETWEEN 1 AND 1024
            AND instr(terminal_reason, char(0)) = 0
            AND length(trim(
                terminal_reason,
                char(9) || char(10) || char(11) || char(12) || char(13) || char(32)
            )) > 0
        )
    ),
    retired_location_count      INTEGER NOT NULL DEFAULT 0 CHECK (
        retired_location_count >= 0
        AND (status = 'succeeded' OR retired_location_count = 0)
    ),
    FOREIGN KEY (location_high_watermark_id, storage_root_id)
        REFERENCES file_locations(id, storage_root_id) ON DELETE RESTRICT,
    FOREIGN KEY (owner_incarnation_id, owner_node_id)
        REFERENCES node_incarnations(incarnation_id, node_id) ON DELETE RESTRICT,
    CHECK (
        batch_count = next_sequence
        AND (
            (batch_count = 0 AND observation_count = 0)
            OR (
                batch_count > 0
                AND observation_count >= batch_count
                AND (
                    observation_count / batch_count < 1000
                    OR (
                        observation_count / batch_count = 1000
                        AND observation_count % batch_count = 0
                    )
                )
            )
        )
    ),
    CHECK (
        (status = 'requested'
         AND owner_incarnation_id IS NULL
         AND started_at IS NULL
         AND location_high_watermark_id IS NULL
         AND terminal_at IS NULL
         AND terminal_reason IS NULL)
        OR
        (status = 'running'
         AND owner_incarnation_id IS NOT NULL
         AND started_at IS NOT NULL
         AND terminal_at IS NULL
         AND terminal_reason IS NULL)
        OR
        (status = 'succeeded'
         AND owner_incarnation_id IS NOT NULL
         AND started_at IS NOT NULL
         AND terminal_at IS NOT NULL
         AND terminal_reason IS NULL)
        OR
        (status IN ('failed', 'cancelled', 'stale')
         AND terminal_at IS NOT NULL
         AND terminal_reason IS NOT NULL
         AND (
             (owner_incarnation_id IS NULL
              AND started_at IS NULL
              AND location_high_watermark_id IS NULL)
             OR (owner_incarnation_id IS NOT NULL AND started_at IS NOT NULL)
         ))
    )
) STRICT;

CREATE UNIQUE INDEX scan_sessions_one_active_per_root
ON scan_sessions(storage_root_id)
WHERE status IN ('requested', 'running');

CREATE INDEX scan_sessions_active_deadline
ON scan_sessions(status, progress_deadline_at, id)
WHERE status IN ('requested', 'running');

CREATE INDEX scan_sessions_by_root_requested_at
ON scan_sessions(storage_root_id, requested_at DESC, id DESC);

CREATE TABLE scan_observation_batches (
    scan_session_id             INTEGER NOT NULL
        REFERENCES scan_sessions(id) ON DELETE RESTRICT,
    sequence                    INTEGER NOT NULL CHECK (sequence >= 0),
    previous_sequence           INTEGER,
    request_hash                TEXT NOT NULL
        CHECK (length(request_hash) = 64 AND request_hash NOT GLOB '*[^0-9a-f]*'),
    observation_count           INTEGER NOT NULL CHECK (observation_count BETWEEN 1 AND 1000),
    accepted_at                 TEXT NOT NULL,
    cumulative_observation_count INTEGER NOT NULL
        CHECK (cumulative_observation_count BETWEEN observation_count AND 100000),
    PRIMARY KEY (scan_session_id, sequence),
    FOREIGN KEY (scan_session_id, previous_sequence)
        REFERENCES scan_observation_batches(scan_session_id, sequence) ON DELETE RESTRICT,
    CHECK (
        (sequence = 0 AND previous_sequence IS NULL)
        OR (sequence > 0 AND previous_sequence = sequence - 1)
    )
) STRICT;

CREATE TRIGGER scan_observation_batches_validate_insert
BEFORE INSERT ON scan_observation_batches
BEGIN
    SELECT CASE
        WHEN NEW.sequence > 0 AND NOT EXISTS (
            SELECT 1 FROM scan_observation_batches AS predecessor
            WHERE predecessor.scan_session_id = NEW.scan_session_id
              AND predecessor.sequence = NEW.previous_sequence
        )
        THEN RAISE(ABORT, 'scan observation batch predecessor missing')
    END;
    SELECT CASE
        WHEN NEW.sequence = 0
             AND NEW.cumulative_observation_count != NEW.observation_count
        THEN RAISE(ABORT, 'scan observation batch cumulative count mismatch')
        WHEN NEW.sequence > 0 AND NEW.cumulative_observation_count != (
            SELECT predecessor.cumulative_observation_count + NEW.observation_count
            FROM scan_observation_batches AS predecessor
            WHERE predecessor.scan_session_id = NEW.scan_session_id
              AND predecessor.sequence = NEW.previous_sequence
        )
        THEN RAISE(ABORT, 'scan observation batch cumulative count mismatch')
    END;
END;

CREATE TRIGGER scan_observation_batches_no_update
BEFORE UPDATE ON scan_observation_batches
BEGIN
    SELECT RAISE(ABORT, 'scan observation batch rows are immutable');
END;

CREATE TRIGGER scan_observation_batches_no_delete
BEFORE DELETE ON scan_observation_batches
BEGIN
    SELECT RAISE(ABORT, 'scan observation batch rows are immutable');
END;

CREATE TABLE scan_observations (
    scan_session_id             INTEGER NOT NULL,
    batch_sequence              INTEGER NOT NULL CHECK (batch_sequence >= 0),
    ordinal                     INTEGER NOT NULL CHECK (ordinal >= 0),
    provider_relative_locator   TEXT NOT NULL CHECK (
        length(CAST(provider_relative_locator AS BLOB)) BETWEEN 1 AND 4096
        AND instr(provider_relative_locator, char(0)) = 0
        AND instr(provider_relative_locator, char(92)) = 0
        AND substr(provider_relative_locator, 1, 1) != '/'
        AND substr(provider_relative_locator, -1, 1) != '/'
        AND instr(provider_relative_locator, '//') = 0
        AND instr('/' || provider_relative_locator || '/', '/./') = 0
        AND instr('/' || provider_relative_locator || '/', '/../') = 0
    ),
    provider_object_identity    TEXT NOT NULL CHECK (
        length(CAST(provider_object_identity AS BLOB)) BETWEEN 1 AND 4096
        AND instr(provider_object_identity, char(0)) = 0
    ),
    size_bytes                  INTEGER NOT NULL CHECK (size_bytes >= 0),
    modified_at                 TEXT NOT NULL,
    stability_started_at        TEXT NOT NULL,
    stability_confirmed_at      TEXT NOT NULL CHECK (stability_confirmed_at >= stability_started_at),
    PRIMARY KEY (scan_session_id, batch_sequence, ordinal),
    FOREIGN KEY (scan_session_id, batch_sequence)
        REFERENCES scan_observation_batches(scan_session_id, sequence) ON DELETE RESTRICT
) STRICT;

CREATE UNIQUE INDEX scan_observations_one_locator_per_session
ON scan_observations(scan_session_id, provider_relative_locator);

ALTER TABLE library_roots ADD COLUMN last_scan_session_id INTEGER
    REFERENCES scan_sessions(id) ON DELETE RESTRICT;

ALTER TABLE file_locations ADD COLUMN retired_by_scan_session_id INTEGER
    REFERENCES scan_sessions(id) ON DELETE RESTRICT
    CHECK (retired_by_scan_session_id IS NULL OR retired_at IS NOT NULL);

CREATE INDEX file_locations_by_retired_scan_session
ON file_locations(retired_by_scan_session_id, id);
