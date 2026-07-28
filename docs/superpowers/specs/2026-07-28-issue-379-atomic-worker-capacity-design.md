# Atomic Worker Operation Capacity

Issue: #379

## Goal

Make a worker grant's effective `max_parallel` limit a store-owned,
operation-specific counting semaphore. Candidate selection remains advisory, but
every lease acquisition rechecks the same predicate while holding SQLite's
write lock. A stale or concurrent acquisition that reaches the limit must not
change the ticket, create a lease, or emit acquisition events.

## Current State

`SqliteWorkerRepo::operation_candidates_in_tx` reads all held leases for a
worker, then combines that count with an operation-specific grant limit.
`SqliteLeaseRepo::acquire_guarded` rechecks worker eligibility after changing
the ticket inside a savepoint, but does not recheck capacity.

Remote acquisition separately implements an operation-specific held-lease
count and repeats the `max_parallel` JSON reduction. Its transaction starts
with `BEGIN IMMEDIATE`, but the local `ControlPlane::acquire_lease` path starts
deferred. The duplicated readers can drift, and local schedulers can race past
the durable limit.

Workflow ticket kinds add another boundary: a worker operation such as
`remux` can be stored as `synthetic.workflow.operation.remux`. Capacity must
count both spellings as the same worker operation.

## Decisions

### Store-owned capacity snapshot

Add `WorkerOperationCapacity` to `voom-store`:

- `active_leases`: held leases for the worker and normalized operation
- `max_parallel`: the effective operation grant limit
- `has_capacity()`: `active_leases < max_parallel`

`SqliteWorkerRepo::operation_capacity_in_tx` is the only implementation of the
capacity predicate. It preserves the current grant semantics:

1. take the greatest explicit limit for the operation across grant rows;
2. otherwise take the greatest wildcard limit;
3. otherwise default to one;
4. reject malformed, zero, or out-of-range durable JSON visibly.

Held leases for both the direct operation kind and the published workflow
prefix count against the same limit. Released, expired, and force-released
leases do not.

Candidate selection, remote scoring and recheck, and lease acquisition all use
this method. Remote node capacity remains separate because it is a node-owned
predicate, not a worker grant.

### Acquisition linearization

`ControlPlane::acquire_lease` starts with `BEGIN IMMEDIATE`. This obtains the
SQLite write lock before the ticket read, so concurrent local acquisition
processes serialize instead of racing through deferred read-to-write upgrades.

Inside `SqliteLeaseRepo::acquire_guarded`:

1. read and normalize the ticket operation;
2. transition the ready ticket inside the existing savepoint;
3. recheck store-owned eligibility;
4. read store-owned capacity;
5. reject when full;
6. insert the held lease.

The ticket transition intentionally precedes the capacity read: it establishes
the writer lock for callers that supply their own deferred transaction. The
savepoint rolls the transition back on any eligibility or capacity rejection.

Capacity exhaustion returns the existing `VoomError::NoEligibleWorker` public
classification with observed active and limit values. Other eligibility
changes retain #343's existing errors.

### Executor behavior

Candidate selection normally filters full workers. If the candidate becomes
full before acquisition, the atomic guard returns `NoEligibleWorker`. The local
executor maps that post-selection result to `CapacityDeferred`, leaving the
ready ticket available for a later scheduling pass. It does not record a
pre-lease failure, consume an attempt, or dispatch a worker request.

Remote acquisition retains its `NoCandidate` scheduler-decision behavior. Its
worker-capacity recheck uses the store snapshot; the lease guard remains the
final invariant.

## Durable Failure Semantics

For a capacity-rejected local acquisition:

- ticket remains `ready`;
- ticket attempt and epoch are unchanged;
- existing held leases remain unchanged;
- no new lease exists;
- no `lease.acquired` or `ticket.leased` event exists.

For concurrent acquisitions at limit one, exactly one transaction may create a
held lease. Every other completed acquisition observes capacity full after
serialization and leaves the same no-partial-state shape.

## Tests

1. Store candidate capacity counts only the normalized requested operation,
   including workflow-prefixed tickets, and ignores terminal leases.
2. A stale candidate followed by another successful acquisition is rejected at
   lease acquisition with ticket, lease, and event state unchanged.
3. Concurrent local acquisitions against a limit of one produce exactly one
   held lease and one acquired/leased event pair; losers observe capacity
   exhaustion without partial state.
4. Separate operation limits do not block each other.
5. Existing remote contention tests continue to prove one lease plus durable
   capacity decisions, now through the shared store predicate.
6. An executor-level test forces capacity to fill after selection and proves no
   worker request is sent and the work is deferred.

## Compatibility

No schema, event payload, CLI envelope, DSL, or compiled-policy wire shape
changes. `NO_ELIGIBLE_WORKER` already exists. Grant reduction semantics remain
unchanged; only their ownership and atomic enforcement change.

## Adversarial Review

- **Check before obtaining the write lock:** unsafe. Two deferred readers can
  both observe spare capacity. The shipped local entry point therefore uses
  `BEGIN IMMEDIATE`, and the repo checks only after the ticket write.
- **Count every lease for the worker:** overly restrictive and contradicts
  per-operation grants. The predicate normalizes and counts one operation.
- **Count only the raw ticket kind:** under-counts workflow tickets. The query
  includes the direct and workflow-prefixed spellings.
- **Rely on process-local reservations:** cannot coordinate separate executor
  processes and remains only an advisory scheduling optimization.
- **Record a pre-lease failure on a stale full candidate:** consumes retry
  budget for ordinary backpressure. Capacity exhaustion is deferred instead.
- **Remove the remote precheck:** would turn a normal remote `NoCandidate`
  response into an error after a scheduler decision was selected. Remote keeps
  the response-shaping precheck but shares the store calculation.

## Implementation Plan

1. Add failing store and control-plane tests for operation normalization, stale
   selection, concurrent acquisition, and no partial durable state.
2. Add `WorkerOperationCapacity` and the shared store method; use it in local
   candidates.
3. Enforce the snapshot in `SqliteLeaseRepo::acquire_guarded` and start the
   local use case with `BEGIN IMMEDIATE`.
4. Replace remote worker-capacity helpers with the store method.
5. Map atomic capacity rejection to executor deferral and add the forced-race
   executor test.
6. Run focused store/control-plane/remote/executor tests, strict clippy, review,
   simplification, and `just ci`.
