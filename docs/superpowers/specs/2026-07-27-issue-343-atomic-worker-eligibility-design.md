# Issue #343 — Atomic worker eligibility

## Status

Approved for implementation after adversarial review.

## Context

Worker authorization is currently decided twice with different semantics.
`SqliteWorkerRepo::operation_eligibility_in_tx` aggregates every grant row and
implements deny-wins, while the local executor joins individual capability and
grant rows. One allowing row can therefore produce a candidate even when a
different row denies the same operation. Lease acquisition then checks only
that the worker exists and is not retired, so eligibility can be revoked after
selection without preventing dispatch.

ADR 0034 already defines an effective capability as a matching capability and
grant, no matching deny across any grant row, and worker status `registered` or
`active`. This design implements that accepted decision. It does not introduce
a new architectural choice requiring another ADR.

## Goals

- Make effective operation eligibility a store-owned decision.
- Use the same decision for local candidate discovery and lease acquisition.
- Recheck eligibility after the lease transaction owns SQLite's write lock.
- Reject stale and retired workers, missing capabilities or grants, and any
  matching deny without durable ticket, lease, or event changes.
- Preserve existing worker, grant, capability, ticket, lease, event, and public
  protocol shapes.

## Non-goals

- Changing the published policy DSL or compiled-policy wire shape.
- Adding a migration or changing grant storage.
- Completing #338 or changing #338, #339, #367, #368, or #369.
- Changing remote scheduler scoring, node limits, or artifact access planning.
- Making local `max_parallel` a store-enforced lease-time semaphore. That
  pre-existing gap is tracked by #379 as a native sub-issue of #325. This
  change preserves candidate-time capacity behavior and must not weaken the
  remote path's existing in-transaction capacity recheck.
- Making #343 mergeable ahead of #352. The PR remains campaign-ordered behind
  #352 and must be rebased and fully reverified after #352 merges.

## Decision

### Store-owned effective predicate

`WorkerOperationEligibility` will include the worker's optional durable status
and expose `is_eligible()`. The method returns true only when:

1. the worker exists and is `registered` or `active`;
2. at least one matching capability row exists;
3. at least one grant row allows the operation; and
4. no grant row denies the operation.

`operation_eligibility_in_tx` remains the single reader that aggregates every
grant row. Malformed durable JSON remains a visible database error; SQL JSON
membership shortcuts will not silently reinterpret malformed rows.

The store will expose one operation-candidate read. It lists each live worker
once, calls the same in-transaction eligibility reader, and returns only
effective workers with their held-lease count and effective `max_parallel`.
Local executor code will map that store result into scheduler views and apply
only its process-local reservation overlay.

Multiple grant rows produce one worker candidate. Their limit follows the
already established remote-acquire rule: the greatest operation-specific limit
wins; if none exists, the greatest wildcard limit wins; otherwise the limit is
one. This removes row-join duplication without creating a second grant
interpretation. The value remains candidate-time information in the local path;
#379 owns the independent durable semaphore gap.

### Atomic lease recheck

`SqliteLeaseRepo::acquire_in_tx` will wrap its mutation in a savepoint:

1. load the immutable ticket operation;
2. transition the ready ticket inside the savepoint, acquiring SQLite's write
   lock before authorization is observed;
3. evaluate the store-owned effective predicate in the same transaction;
4. insert the lease only when eligibility is effective; and
5. release the savepoint on success.

On any eligibility or insert error, the savepoint is rolled back before the
error is returned. Even a caller that catches the error and commits its outer
transaction cannot persist the provisional ticket transition. Once the first
write succeeds, other writers cannot change worker status, capabilities, or
grants before the eligibility read and lease insert complete.

The ticket operation comes from the durable ticket row. `NewLease` gains no
caller-controlled operation field, preventing a caller from authorizing one
operation while leasing another.

### Failure behavior

- Missing worker: `NOT_FOUND`.
- Stale or retired worker: conflict naming the lifecycle state.
- Matching deny: conflict naming the denied operation.
- Missing capability or grant: conflict naming the missing requirement.
- Malformed durable capability/grant data: existing contextual database error.
- Any rejection: ticket remains `ready` with unchanged attempt/epoch, no lease
  exists, and control-plane acquisition emits no lease or ticket event.

Candidate discovery treats all ineffective states as absent candidates. The
workflow's existing no-eligible-worker path owns retry/failure events.

## Compatibility and rollback

No schema or durable payload changes are required. Older binaries can read all
rows written by this change. Rolling back restores the older, weaker
authorization behavior but does not require data conversion.

Existing tests that acquired leases from workers without capability and grant
rows must seed truthful eligibility. No production compatibility shim or
implicit synthetic-worker grant is added.

## Security and concurrency

Worker grants are an authorization boundary. Deny is aggregated across all
rows, and lease acquisition rechecks after obtaining write ownership. Candidate
selection remains observational and may become stale; correctness does not
depend on that snapshot because lease acquisition is authoritative.

The savepoint also protects caller-owned transactions from partial writes on an
error. Events remain outside the repository savepoint but inside the outer
control-plane transaction, preserving the existing state-plus-event atomicity.

## Test strategy

- Store eligibility tests cover registered, active, stale, retired, missing,
  allowed, and split allow/deny states.
- Store candidate tests prove one worker row, deny-wins, and established limit
  aggregation.
- Lease tests prove each rejection leaves ticket state, attempt, epoch, and
  lease rows unchanged, including when the caller commits after an in-transaction
  failure.
- Control-plane tests prove rejected acquisition emits neither lease nor ticket
  events.
- Executor tests prove split allow/deny workers are not dispatched and inspect
  the durable failed ticket and pre-lease failure event.
- A selection-then-mutation regression obtains an eligible store candidate,
  appends a deny, and proves lease acquisition sends no work and writes no
  lease/ticket events.
- Focused store and control-plane tests run during TDD; final verification is
  `just ci`.

## Success criteria

The same store-owned effective decision filters candidates and authorizes lease
acquisition. Every rejection is fail-closed and leaves no partial durable
execution state. Focused tests and `just ci` pass, with the PR left unmerged
behind #352.
