CREATE TABLE scan_sessions (
    id                          INTEGER PRIMARY KEY,
    storage_root_id             INTEGER NOT NULL
        REFERENCES library_roots(id) ON DELETE RESTRICT,
    root_epoch                  INTEGER NOT NULL CHECK (root_epoch >= 0),
    owner_node_id               INTEGER NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
    owner_incarnation_id        TEXT
        REFERENCES node_incarnations(incarnation_id) ON DELETE RESTRICT,
    status                      TEXT NOT NULL
        CHECK (status IN ('requested', 'running', 'succeeded', 'failed', 'cancelled', 'stale')),
    next_sequence               INTEGER NOT NULL DEFAULT 0 CHECK (next_sequence >= 0),
    batch_count                 INTEGER NOT NULL DEFAULT 0 CHECK (batch_count >= 0),
    observation_count           INTEGER NOT NULL DEFAULT 0 CHECK (observation_count >= 0),
    idle_timeout_seconds        INTEGER NOT NULL CHECK (idle_timeout_seconds BETWEEN 1 AND 86400),
    progress_deadline_at        TEXT NOT NULL,
    location_high_watermark_id  INTEGER
        REFERENCES file_locations(id) ON DELETE RESTRICT,
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
    retired_location_count      INTEGER NOT NULL DEFAULT 0 CHECK (retired_location_count >= 0),
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
    request_hash                TEXT NOT NULL
        CHECK (length(request_hash) = 64 AND request_hash NOT GLOB '*[^0-9a-f]*'),
    observation_count           INTEGER NOT NULL CHECK (observation_count BETWEEN 1 AND 1000),
    accepted_at                 TEXT NOT NULL,
    cumulative_observation_count INTEGER NOT NULL CHECK (cumulative_observation_count >= 0),
    PRIMARY KEY (scan_session_id, sequence)
) STRICT;

CREATE TABLE scan_observations (
    scan_session_id             INTEGER NOT NULL,
    batch_sequence              INTEGER NOT NULL CHECK (batch_sequence >= 0),
    ordinal                     INTEGER NOT NULL CHECK (ordinal >= 0),
    provider_relative_locator   TEXT NOT NULL CHECK (
        length(CAST(provider_relative_locator AS BLOB)) BETWEEN 1 AND 4096
        AND instr(provider_relative_locator, char(0)) = 0
        AND instr(provider_relative_locator, '\\') = 0
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
