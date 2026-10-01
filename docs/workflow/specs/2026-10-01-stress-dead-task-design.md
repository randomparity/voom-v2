# Surface dead stress sessions and lanes

Issue: #628. Scope: q628-15cbddc4. Lane: light-spec, S (100 lines).
## Problem

`RemoteNodeSession::run_until_stopped` joins its heartbeat and lane tasks only after
`stop`, and the stress harness joins sessions only after the drain. Under a
`ManualClock` an orphaned lease never expires, so a dead lane reads `stress drain timed
out ... held=1` and a dead session `worker activation timed out`, hiding the error.

## Scope

`run_until_stopped` surfaces its heartbeat and lane tasks; the harness its session tasks.

- `remote_runner.rs`: hold heartbeat and lane tasks in a `JoinSet`. Until `stop` is observed,
  `select!` on `stop.changed()` and `join_next()`. A task ending in `Err` or a join
  error returns that error at once; dropping the set aborts sibling tasks. A task ending
  `Ok` adds to the summary. After stop, join the rest as today.
- `remote_stress_test.rs`: hold session tasks in a `JoinSet`. First in every iteration
  (before `recover_abandoned`), `wait_for_workers` and the drain loop call one helper:
  any session finished before stop fails with its error, or with "remote session ended
  before stop" when it returned `Ok`.

### Failure model

- Actors: a developer or the scheduled constrained-resources CI job (`just stress`).
- Invariants: the harness reports the first observed task failure, not a timeout;
  a successful drain and its conservation checks are unchanged.
- Accepted: a session whose lane is parked in `wait_until_recovered` cannot fail until
  recovery (unchanged; no server call happens there). Only the first of several
  concurrent failures is reported.
- Covered elsewhere: the server error this exposes (follow-up under #577); drain budget
  size (excluded); duplicate notifier (#620); SQLITE_BUSY (#520).

## Success

1. A session task failing during the activation wait or the drain fails the stress
   harness within one poll iteration (10 ms sleep) with that task's error text.
2. `run_until_stopped` returns a lane's error before `stop` is sent.
3. Normal `just stress` and existing `voom-fakes` tests pass.

## Validation

- Session lane failure — `focused-test`: `remote_runner_test.rs`
  `node_session_reports_dead_lane_before_stop` retires an active worker, then requires
  `run_until_stopped` to return the server error within 10 s with stop unsent. Red on
  main: timeout. Green: `cargo test -p voom-fakes --lib dead_lane_before_stop`.
- Harness activation-wait failure — `focused-test`: `remote_stress_test.rs`
  `worker_wait_reports_dead_session_error` runs a session against a closed port and
  requires `wait_for_workers` (60 s budget) to return its `http` error within 5 s.
  Red when the helper call is removed. Green: `cargo test -p voom-fakes --lib
  dead_session_error`.
- Session-liveness helper — `focused-test`: `session_check_reports_finished_sessions`
  requires the helper to pass while tasks run and to fail with the task's error, or the
  "ended before stop" text for `Ok`. Green: `cargo test -p voom-fakes --lib
  session_check`.
- Drain-loop call site — `task-test-not-applicable`: reaching it needs the ignored
  1,000-ticket harness plus a mid-drain lane fault, and no seam exists without
  restructuring `run_stress`; it calls the tested helper first, before recovery.
- Regression: `just stress` and `cargo test -p voom-fakes`.
