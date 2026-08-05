# Issue 435: Heartbeat and audio-extract prepare contention design

## Problem

The source-backed adapter heartbeat test exercises audio extraction after a worker has
returned. During that terminal work, the lease heartbeat and audio-extract prepare use
separate SQLite connections. `prepare_extract_set` currently opens a deferred transaction,
reads the commit-safety gate, and only then attempts its first write when it inserts an
artifact commit record.

If a heartbeat commits between that read and write, SQLite cannot upgrade the stale read
snapshot. The insert fails with `SQLITE_BUSY_SNAPSHOT` (extended code 517), reported as
`artifact_commit_records insert: database is locked`. A rerun changes only incidental task
scheduling and therefore cannot prove the heartbeat behavior.

## Scope and invariants

This change is limited to the audio-extract prepare transaction, the direct lease-heartbeat
transaction boundary, and regressions for their interaction.

- Preserve the existing read-only gate diagnostic before contended writer acquisition, then
  reserve SQLite's writer before the authoritative prepare transaction's gate read.
- Keep the gate read, pending commit records, started events, and operation preparation in
  their existing transaction and order.
- Let `busy_timeout` serialize a concurrent heartbeat; do not add another retry policy.
- Begin the direct heartbeat use case with the same immediate-writer invariant. The heartbeat
  update, audio-claim renewals, and result read remain in their existing transaction and order.
- Preserve heartbeat ownership, lease release/failure behavior, and all filesystem mutation
  ordering.
- Use real Tokio time with the real SQLite pool. Synchronize tests with notifications and
  semaphores, not sleeps or elapsed-time claims.
- Do not change schemas, APIs, error codes, timeouts, or unrelated audio behavior.

These guarantees come from issue #435 and the campaign's approved scope expansion. The
existing `begin_immediate_tx` helper and the established read-then-write transaction pattern
implement them; this design does not introduce a new architectural decision.

## Considered approaches

### Reserve the writer at transaction start (selected)

Run the gate once in a short read-only preflight transaction, then replace the durable
prepare transaction's deferred begin with `begin_immediate_tx` and re-run the authoritative
gate check inside it. The preflight preserves the existing domain refusal when a blocking
use lease and writer contention coexist. The authoritative check closes the preflight-to-lock
race before any pending record or event is written.

The heartbeat use case also changes from deferred begin to `begin_immediate_tx`. Its current
first SQL statement is already the lease update, so this does not repair another stale read
snapshot. It makes the contention contract explicit at transaction acquisition: one direct
heartbeat database attempt waits under the configured SQLite busy timeout before it reads or
renews any durable lease or claim state. This matches the control plane's established writer
transaction pattern and changes neither transaction's contents nor commit point.

### Retry the complete prepare transaction

Rejected. Retrying a multi-step orchestration sequence would duplicate policy in the
executor, requires proving which steps are replay-safe, and is explicitly outside the
issue. The lock can be avoided at acquisition instead.

### Serialize or suppress the heartbeat in the test

Rejected. That would make the test deterministic by removing the production concurrency it
is meant to cover. It would leave the same stale-snapshot failure reachable outside the
test.

## Deterministic regression

Add a test-only synchronization point immediately after the authoritative audio-extract gate
read. The regression pauses the prepare transaction there and probes a second connection
with a zero-wait `BEGIN IMMEDIATE`:

1. On the corrected path, the probe receives `SQLITE_BUSY`, proving prepare already owns
   the writer reservation before any artifact write.
2. The test keeps the zero-wait probe connection checked out so its connection-local
   `busy_timeout` cannot return to and contaminate the pool. While prepare remains held, a
   heartbeat task checks out and pins a different pool connection, then calls the exact
   transaction opener used by the production heartbeat path. A `cfg(test)` observer around
   that shared opener records its invocation and successful acquisition without replacing
   or bypassing production behavior.
3. After the opener-invoked signal, the same production heartbeat call must remain
   incomplete while prepare is held. The observer and the outer executor retry seam both
   count attempts; each must remain exactly one. This is an observable deterministic
   serialization proof, not a claim that the test can observe SQLite's internal busy-handler
   callback: `sqlx-sqlite` exposes no such public hook, and adding unsafe SQLite FFI or a new
   dependency is outside this change.
4. The test releases prepare, waits for its transaction to end, and asserts that the same
   heartbeat task completes successfully and advances the target lease. The unchanged
   single-attempt counters prove success did not come from a second database attempt or the
   executor retry wrapper.
5. Against the old deferred begin, the probe acquires the writer while prepare owns only a
   read snapshot, so the assertion fails deterministically before implementation.

The existing end-to-end post-dispatch test remains the behavioral proof that heartbeats
continue through terminal adapter work and the workflow reaches its intended result.

## Failure behavior

A blocking use lease is still reported by the read-only preflight before any contended
writer acquisition. If the preflight passes, failure to acquire `BEGIN IMMEDIATE` is a
contextual database error at the durable transaction start. The gate is evaluated again
after acquisition, so a lease that becomes blocking between the checks still fails closed
before any pending record or event. Failures after acquisition retain the current rollback
behavior and ordering. No database lock is held across filesystem promotion; only the
existing durable prepare transaction is serialized.

The heartbeat still performs its lease update first, then renews extraction claims, then
renews synthesis claims. Missing or non-held leases, expired claims, and claims owned by a
different lease therefore retain their current domain outcomes. The immediate begin only
moves writer acquisition ahead of those authoritative operations; it adds no preflight read
that could become stale and no retry inside the use case.

Direct before/after durable-row assertions define the diagnostic and rollback matrix:

- a missing or non-held lease returns the existing conflict and changes no extraction or
  synthesis claim;
- an expired extraction claim returns its existing conflict and rolls back the lease plus
  every extraction and synthesis claim mutation;
- an expired synthesis claim returns its existing conflict and likewise rolls back the
  earlier lease and extraction renewals;
- a claim owned by a different lease remains byte-for-byte unchanged while the target held
  lease and only its live claims advance.

## Verification

- Run the new focused reservation regression and demonstrate its red result against the
  deferred begin.
- Run a focused coexistence regression proving a blocking use lease remains the diagnostic
  before contended writer acquisition.
- Run direct lease-heartbeat regressions for missing/non-held leases and expired or
  differently owned audio claims, comparing the durable lease, extraction-claim, and
  synthesis-claim rows before and after to prove diagnostics and atomic mutation ordering.
- Run the focused post-dispatch heartbeat and heartbeat-failure tests.
- Run all-feature `voom-control-plane` library tests.
- Run the repository's bare `just ci` suite.
- Adversarially review the branch for races, lock lifetime, failure ordering, and test
  determinism. The diff adds no trust boundary, secret, input parser, permission, dependency,
  or security-relevant default, so a threat scan is not triggered unless implementation
  widens beyond this design.

## Rollback

A normal code revert restores the deferred transaction behavior and the known flake. There
is no data migration, persisted format change, or external cleanup.
