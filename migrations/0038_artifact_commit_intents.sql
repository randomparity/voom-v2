-- Migration 0038 (physical version 3): fenced artifact commit intents
-- (issue #422, ADR 0074).
--
-- Adds `artifact_commit_intents`, the durable authorization state machine
-- for node-local staged-commit promotion, 1:1 with its
-- `artifact_commit_records` row:
--
--   1. A preflight guard rejects any non-terminal `artifact_commit_records`
--      row (`pending` or `recovery_required`) BEFORE any schema mutation.
--      This change deletes the host-side recovery code those rows depend on,
--      so they must be resolved under the prior binary; the guard fails the
--      whole migration inside ADR 0068's single outer transaction, leaving
--      the schema untouched (pre-release databases are disposable; see the
--      issue-#505 squash precedent).
--   2. The new table pins the full authorized scope at creation: artifact
--      handle, source file version, verification id, staging location id +
--      epoch, target root id + `root_epoch`, target provider-relative
--      locator, typed expected facts JSON, and the resolved owner node.
--      Authorization adds the owner incarnation and a one-time opaque
--      32-byte `commit_fence`; node receipts (`applying`, `applied`,
--      `mismatched`, `outcome_unknown`) land in typed JSON; an absent
--      receipt means not started. The fence stays at rest only while it can
--      still gate a mutation (`authorized`, `recovery_required`); terminal
--      transitions null it so completed and aborted rows retain no fence
--      material. State coherence is enforced by CHECK.

-- ---------------------------------------------------------------------------
-- 1. Preflight guard: reject unresolvable legacy commit rows before any DDL.
-- ---------------------------------------------------------------------------
CREATE TEMP TABLE _0038_no_nonterminal_commit_records (
    no_nonterminal_rows INTEGER NOT NULL CHECK (no_nonterminal_rows = 1)
);
INSERT INTO _0038_no_nonterminal_commit_records (no_nonterminal_rows)
SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM artifact_commit_records
    WHERE state IN ('pending', 'recovery_required')
) THEN 1 END;
DROP TABLE _0038_no_nonterminal_commit_records;

-- ---------------------------------------------------------------------------
-- 2. Fenced artifact commit intents.
-- ---------------------------------------------------------------------------
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
        OR (state = 'aborted' AND terminal_at IS NOT NULL)
    )
);

CREATE INDEX artifact_commit_intents_by_state
    ON artifact_commit_intents (state, id);
