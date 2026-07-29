-- Durable bounded file admission and per-file execution cursors (#401, ADR 0048).

CREATE TABLE workflow_file_windows (
    job_id              INTEGER PRIMARY KEY REFERENCES jobs(id) ON DELETE CASCADE,
    max_in_flight_files INTEGER NOT NULL CHECK (max_in_flight_files > 0),
    created_at          TEXT NOT NULL
) STRICT;

CREATE TABLE workflow_file_progress (
    job_id             INTEGER NOT NULL,
    branch_id          TEXT NOT NULL,
    input_ordinal      INTEGER NOT NULL CHECK (input_ordinal >= 0),
    admission_tier     INTEGER NOT NULL CHECK (admission_tier IN (0, 1)),
    state              TEXT NOT NULL
        CHECK (state IN ('pending', 'active', 'terminalizing', 'terminal')),
    next_phase_ordinal INTEGER NOT NULL CHECK (next_phase_ordinal >= 0),
    admitted_at        TEXT,
    terminal_at        TEXT,
    PRIMARY KEY (job_id, branch_id),
    UNIQUE (job_id, input_ordinal),
    FOREIGN KEY (job_id, branch_id)
        REFERENCES workflow_file_run_starts (job_id, branch_id)
        ON DELETE CASCADE,
    CHECK (
        (state = 'pending' AND admitted_at IS NULL AND terminal_at IS NULL)
        OR (state = 'active' AND admitted_at IS NOT NULL AND terminal_at IS NULL)
        OR (state = 'terminalizing' AND admitted_at IS NOT NULL AND terminal_at IS NULL)
        OR (state = 'terminal' AND admitted_at IS NOT NULL AND terminal_at IS NOT NULL)
    )
) STRICT;

CREATE INDEX workflow_file_progress_admission
    ON workflow_file_progress (job_id, state, admission_tier, input_ordinal);

CREATE TABLE workflow_file_phase_entries (
    job_id            INTEGER NOT NULL,
    phase_ordinal     INTEGER NOT NULL CHECK (phase_ordinal >= 0),
    branch_id         TEXT NOT NULL,
    media_snapshot_id INTEGER NOT NULL
        REFERENCES media_snapshots(id) ON DELETE RESTRICT,
    gate_admitted     INTEGER NOT NULL CHECK (gate_admitted IN (0, 1)),
    created_at        TEXT NOT NULL,
    PRIMARY KEY (job_id, phase_ordinal, branch_id),
    FOREIGN KEY (job_id, branch_id)
        REFERENCES workflow_file_run_starts (job_id, branch_id)
        ON DELETE CASCADE
) STRICT;

CREATE TABLE workflow_file_run_history_new (
    job_id        INTEGER NOT NULL,
    branch_id     TEXT NOT NULL,
    phase_ordinal INTEGER NOT NULL CHECK (phase_ordinal >= 0),
    outcome       TEXT NOT NULL
        CHECK (outcome IN ('committed', 'verified', 'skipped', 'blocked')),
    PRIMARY KEY (job_id, branch_id, phase_ordinal),
    FOREIGN KEY (job_id, branch_id)
        REFERENCES workflow_file_run_starts (job_id, branch_id)
        ON DELETE CASCADE
) STRICT;

INSERT INTO workflow_file_run_history_new (job_id, branch_id, phase_ordinal, outcome)
SELECT job_id, branch_id, phase_ordinal, outcome
FROM workflow_file_run_history;

DROP TABLE workflow_file_run_history;
ALTER TABLE workflow_file_run_history_new RENAME TO workflow_file_run_history;

-- Pre-window jobs treated every selected file as admitted. Project their
-- durable phase tails into an interrupted tier so an upgraded binary can
-- resume them before untouched work instead of stranding existing commits.
INSERT INTO workflow_file_windows (job_id, max_in_flight_files, created_at)
SELECT DISTINCT starts.job_id, 4, jobs.created_at
FROM workflow_file_run_starts AS starts
JOIN jobs ON jobs.id = starts.job_id;

WITH legacy_progress AS (
    SELECT
        starts.job_id,
        starts.branch_id,
        ROW_NUMBER() OVER (
            PARTITION BY starts.job_id ORDER BY starts.branch_id
        ) - 1 AS input_ordinal,
        COALESCE(
            (
                SELECT MAX(rows.phase_ordinal) + 1
                FROM workflow_file_phase_summaries AS rows
                WHERE rows.job_id = starts.job_id
                  AND rows.branch_id = starts.branch_id
            ),
            starts.starting_phase_ordinal
        ) AS next_phase_ordinal,
        jobs.state AS job_state,
        jobs.created_at,
        jobs.updated_at
    FROM workflow_file_run_starts AS starts
    JOIN jobs ON jobs.id = starts.job_id
)
INSERT INTO workflow_file_progress (
    job_id,
    branch_id,
    input_ordinal,
    admission_tier,
    state,
    next_phase_ordinal,
    admitted_at,
    terminal_at
)
SELECT
    job_id,
    branch_id,
    input_ordinal,
    0,
    CASE WHEN job_state = 'succeeded' THEN 'terminal' ELSE 'active' END,
    next_phase_ordinal,
    created_at,
    CASE WHEN job_state = 'succeeded' THEN updated_at ELSE NULL END
FROM legacy_progress;
