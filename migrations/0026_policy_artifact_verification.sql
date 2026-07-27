-- Policy-driven artifact verification (#334).
--
-- Artifact evidence is correlated to the workflow ticket and lease that
-- produced it. Existing staged-commit verification rows remain unowned.

ALTER TABLE artifact_verifications
    ADD COLUMN workflow_ticket_id INTEGER REFERENCES tickets(id) ON DELETE RESTRICT;

ALTER TABLE artifact_verifications
    ADD COLUMN workflow_lease_id INTEGER REFERENCES leases(id) ON DELETE RESTRICT;

CREATE UNIQUE INDEX artifact_verifications_by_workflow_lease
    ON artifact_verifications (workflow_lease_id)
    WHERE workflow_lease_id IS NOT NULL;

CREATE INDEX artifact_verifications_by_workflow_ticket
    ON artifact_verifications (workflow_ticket_id, id)
    WHERE workflow_ticket_id IS NOT NULL;

-- A successful verification is durable read-only work. It carries the
-- unchanged active file refs plus the exact artifact and verification evidence.

CREATE TABLE workflow_file_phase_summaries_new (
    id                         INTEGER PRIMARY KEY,
    job_id                     INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    phase_ordinal              INTEGER NOT NULL CHECK (phase_ordinal >= 0),
    branch_id                  TEXT NOT NULL,
    ticket_ids                 TEXT NOT NULL CHECK (json_valid(ticket_ids)),
    produced_file_version_id   INTEGER REFERENCES file_versions(id) ON DELETE RESTRICT,
    produced_file_location_id  INTEGER REFERENCES file_locations(id) ON DELETE RESTRICT,
    artifact_handle_id         INTEGER REFERENCES artifact_handles(id) ON DELETE RESTRICT,
    artifact_verification_id   INTEGER REFERENCES artifact_verifications(id) ON DELETE RESTRICT,
    reprobe_snapshot_id        INTEGER REFERENCES media_snapshots(id) ON DELETE RESTRICT,
    outcome                    TEXT NOT NULL
        CHECK (outcome IN ('committed', 'verified', 'skipped', 'blocked')),
    created_at                 TEXT NOT NULL,
    CHECK (
        (outcome = 'committed'
            AND produced_file_version_id IS NOT NULL
            AND produced_file_location_id IS NOT NULL
            AND artifact_verification_id IS NULL
            AND reprobe_snapshot_id IS NOT NULL)
        OR (outcome = 'verified'
            AND produced_file_version_id IS NOT NULL
            AND produced_file_location_id IS NOT NULL
            AND artifact_handle_id IS NOT NULL
            AND artifact_verification_id IS NOT NULL
            AND reprobe_snapshot_id IS NOT NULL)
        OR (outcome IN ('skipped', 'blocked')
            AND produced_file_version_id IS NULL
            AND produced_file_location_id IS NULL
            AND artifact_handle_id IS NULL
            AND artifact_verification_id IS NULL
            AND reprobe_snapshot_id IS NULL)
    )
) STRICT;

INSERT INTO workflow_file_phase_summaries_new (
    id,
    job_id,
    phase_ordinal,
    branch_id,
    ticket_ids,
    produced_file_version_id,
    produced_file_location_id,
    artifact_handle_id,
    artifact_verification_id,
    reprobe_snapshot_id,
    outcome,
    created_at
)
SELECT
    id,
    job_id,
    phase_ordinal,
    branch_id,
    ticket_ids,
    produced_file_version_id,
    produced_file_location_id,
    artifact_handle_id,
    NULL,
    reprobe_snapshot_id,
    outcome,
    created_at
FROM workflow_file_phase_summaries;

DROP TABLE workflow_file_phase_summaries;
ALTER TABLE workflow_file_phase_summaries_new RENAME TO workflow_file_phase_summaries;

CREATE UNIQUE INDEX workflow_file_phase_summaries_key
    ON workflow_file_phase_summaries (job_id, phase_ordinal, branch_id);

CREATE TABLE workflow_file_run_history_new (
    job_id        INTEGER NOT NULL,
    branch_id     TEXT NOT NULL,
    phase_ordinal INTEGER NOT NULL CHECK (phase_ordinal >= 0),
    outcome       TEXT NOT NULL CHECK (outcome IN ('committed', 'verified', 'skipped')),
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
