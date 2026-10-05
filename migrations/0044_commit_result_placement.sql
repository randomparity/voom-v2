-- Migration 0044 (physical version 7): durable placement state for committed
-- results (issue #677, ADR 0103).
--
-- `artifact_commit_records` has three incoming ON DELETE RESTRICT foreign keys,
-- so it cannot be rebuilt inside the migration transaction, and a per-state
-- column CHECK added by ALTER TABLE is tested against existing committed rows
-- before any backfill. The columns are therefore added with domain CHECKs only,
-- backfilled, and the per-row rule is enforced by BEFORE triggers.
--
-- Backfill classifies by the working-dir convention the transitional promotion
-- path matches: a commit-time target_path with a `.committed` component was a
-- workflow commit expecting a move (`staged`); anything else was retained. A
-- staged committed row whose result locator no longer has that component was
-- moved (`placed`).

ALTER TABLE artifact_commit_records ADD COLUMN placement_intent TEXT
    CHECK (placement_intent IS NULL OR placement_intent IN ('staged','retained'));
ALTER TABLE artifact_commit_records ADD COLUMN placement_state TEXT
    CHECK (placement_state IS NULL OR placement_state IN ('staged','retained','placed'));

UPDATE artifact_commit_records
SET placement_intent = CASE
    WHEN instr(target_path, '/.committed/') > 0
      OR instr(target_path, '\.committed\') > 0 THEN 'staged'
    ELSE 'retained'
END;

UPDATE artifact_commit_records
SET placement_state = CASE
    WHEN placement_intent = 'staged' AND EXISTS (
        SELECT 1 FROM file_locations fl
        WHERE fl.id = artifact_commit_records.result_file_location_id
          AND fl.provider_relative_locator IS NOT NULL
          AND instr('/' || fl.provider_relative_locator, '/.committed/') = 0
    ) THEN 'placed'
    ELSE placement_intent
END
WHERE state = 'committed';

CREATE TRIGGER artifact_commit_records_placement_insert
BEFORE INSERT ON artifact_commit_records
WHEN NOT coalesce(
    NEW.placement_intent IN ('staged','retained')
    AND ((NEW.state = 'committed'
          AND (NEW.placement_state = NEW.placement_intent
               OR (NEW.placement_state = 'placed' AND NEW.placement_intent = 'staged')))
         OR (NEW.state <> 'committed' AND NEW.placement_state IS NULL)),
    0)
BEGIN
    SELECT RAISE(ABORT, 'artifact_commit_records placement violates its per-state rule');
END;

CREATE TRIGGER artifact_commit_records_placement_update
BEFORE UPDATE OF state, placement_intent, placement_state ON artifact_commit_records
WHEN NOT coalesce(
    NEW.placement_intent IN ('staged','retained')
    AND NEW.placement_intent = OLD.placement_intent
    AND (OLD.placement_state IS NOT 'placed' OR NEW.placement_state = 'placed')
    AND ((NEW.state = 'committed'
          AND (NEW.placement_state = NEW.placement_intent
               OR (NEW.placement_state = 'placed' AND NEW.placement_intent = 'staged')))
         OR (NEW.state <> 'committed' AND NEW.placement_state IS NULL)),
    0)
BEGIN
    SELECT RAISE(ABORT, 'artifact_commit_records placement violates its per-state rule');
END;

CREATE INDEX artifact_commit_records_by_result_location
    ON artifact_commit_records (result_file_location_id)
    WHERE result_file_location_id IS NOT NULL;
