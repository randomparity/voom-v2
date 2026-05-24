-- Sprint 9 - durable scheduler decisions and scheduler-owned node limits.

CREATE TABLE scheduler_node_limits (
    node_id                 INTEGER PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    max_parallel_leases     INTEGER NOT NULL CHECK (max_parallel_leases > 0),
    created_at              TEXT NOT NULL,
    updated_at              TEXT NOT NULL
) STRICT;

CREATE TABLE scheduler_decisions (
    id                      INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at              TEXT NOT NULL,
    updated_at              TEXT NOT NULL,
    first_seen_at           TEXT NOT NULL,
    last_seen_at            TEXT NOT NULL,
    decision_kind           TEXT NOT NULL CHECK (decision_kind IN (
                                'lease_acquire',
                                'idle',
                                'no_candidate'
                            )),
    request_source          TEXT NOT NULL CHECK (request_source IN ('remote_acquire')),
    idempotency_key         TEXT,
    request_node_id         INTEGER REFERENCES nodes(id) ON DELETE SET NULL,
    request_worker_id       INTEGER REFERENCES workers(id) ON DELETE SET NULL,
    ticket_id               INTEGER REFERENCES tickets(id) ON DELETE SET NULL,
    selected_worker_id      INTEGER REFERENCES workers(id) ON DELETE SET NULL,
    selected_node_id        INTEGER REFERENCES nodes(id) ON DELETE SET NULL,
    selected_lease_id       INTEGER REFERENCES leases(id) ON DELETE SET NULL,
    outcome                 TEXT NOT NULL CHECK (outcome IN (
                                'selected',
                                'idle',
                                'no_eligible_candidate',
                                'rejected'
                            )),
    reason_code             TEXT NOT NULL CHECK (reason_code IN (
                                'selected',
                                'no_ready_ticket',
                                'missing_capability',
                                'missing_grant',
                                'operation_denied',
                                'worker_not_executable',
                                'node_not_executable',
                                'heartbeat_expired',
                                'unsupported_artifact_access',
                                'worker_capacity_full',
                                'node_capacity_full',
                                'no_eligible_candidate'
                            )),
    summary                 TEXT NOT NULL,
    candidate_count         INTEGER NOT NULL CHECK (candidate_count >= 0),
    selected_score          INTEGER,
    suppressed_count        INTEGER NOT NULL DEFAULT 0 CHECK (suppressed_count >= 0),
    suppression_key         TEXT CHECK (
                                suppression_key IS NULL
                                OR (
                                    decision_kind = 'idle'
                                    AND outcome = 'idle'
                                )
                                OR (
                                    decision_kind = 'no_candidate'
                                    AND outcome = 'no_eligible_candidate'
                                )
                            ),
    explanation_json        TEXT NOT NULL CHECK (json_valid(explanation_json)),
    CHECK (
        (
            decision_kind = 'lease_acquire'
            AND outcome = 'selected'
            AND reason_code = 'selected'
            AND candidate_count > 0
        )
        OR (
            decision_kind = 'idle'
            AND outcome = 'idle'
            AND reason_code = 'no_ready_ticket'
            AND candidate_count = 0
            AND ticket_id IS NULL
            AND selected_worker_id IS NULL
            AND selected_node_id IS NULL
            AND selected_lease_id IS NULL
            AND selected_score IS NULL
        )
        OR (
            decision_kind = 'no_candidate'
            AND outcome = 'no_eligible_candidate'
            AND reason_code != 'selected'
            AND candidate_count > 0
            AND selected_worker_id IS NULL
            AND selected_node_id IS NULL
            AND selected_lease_id IS NULL
            AND selected_score IS NULL
        )
        OR (
            decision_kind = 'lease_acquire'
            AND outcome = 'rejected'
            AND reason_code != 'selected'
            AND selected_worker_id IS NULL
            AND selected_node_id IS NULL
            AND selected_lease_id IS NULL
            AND selected_score IS NULL
        )
    )
) STRICT;

CREATE INDEX scheduler_decisions_by_created_at
    ON scheduler_decisions (created_at DESC, id DESC);

CREATE INDEX scheduler_decisions_by_ticket
    ON scheduler_decisions (ticket_id, id);

CREATE INDEX scheduler_decisions_by_request_worker
    ON scheduler_decisions (request_worker_id, id);

CREATE INDEX scheduler_decisions_by_request_node
    ON scheduler_decisions (request_node_id, id);

CREATE INDEX scheduler_decisions_by_selected_worker
    ON scheduler_decisions (selected_worker_id, id);

CREATE INDEX scheduler_decisions_by_selected_node
    ON scheduler_decisions (selected_node_id, id);

CREATE INDEX scheduler_decisions_by_outcome
    ON scheduler_decisions (outcome, id);

CREATE INDEX scheduler_decisions_by_reason_code
    ON scheduler_decisions (reason_code, id);

CREATE UNIQUE INDEX scheduler_decisions_by_suppression_key
    ON scheduler_decisions (suppression_key)
    WHERE suppression_key IS NOT NULL;
