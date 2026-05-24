# VOOM Sprint 8 Closeout

## Transport Boundary

Sprint 8 proves remote synthetic execution over bearer-token HTTP routes for loopback, integration tests, and trusted isolated networks. It is not production-safe for untrusted networks: TLS, certificate management, token rotation, and broad API hardening are explicit Sprint 9+ work.

## Acceptance Matrix

| Acceptance item | Evidence |
|---|---|
| Node-token authenticated remote execution routes | `cargo test -p voom-api --test remote_execution_route acquire_requires_bearer_token_and_idempotency_key`; route handlers call control-plane token verification. |
| Explicit non-production bearer-token transport boundary | This closeout document's Transport Boundary section. |
| Worker-to-node ownership enforcement | `cargo test -p voom-control-plane remote_acquire_requires_worker_node_ownership_capability_grant_and_no_deny`; `cargo test -p voom-api --test remote_execution_route lease_routes_reject_worker_node_mismatch`. |
| Remote node heartbeat plus lease acquire, heartbeat, complete, and fail | `cargo test -p voom-api --test remote_execution_route node_and_lease_heartbeat_routes_are_idempotent complete_route_releases_ticket_consumes_plan_and_replays fail_route_fails_ticket_rejects_plan_and_replays`; `cargo test -p voom-control-plane remote`. |
| Remote execution route idempotency and duplicate-key rejection | `cargo test -p voom-api --test remote_execution_route acquire_same_key_replays_and_different_body_conflicts`; `cargo test -p voom-store remote_idempotency`. |
| Malformed route input preserves API error envelope | `cargo test -p voom-api --test remote_execution_route malformed_json_returns_api_error_envelope malformed_path_ids_return_api_error_envelope`. |
| Worker capability and grant enforcement during acquire | `cargo test -p voom-control-plane remote_acquire_requires_worker_node_ownership_capability_grant_and_no_deny`; `cargo test -p voom-store operation_eligibility`. |
| Synthetic worker setup with explicit execution grants | `cargo test -p voom-fakes remote_runner`; fixture registers a remote worker with explicit `can_execute` and `shared_mount` capability. |
| Remote runner executes synthetic durable tickets over HTTP | `cargo test -p voom-fakes runner_polls_acquires_dispatches_heartbeats_and_completes`; fixture starts the `voom-api` router on loopback and runner calls HTTP routes with bearer auth. |
| Remote runner fails incompatible artifact access visibly | `cargo test -p voom-fakes runner_fails_lease_when_configured_artifact_access_is_incompatible`. |
| Stale lease recovery | `cargo test -p voom-control-plane remote_recover_marks_stale_nodes_and_expires_due_leases`. |
| Stale node recovery | `cargo test -p voom-control-plane remote_heartbeat_reactivates_stale_node_and_replays_lease_heartbeat`. |
| No audit events for individual missed heartbeats | `cargo test -p voom-control-plane remote_heartbeat_reactivates_stale_node_and_replays_lease_heartbeat`; test asserts heartbeat does not emit release events. |
| Artifact access plan persistence | `cargo test -p voom-store artifact_access`; `cargo test -p voom-control-plane remote_complete_reuses_success_path_and_replays_same_idempotency_key`. |
| Remote complete validates selected artifact evidence | `cargo test -p voom-control-plane remote_complete_rejects_incomplete_or_mismatched_artifact_evidence`. |
| Synthetic artifact access validation | `cargo test -p voom-fake-support artifact_access`. |
| Scheduler scoring and broad API hardening deferred | Sprint 8 spec section 2 and this closeout transport boundary. |

## Verification Commands

Targeted commands run during implementation:

```bash
cargo test -p voom-api --test remote_execution_route
cargo test -p voom-control-plane remote
cargo test -p voom-store artifact_access
cargo test -p voom-fake-support artifact_access
cargo test -p voom-fakes remote
```

Full local gate:

```bash
just ci
```
