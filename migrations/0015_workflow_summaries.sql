-- Sprint 16 -- durable three-grain workflow summaries (job / phase / file-phase).
-- Per-phase compliance reports fold into the per-phase row; there is no reports table.
--
-- The three grains key off jobs(id) directly, not off each other: per-(file, phase)
-- and per-phase child rows are written incrementally as each file's phase artifact
-- commits, before the job-level parent's final counters exist (ADR-0006). A
-- child FK to the parent would force the parent to exist first and break that
-- incremental-write invariant.

CREATE TABLE workflow_summaries (
    job_id                       INTEGER PRIMARY KEY REFERENCES jobs(id) ON DELETE CASCADE,
    branch_count                 INTEGER NOT NULL CHECK (branch_count >= 0),
    ticket_count                 INTEGER NOT NULL CHECK (ticket_count >= 0),
    dispatch_count               INTEGER NOT NULL CHECK (dispatch_count >= 0),
    retry_count                  INTEGER NOT NULL CHECK (retry_count >= 0),
    failure_count                INTEGER NOT NULL CHECK (failure_count >= 0),
    peak_active_workflow_leases  INTEGER NOT NULL CHECK (peak_active_workflow_leases >= 0),
    elapsed_ns                   INTEGER NOT NULL CHECK (elapsed_ns >= 0),
    per_operation                TEXT NOT NULL CHECK (json_valid(per_operation)),
    created_at                   TEXT NOT NULL
) STRICT;

CREATE TABLE workflow_phase_summaries (
    id            INTEGER PRIMARY KEY,
    job_id        INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    phase_ordinal INTEGER NOT NULL CHECK (phase_ordinal >= 0),
    phase_name    TEXT NOT NULL,
    report_id     TEXT,
    report        TEXT CHECK (report IS NULL OR json_valid(report)),
    outcome       TEXT NOT NULL
        CHECK (outcome IN ('completed', 'partially-committed', 'skipped', 'blocked')),
    created_at    TEXT NOT NULL,
    -- report_id and report live or die together (skipped/blocked phases have neither).
    CHECK ((report_id IS NULL AND report IS NULL)
        OR (report_id IS NOT NULL AND report IS NOT NULL))
) STRICT;

CREATE UNIQUE INDEX workflow_phase_summaries_key
    ON workflow_phase_summaries (job_id, phase_ordinal);

CREATE TABLE workflow_file_phase_summaries (
    id                         INTEGER PRIMARY KEY,
    job_id                     INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    phase_ordinal              INTEGER NOT NULL CHECK (phase_ordinal >= 0),
    branch_id                  TEXT NOT NULL,
    ticket_ids                 TEXT NOT NULL CHECK (json_valid(ticket_ids)),
    produced_file_version_id   INTEGER REFERENCES file_versions(id)    ON DELETE RESTRICT,
    produced_file_location_id  INTEGER REFERENCES file_locations(id)   ON DELETE RESTRICT,
    artifact_handle_id         INTEGER REFERENCES artifact_handles(id) ON DELETE RESTRICT,
    reprobe_snapshot_id        INTEGER REFERENCES media_snapshots(id)  ON DELETE RESTRICT,
    outcome                    TEXT NOT NULL
        CHECK (outcome IN ('committed', 'skipped', 'blocked')),
    created_at                 TEXT NOT NULL,
    -- A committed file carries its produced version, location, and re-probe
    -- snapshot (the row is written only after commit AND re-probe; ADR-0006).
    -- A non-advancing file (skipped/blocked) carries none.
    CHECK (
        (outcome = 'committed'
            AND produced_file_version_id IS NOT NULL
            AND produced_file_location_id IS NOT NULL
            AND reprobe_snapshot_id IS NOT NULL)
        OR (outcome IN ('skipped', 'blocked')
            AND produced_file_version_id IS NULL
            AND produced_file_location_id IS NULL
            AND artifact_handle_id IS NULL
            AND reprobe_snapshot_id IS NULL)
    )
) STRICT;

CREATE UNIQUE INDEX workflow_file_phase_summaries_key
    ON workflow_file_phase_summaries (job_id, phase_ordinal, branch_id);
