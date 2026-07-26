-- Carry committed/skipped phase outcomes into each phase-barrier run (ADR 0038).
-- The projection is self-contained for resume: the coordinator never needs to
-- walk an unbounded chain of earlier jobs.

CREATE TABLE workflow_file_run_history (
    job_id        INTEGER NOT NULL,
    branch_id     TEXT NOT NULL,
    phase_ordinal INTEGER NOT NULL CHECK (phase_ordinal >= 0),
    outcome       TEXT NOT NULL CHECK (outcome IN ('committed', 'skipped')),
    PRIMARY KEY (job_id, branch_id, phase_ordinal),
    FOREIGN KEY (job_id, branch_id)
        REFERENCES workflow_file_run_starts (job_id, branch_id)
        ON DELETE CASCADE
) STRICT;
