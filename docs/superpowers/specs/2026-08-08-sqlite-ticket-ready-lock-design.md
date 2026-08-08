# SQLite Ticket-Ready Lock Design

## Scope

Issue #453 reports that `ControlPlane::mark_ready_if_unblocked` can return
`SQLITE_BUSY` when it races another SQLite writer. The change is limited to that
ticket-ready entry point and focused regression coverage. It does not change the
public API, schema, event shape, or the caller-owned `_in_tx` path.

Success means the public entry point waits for transient writer contention,
then preserves the existing ticket update and `ticket.ready` event in one
transaction. The focused test must force the contention rather than depend on
many probabilistic repetitions, and `just ci` must pass.

## Existing Decision

This change introduces no new architecture decision and therefore does not use
reserved ADR 0063. Commit `7b30ab2a8cd1768fe9322ab12a803eb46089ce78`
already established `BEGIN IMMEDIATE` for read-then-write control-plane
transactions under contention, and `cases::begin_immediate_tx` documents the
lock-upgrade failure that it prevents. Issue #453 is a missed application of
that policy.

## Considered Approaches

### Use `begin_immediate_tx` in the public ticket-ready entry point

This is the selected approach. The entry point reads ticket and dependency
state before updating the ticket and appending its event, so it must acquire
SQLite's write lock before those reads. The existing busy timeout then waits for
the current writer. Transaction ownership, statement ordering, and rollback
behavior remain unchanged after the begin operation.

### Change the shared `begin_tx` helper globally

Rejected because it would alter every control-plane transaction, including
write-first paths that do not have the lock-upgrade defect. That would widen
lock hold times and the review surface without being required by #453.

### Retry `SQLITE_BUSY` around the operation

Rejected because retrying an audited mutation risks replaying repository and
event work and creates a second contention mechanism beside SQLite's configured
busy handler. Acquiring the correct lock up front fixes the cause.

## Implementation

The public `ControlPlane::mark_ready_if_unblocked` wrapper will call
`begin_immediate_tx` instead of `begin_tx`. The `_in_tx` method remains unchanged
because its caller owns the outer transaction and therefore owns its lock policy.

The production wrapper will delegate to a private helper carrying an optional,
test-only transaction observer, following the existing heartbeat observer
pattern in `cases::execution::leases`. The observer pauses the operation
immediately after its transaction begins and never exists in non-test builds.

While the observed operation is paused, a separate connection with
`busy_timeout = 0` will attempt `BEGIN IMMEDIATE`. The attempt must return
`SQLITE_BUSY`, proving that `mark_ready_if_unblocked` already owns the writer
lock before its repository reads. Under the old deferred begin, the contender
instead acquires the writer lock, so the assertion deterministically fails.
The test will release the observer, then assert successful promotion plus exactly
one `ticket.ready` event. Notifications, rather than sleeps or elapsed-time
assumptions, coordinate every phase.

## Failure and Ordering Guarantees

- Failure to acquire the immediate transaction remains a contextual database
  error and occurs before any ticket or event mutation.
- After lock acquisition, ticket-state and dependency reads retain their current
  order, followed by ticket promotion, event append, and commit.
- A repository or event failure still rolls back both the ticket mutation and
  its event.
- Ineligible and missing tickets retain their existing results after contention
  clears.
- The test-only observer pauses after begin and before the first repository read;
  it cannot reorder production statements because it is absent outside tests.

## Verification

The regression must fail against the old deferred begin and pass with the
immediate begin. Run the focused test first, then `just ci`. No migration,
snapshot, or ADR-index update is expected.
