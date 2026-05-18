-- M3 (Sprint 1) — Asset use leases, commit safety gate, ancillary registries.
-- Architectural spec: docs/specs/voom-control-plane-design.md
-- Sprint 1 spec:      docs/superpowers/specs/2026-05-16-voom-sprint-1-design.md §§9.1, 10.1, 10.2, 10.3
-- Sequencing plan:    docs/superpowers/specs/2026-05-17-voom-sprint-1-m3-design.md Phase 0

-- ============================================================================
-- §9.1 — Asset use leases
-- ============================================================================

CREATE TABLE asset_use_leases (
    id                  INTEGER PRIMARY KEY,
    kind                TEXT NOT NULL
        CHECK (kind IN ('playback','scan','copy','manual_lock','external_lock','worker_operation')),
    scope_asset_id      INTEGER     REFERENCES file_assets(id)    ON DELETE RESTRICT,
    scope_bundle_id     INTEGER     REFERENCES asset_bundles(id)  ON DELETE RESTRICT,
    scope_version_id    INTEGER     REFERENCES file_versions(id)  ON DELETE RESTRICT,
    scope_location_id   INTEGER     REFERENCES file_locations(id) ON DELETE RESTRICT,
    issuer_kind         TEXT NOT NULL
        CHECK (issuer_kind IN ('user','control_plane','worker','external_system')),
    issuer_ref          TEXT NOT NULL,
    blocking_mode       TEXT NOT NULL
        CHECK (blocking_mode IN ('blocking','advisory')),
    ttl_bound           INTEGER NOT NULL CHECK (ttl_bound IN (0,1)),
    acquired_at         TEXT NOT NULL,
    expires_at          TEXT,
    last_heartbeat_at   TEXT,
    clock_source        TEXT NOT NULL CHECK (clock_source IN ('control_plane')),
    release_reason      TEXT
        CHECK (release_reason IS NULL OR release_reason IN
               ('released','expired','issuer_lost','superseded','force_released')),
    released_at         TEXT,
    epoch               INTEGER NOT NULL DEFAULT 0,
    -- TTL-bound leases carry an expires_at; manual locks do not.
    CHECK (
        (ttl_bound = 1 AND expires_at IS NOT NULL)
     OR (ttl_bound = 0 AND expires_at IS NULL)
    ),
    -- Terminal state requires both columns set; non-terminal requires both NULL.
    CHECK (
        (release_reason IS NULL AND released_at IS NULL)
     OR (release_reason IS NOT NULL AND released_at IS NOT NULL)
    ),
    -- Exactly one of the four scope_*_id columns is non-NULL.
    CHECK (
        (CASE WHEN scope_asset_id    IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN scope_bundle_id   IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN scope_version_id  IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN scope_location_id IS NULL THEN 0 ELSE 1 END)
      = 1
    )
) STRICT;

CREATE INDEX use_leases_by_asset
    ON asset_use_leases (scope_asset_id)
    WHERE scope_asset_id IS NOT NULL AND release_reason IS NULL;
CREATE INDEX use_leases_by_bundle
    ON asset_use_leases (scope_bundle_id)
    WHERE scope_bundle_id IS NOT NULL AND release_reason IS NULL;
CREATE INDEX use_leases_by_version
    ON asset_use_leases (scope_version_id)
    WHERE scope_version_id IS NOT NULL AND release_reason IS NULL;
CREATE INDEX use_leases_by_location
    ON asset_use_leases (scope_location_id)
    WHERE scope_location_id IS NOT NULL AND release_reason IS NULL;

CREATE INDEX use_leases_by_expiry
    ON asset_use_leases (expires_at)
    WHERE release_reason IS NULL AND ttl_bound = 1;

-- ============================================================================
-- §9.1 — Commit intents (durable journal for three-phase destructive commits)
-- ============================================================================

