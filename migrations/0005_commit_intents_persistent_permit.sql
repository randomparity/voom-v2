-- M3 (Sprint 1) Phase 2 — Make the authorize-time epoch snapshot durable, and
-- give the recovery_required state its own reason column.
--
-- Codex round-4 review (post commit ae4be4b): the original commit_intents
-- table from migration 0004 made the per-member epoch snapshot a caller-held
-- field on CommitPermit (no DB column), and required abort_reason IS NULL for
-- state = 'recovery_required'. Both contracts conflict with how Phase C must
-- actually behave (re-read snapshot from durable state on crash recovery;
-- record why a row is in recovery_required so operators can triage).
--
-- The tables `commit_intents` and `commit_intent_scope_members` were
-- introduced in migration 0004 and have no rows in any environment yet
-- (Phase 2 algorithm code does not exist). Drop-and-recreate is safe.
--
-- Codex round-6 review (post commit edba8e4) tightened the CHECK
-- further: the three post-Phase-B states (authorized, completed,
-- recovery_required) now require closure_authorized IS NOT NULL.
-- Before the tightening, a row could satisfy the constraint with the
-- closure column unset, defeating crash recovery / list / finalize
-- inspection. The 'aborted' branch is deliberately untouched —
-- aborted-from-pending has no closure; aborted-from-trip-wire has
-- mixed shape depending on which wire fired.

DROP INDEX IF EXISTS commit_intent_scope_members_by_location;
DROP INDEX IF EXISTS commit_intent_scope_members_by_version;
DROP INDEX IF EXISTS commit_intent_scope_members_by_bundle;
DROP INDEX IF EXISTS commit_intent_scope_members_by_asset;
DROP INDEX IF EXISTS commit_intent_scope_members_by_intent;
DROP INDEX IF EXISTS commit_intents_in_flight;

DROP TABLE commit_intent_scope_members;
DROP TABLE commit_intents;

CREATE TABLE commit_intents (
    id                    INTEGER PRIMARY KEY,
    target                TEXT NOT NULL,    -- JSON-encoded CommitTarget
    closure_initial       TEXT NOT NULL,    -- JSON-encoded AffectedScopeClosure
    closure_authorized    TEXT,             -- JSON; set at Phase B success
    accepted_evidence_ids TEXT NOT NULL,    -- JSON array of EvidenceId values
    override_token        TEXT,             -- JSON-encoded ForcePathToken | NULL
    -- Per-member epoch snapshot taken inside the Phase B authorize tx.
    -- JSON array of [kind, row_id, epoch] triples. NOT NULL for any state
    -- the row has reached past 'pending' (authorize sets it; finalize
    -- and recovery-required preserve it; aborted-from-pending leaves it
    -- NULL since prepare never had a snapshot).
    target_row_epochs     TEXT,
    state                 TEXT NOT NULL
        CHECK (state IN ('pending','authorized','completed','aborted','recovery_required')),
    started_at            TEXT NOT NULL,
    authorized_at         TEXT,
    finalized_at          TEXT,
    aborted_at            TEXT,
    abort_reason          TEXT,
    -- Reason the row is in 'recovery_required'. Mutually exclusive with
    -- abort_reason: aborted rows have abort_reason; recovery_required rows
    -- have recovery_reason. The split keeps recovery-tooling queries
    -- single-column.
    recovery_reason       TEXT,
    epoch                 INTEGER NOT NULL DEFAULT 0,
    -- Exclusive-shape encoding: each state value owns the full column shape
    -- so contradictory rows are unrepresentable.
    CHECK (
           (state = 'pending'           AND authorized_at IS NULL     AND finalized_at IS NULL     AND aborted_at IS NULL     AND abort_reason IS NULL     AND recovery_reason IS NULL     AND target_row_epochs IS NULL)
        OR (state = 'authorized'        AND authorized_at IS NOT NULL AND finalized_at IS NULL     AND aborted_at IS NULL     AND abort_reason IS NULL     AND recovery_reason IS NULL     AND target_row_epochs IS NOT NULL AND closure_authorized IS NOT NULL)
        OR (state = 'completed'         AND authorized_at IS NOT NULL AND finalized_at IS NOT NULL AND aborted_at IS NULL     AND abort_reason IS NULL     AND recovery_reason IS NULL     AND target_row_epochs IS NOT NULL AND closure_authorized IS NOT NULL)
        OR (state = 'aborted'           AND finalized_at IS NULL      AND aborted_at IS NOT NULL   AND abort_reason IS NOT NULL AND recovery_reason IS NULL)
        OR (state = 'recovery_required' AND authorized_at IS NOT NULL AND finalized_at IS NULL     AND aborted_at IS NULL     AND abort_reason IS NULL     AND recovery_reason IS NOT NULL AND target_row_epochs IS NOT NULL AND closure_authorized IS NOT NULL)
    ),
    CHECK (override_token IS NULL OR json_valid(override_token)),
    CHECK (target_row_epochs IS NULL OR json_valid(target_row_epochs)),
    CHECK (json_valid(target)),
    CHECK (json_valid(closure_initial)),
    CHECK (closure_authorized IS NULL OR json_valid(closure_authorized)),
    CHECK (json_valid(accepted_evidence_ids))
) STRICT;

CREATE INDEX commit_intents_in_flight
    ON commit_intents (state, started_at)
    WHERE state IN ('pending','authorized');

CREATE TABLE commit_intent_scope_members (
    id                INTEGER PRIMARY KEY,
    commit_intent_id  INTEGER NOT NULL REFERENCES commit_intents(id) ON DELETE CASCADE,
    scope_asset_id    INTEGER REFERENCES file_assets(id)    ON DELETE RESTRICT,
    scope_bundle_id   INTEGER REFERENCES asset_bundles(id)  ON DELETE RESTRICT,
    scope_version_id  INTEGER REFERENCES file_versions(id)  ON DELETE RESTRICT,
    scope_location_id INTEGER REFERENCES file_locations(id) ON DELETE RESTRICT,
    -- Exactly one of the four scope_*_id columns is non-NULL.
    CHECK (
        (CASE WHEN scope_asset_id    IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN scope_bundle_id   IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN scope_version_id  IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN scope_location_id IS NULL THEN 0 ELSE 1 END)
      = 1
    )
) STRICT;

CREATE INDEX commit_intent_scope_members_by_intent
    ON commit_intent_scope_members (commit_intent_id);
CREATE INDEX commit_intent_scope_members_by_asset
    ON commit_intent_scope_members (scope_asset_id)
    WHERE scope_asset_id IS NOT NULL;
CREATE INDEX commit_intent_scope_members_by_bundle
    ON commit_intent_scope_members (scope_bundle_id)
    WHERE scope_bundle_id IS NOT NULL;
CREATE INDEX commit_intent_scope_members_by_version
    ON commit_intent_scope_members (scope_version_id)
    WHERE scope_version_id IS NOT NULL;
CREATE INDEX commit_intent_scope_members_by_location
    ON commit_intent_scope_members (scope_location_id)
    WHERE scope_location_id IS NOT NULL;
