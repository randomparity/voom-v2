# Self-heal retires a local worker only on a refused endpoint

Issue: #667. Scope: q667-dcbee995. Lane: full-spec, M (250 lines).
Decision: [ADR 0102](../../adr/0102-self-heal-retires-only-on-a-refused-endpoint.md).
Plan: `docs/workflow/plans/2026-10-01-self-heal-liveness-evidence.md`.

## Problem

`concurrent_same_and_mixed_kind_shutdowns_retire_every_worker` failed in CI: a `run-local`
`mkvtoolnix` supervisor (worker 5) exited 2 on shutdown. Each later `mkvtoolnix` start runs
`self_heal_stale_workers` (`crates/voom-control-plane/src/local_worker.rs`), which retires
every same-kind `registered`/`active` row absent from `live_policy_runtime_registry()`.
Membership there is one `handshake` within `LIVENESS_PROBE_TIMEOUT` (500 ms,
`cases/policy/compliance.rs`). A live sibling that misses it on a loaded runner is retired
by its peer. Its own `shutdown_and_retire` then fails with `CONFLICT … already retired`
(`voom-store` `retire_in_tx`) and `emit_voom_error` exits 2. The test harness prints only
stderr, so the stdout `CONFLICT` envelope never reached the CI log.

The same verdict has two more effects. A sibling still starting (row registered, endpoint
not yet recorded) is absent from the registry and is retired the same way. Two starters
that both judge one stale row dead both retire it; the slower gets `CONFLICT` through `?`
and its startup fails.

## Decision

1. **Verdict (ADR 0102).** For each same-kind `registered`/`active` row, self-heal looks up
   the row's recorded endpoint among that kind's runtime capabilities.
   - With an endpoint: retired only when `TcpStream::connect(endpoint)` fails with
     `ConnectionRefused` within `SELF_HEAL_CONNECT_TIMEOUT` (1 s). Success, timeout, or any
     other error keeps the row.
   - Without one: retired only when `registered_at + UNRECORDED_ENDPOINT_GRACE` (15 min)
     is before now. The grace exceeds every local startup timeout (largest: 465 s).
2. **Peer race.** A retirement that fails with `VoomError::Conflict` is treated as done when
   re-reading the row shows `WorkerStatus::Retired`; any other error propagates.
3. **Endpoint source.** Endpoints come from
   `workers.runtime_capabilities_for_operations(kind's operations)` parsed by the existing
   `runtime_metadata` (made `pub(crate)`). Self-heal no longer calls
   `live_policy_runtime_registry`. That function, `probe_live_runtimes` and
   `LIVENESS_PROBE_TIMEOUT` are not edited, so dispatch-time rejection is unchanged.
   `voom-control-plane` declares tokio's `net` feature, which it already receives through
   `voom-worker-protocol`.
4. **Diagnostics.** `finish_shutdown` in `crates/voom-cli/tests/run_local_stdout_contract.rs`
   includes the captured stdout lines (the error envelope) in its nonzero-exit message.
5. **Existing self-heal test.** `start_local_worker_self_heals_a_stale_same_name_worker`
   drops a worker (SIGKILL, not awaited) and starts another at once. It waits until the
   killed worker's endpoint stops accepting connections before the second start: the
   state a hard kill leaves once the process is gone. This waits for a precondition; it
   does not retry the asserted behaviour.

Unchanged: `shutdown_and_retire` still reports a retire conflict as an error.

## Failure model

1. Actors and deployments:
   - local operator or script running one or more `voom worker run-local` supervisors
     against one SQLite database on one host;
   - CI running the `voom-cli` and `voom-control-plane` suites on Linux and macOS runners.
2. Invariants and assets at stake:
   - a live supervised worker's row is not retired by another process's self-heal (a
     retired row is never dispatched again);
   - a row left by a hard-killed supervisor is still retired on a later same-kind start:
     at once when its port is closed, after the grace when it never recorded an endpoint;
   - dispatch-time dead-endpoint rejection keeps its current verdicts.
3. Accepted failure classes:
   - a wedged worker with an open port is not self-healed — bounded: dispatch excludes it
     and its supervisor or an operator retires it (ADR 0102);
   - a stale row whose loopback port was rebound is kept until the port frees — bounded:
     the dispatch probe excludes it unless the rebinder is another voom worker, whose
     unauthenticated handshake passes (base kept that row too; it retired the row only
     when the rebinder did not answer the handshake);
   - a non-loopback recorded endpoint is never refused-checked conclusively — not reachable:
     `run-local` binds `127.0.0.1:0`;
   - a peer that starts in the window between a sibling's worker exit and that sibling's
     own retire retires the row (correctly: the worker is gone), and the sibling's shutdown
     exits 2 with `CONFLICT` — bounded: the window is one SQLite read and write long, the
     row ends retired either way, and changing `shutdown_and_retire`'s reporting is outside
     this change's criteria (reported as a follow-up candidate).
4. Covered elsewhere:
   - accelerated workers use `recover_accelerator_claim` — operator exclusion.

## Success

1. A same-kind row whose endpoint accepts TCP connections but never answers HTTP survives
   `self_heal_stale_workers` (criteria 1, 3).
2. A same-kind row whose endpoint refuses connections is retired by it.
3. A same-kind row without an endpoint survives it while within the grace and is retired
   after it.
4. A retirement conflict on a row already retired by a peer does not fail startup
   (criterion 2).
5. A nonzero `run-local` exit in the harness prints its stdout lines (criterion 4).
6. `compliance.rs` changes only `runtime_metadata`'s visibility (criterion 5).
7. `just ci` exits 0 (criterion 6).

## Validation

Unit tests live in `crates/voom-control-plane/src/local_worker_test.rs`.

- Success 1 — Mode: focused-test.
  `self_heal_keeps_a_row_whose_endpoint_accepts_but_never_answers`. Red at base: the row is
  retired (the 500 ms handshake times out).
  Green: `cargo test -p voom-control-plane --lib -- self_heal_ retire_stale_worker_ grace`.
- Success 2 — Mode: focused-test. `self_heal_retires_a_row_whose_endpoint_refuses` (the port
  is held bound without listening, so no other test can take it); also
  `start_local_worker_self_heals_a_stale_same_name_worker` in
  `crates/voom-control-plane/tests/local_worker_lifecycle.rs`.
- Success 3 — Mode: focused-test. `self_heal_keeps_a_fresh_row_without_an_endpoint` (red at
  base: retired) and `self_heal_retires_a_row_without_an_endpoint_after_the_grace`;
  `unrecorded_endpoint_grace_outlasts_every_startup_timeout` pins the bound.
- Success 4 — Mode: focused-test. `retire_stale_worker_accepts_a_row_a_peer_already_retired`.
  Red with a plain `retire_worker` call: `CONFLICT`. The call from `self_heal_stale_workers`
  into `retire_stale_worker` is covered by inspection only: no test can force two starters
  to interleave between the scan and the retire without a scheduling seam.
- Success 5 — Mode: task-test-not-applicable. Changed surface: a test helper's panic
  message. No executable consumer reads it; a test of its text would snapshot prose.
- Success 6 — Mode: focused-test. Existing
  `live_policy_runtime_registry_drops_unreachable_endpoint` and
  `execute_reports_actionable_error_when_no_live_worker_for_remux` stay green unmodified:
  `cargo test -p voom-control-plane --lib -- live_policy_runtime_registry_drops no_live_worker_for_remux`.
- Success 7 — `just ci`.