CREATE TABLE commit_intents (
    id                    INTEGER PRIMARY KEY,
    target                TEXT NOT NULL,    -- JSON-encoded CommitTarget
    closure_initial       TEXT NOT NULL,    -- JSON-encoded AffectedScopeClosure
    closure_authorized    TEXT,             -- JSON; set at Phase B success
    accepted_evidence_ids TEXT NOT NULL,    -- JSON array of EvidenceId values
    override_token        TEXT,             -- JSON-encoded ForcePathToken | NULL
    state                 TEXT NOT NULL
        CHECK (state IN ('pending','authorized','completed','aborted','recovery_required')),
    started_at            TEXT NOT NULL,
    authorized_at         TEXT,
    finalized_at          TEXT,
    aborted_at            TEXT,
    abort_reason          TEXT,
    epoch                 INTEGER NOT NULL DEFAULT 0,
    -- Exclusive-shape encoding: each state value owns the full column shape
    -- so contradictory rows (e.g., both finalized_at and aborted_at non-null)
    -- are unrepresentable.
    CHECK (
           (state = 'pending'           AND authorized_at IS NULL     AND finalized_at IS NULL     AND aborted_at IS NULL     AND abort_reason IS NULL)
        OR (state = 'authorized'        AND authorized_at IS NOT NULL AND finalized_at IS NULL     AND aborted_at IS NULL     AND abort_reason IS NULL)
        OR (state = 'completed'         AND authorized_at IS NOT NULL AND finalized_at IS NOT NULL AND aborted_at IS NULL     AND abort_reason IS NULL)
        OR (state = 'aborted'           AND finalized_at IS NULL      AND aborted_at IS NOT NULL   AND abort_reason IS NOT NULL)
        OR (state = 'recovery_required' AND authorized_at IS NOT NULL AND finalized_at IS NULL     AND aborted_at IS NULL     AND abort_reason IS NULL)
    ),
    -- The override token (if present) must be valid JSON.
    CHECK (override_token IS NULL OR json_valid(override_token)),
    -- The closure columns must be valid JSON.
    CHECK (json_valid(target)),
    CHECK (json_valid(closure_initial)),
    CHECK (closure_authorized IS NULL OR json_valid(closure_authorized)),
    CHECK (json_valid(accepted_evidence_ids))
) STRICT;

CREATE INDEX commit_intents_in_flight
    ON commit_intents (state, started_at)
    WHERE state IN ('pending','authorized');

-- ============================================================================
-- §9.1 — Commit intent scope members (durable pending-commit lock store)
-- ============================================================================

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

-- ============================================================================
-- §10.1 — External systems
-- ============================================================================

CREATE TABLE external_systems (
    id                  INTEGER PRIMARY KEY,
    kind                TEXT NOT NULL
        CHECK (kind IN ('plex','jellyfin','emby','radarr','sonarr','bazarr','s3','filesystem','custom')),
    display_name        TEXT NOT NULL,
    connection_profile  TEXT NOT NULL,
    auth_ref            TEXT NOT NULL,
    health_status       TEXT NOT NULL
        CHECK (health_status IN ('unknown','healthy','degraded','unreachable')),
    rate_limit_config   TEXT NOT NULL DEFAULT '{}',
    created_at          TEXT NOT NULL,
    retired_at          TEXT,
    epoch               INTEGER NOT NULL DEFAULT 0,
    CHECK (json_valid(connection_profile)),
    CHECK (json_valid(rate_limit_config))
) STRICT;

CREATE TABLE external_system_links (
    id                  INTEGER PRIMARY KEY,
    external_system_id  INTEGER NOT NULL REFERENCES external_systems(id) ON DELETE RESTRICT,
    target_type         TEXT NOT NULL
        CHECK (target_type IN ('media_work','media_variant','asset_bundle','file_asset')),
    target_id           INTEGER NOT NULL,
    external_ref        TEXT NOT NULL,
    created_at          TEXT NOT NULL,
    retired_at          TEXT
) STRICT;

CREATE INDEX external_system_links_by_system
    ON external_system_links (external_system_id)
    WHERE retired_at IS NULL;
CREATE INDEX external_system_links_by_target
    ON external_system_links (target_type, target_id)
    WHERE retired_at IS NULL;

CREATE TABLE external_path_mappings (
    id                  INTEGER PRIMARY KEY,
    external_system_id  INTEGER NOT NULL REFERENCES external_systems(id) ON DELETE RESTRICT,
    internal_prefix     TEXT NOT NULL,
    external_prefix     TEXT NOT NULL,
    visibility          TEXT NOT NULL
        CHECK (visibility IN ('read_only','read_write')),
    created_at          TEXT NOT NULL,
    retired_at          TEXT
) STRICT;

