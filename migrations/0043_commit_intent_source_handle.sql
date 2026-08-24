-- Migration 0043 (physical version 6): fenced commit-intent source handle
-- (issue #423 T7, ADR 0075 section "Staging copy joins the fenced commit
-- intent").
--
-- The control-plane staging byte copy (`artifact/stage.rs`) goes away: the
-- storage-owner node now materializes the staging bytes itself during the
-- fenced commit's `applying` phase, then promotes staging -> target exactly
-- as before. To do that without trusting ambient filesystem state, each
-- `artifact_commit_intents` row pins WHERE the bytes come from: the source
-- file version's live rooted address at prepare time.
--
-- 1. A preflight guard rejects any existing `artifact_commit_intents` row
--    BEFORE any schema mutation. Pre-release databases are disposable (the
--    issue-#505 squash precedent) and intents are transient fence state, so
--    an empty table is the only acceptable precondition.
-- 2. The table is recreated with two new pinned columns that mirror the
--    target locator's CHECK shape: `source_storage_root_id` and
--    `source_provider_relative_locator`.

-- ---------------------------------------------------------------------------
-- 1. Preflight guard: reject any existing intent rows before any DDL.
-- ---------------------------------------------------------------------------
CREATE TEMP TABLE _0043_no_artifact_commit_intents (
    no_intent_rows INTEGER NOT NULL CHECK (no_intent_rows = 1)
);
INSERT INTO _0043_no_artifact_commit_intents (no_intent_rows)
SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM artifact_commit_intents
) THEN 1 END;
DROP TABLE _0043_no_artifact_commit_intents;

-- ---------------------------------------------------------------------------
-- 2. Recreate the intents table with the pinned source handle.
-- ---------------------------------------------------------------------------
DROP TABLE artifact_commit_intents;

CREATE TABLE artifact_commit_intents (
    id                             INTEGER PRIMARY KEY,
    commit_record_id               INTEGER NOT NULL UNIQUE
        REFERENCES artifact_commit_records(id) ON DELETE RESTRICT,
    artifact_handle_id             INTEGER NOT NULL
        REFERENCES artifact_handles(id) ON DELETE RESTRICT,
    source_file_version_id         INTEGER NOT NULL
        REFERENCES file_versions(id) ON DELETE RESTRICT,
    verification_id                INTEGER NOT NULL
        REFERENCES artifact_verifications(id) ON DELETE RESTRICT,
    staging_location_id            INTEGER NOT NULL
        REFERENCES file_locations(id) ON DELETE RESTRICT,
    staging_location_epoch         INTEGER NOT NULL CHECK (staging_location_epoch >= 0),
    source_storage_root_id         INTEGER NOT NULL
        REFERENCES library_roots(id) ON DELETE RESTRICT,
    source_provider_relative_locator TEXT NOT NULL CHECK (
        length(CAST(source_provider_relative_locator AS BLOB)) BETWEEN 1 AND 4096
        AND instr(source_provider_relative_locator, char(0)) = 0
    ),
    target_storage_root_id         INTEGER NOT NULL
        REFERENCES library_roots(id) ON DELETE RESTRICT,
    target_root_epoch              INTEGER NOT NULL CHECK (target_root_epoch >= 0),
    target_provider_relative_locator TEXT NOT NULL CHECK (
        length(CAST(target_provider_relative_locator AS BLOB)) BETWEEN 1 AND 4096
        AND instr(target_provider_relative_locator, char(0)) = 0
    ),
    owner_node_id                  INTEGER NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
    owner_incarnation_id           TEXT
        REFERENCES node_incarnations(incarnation_id) ON DELETE RESTRICT,
    expected_facts                 TEXT NOT NULL CHECK (json_valid(expected_facts)),
    state                          TEXT NOT NULL CHECK (state IN ('pending','authorized','completed','aborted','recovery_required')),
    intent_epoch                   INTEGER NOT NULL DEFAULT 0 CHECK (intent_epoch >= 0),
    commit_fence                   BLOB CHECK (commit_fence IS NULL OR length(commit_fence) = 32),
    receipt                        TEXT CHECK (receipt IS NULL OR json_valid(receipt)),
    -- Recovery classification evidence: the current root owner's typed
    -- re-observation, kept alongside (not replacing) the original receipt
    -- so the stuck record carries both for a human.
    supplemental_receipt           TEXT CHECK (supplemental_receipt IS NULL OR json_valid(supplemental_receipt)),
    requested_at                   TEXT NOT NULL,
    authorized_at                  TEXT,
    terminal_at                    TEXT,
    CHECK (
           (state = 'pending' AND commit_fence IS NULL AND authorized_at IS NULL
            AND owner_incarnation_id IS NULL AND receipt IS NULL
            AND supplemental_receipt IS NULL AND terminal_at IS NULL)
        OR (state = 'authorized' AND commit_fence IS NOT NULL AND authorized_at IS NOT NULL
            AND owner_incarnation_id IS NOT NULL AND supplemental_receipt IS NULL
            AND terminal_at IS NULL)
        OR (state = 'completed' AND commit_fence IS NULL
            AND authorized_at IS NOT NULL
            AND owner_incarnation_id IS NOT NULL AND terminal_at IS NOT NULL)
        OR (state = 'recovery_required' AND commit_fence IS NOT NULL
            AND authorized_at IS NOT NULL
            AND owner_incarnation_id IS NOT NULL AND terminal_at IS NOT NULL)
        OR (state = 'aborted' AND commit_fence IS NULL AND terminal_at IS NOT NULL)
    )
);

CREATE INDEX artifact_commit_intents_by_state
    ON artifact_commit_intents (state, id);
