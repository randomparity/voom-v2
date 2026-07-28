# Cross-process worker-capacity retry

Issue: #389

## Goal

Treat a durable worker-operation capacity limit held by another executor as
transient backpressure. The local executor must wait without consuming ticket
attempts, dispatching a worker request, or spinning, while worker
ineligibility remains a terminal pre-lease failure.

The atomic semaphore implemented by #379 remains the final authority.

## Current state

`SqliteWorkerRepo::operation_candidates` returns otherwise-eligible workers
with their durable active-lease count and effective operation limit. The local
selector returns `NoEligibleWorker` both when no eligible worker exists and
when every eligible worker is full.

The executor recognizes capacity represented by its own reservations. It can
wait for one of its own active dispatches, but it cannot safely handle capacity
held through another SQLite connection or process. If lease acquisition loses
a race after selection, the store also returns `NoEligibleWorker`. That error
currently enters the fatal run path even though #379 rolls the attempted
ticket transition back.

## Decisions

### Typed store acquisition outcome

Add a `LeaseAcquireOutcome` in `voom-store`:

- `Acquired(Lease)` for a committed lease;
- `CapacityFull(WorkerCapacitySaturation)` when the store-owned operation
  semaphore is full.

`SqliteLeaseRepo::try_acquire_in_tx` performs the existing guarded acquisition
inside its savepoint. A capacity-full result rolls back that savepoint and
returns the observed worker, normalized operation, active count, and limit.
The existing `acquire_in_tx` and `acquire` APIs map this typed outcome back to
their existing `VoomError::NoEligibleWorker` behavior, preserving callers and
the public error-code contract.

`ControlPlane::try_acquire_lease` propagates the typed outcome to the workflow
executor. It emits `lease.acquired` and `ticket.leased` only for `Acquired`.
The existing control-plane acquisition API remains unchanged.

This makes classification structural rather than dependent on parsing an
error message.

### Executor classification

The selector path classifies `NoEligibleWorker` as transient capacity only
when the candidate query returned at least one otherwise-eligible worker and
every candidate's effective active count is at its limit. An empty candidate
set still records the existing terminal pre-lease failure. Ambiguous selection
and other eligibility failures keep their current behavior.

If a candidate had spare capacity but another process wins before acquisition,
the typed `CapacityFull` acquisition outcome enters the same transient wait.

### Bounded non-busy wait

When capacity is deferred and the executor has an active local dispatch, it
continues to await that dispatch. When no local dispatch can release capacity,
the executor polls durable state every 250 milliseconds, up to 60 seconds.
Test options use a 10-millisecond interval and 250-millisecond bound.

The wait deadline starts only when saturation is the sole reason no progress
can be made and no local dispatch is active. Dispatch progress or a local
completion resets it. At the deadline the executor returns the existing
`NoEligibleWorker` classification through the normal failed-job path. The
ready ticket is still untouched, so a later operator resume creates new work
from the durable policy state without a consumed ticket attempt.

Before each poll the executor reads the owning job. An externally cancelled
job stops the wait with `UserCancellation`; it does not overwrite the
cancelled job with a failure.

### Restart behavior

Waiting state is process-local and deliberately not durable. If an executor is
stopped while waiting, the job and ready ticket retain their durable state.
Running the same invocation again reconstructs the wait from the durable
candidate and lease rows. No synthetic retry event or scheduler ticket
transition is needed.

## Durable behavior

Each capacity-rejected acquisition leaves:

- the ticket `ready`, with attempt and epoch unchanged;
- the owning job unchanged;
- all existing leases unchanged and no new lease;
- all event rows unchanged;
- no worker request.

If capacity is released within the bound, the next poll acquires exactly once
and normal dispatch events and state transitions follow. If the bound expires,
only the subsequent job-failure operation changes durable state; the rejected
lease acquisitions themselves remain side-effect free.

Cancellation changes only the rows and event emitted by the existing
`cancel_job` transaction. The waiting executor adds no ticket, lease, worker,
or failure event.

## Compatibility

There is no schema, DSL, compiled-policy wire, CLI envelope, durable payload,
or public error-code change. Existing acquisition methods retain
`NO_ELIGIBLE_WORKER` for callers that do not consume the typed store outcome.
The #379 capacity predicate and `BEGIN IMMEDIATE` linearization remain
unchanged.

## Verification

Tests cover:

- typed capacity rejection with ticket attempt/epoch, job, lease, and event
  snapshots unchanged;
- capacity held through a distinct SQLite connection, with no worker request
  before release and eventual dispatch afterward;
- bounded timeout with durable ticket, lease, job, and event inspection;
- executor stop and restart while the same durable capacity remains full;
- durable job cancellation while waiting;
- stale, retired, denied, incapable, and ungranted workers remaining terminal;
- a real CLI execution process waiting behind capacity held from another
  process boundary, then dispatching after release.
