# Implementation plan — heartbeat watchdog tie classification (#590)

**Goal.** Make tied heartbeat/progress watchdog deadlines deterministically
classify as heartbeat timeout without changing non-tie failure ordering.

**Architecture.** `voom-control-plane` owns workflow execution. A pure helper in
`execution/leases.rs` becomes the single watchdog classifier; the stream
consumer in `execution/dispatch.rs` awaits the earliest absolute deadline and
delegates classification and lease failure to the existing helper path.

**Tech stack.** Rust 2024, Tokio time/select, existing `FailureClass` and
`WorkflowTimingOptions`; no dependency changes.

## Global Constraints

- Preserve transaction, mutation, event, validation, and terminal-frame order.
- Unit tests live in sibling `*_test.rs` files; no paused Tokio clock with a
  real `SqlitePool` or `ControlPlane`.
- Keep domain types intact and introduce no API, schema, migration, dependency,
  or worker-protocol changes.
- Lines are at most 100 characters; warnings are denied.
- Guardrails: focused `cargo test -p voom-control-plane`, then `just ci`.
- Branch `feat/watchdog-tie-590`; base branch `main`; ADR index is coupled.

## File map

| File | Responsibility |
|---|---|
| `crates/voom-control-plane/src/workflow/execution/leases.rs` | deterministic classifier and existing failure side effect |
| `crates/voom-control-plane/src/workflow/execution/leases_test.rs` | direct deadline-order regression tests |
| `crates/voom-control-plane/src/workflow/execution/dispatch.rs` | shared stream-start instant and one earliest-deadline timer branch |
| `crates/voom-control-plane/src/workflow/execution/executor/mod_test.rs` | persisted-class end-to-end regression |

## Task 1 — Implement deterministic watchdog classification

**Interfaces.** Add
`pub(super) fn next_watchdog_deadline(last_heartbeat: Instant, last_progress: Instant,
timing: &WorkflowTimingOptions) -> (Instant, FailureClass)` and
`fn elapsed_watchdog_class(now: Instant, last_heartbeat: Instant,
last_progress: Instant, timing: &WorkflowTimingOptions) -> Option<FailureClass>`
inside `leases.rs`. The elapsed helper consumes the first;
`consume_dispatch_stream` consumes the first directly;
`fail_if_watchdog_elapsed` consumes the second.

1. Append `#[cfg(test)]`, `#[path = "leases_test.rs"]`, and `mod tests;` to
   `leases.rs`, then add the sibling test file. Add unit tests constructing one
   `Instant` origin and durations that
   make heartbeat earlier, progress earlier, and both equal. For strict-order
   cases, place `now` after both deadlines so delayed polling cannot invert the
   expected winner.
2. Run `cargo test -p voom-control-plane watchdog_deadline`; require the compiler
   diagnostic to name missing `next_watchdog_deadline` (and no unrelated
   failure), proving the registered test bites.
3. Implement `next_watchdog_deadline` by selecting the earlier absolute
   deadline and using heartbeat on equality. Implement the elapsed helper by
   checking the selected deadline against `now`. Update
   `fail_if_watchdog_elapsed` to use its result while retaining the existing
   failure class/error mapping.
4. Run the same focused command; expect all deadline/classifier tests to pass.
5. Capture one stream-start instant for both initial observation values. Replace
   the two watchdog select branches with one branch that sleeps inline at the
   deadline returned by `next_watchdog_deadline` and then calls
   `fail_if_watchdog_elapsed`; do not reorder the frame branch or classify
   directly in `dispatch.rs`.
6. Strengthen `heartbeat_timeout_wins_when_watchdog_deadlines_tie` to exercise
   the production wait seam, shared startup instant, and persisted failure
   class. Run the focused tie test and existing heartbeat/progress timeout
   tests; expect all to pass.

Acceptance: equal returns `WorkerTimeout`; strict progress-first returns
`ProgressTimeout`; strict heartbeat-first returns `WorkerTimeout`; neither
elapsed returns `None`. The timer branch has one readiness source and all timeout
classification flows through the deterministic helper. As a controlled fault,
initialize `last_heartbeat` to `stream_started + Duration::from_nanos(1)` and
confirm the tie regression fails with persisted `progress_timeout`, then restore
the shared instant and confirm it passes.

## Task 2 — Verify and commit

**Interfaces.** No new interfaces.

1. Run `just fmt` and inspect `git diff --check`; expect no output from the
   latter.
2. Run `cargo test -p voom-control-plane`; expect all crate tests to pass.
3. Run `just ci`; expect every recipe to pass with zero skipped failures.
4. Commit the implementation as one conventional commit after the design
   artifact commit.

Rollback is `git revert` of the implementation commit; no persisted state or
cleanup is required.
