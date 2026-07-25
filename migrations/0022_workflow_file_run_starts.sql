-- Immutable per-file starting cursors for phase-barrier runs (ADR 0037).
-- Phase completion remains recorded by workflow_file_phase_summaries; this
-- table records only the authoritative version and phase ordinal at job open.

CREATE TABLE workflow_file_run_starts (
    job_id                   INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    branch_id                TEXT NOT NULL,
    starting_file_version_id INTEGER NOT NULL REFERENCES file_versions(id) ON DELETE RESTRICT,
    starting_phase_ordinal   INTEGER NOT NULL CHECK (starting_phase_ordinal >= 0),
    PRIMARY KEY (job_id, branch_id)
) STRICT;
