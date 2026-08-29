# Capacity cancellation test synchronization plan

## Goal

Make the external-capacity cancellation test hold the executor at the existing test-only deferral
boundary before cancelling, eliminating its load-dependent timeout race without production
changes.

Tech stack: Rust, Tokio `Notify`, sqlx-backed control-plane test fixtures.

## Global constraints

- Branch `fix/capacity-cancel-race-559`; base `main`; scope token `q559-cc04fd18`.
- Modify only
  `crates/voom-control-plane/src/workflow/execution/executor/mod_test.rs` after design records.
- Reuse `CapacityDeferredTestSync`; add no production API or dependency.
- Keep real time and real SQLite; never pair a paused Tokio clock with the pool.
- Preserve the test's complete existing behavioral and durable-state assertion set.
- Guardrails: focused test, 50 focused repetitions, `just test-serial`, `just test-parallel`, and
  `just ci`.

## Task 1 — Reproduce and fix the ordering race

Files:

- Modify and test
  `crates/voom-control-plane/src/workflow/execution/executor/mod_test.rs`.

Interfaces:

- Consume existing `CapacityDeferredTestSync { observed: Arc<Notify>, resume: Arc<Notify> }` and
  `WorkflowExecutorOptions::capacity_deferred_sync`.
- Produce no new interface; only update
  `cancelling_job_stops_external_capacity_wait_without_failure_events`.

Steps:

1. Record the file blob. Transiently configure the old ungated test with a near-zero
   `capacity_retry_timeout`, run the focused test until the captured already-failed transition is
   observed, then restore the exact blob before writing the fix.
2. In the test, construct `CapacityDeferredTestSync` with two `Arc<Notify>` values. Install a clone
   on mutable `WorkflowExecutorOptions::for_tests()` and construct the executor from those options.
3. After `wait_for_workflow_ticket`, wrap `sync.observed.notified()` in a five-second timeout and
   require success. Assert the spawned run is unfinished.
4. Cancel the job, call `sync.resume.notify_one()`, then await the spawned run and retain every
   existing assertion.
5. Run `cargo test -p voom-control-plane
   cancelling_job_stops_external_capacity_wait_without_failure_events`; expect one pass. Run 50
   focused repetitions; expect no failure.
6. Run `just fmt-check`, `just lint`, `just check-test-layout`, `just check-paused-time-db`,
   `just check-transaction-openers`, `just test-serial`, `just test-parallel`, and `just ci`.
7. Commit as `test: synchronize capacity cancellation at deferral` after all required focused
   checks are green.

## Acceptance

- Cancellation happens only after the executor reports capacity deferral and while it is held.
- The run cannot consume the 250 ms timeout before cancellation, regardless of host scheduling.
- The existing user-cancellation and zero-mutation assertions all remain and pass under repetition
  and both suite parallelism modes.

## Durable checkpoint

- Current phase after plan review: oathbind scope audit, then forge.
- Open findings and deferrals: none.
