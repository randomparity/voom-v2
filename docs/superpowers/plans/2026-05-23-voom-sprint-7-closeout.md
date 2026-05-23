# VOOM Sprint 7 Closeout Evidence

## Schema And Migration
- Evidence: `cargo test -p voom-store nodes_`; `cargo test -p voom-store worker`
- Covers: `nodes`, nullable `workers.node_id`, JSON checks, foreign keys.

## Token Storage And Non-Disclosure
- Evidence: `cargo test -p voom-control-plane node_auth nodes_`; `cargo test -p voom-cli node_envelope`
- Covers: plaintext returned only by register; list/show omit token hash and plaintext.

## Heartbeat And Stale State
- Evidence: `cargo test -p voom-control-plane nodes_`
- Covers: heartbeat activation, stale idempotence, retired rejection.

## Node-Aware Worker Registration
- Evidence: `cargo test -p voom-control-plane register_worker_for_node`; `cargo test -p voom-cli worker_envelope`
- Covers: node token verification, freshness, linked worker inspection, legacy null node.

## Audit Events
- Evidence: `cargo test -p voom-events node`; `cargo test -p voom-events worker_linked`; control-plane event-order tests.
- Covers: node and worker-link event payloads in same transaction as mutations.

## Explicit Deferrals
- Sprint 8: HTTP registration and heartbeat routes.
- Sprint 9: scheduler scoring, node-level policy, locality, and concurrency.
- Deferred: daemon heartbeat loops, token rotation, TLS/cert management, real media workers, web UI.
