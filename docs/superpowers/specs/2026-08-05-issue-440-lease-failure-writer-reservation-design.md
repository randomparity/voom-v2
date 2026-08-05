# Issue 440: Lease-failure writer reservation design

## Problem

`ControlPlane::fail_lease` opens a deferred SQLite transaction. Its repository call first
reads the held lease and ticket attempt, then releases the lease and transitions the ticket.
When several dispatch-cleanup tasks fail leases concurrently, each transaction can establish
a read snapshot before its first write. A sibling writer can then commit, leaving the reader
unable to upgrade its stale snapshot. The outer eight-attempt lock retry can exhaust and the
aborted cleanup leaves its lease held.

## Scope and invariants

This change is limited to the direct `fail_lease` transaction boundary and focused tests.

- Reserve SQLite's writer at the start of the direct failure transaction.
- Keep validation, lease mutation, ticket mutation, terminal-issue creation, event append,
  rollback, and commit order unchanged.
- Keep caller-owned `fail_lease_in_tx` behavior unchanged.
- Let SQLite's configured busy timeout serialize writers; do not add or widen retries.
- Use real Tokio time and real SQLite. Synchronize concurrency tests with `Notify` and durable
  transaction state, never sleeps or elapsed-time assertions.
- Do not change schemas, public APIs, error codes, or unrelated lease lifecycle operations.

These requirements come from issue #440 and the active campaign dispatch. The accepted
control-plane architecture already identifies `BEGIN IMMEDIATE` as the serialization boundary
for lease release transactions, and `begin_immediate_tx` is the existing implementation. This
repair applies that decision; it introduces no new architectural choice and needs no ADR.

## Considered approaches

### Reserve the writer at the direct use-case boundary (selected)

Replace `begin_tx` with `begin_immediate_tx` in `ControlPlane::fail_lease`. Writer acquisition
then happens before the first held-lease/ticket read and covers the existing mutation-and-event
transaction unchanged. Contending failure cleanups wait under SQLite's configured busy timeout
instead of creating snapshots that cannot later be upgraded.

### Change the store-owned convenience method

Rejected. Workflow cleanup calls the control-plane use case, whose transaction also owns event
appends and terminal issue creation. Changing only `SqliteLeaseRepo::fail` would not affect that
path and would put the reservation outside the actual atomic boundary.

### Rely on or extend the outer retry loop

Rejected. Retrying after a stale-snapshot failure is the current behavior and has already
exhausted under concurrent cleanup. Increasing the retry budget changes latency and replay
policy while leaving the incorrect transaction boundary in place.

## Deterministic regressions

Add two focused tests around the production use case, one for a retriable failure and one
for a terminal failure:

1. Configure both pool connections with zero busy timeout and hold a writer transaction on one.
2. Snapshot the target lease, ticket, event counts, and terminal-issue rows, then fail the held
   lease through `ControlPlane::fail_lease` on the other connection.
3. The corrected path reports contention from `begin immediate`; the deferred path completes
   its read and fails at a later write, so the exact error-boundary assertion deterministically
   reddens before the fix.
4. Assert every snapshot is unchanged. The retriable case covers the ready transition contract;
   the terminal case covers failed-ticket, issue-creation, and terminal-event ordering.

Run the existing three-concurrent-crash executor regression as the end-to-end proof that every
cleanup reaches terminal durable state before invocation failure returns.

## Failure behavior and rollback

Failure to acquire the writer remains a contextual database error, now identified at `begin
immediate`. Once acquired, every domain rejection and event-append failure follows the existing
transaction and rollback behavior. A normal code revert restores the deferred transaction and
the known race; there is no migration or external cleanup.

## Verification

- Demonstrate the focused regressions fail with the deferred opener and pass with the immediate
  opener.
- Run the existing concurrent-crash executor regression.
- Run the affected control-plane lease tests and clippy target.
- Run bare `just ci` and `just smoke`.
- Adversarially review lock lifetime, rollback, mutation/event ordering, and test determinism.
  No trust boundary or dependency changes, so a threat scan is not triggered.
