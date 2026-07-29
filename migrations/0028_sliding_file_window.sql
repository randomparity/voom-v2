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
    ON workflow_file_progress (job_id, state, input_ordinal);

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
