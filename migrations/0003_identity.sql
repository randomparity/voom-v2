-- M2 (Sprint 1) — Durable identity & bundles.
-- Architectural spec: docs/specs/voom-control-plane-design.md
-- Sprint 1 spec:      docs/superpowers/specs/2026-05-16-voom-sprint-1-design.md §8

-- ---------- media_works ----------------------------------------------------
CREATE TABLE media_works (
    id             INTEGER PRIMARY KEY,
    kind           TEXT NOT NULL CHECK (kind IN ('movie','episode','personal','unknown')),
    display_title  TEXT NOT NULL,
    provisional    INTEGER NOT NULL DEFAULT 1 CHECK (provisional IN (0,1)),
    created_at     TEXT NOT NULL,
    epoch          INTEGER NOT NULL DEFAULT 0
) STRICT;

-- ---------- media_variants -------------------------------------------------
CREATE TABLE media_variants (
    id             INTEGER PRIMARY KEY,
    media_work_id  INTEGER NOT NULL REFERENCES media_works(id) ON DELETE RESTRICT,
    label          TEXT NOT NULL,
    provisional    INTEGER NOT NULL DEFAULT 1 CHECK (provisional IN (0,1)),
    created_at     TEXT NOT NULL,
    epoch          INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE INDEX media_variants_by_work ON media_variants (media_work_id);

-- ---------- file_assets ----------------------------------------------------
CREATE TABLE file_assets (
    id          INTEGER PRIMARY KEY,
    created_at  TEXT NOT NULL,
    retired_at  TEXT,
    epoch       INTEGER NOT NULL DEFAULT 0
) STRICT;

-- ---------- file_versions --------------------------------------------------
CREATE TABLE file_versions (
    id                          INTEGER PRIMARY KEY,
    file_asset_id               INTEGER NOT NULL REFERENCES file_assets(id) ON DELETE RESTRICT,
    content_hash                TEXT NOT NULL,
    size_bytes                  INTEGER NOT NULL CHECK (size_bytes >= 0),
    produced_by                 TEXT NOT NULL
        CHECK (produced_by IN ('ingest','transcode','remux','restore','external_observed')),
    produced_from_version_id    INTEGER REFERENCES file_versions(id) ON DELETE RESTRICT,
    created_at                  TEXT NOT NULL,
    retired_at                  TEXT,
    epoch                       INTEGER NOT NULL DEFAULT 0,
    -- `ingest` and `external_observed` may omit a parent; every other
    -- producer must name one. The repo additionally enforces that the
    -- referenced parent has the same file_asset_id.
    CHECK (
        (produced_by IN ('ingest','external_observed'))
        OR produced_from_version_id IS NOT NULL
    )
) STRICT;

CREATE INDEX file_versions_by_asset ON file_versions (file_asset_id);
CREATE INDEX file_versions_by_hash  ON file_versions (content_hash);

-- ---------- file_locations -------------------------------------------------
CREATE TABLE file_locations (
    id                INTEGER PRIMARY KEY,
    file_version_id   INTEGER NOT NULL REFERENCES file_versions(id) ON DELETE RESTRICT,
    kind              TEXT NOT NULL
        CHECK (kind IN ('local_path','shared_mount','object_store_key','backup_path','historical')),
    value             TEXT NOT NULL,
    proof_kind        TEXT
        CHECK (proof_kind IS NULL OR proof_kind IN ('file_id_generation','object_version_id')),
    proof_value       TEXT,
    observed_at       TEXT NOT NULL,
    retired_at        TEXT,
    epoch             INTEGER NOT NULL DEFAULT 0,
    -- proof_kind and proof_value live or die together.
    CHECK ((proof_kind IS NULL AND proof_value IS NULL)
        OR (proof_kind IS NOT NULL AND proof_value IS NOT NULL))
) STRICT;

CREATE INDEX file_locations_by_version ON file_locations (file_version_id);
CREATE INDEX file_locations_live
    ON file_locations (file_version_id) WHERE retired_at IS NULL;

-- ---------- asset_bundles --------------------------------------------------
CREATE TABLE asset_bundles (
    id                 INTEGER PRIMARY KEY,
    media_variant_id   INTEGER NOT NULL REFERENCES media_variants(id) ON DELETE RESTRICT,
    display_name       TEXT NOT NULL,
    created_at         TEXT NOT NULL,
    epoch              INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE INDEX asset_bundles_by_variant ON asset_bundles (media_variant_id);

-- ---------- asset_bundle_members -------------------------------------------
CREATE TABLE asset_bundle_members (
    id              INTEGER PRIMARY KEY,
    bundle_id       INTEGER NOT NULL REFERENCES asset_bundles(id) ON DELETE CASCADE,
    file_asset_id   INTEGER NOT NULL REFERENCES file_assets(id) ON DELETE RESTRICT,
    role            TEXT NOT NULL
        CHECK (role IN ('primary_video','commentary_audio','external_subtitle',
                        'poster','nfo','trailer','transcript','thumbnail','report')),
    -- An asset is a member of at most one bundle at a time; replacing a
    -- primary video swaps the membership row, not the bundle.
    UNIQUE (file_asset_id)
) STRICT;

CREATE INDEX asset_bundle_members_by_bundle ON asset_bundle_members (bundle_id);

-- ---------- identity_evidence ----------------------------------------------
CREATE TABLE identity_evidence (
    id                       INTEGER PRIMARY KEY,
    target_type              TEXT NOT NULL
        CHECK (target_type IN ('media_work','media_variant','asset_bundle',
                               'file_asset','file_version','file_location')),
    target_id                INTEGER NOT NULL,
    assertion_type           TEXT NOT NULL,
    candidate_id             INTEGER,
    candidate_value          TEXT,
    provider                 TEXT NOT NULL,
    provider_version         TEXT NOT NULL,
    confidence               REAL NOT NULL CHECK (confidence >= 0.0 AND confidence <= 1.0),
    provenance               TEXT NOT NULL,
    observed_at              TEXT NOT NULL,
    superseded_at            TEXT,
    superseded_by_id         INTEGER REFERENCES identity_evidence(id) ON DELETE RESTRICT,
    accepted_at              TEXT,
    accepted_user_id         TEXT,
    accepted_policy_id       INTEGER,
    pinned_file_version_ids  TEXT,
    pinned_hashes            TEXT,
    pinned_locations         TEXT,
    CHECK (json_valid(provenance)),
    CHECK (pinned_file_version_ids IS NULL OR json_valid(pinned_file_version_ids)),
    CHECK (pinned_hashes IS NULL OR json_valid(pinned_hashes)),
    CHECK (pinned_locations IS NULL OR json_valid(pinned_locations)),
    -- supersession pair stays consistent: either both set or both null.
    CHECK ((superseded_at IS NULL AND superseded_by_id IS NULL)
        OR (superseded_at IS NOT NULL AND superseded_by_id IS NOT NULL)),
    -- accepted_at gates the pinned columns: a row is either un-accepted
    -- (all pinned_* null, no accepted_at) or accepted (accepted_at set,
    -- pinned_* free to be NULL or JSON arrays; the repo populates them
    -- at accept-time).
    CHECK ((accepted_at IS NULL AND accepted_user_id IS NULL
            AND accepted_policy_id IS NULL
            AND pinned_file_version_ids IS NULL
            AND pinned_hashes IS NULL
            AND pinned_locations IS NULL)
        OR accepted_at IS NOT NULL)
) STRICT;

CREATE INDEX identity_evidence_by_target
    ON identity_evidence (target_type, target_id);
CREATE INDEX identity_evidence_live
    ON identity_evidence (target_type, target_id)
    WHERE superseded_at IS NULL;

-- ---------- media_snapshots ------------------------------------------------
CREATE TABLE media_snapshots (
    id                INTEGER PRIMARY KEY,
    file_version_id   INTEGER NOT NULL REFERENCES file_versions(id) ON DELETE RESTRICT,
    probed_by         INTEGER REFERENCES workers(id) ON DELETE RESTRICT,
    probed_at         TEXT NOT NULL,
    payload           TEXT NOT NULL,
    CHECK (json_valid(payload))
) STRICT;

CREATE INDEX media_snapshots_by_version ON media_snapshots (file_version_id);

-- ---------- artifact_handles: identity-link columns ------------------------
-- Spec §7.7 places these FK columns on artifact_handles; M1 omitted them
-- because the target tables did not yet exist. SQLite supports ALTER TABLE
-- ADD COLUMN with REFERENCES; the columns are nullable so existing M1 rows
-- (if any — there should be none in production yet) remain valid.
ALTER TABLE artifact_handles
    ADD COLUMN media_work_id    INTEGER REFERENCES media_works(id)    ON DELETE RESTRICT;
ALTER TABLE artifact_handles
    ADD COLUMN media_variant_id INTEGER REFERENCES media_variants(id) ON DELETE RESTRICT;
ALTER TABLE artifact_handles
    ADD COLUMN asset_bundle_id  INTEGER REFERENCES asset_bundles(id)  ON DELETE RESTRICT;
ALTER TABLE artifact_handles
    ADD COLUMN file_asset_id    INTEGER REFERENCES file_assets(id)    ON DELETE RESTRICT;
ALTER TABLE artifact_handles
    ADD COLUMN file_version_id  INTEGER REFERENCES file_versions(id)  ON DELETE RESTRICT;

CREATE INDEX artifact_handles_by_media_work    ON artifact_handles (media_work_id)    WHERE media_work_id    IS NOT NULL;
CREATE INDEX artifact_handles_by_media_variant ON artifact_handles (media_variant_id) WHERE media_variant_id IS NOT NULL;
CREATE INDEX artifact_handles_by_asset_bundle  ON artifact_handles (asset_bundle_id)  WHERE asset_bundle_id  IS NOT NULL;
CREATE INDEX artifact_handles_by_file_asset    ON artifact_handles (file_asset_id)    WHERE file_asset_id    IS NOT NULL;
CREATE INDEX artifact_handles_by_file_version  ON artifact_handles (file_version_id)  WHERE file_version_id  IS NOT NULL;
