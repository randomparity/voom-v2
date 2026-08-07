CREATE TABLE node_incarnations (
    incarnation_id TEXT PRIMARY KEY,
    node_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
    status TEXT NOT NULL CHECK (status IN ('active', 'superseded', 'retired', 'failed')),
    started_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    ended_at TEXT,
    end_reason TEXT,
    CHECK (
        (status = 'active' AND ended_at IS NULL AND end_reason IS NULL)
        OR (status = 'superseded' AND ended_at IS NOT NULL AND end_reason = 'superseded')
        OR (
            status = 'retired'
            AND ended_at IS NOT NULL
            AND end_reason IN ('graceful_shutdown', 'logical_node_retired')
        )
        OR (
            status = 'failed'
            AND ended_at IS NOT NULL
            AND end_reason IN (
                'child_startup_failed',
                'child_restart_exhausted',
                'heartbeat_expired'
            )
        )
    )
) STRICT;

CREATE UNIQUE INDEX node_incarnations_one_active_per_node
    ON node_incarnations(node_id)
    WHERE status = 'active';

CREATE INDEX node_incarnations_history
    ON node_incarnations(node_id, started_at DESC, incarnation_id DESC);

ALTER TABLE nodes ADD COLUMN active_incarnation_id TEXT
    REFERENCES node_incarnations(incarnation_id) ON DELETE RESTRICT;

ALTER TABLE workers ADD COLUMN node_incarnation_id TEXT
    REFERENCES node_incarnations(incarnation_id) ON DELETE RESTRICT
    CHECK (node_incarnation_id IS NULL OR node_id IS NOT NULL);

CREATE INDEX workers_node_incarnation
    ON workers(node_incarnation_id, id);
