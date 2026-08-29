# 0092 — Synchronize capacity-cancellation tests at deferral

## Status

Accepted (2026-08-29)

## Context

`cancelling_job_stops_external_capacity_wait_without_failure_events` waits until its workflow
ticket exists, then cancels the job. Ticket creation precedes the executor's capacity-deferred
wait. Test defaults bound that wait to 250 ms, so under suite load the executor can time out,
persist `failed`, and make the later cancellation transition conflict (#559).

The executor already exposes `CapacityDeferredTestSync` under `cfg(test)`. Neighboring capacity
tests use its `observed` notification to prove the run reached deferral and its `resume`
notification to control when the run continues.

## Decision

The cancellation test uses `CapacityDeferredTestSync`. It waits with a bounded timeout for
`observed`, cancels while the executor is held at the deferral boundary, then signals `resume` and
asserts the existing cancellation outcome and durable no-mutation state.

## Consequences

- The test controls the ordering it claims instead of relying on the 250 ms retry budget.
- Production behavior and timing remain unchanged.
- The test still uses real time and a real SQLite pool; only the test-only deferral gate is held.
- A controlled pre-fix proof can shrink the retry timeout and show the ungated test loses the
  cancellation race; the fixed test remains independent of that timeout.

## Considered & rejected

- **Increase the test timeout.** judgment: this changes the odds under load without establishing
  the required ordering.
- **Cancel immediately after ticket creation.** verified: ticket creation is observed by
  `wait_for_workflow_ticket`, while `capacity_deferred_sync.observed` is emitted later inside
  `wait_for_external_capacity` (`executor/mod.rs` on `main` at
  `16ee12bfde14e9f8378d2e5dac966d6e4bfa9d21`); the earlier observation cannot prove deferral.
- **Change production cancellation/failure precedence.** judgment: the captured failure is
  explained by a test that allows the configured capacity timeout to win; no production defect is
  established.
