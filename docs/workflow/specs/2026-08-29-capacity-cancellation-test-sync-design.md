# Capacity cancellation test synchronization design

## Goal and scope

Resolve issue #559 by making the existing executor test deterministically cancel while the run is
parked for external worker capacity. The final change is limited to the test, its existing
test-only synchronization hook, and quest design records. It does not change workflow state
semantics, production timing, schemas, dependencies, or issue #579.

[ADR 0092](../../adr/0092-synchronize-capacity-cancellation-tests-at-deferral.md) governs the
ordering proof.

## Root cause

The test's `wait_for_workflow_ticket` observes durable ticket creation, not entry into
`wait_for_external_capacity`. `WorkflowExecutorOptions::for_tests` permits only 250 ms of external
capacity waiting. Under full-suite load, the spawned executor can exhaust that budget and fail the
job before the test task resumes and calls `cancel_job`. This explains the captured conflict that
the job was already failed and the earlier `Internal`/`job_failed` variants.

## Design and failure behavior

Create a `CapacityDeferredTestSync` in the test and install it on
`WorkflowExecutorOptions::for_tests`. After the ticket appears, wait at most five seconds for
`sync.observed.notified()`. At that point the executor is blocked on `sync.resume`, so assert the
run is unfinished, cancel the job, signal `sync.resume.notify_one()`, and await the run. Preserve
all existing assertions: `UserCancellation`, no job failure, cancelled job, ready ticket, no
worker dispatch, and no lease or terminal-failure event.

A timeout waiting for `observed` is an actionable setup failure. The test must always signal
`resume` after cancellation succeeds so the spawned task cannot remain parked.

## Verification

- Demonstrate the old ordering failure once with a transient 1 ms capacity timeout and a 10 ms
  delay between ticket observation and cancellation, requiring the exact already-failed
  cancellation conflict. Restore and verify the original file before the fix.
- Run the focused test, then `just test-repeat voom-control-plane
  cancelling_job_stops_external_capacity_wait_without_failure_events 50`.
- Run `just test-serial`, `just test-parallel`, and `just ci` because the defect appears only under
  full-suite load and differing parallelism.

## Durable checkpoint

- Branch: `fix/capacity-cancel-race-559`; base: `main`; scope token: `q559-cc04fd18`.
- Host: x86_64; target architectures: none declared; relationship: no-target-declared.
- Open findings and deferrals: none at design creation.
