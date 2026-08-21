-- Migration 0037 (physical version 2): owner-local scheduling evidence
-- (issue #477, ADR 0071).
--
-- Replaces the legacy synthetic/shared-mount artifact-access plan
-- representation with one owner-local representation, without retaining dual
-- formats:
--
--   1. A preflight guard rejects any existing `artifact_access_plans` row
--      BEFORE any schema mutation. Legacy rows carry `selected_access_mode`
--      and synthetic handle strings with no owner-local translation; the
--      guard fails the whole migration inside ADR 0068's single outer
--      transaction, leaving the schema untouched. The remedy for a database
--      that trips this guard is to remove the legacy rows (pre-release
--      databases are disposable; see the issue-#505 squash precedent).
--   2. `artifact_access_plans` is rebuilt in its final owner-local shape:
--      `selected_access_mode`, `input_handles`, and `output_handles` are
--      gone; `owner_node_id` and typed `access_evidence` JSON take their
--      place, bound together so exactly one of "full owner-local proof"
--      (owner = acquiring node, evidence present) or "no declared byte work"
--      (both absent) holds per row. Status lifecycle, reason,
--      worker-validated evidence passthrough, timestamps, UNIQUE (lease_id),
--      and the by-ticket/by-worker/by-node indexes are preserved;
--      `by_mode_status` becomes `by_owner_status`.
--   3. `scheduler_decisions` gains one nullable typed column,
--      `access_evidence`. Every existing column, index, the AUTOINCREMENT
--      sequence, and all supported reason codes are untouched.

-- ---------------------------------------------------------------------------
-- 1. Preflight guard: reject incompatible legacy rows before any DDL.
-- ---------------------------------------------------------------------------
CREATE TEMP TABLE _0037_legacy_artifact_access_plan_rows_present (
    no_legacy_rows INTEGER NOT NULL CHECK (no_legacy_rows = 1)
);
INSERT INTO _0037_legacy_artifact_access_plan_rows_present (no_legacy_rows)
SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM artifact_access_plans) THEN 1 END;
DROP TABLE _0037_legacy_artifact_access_plan_rows_present;

-- ---------------------------------------------------------------------------
-- 2. Rebuild artifact_access_plans in its owner-local shape.
-- ---------------------------------------------------------------------------
CREATE TABLE artifact_access_plans_0037_next (
    id                      INTEGER PRIMARY KEY,
    lease_id                INTEGER NOT NULL REFERENCES leases(id) ON DELETE RESTRICT,
    ticket_id               INTEGER NOT NULL REFERENCES tickets(id) ON DELETE RESTRICT,
    worker_id               INTEGER NOT NULL REFERENCES workers(id) ON DELETE RESTRICT,
    node_id                 INTEGER NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
    owner_node_id           INTEGER          REFERENCES nodes(id) ON DELETE RESTRICT,
    access_evidence         TEXT             CHECK (access_evidence IS NULL OR json_valid(access_evidence)),
    status                  TEXT NOT NULL CHECK (status IN ('selected','consumed','rejected','failed')),
    reason                  TEXT,
    evidence                TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(evidence)),
    created_at              TEXT NOT NULL,
    updated_at              TEXT NOT NULL,
    UNIQUE (lease_id),
    CHECK (
        (owner_node_id IS NULL AND access_evidence IS NULL)
        OR (owner_node_id = node_id AND access_evidence IS NOT NULL)
    )
) STRICT;

INSERT INTO artifact_access_plans_0037_next (
    id, lease_id, ticket_id, worker_id, node_id, owner_node_id,
    access_evidence, status, reason, evidence, created_at, updated_at
)
SELECT
    id, lease_id, ticket_id, worker_id, node_id, NULL,
    NULL, status, reason, evidence, created_at, updated_at
FROM artifact_access_plans;

DROP TABLE artifact_access_plans;
ALTER TABLE artifact_access_plans_0037_next RENAME TO artifact_access_plans;

CREATE INDEX artifact_access_plans_by_ticket
    ON artifact_access_plans (ticket_id, id);
CREATE INDEX artifact_access_plans_by_worker
    ON artifact_access_plans (worker_id, id);
CREATE INDEX artifact_access_plans_by_node
    ON artifact_access_plans (node_id, id);
CREATE INDEX artifact_access_plans_by_owner_status
    ON artifact_access_plans (owner_node_id, status, id);

-- ---------------------------------------------------------------------------
-- 3. scheduler_decisions: additive typed evidence column. Existing columns,
--    indexes, sequence, and reason vocabulary are preserved untouched.
-- ---------------------------------------------------------------------------
ALTER TABLE scheduler_decisions
    ADD COLUMN access_evidence TEXT
    CHECK (access_evidence IS NULL OR json_valid(access_evidence));
