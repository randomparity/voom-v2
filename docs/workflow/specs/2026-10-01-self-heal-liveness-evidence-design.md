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

A second effect of the same verdict: two starters that both judge one stale row dead both
retire it; the slower gets `CONFLICT` through `?` and its startup fails.

## Decision

1. **Verdict (ADR 0102).** For each same-kind `registered`/`active` row, self-heal looks up
   the row's recorded endpoint among that kind's runtime capabilities. A row with an
   endpoint is retired only when `TcpStream::connect(endpoint)` fails with
   `ConnectionRefused` within `SELF_HEAL_CONNECT_TIMEOUT` (1 s). A connect that succeeds,
   times out, or fails another way keeps the row. A row with no endpoint is retired, as
   today.
2. **Peer race.** A retirement that fails with `VoomError::Conflict` is treated as done when
   re-reading the row shows `WorkerStatus::Retired`; any other error propagates.
3. **Endpoint source.** Endpoints come from
   `workers.runtime_capabilities_for_operations(kind's operations)` parsed by the existing
   `runtime_metadata` (made `pub(crate)`). Self-heal no longer calls
   `live_policy_runtime_registry`. That function, `probe_live_runtimes` and
   `LIVENESS_PROBE_TIMEOUT` are not edited, so dispatch-time rejection is unchanged.
4. **Diagnostics.** `finish_shutdown` in `crates/voom-cli/tests/run_local_stdout_contract.rs`
   includes the captured stdout lines (the error envelope) in its nonzero-exit message.

Unchanged: `shutdown_and_retire` still reports a retire conflict as an error.

## Failure model

1. Actors and deployments:
   - local operator or script running one or more `voom worker run-local` supervisors
     against one SQLite database on one host;
   - CI running the `voom-cli` and `voom-control-plane` suites on Linux and macOS runners.
2. Invariants and assets at stake:
   - a live supervised worker's row is not retired by another process (a retired row is
     never dispatched again);
   - a row left by a hard-killed supervisor whose port is closed is still retired on the
     next same-kind start;
   - dispatch-time dead-endpoint rejection keeps its current verdicts.
3. Accepted failure classes:
   - a wedged worker with an open port is not self-healed — bounded: dispatch excludes it
     and its supervisor or an operator retires it (ADR 0102);
   - a stale row whose port was rebound by another process is kept until the port frees —
     bounded: the dispatch probe excludes it;
   - a non-loopback recorded endpoint is never refused-checked conclusively — not reachable:
     `run-local` binds `127.0.0.1:0`.
4. Covered elsewhere:
   - a peer mid-startup (row without endpoint) retired by another starter — follow-up
     candidate reported by this change;
   - a peer retiring a sibling during its shutdown window — follow-up candidate reported by
     this change;
   - accelerated workers use `recover_accelerator_claim` — operator exclusion.

## Success

1. A same-kind row whose endpoint accepts TCP connections but never answers HTTP survives
   `self_heal_stale_workers` (criterion 1, 3).
2. A same-kind row whose endpoint refuses connections is retired by it (preserves the
   self-heal purpose).
3. A retirement conflict on a row already retired by a peer does not fail startup
   (criterion 2).
4. A nonzero `run-local` exit in the harness prints its stdout lines (criterion 4).
5. `compliance.rs` changes only `runtime_metadata`'s visibility (criterion 5).
6. `just ci` exits 0 (criterion 6).

## Validation

- Success 1 — Mode: focused-test. `self_heal_keeps_a_row_whose_endpoint_accepts_but_never_answers`
  in `crates/voom-control-plane/src/local_worker_test.rs`. Red at base: the row is retired
  (the 500 ms handshake times out). Green: `cargo test -p voom-control-plane --lib self_heal_`.
- Success 2 — Mode: focused-test. `self_heal_retires_a_row_whose_endpoint_refuses` in the
  same file; existing `start_local_worker_self_heals_a_stale_same_name_worker` stays green.
- Success 3 — Mode: focused-test. `retire_stale_worker_accepts_a_row_a_peer_already_retired`
  in the same file. Red with a plain `retire_worker` call: `CONFLICT`.
- Success 4 — Mode: task-test-not-applicable. Changed surface: a test helper's panic
  message. No executable consumer reads it; a test of its text would snapshot prose.
- Success 5 — Mode: focused-test. Existing `live_policy_runtime_registry_drops_unreachable_endpoint`
  and `execute_reports_actionable_error_when_no_live_worker_for_remux` in
  `cases/policy/compliance_test.rs` stay green unmodified:
  `cargo test -p voom-control-plane --lib -- live_policy_runtime_registry_drops no_live_worker_for_remux`.
- Success 6 — `just ci`.