CREATE INDEX external_path_mappings_by_system
    ON external_path_mappings (external_system_id)
    WHERE retired_at IS NULL;

-- ============================================================================
-- §10.2 — Issues + issue links
-- ============================================================================

CREATE TABLE issues (
    id                INTEGER PRIMARY KEY,
    kind              TEXT NOT NULL
        CHECK (kind IN (
            'unknown_identity','missing_subtitle','duplicate_candidate',
            'policy_noncompliant','health_failed','external_sync_failed',
            'artifact_unavailable','variant_retention_conflict',
            'worker_untrusted','terminal_failure'
        )),
    severity          TEXT NOT NULL
        CHECK (severity IN ('critical','high','medium','low','info')),
    priority          TEXT NOT NULL
        CHECK (priority IN ('urgent','high','normal','low','someday')),
    priority_source   TEXT NOT NULL
        CHECK (priority_source IN ('system','user','policy','external')),
    priority_reason   TEXT,
    status            TEXT NOT NULL
        CHECK (status IN ('open','planned','resolved','suppressed','accepted')),
    suppressed_until  TEXT,
    title             TEXT NOT NULL,
    body              TEXT NOT NULL,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL,
    resolved_at       TEXT,
    epoch             INTEGER NOT NULL DEFAULT 0,
    -- Resolved status implies a resolved_at timestamp; non-resolved leaves it NULL.
    CHECK (
        (status = 'resolved' AND resolved_at IS NOT NULL)
     OR (status <> 'resolved' AND resolved_at IS NULL)
    ),
    -- Suppressed status implies a suppressed_until horizon; non-suppressed leaves it NULL.
    CHECK (
        (status = 'suppressed' AND suppressed_until IS NOT NULL)
     OR (status <> 'suppressed' AND suppressed_until IS NULL)
    )
) STRICT;

CREATE INDEX issues_by_status_priority
    ON issues (status, priority);
CREATE INDEX issues_by_kind
    ON issues (kind);

CREATE TABLE issue_links (
    id           INTEGER PRIMARY KEY,
    issue_id     INTEGER NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    link_type    TEXT NOT NULL
        CHECK (link_type IN ('evidence','file_asset','bundle','worker',
                             'external_system','ticket','lease','use_lease')),
    target_type  TEXT NOT NULL,
    target_id    INTEGER NOT NULL,
    created_at   TEXT NOT NULL
) STRICT;

CREATE INDEX issue_links_by_issue
    ON issue_links (issue_id);
CREATE INDEX issue_links_by_target
    ON issue_links (target_type, target_id);

-- ============================================================================
-- §10.3 — Quality scoring profiles + scores
-- ============================================================================

CREATE TABLE quality_scoring_profiles (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    version     INTEGER NOT NULL,
    definition  TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    retired_at  TEXT,
    CHECK (json_valid(definition))
) STRICT;

CREATE TABLE quality_scores (
    id                INTEGER PRIMARY KEY,
    profile_id        INTEGER NOT NULL REFERENCES quality_scoring_profiles(id) ON DELETE RESTRICT,
    profile_version   INTEGER NOT NULL,
    provider          TEXT NOT NULL,
    provider_version  TEXT NOT NULL,
    target_type       TEXT NOT NULL
        CHECK (target_type IN ('media_variant','asset_bundle','file_asset','file_version')),
    target_id         INTEGER NOT NULL,
    total_score       REAL NOT NULL,
    dimension_scores  TEXT NOT NULL,
    provenance        TEXT NOT NULL,
    observed_at       TEXT NOT NULL,
    superseded_at     TEXT,
    CHECK (json_valid(dimension_scores)),
    CHECK (json_valid(provenance))
) STRICT;

CREATE INDEX quality_scores_by_target
    ON quality_scores (target_type, target_id)
    WHERE superseded_at IS NULL;
CREATE INDEX quality_scores_by_profile
    ON quality_scores (profile_id)
    WHERE superseded_at IS NULL;
