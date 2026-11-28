# Heartbeat regression implementation plan (#502)

Scope: WORK:SCOPE q502-e5f802c6; full-spec; S/100; iterating. Base: main.

## Task 1
Link the natural operation_adapters sibling test and create one real leased VerifyArtifact ticket using existing shared database/worker support. Await the heartbeat helper with injected Conflict and a pending operation. Assert exact error, failed ticket, zero held leases. Preserve production semantics.

Verification: focused-test — heartbeat_failure_durably_fails_leased_ticket in operation_adapters/mod_test.rs. Controlled red: omit fail_lease_and_return durable transition, expect failed-state assertion to fail. Green: cargo test -p voom-control-plane heartbeat_failure_durably_fails_leased_ticket. Repetition: just test-repeat voom-control-plane heartbeat_failure_durably_fails_leased_ticket 25. Serial scheduling: just test-serial heartbeat_failure_durably_fails_leased_ticket. Integration/final: just ci and mandatory commit hooks.

Documentation: task-test-not-applicable — spec/plan are human-readable decisions without executable consumers.

No ADR, migration, manifest, shared index, dispatch.rs or executor/mod_test.rs changes. No deferrals or adjacent candidates currently identified. Landing follows #492.

Diagnostic limitation: if heartbeat tick delivery regresses, both futures can remain pending. A focused run or repetition then needs runner interruption or an external diagnostic watchdog; elapsed time remains outside the regression assertions.
