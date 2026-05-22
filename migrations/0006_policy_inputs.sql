-- Sprint 3 - Policy input sets and fixture-scoped policy facts.
-- Spec: docs/superpowers/specs/2026-05-22-voom-sprint-3-design.md section 5

CREATE TABLE policy_input_sets (
    id              INTEGER PRIMARY KEY,
    slug            TEXT NOT NULL UNIQUE,
    display_name    TEXT NOT NULL,
    schema_version  INTEGER NOT NULL CHECK (schema_version > 0),
    source_kind     TEXT NOT NULL CHECK (source_kind IN ('fixture','test','imported','manual')),
    created_at      TEXT NOT NULL,
    description     TEXT,
    epoch           INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE INDEX policy_input_sets_by_source_kind
    ON policy_input_sets (source_kind, created_at);

CREATE TABLE policy_input_set_fixture_labels (
    id                  INTEGER PRIMARY KEY,
    policy_input_set_id INTEGER NOT NULL REFERENCES policy_input_sets(id) ON DELETE CASCADE,
    label               TEXT NOT NULL,
    UNIQUE (label),
    UNIQUE (policy_input_set_id, label)
) STRICT;

CREATE INDEX policy_input_set_fixture_labels_by_set
    ON policy_input_set_fixture_labels (policy_input_set_id);

CREATE TABLE policy_input_synthetic_targets (
    id                  INTEGER PRIMARY KEY,
    policy_input_set_id INTEGER NOT NULL REFERENCES policy_input_sets(id) ON DELETE CASCADE,
    synthetic_key       TEXT NOT NULL,
    target_kind         TEXT NOT NULL CHECK (target_kind IN ('media_work','media_variant','asset_bundle','file_asset','file_version','file_location')),
    display_name        TEXT,
    UNIQUE (policy_input_set_id, synthetic_key),
    UNIQUE (policy_input_set_id, id)
) STRICT;

CREATE INDEX policy_input_synthetic_targets_by_set
    ON policy_input_synthetic_targets (policy_input_set_id);

CREATE TABLE policy_media_snapshot_inputs (
    id                         INTEGER PRIMARY KEY,
    policy_input_set_id        INTEGER NOT NULL REFERENCES policy_input_sets(id) ON DELETE CASCADE,
    ordinal                    INTEGER NOT NULL CHECK (ordinal >= 0),
    media_work_id              INTEGER REFERENCES media_works(id) ON DELETE RESTRICT,
    media_variant_id           INTEGER REFERENCES media_variants(id) ON DELETE RESTRICT,
    asset_bundle_id            INTEGER REFERENCES asset_bundles(id) ON DELETE RESTRICT,
    file_asset_id              INTEGER REFERENCES file_assets(id) ON DELETE RESTRICT,
    file_version_id            INTEGER REFERENCES file_versions(id) ON DELETE RESTRICT,
    file_location_id           INTEGER REFERENCES file_locations(id) ON DELETE RESTRICT,
    synthetic_target_id        INTEGER,
    container                  TEXT,
    stream_summary             TEXT NOT NULL,
    video_codec                TEXT,
    width                      INTEGER CHECK (width IS NULL OR width >= 0),
    height                     INTEGER CHECK (height IS NULL OR height >= 0),
    hdr                        TEXT,
    bitrate                    INTEGER CHECK (bitrate IS NULL OR bitrate >= 0),
    duration_millis            INTEGER CHECK (duration_millis IS NULL OR duration_millis >= 0),
    audio_languages            TEXT NOT NULL DEFAULT '[]',
    subtitle_languages         TEXT NOT NULL DEFAULT '[]',
    health_flags               TEXT NOT NULL DEFAULT '[]',
    existing_media_snapshot_id INTEGER REFERENCES media_snapshots(id) ON DELETE RESTRICT,
    FOREIGN KEY (policy_input_set_id, synthetic_target_id)
        REFERENCES policy_input_synthetic_targets(policy_input_set_id, id),
    CHECK (json_valid(stream_summary)),
    CHECK (json_valid(audio_languages)),
    CHECK (json_valid(subtitle_languages)),
    CHECK (json_valid(health_flags)),
    CHECK (
        (CASE WHEN media_work_id       IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN media_variant_id    IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN asset_bundle_id     IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN file_asset_id       IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN file_version_id     IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN file_location_id    IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN synthetic_target_id IS NULL THEN 0 ELSE 1 END)
      = 1
    ),
    UNIQUE (policy_input_set_id, ordinal)
) STRICT;

CREATE INDEX policy_media_snapshot_inputs_by_set
    ON policy_media_snapshot_inputs (policy_input_set_id, ordinal);

CREATE TABLE policy_identity_evidence_inputs (
    id                   INTEGER PRIMARY KEY,
    policy_input_set_id  INTEGER NOT NULL REFERENCES policy_input_sets(id) ON DELETE CASCADE,
    ordinal              INTEGER NOT NULL CHECK (ordinal >= 0),
    media_work_id        INTEGER REFERENCES media_works(id) ON DELETE RESTRICT,
    media_variant_id     INTEGER REFERENCES media_variants(id) ON DELETE RESTRICT,
    asset_bundle_id      INTEGER REFERENCES asset_bundles(id) ON DELETE RESTRICT,
    file_asset_id        INTEGER REFERENCES file_assets(id) ON DELETE RESTRICT,
    file_version_id      INTEGER REFERENCES file_versions(id) ON DELETE RESTRICT,
    file_location_id     INTEGER REFERENCES file_locations(id) ON DELETE RESTRICT,
    synthetic_target_id  INTEGER,
    assertion_type       TEXT NOT NULL,
    provider             TEXT NOT NULL,
    provider_version     TEXT NOT NULL,
    confidence           REAL NOT NULL CHECK (confidence >= 0.0 AND confidence <= 1.0),
    provenance           TEXT NOT NULL,
    observed_at          TEXT NOT NULL,
    existing_evidence_id INTEGER REFERENCES identity_evidence(id) ON DELETE RESTRICT,
    FOREIGN KEY (policy_input_set_id, synthetic_target_id)
        REFERENCES policy_input_synthetic_targets(policy_input_set_id, id),
    CHECK (json_valid(provenance)),
    CHECK (
        (CASE WHEN media_work_id       IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN media_variant_id    IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN asset_bundle_id     IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN file_asset_id       IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN file_version_id     IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN file_location_id    IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN synthetic_target_id IS NULL THEN 0 ELSE 1 END)
      = 1
    ),
    UNIQUE (policy_input_set_id, ordinal)
) STRICT;

CREATE INDEX policy_identity_evidence_inputs_by_set
    ON policy_identity_evidence_inputs (policy_input_set_id, ordinal);

CREATE TABLE policy_bundle_target_inputs (
    id                       INTEGER PRIMARY KEY,
    policy_input_set_id      INTEGER NOT NULL REFERENCES policy_input_sets(id) ON DELETE CASCADE,
    ordinal                  INTEGER NOT NULL CHECK (ordinal >= 0),
    media_work_id            INTEGER REFERENCES media_works(id) ON DELETE RESTRICT,
    media_variant_id         INTEGER REFERENCES media_variants(id) ON DELETE RESTRICT,
    asset_bundle_id          INTEGER REFERENCES asset_bundles(id) ON DELETE RESTRICT,
    file_asset_id            INTEGER REFERENCES file_assets(id) ON DELETE RESTRICT,
    file_version_id          INTEGER REFERENCES file_versions(id) ON DELETE RESTRICT,
    file_location_id         INTEGER REFERENCES file_locations(id) ON DELETE RESTRICT,
    synthetic_target_id      INTEGER,
    role                     TEXT NOT NULL,
    desired_state            TEXT NOT NULL CHECK (desired_state IN ('required','allowed','forbidden','preferred')),
    language                 TEXT,
    label                    TEXT,
    disposition              TEXT,
    artifact_expectation     TEXT NOT NULL,
    FOREIGN KEY (policy_input_set_id, synthetic_target_id)
        REFERENCES policy_input_synthetic_targets(policy_input_set_id, id),
    CHECK (json_valid(artifact_expectation)),
    CHECK (
        (CASE WHEN media_work_id       IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN media_variant_id    IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN asset_bundle_id     IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN file_asset_id       IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN file_version_id     IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN file_location_id    IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN synthetic_target_id IS NULL THEN 0 ELSE 1 END)
      = 1
    ),
    UNIQUE (policy_input_set_id, ordinal)
) STRICT;

CREATE INDEX policy_bundle_target_inputs_by_set
    ON policy_bundle_target_inputs (policy_input_set_id, ordinal);

CREATE TABLE policy_quality_profile_selections (
    id                  INTEGER PRIMARY KEY,
    policy_input_set_id INTEGER NOT NULL REFERENCES policy_input_sets(id) ON DELETE CASCADE,
    ordinal             INTEGER NOT NULL CHECK (ordinal >= 0),
    media_work_id       INTEGER REFERENCES media_works(id) ON DELETE RESTRICT,
    media_variant_id    INTEGER REFERENCES media_variants(id) ON DELETE RESTRICT,
    asset_bundle_id     INTEGER REFERENCES asset_bundles(id) ON DELETE RESTRICT,
    file_asset_id       INTEGER REFERENCES file_assets(id) ON DELETE RESTRICT,
    file_version_id     INTEGER REFERENCES file_versions(id) ON DELETE RESTRICT,
    file_location_id    INTEGER REFERENCES file_locations(id) ON DELETE RESTRICT,
    synthetic_target_id INTEGER,
    profile_name        TEXT NOT NULL,
    profile_version     TEXT NOT NULL,
    dimension_weights   TEXT NOT NULL,
    FOREIGN KEY (policy_input_set_id, synthetic_target_id)
        REFERENCES policy_input_synthetic_targets(policy_input_set_id, id),
    CHECK (json_valid(dimension_weights)),
    CHECK (
        (CASE WHEN media_work_id       IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN media_variant_id    IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN asset_bundle_id     IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN file_asset_id       IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN file_version_id     IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN file_location_id    IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN synthetic_target_id IS NULL THEN 0 ELSE 1 END)
      = 1
    ),
    UNIQUE (policy_input_set_id, ordinal)
) STRICT;

CREATE INDEX policy_quality_profile_selections_by_set
    ON policy_quality_profile_selections (policy_input_set_id, ordinal);

CREATE TABLE policy_issue_inputs (
    id                  INTEGER PRIMARY KEY,
    policy_input_set_id INTEGER NOT NULL REFERENCES policy_input_sets(id) ON DELETE CASCADE,
    ordinal             INTEGER NOT NULL CHECK (ordinal >= 0),
    media_work_id       INTEGER REFERENCES media_works(id) ON DELETE RESTRICT,
    media_variant_id    INTEGER REFERENCES media_variants(id) ON DELETE RESTRICT,
    asset_bundle_id     INTEGER REFERENCES asset_bundles(id) ON DELETE RESTRICT,
    file_asset_id       INTEGER REFERENCES file_assets(id) ON DELETE RESTRICT,
    file_version_id     INTEGER REFERENCES file_versions(id) ON DELETE RESTRICT,
    file_location_id    INTEGER REFERENCES file_locations(id) ON DELETE RESTRICT,
    synthetic_target_id INTEGER,
    kind                TEXT NOT NULL,
    severity            TEXT NOT NULL CHECK (severity IN ('critical','high','medium','low','info')),
    priority            TEXT NOT NULL CHECK (priority IN ('urgent','high','normal','low','someday')),
    state               TEXT NOT NULL CHECK (state IN ('open','accepted','suppressed','planned')),
    reason              TEXT NOT NULL,
    provenance          TEXT NOT NULL,
    existing_issue_id   INTEGER REFERENCES issues(id) ON DELETE RESTRICT,
    FOREIGN KEY (policy_input_set_id, synthetic_target_id)
        REFERENCES policy_input_synthetic_targets(policy_input_set_id, id),
    CHECK (json_valid(provenance)),
    CHECK (
        (CASE WHEN media_work_id       IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN media_variant_id    IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN asset_bundle_id     IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN file_asset_id       IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN file_version_id     IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN file_location_id    IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN synthetic_target_id IS NULL THEN 0 ELSE 1 END)
      = 1
    ),
    UNIQUE (policy_input_set_id, ordinal)
) STRICT;

CREATE INDEX policy_issue_inputs_by_set
    ON policy_issue_inputs (policy_input_set_id, ordinal);
