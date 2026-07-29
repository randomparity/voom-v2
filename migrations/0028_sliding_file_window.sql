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
    state              TEXT NOT NULL CHECK (state IN ('pending', 'active', 'terminal')),
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
        OR (state = 'terminal' AND admitted_at IS NOT NULL AND terminal_at IS NOT NULL)
    )
) STRICT;

CREATE INDEX workflow_file_progress_admission
    ON workflow_file_progress (job_id, state, input_ordinal);
