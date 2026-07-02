-- Sprint 17 -- T16 (#285): profile management support columns.
--
-- The durable `quality_scoring_profiles` registry already exists (migration
-- 0004, §10.3) with `name`/`version`/`definition`/`created_at`/`retired_at`;
-- this issue adds its CRUD repo and CLI, not the table. Two additive columns are
-- all the schema needs:
--
--   * `video_profiles.retired_at` -- soft-retire marker for durable named video
--     profiles, mirroring `quality_scoring_profiles.retired_at`. A profile name
--     can be pinned by a compiled policy version, so retire hides a row from
--     `list` without orphaning references (ADR 0032).
--
--   * `libraries.default_scoring_profile_name` -- the per-library default the
--     0019 libraries migration reserved for this issue. It is a plain nullable
--     TEXT column, not a declared foreign key: referential integrity is enforced
--     at write time by the repository (a set that names an unknown profile is
--     refused), and is safe because scoring profiles are soft-retired, never
--     hard-deleted (`quality_scores.profile_id` is ON DELETE RESTRICT), so a
--     referenced name is never removed.

ALTER TABLE video_profiles ADD COLUMN retired_at TEXT;

ALTER TABLE libraries ADD COLUMN default_scoring_profile_name TEXT;
