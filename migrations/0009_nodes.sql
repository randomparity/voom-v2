-- Sprint 7 - durable node registry and node-aware workers.

CREATE TABLE nodes (
    id                      INTEGER PRIMARY KEY,
    name                    TEXT NOT NULL UNIQUE,
    kind                    TEXT NOT NULL CHECK (kind IN ('local','remote','synthetic')),
    status                  TEXT NOT NULL CHECK (status IN ('registered','active','stale','retired')),
    registered_at           TEXT NOT NULL,
    last_seen_at            TEXT NOT NULL,
    retired_at              TEXT,
    heartbeat_ttl_seconds   INTEGER NOT NULL CHECK (heartbeat_ttl_seconds > 0),
    auth_token_hash         TEXT NOT NULL,
    auth_token_hint         TEXT NOT NULL,
    metadata                TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata)),
    epoch                   INTEGER NOT NULL DEFAULT 0,
    CHECK ((status = 'retired' AND retired_at IS NOT NULL)
        OR (status != 'retired' AND retired_at IS NULL))
) STRICT;

CREATE INDEX nodes_by_status_seen ON nodes (status, last_seen_at, id);

ALTER TABLE workers
    ADD COLUMN node_id INTEGER REFERENCES nodes(id) ON DELETE RESTRICT;

CREATE INDEX workers_by_node ON workers (node_id) WHERE node_id IS NOT NULL;
