-- M1 (Sprint 1) — Durable execution & events.
-- Architectural spec: docs/specs/voom-control-plane-design.md
-- Sprint 1 spec: docs/superpowers/specs/2026-05-16-voom-sprint-1-design.md §6, §7

-- ---------- events ----------------------------------------------------------
CREATE TABLE events (
    event_id     INTEGER PRIMARY KEY,
    occurred_at  TEXT NOT NULL,
    kind         TEXT NOT NULL,
    subject_type TEXT NOT NULL,
    subject_id   INTEGER,
    trace_id     TEXT,
    payload      TEXT NOT NULL,
    CHECK (json_valid(payload))
) STRICT;

CREATE INDEX events_by_subject ON events (subject_type, subject_id, occurred_at, event_id);
CREATE INDEX events_by_kind_time ON events (kind, occurred_at, event_id);
CREATE INDEX events_by_time ON events (occurred_at, event_id);

CREATE TRIGGER events_no_update
BEFORE UPDATE ON events
BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;

CREATE TRIGGER events_no_delete
BEFORE DELETE ON events
BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;

-- ---------- jobs -----------------------------------------------------------
CREATE TABLE jobs (
    id          INTEGER PRIMARY KEY,
    kind        TEXT NOT NULL,
    state       TEXT NOT NULL CHECK (state IN ('open','succeeded','failed','cancelled')),
    priority    INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    epoch       INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE INDEX jobs_by_state_priority ON jobs (state, priority DESC, id);

-- ---------- tickets --------------------------------------------------------
CREATE TABLE tickets (
    id                INTEGER PRIMARY KEY,
    job_id            INTEGER REFERENCES jobs(id) ON DELETE RESTRICT,
    kind              TEXT NOT NULL,
    state             TEXT NOT NULL CHECK (state IN ('pending','ready','leased','succeeded','failed')),
    priority          INTEGER NOT NULL DEFAULT 0,
    payload           TEXT NOT NULL,
    result            TEXT,
    attempt           INTEGER NOT NULL DEFAULT 0,
    max_attempts      INTEGER NOT NULL DEFAULT 1 CHECK (max_attempts >= 1),
    next_eligible_at  TEXT NOT NULL,
    created_at        TEXT NOT NULL,
    state_changed_at  TEXT NOT NULL,
    epoch             INTEGER NOT NULL DEFAULT 0,
    CHECK (json_valid(payload)),
    CHECK (result IS NULL OR json_valid(result))
) STRICT;

CREATE INDEX tickets_by_state_priority
    ON tickets (state, priority DESC, next_eligible_at, id);
CREATE INDEX tickets_by_job ON tickets (job_id) WHERE job_id IS NOT NULL;

-- ---------- ticket_dependencies --------------------------------------------
CREATE TABLE ticket_dependencies (
    id                     INTEGER PRIMARY KEY,
    ticket_id              INTEGER NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
    depends_on_ticket_id   INTEGER NOT NULL REFERENCES tickets(id) ON DELETE RESTRICT,
    kind                   TEXT NOT NULL CHECK (kind IN ('phase')),
    UNIQUE (ticket_id, depends_on_ticket_id),
    CHECK (ticket_id != depends_on_ticket_id)
) STRICT;

CREATE INDEX ticket_dependencies_by_ticket ON ticket_dependencies (ticket_id);
CREATE INDEX ticket_dependencies_by_depends_on ON ticket_dependencies (depends_on_ticket_id);

-- ---------- workers --------------------------------------------------------
CREATE TABLE workers (
    id             INTEGER PRIMARY KEY,
    name           TEXT NOT NULL UNIQUE,
    kind           TEXT NOT NULL CHECK (kind IN ('synthetic','local','remote')),
    status         TEXT NOT NULL CHECK (status IN ('registered','active','stale','retired')),
    registered_at  TEXT NOT NULL,
    last_seen_at   TEXT NOT NULL,
    retired_at     TEXT,
    epoch          INTEGER NOT NULL DEFAULT 0,
    CHECK ((status = 'retired' AND retired_at IS NOT NULL)
        OR (status != 'retired' AND retired_at IS NULL))
) STRICT;

CREATE TABLE worker_capabilities (
    id                INTEGER PRIMARY KEY,
    worker_id         INTEGER NOT NULL REFERENCES workers(id) ON DELETE CASCADE,
    operation         TEXT NOT NULL,
    codecs            TEXT NOT NULL,
    hardware          TEXT NOT NULL,
    artifact_access   TEXT NOT NULL,
    extra             TEXT NOT NULL DEFAULT '{}',
    CHECK (json_valid(codecs)),
    CHECK (json_valid(hardware)),
    CHECK (json_valid(artifact_access)),
    CHECK (json_valid(extra))
) STRICT;

CREATE INDEX worker_capabilities_by_worker ON worker_capabilities (worker_id);

CREATE TABLE worker_grants (
    id                  INTEGER PRIMARY KEY,
    worker_id           INTEGER NOT NULL REFERENCES workers(id) ON DELETE CASCADE,
    can_execute         TEXT NOT NULL,
    can_access_read     TEXT NOT NULL,
    can_access_write    TEXT NOT NULL,
    denies              TEXT NOT NULL,
    max_parallel        TEXT NOT NULL,
    CHECK (json_valid(can_execute)),
    CHECK (json_valid(can_access_read)),
    CHECK (json_valid(can_access_write)),
    CHECK (json_valid(denies)),
    CHECK (json_valid(max_parallel))
) STRICT;

CREATE INDEX worker_grants_by_worker ON worker_grants (worker_id);

-- ---------- leases (worker-execution leases) -------------------------------
CREATE TABLE leases (
    id                  INTEGER PRIMARY KEY,
    ticket_id           INTEGER NOT NULL REFERENCES tickets(id) ON DELETE RESTRICT,
    worker_id           INTEGER NOT NULL REFERENCES workers(id) ON DELETE RESTRICT,
    state               TEXT NOT NULL CHECK (state IN ('held','released','expired','force_released')),
    acquired_at         TEXT NOT NULL,
    expires_at          TEXT NOT NULL,
    last_heartbeat_at   TEXT NOT NULL,
    ttl_seconds         INTEGER NOT NULL CHECK (ttl_seconds > 0),
    release_reason      TEXT,
    released_at         TEXT,
    epoch               INTEGER NOT NULL DEFAULT 0,
    CHECK ((state = 'held'           AND release_reason IS NULL     AND released_at IS NULL)
        OR (state = 'released'       AND release_reason IS NOT NULL AND released_at IS NOT NULL)
        OR (state = 'expired'        AND release_reason IS NOT NULL AND released_at IS NOT NULL)
        OR (state = 'force_released' AND release_reason IS NOT NULL AND released_at IS NOT NULL))
) STRICT;

CREATE INDEX leases_held_by_expires_at
    ON leases (expires_at) WHERE state = 'held';
CREATE INDEX leases_by_ticket ON leases (ticket_id);
CREATE INDEX leases_by_worker ON leases (worker_id);

-- ---------- artifact catalog -----------------------------------------------
CREATE TABLE artifact_handles (
    id                    INTEGER PRIMARY KEY,
    size_bytes            INTEGER,
    checksum              TEXT,
    privacy_class         TEXT NOT NULL,
    durability_class      TEXT NOT NULL,
    allowed_access_modes  TEXT NOT NULL,
    mutability            TEXT NOT NULL,
    source_lineage        TEXT,
    created_at            TEXT NOT NULL,
    CHECK (json_valid(allowed_access_modes)),
    CHECK (source_lineage IS NULL OR json_valid(source_lineage))
) STRICT;
-- Note: the link columns from artifact_handles to media_work /
-- media_variant / asset_bundle / file_asset / file_version are
-- intentionally OMITTED from this M1 migration. The identity tables
-- land in M2's 0003_identity.sql; M2 will ADD those columns with
-- proper FOREIGN KEY clauses in the same migration, so the columns
-- can never carry orphan references. M1 has no caller that needs
-- these links (the M1 use cases don't construct artifact handles
-- tied to specific identity rows).

CREATE TABLE artifact_locations (
    id                   INTEGER PRIMARY KEY,
    artifact_handle_id   INTEGER NOT NULL REFERENCES artifact_handles(id) ON DELETE RESTRICT,
    kind                 TEXT NOT NULL CHECK (kind IN ('local_path','shared_mount','object_store','staging','backup')),
    value                TEXT NOT NULL,
    observed_at          TEXT NOT NULL,
    retired_at           TEXT
) STRICT;

CREATE INDEX artifact_locations_by_handle ON artifact_locations (artifact_handle_id);
CREATE INDEX artifact_locations_live
    ON artifact_locations (artifact_handle_id) WHERE retired_at IS NULL;

CREATE TABLE artifact_lineage (
    id                    INTEGER PRIMARY KEY,
    parent_artifact_id    INTEGER NOT NULL REFERENCES artifact_handles(id) ON DELETE RESTRICT,
    child_artifact_id     INTEGER NOT NULL REFERENCES artifact_handles(id) ON DELETE RESTRICT,
    operation             TEXT NOT NULL,
    recorded_at           TEXT NOT NULL,
    CHECK (parent_artifact_id != child_artifact_id)
) STRICT;

CREATE INDEX artifact_lineage_by_parent ON artifact_lineage (parent_artifact_id);
CREATE INDEX artifact_lineage_by_child ON artifact_lineage (child_artifact_id);
