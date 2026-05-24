-- Sprint 8 - remote execution idempotency and synthetic artifact access plans.

CREATE TABLE remote_idempotency_keys (
    id                  INTEGER PRIMARY KEY,
    node_id             INTEGER NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
    route_key           TEXT NOT NULL,
    worker_scope_id     INTEGER NOT NULL,
    worker_id           INTEGER REFERENCES workers(id) ON DELETE RESTRICT,
    idempotency_key     TEXT NOT NULL,
    request_hash        TEXT NOT NULL,
    response_json       TEXT CHECK (response_json IS NULL OR json_valid(response_json)),
    status              TEXT NOT NULL CHECK (status IN ('in_progress','completed')),
    created_at          TEXT NOT NULL,
    UNIQUE (node_id, route_key, worker_scope_id, idempotency_key),
    CHECK ((worker_scope_id = 0 AND worker_id IS NULL)
        OR (worker_scope_id > 0 AND worker_id IS NOT NULL AND worker_scope_id = worker_id)),
    CHECK ((status = 'in_progress' AND response_json IS NULL)
        OR (status = 'completed' AND response_json IS NOT NULL))
) STRICT;

CREATE INDEX remote_idempotency_by_node_created
    ON remote_idempotency_keys (node_id, created_at, id);

CREATE TABLE artifact_access_plans (
    id                      INTEGER PRIMARY KEY,
    lease_id                INTEGER NOT NULL REFERENCES leases(id) ON DELETE RESTRICT,
    ticket_id               INTEGER NOT NULL REFERENCES tickets(id) ON DELETE RESTRICT,
    worker_id               INTEGER NOT NULL REFERENCES workers(id) ON DELETE RESTRICT,
    node_id                 INTEGER NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
    input_handles           TEXT NOT NULL CHECK (json_valid(input_handles)),
    output_handles          TEXT NOT NULL CHECK (json_valid(output_handles)),
    selected_access_mode    TEXT NOT NULL CHECK (selected_access_mode IN (
                                'shared_mount',
                                'control_plane_placeholder',
                                'staged_output_placeholder'
                            )),
    status                  TEXT NOT NULL CHECK (status IN ('selected','consumed','rejected','failed')),
    reason                  TEXT,
    evidence                TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(evidence)),
    created_at              TEXT NOT NULL,
    updated_at              TEXT NOT NULL,
    UNIQUE (lease_id)
) STRICT;

CREATE INDEX artifact_access_plans_by_ticket
    ON artifact_access_plans (ticket_id, id);

CREATE INDEX artifact_access_plans_by_worker
    ON artifact_access_plans (worker_id, id);

CREATE INDEX artifact_access_plans_by_node
    ON artifact_access_plans (node_id, id);

CREATE INDEX artifact_access_plans_by_mode_status
    ON artifact_access_plans (selected_access_mode, status, id);
