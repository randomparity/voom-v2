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
