# Deterministic lease-heartbeat regression (#502)

## Problem
The historical source-backed transcode test disappeared in a8505842. Bundled policy verification still uses `await_with_lease_heartbeats`; its injected failure must propagate after durable cleanup. The historical macOS stall cause remains unknown.

## Scope
Restore coverage in operation_adapters/mod_test.rs, linked by mod.rs, using existing TempDatabase and worker helpers. Exercise the helper with a real ready ticket and acquired lease and a pending future. Use real Tokio time and the existing injected heartbeat Conflict. No runtime changes or ownership transition are required. Retired transcode and owner cutover belong to #424/#528; broader timing cleanup belongs to #520. Preserve #492-owned files.

## Success
The returned error is the injected Conflict for VerifyArtifact; the persisted ticket is failed and no held lease remains. Assertions depend on completion and durable state, never a wall-clock deadline. Runtime heartbeat priority, timing, and failure ordering remain unchanged.

## Failure model
- Actors and deployments: local Rust tests and Linux/macOS CI using real SQLite.
- Invariants and assets at stake: injected failure propagation, terminal ticket state, held-lease cleanup.
- Accepted failure classes: historical macOS cause remains unestablished; actual media policy verification and retired transcode are outside this helper regression.
- Required coverage: pending operation cannot win the select; injected heartbeat returns Conflict; durable failure occurs before return; no held leases remain.

## Validation
Run the focused regression, repeat it and run both serial and default test scheduling. Prove sensitivity with a temporary controlled mutation that omits durable failure, then restore it. Run just ci and commit hooks. No ADR: no production decision changes.
