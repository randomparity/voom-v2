-- Migration 0042 (physical version 5): node-local location-handle media
-- dispatch preflight (issue #423, ADR 0075).
--
-- Pure preflight guard, no schema mutation:
--
--   1. The guard rejects any non-terminal media workflow ticket whose
--      payload would not be re-renderable by the new binary BEFORE any
--      schema mutation. This change moves byte-touching dispatch to the
--      handle-shaped `media_dispatch` envelope: the agent strict-decodes it
--      before lease execution and rejects anything else as malformed. A
--      ticket rendered by a prior binary carries path-shaped fields and no
--      nested `rendered_payload.media_dispatch`, so on retry its lease
--      could never execute; such rows must drain to a terminal state under
--      the prior binary. The guard fails the whole migration inside ADR
--      0068's single outer transaction, leaving the schema untouched
--      (pre-release databases are disposable; see the issue-#505 squash
--      precedent, same as migration 0038's guard).
--   2. Scope: only the seven byte-touching operation kinds whose payloads
--      this change re-shapes (`probe_file`, `transcode_audio`,
--      `extract_audio`, `transcode_video`, `remux`, `back_up_file`,
--      `verify_artifact`). The payload's top-level `operation` field is the
--      canonical OperationKind vocabulary written identically for bare and
--      workflow-namespaced `tickets.kind` encodings, so the scoping is
--      expressible simply in SQL without duplicating the namespace rules.
--      All other tickets stay executable under the new binary (`scan_library`
--      keeps its existing agent pump; synthetic operations are handled by
--      the control plane) and are deliberately not gated.
--   3. Re-renderability is tested by presence of the envelope, not by
--      content: a payload already carrying a `media_dispatch` object was
--      produced by the new renderers and passes.

-- ---------------------------------------------------------------------------
-- 1. Preflight guard: reject unexecutable legacy media payloads before any DDL.
-- ---------------------------------------------------------------------------
CREATE TEMP TABLE _0042_no_unrenderable_media_workflow_tickets (
    no_unrenderable_rows INTEGER NOT NULL CHECK (no_unrenderable_rows = 1)
);
INSERT INTO _0042_no_unrenderable_media_workflow_tickets (no_unrenderable_rows)
SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM tickets
    WHERE state IN ('pending', 'ready', 'leased')
      AND json_extract(payload, '$.operation') IN (
          'probe_file',
          'transcode_audio',
          'extract_audio',
          'transcode_video',
          'remux',
          'back_up_file',
          'verify_artifact'
      )
      AND json_type(payload, '$.rendered_payload.media_dispatch') IS NOT 'object'
) THEN 1 END;
DROP TABLE _0042_no_unrenderable_media_workflow_tickets;
