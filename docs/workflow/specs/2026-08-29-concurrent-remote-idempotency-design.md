# Concurrent remote idempotency design

## Scope and goal

Issue #579 requires deterministic tests proving that concurrent remote acquire and complete
requests with the same idempotency key cause exactly one mutation. The permitted surface is the
remote-execution test module, its existing fixtures, and this workflow's design records. Runtime
behavior, schemas, dependencies, public contracts, broader stress infrastructure, and CI lanes are
excluded.

The governing decision is [ADR 0091](../../adr/0091-test-idempotency-at-the-remote-use-case-boundary.md).

## Design

Add two tests to
`crates/voom-control-plane/src/cases/execution/remote_execution/mod_test.rs`, beside the existing
sequential replay tests:

1. Acquire: create one ready ticket, clone one same-key/same-hash input into six tasks, and release
   them with a seven-party barrier (six tasks plus the test). Classify every result after all tasks
   join. A successful result must be `Leased`, and every success must name the same lease. A failure
   is acceptable only when its public code is `CONFLICT` and its message identifies an idempotency
   key already in progress. Then assert one lease row, one `LeaseAcquired` event, exactly one
   scheduler-decision row, and one ticket attempt. Every successful/replayed outcome must reference
   that same durable scheduler decision; an in-progress conflict creates no decision.
2. Complete: start from one held lease, race six clones of one same-key/same-hash completion input
   through the same barrier pattern, and classify all results after join. Successful outcomes must
   be identical and name the original lease; only the clean in-progress conflict is acceptable.
   Then assert the lease was released once, the ticket succeeded, the artifact access plan is
   consumed, and the `LeaseReleased` and `TicketSucceeded` events each occur once.

Both tests use `#[tokio::test(flavor = "multi_thread", worker_threads = 4)]`, the existing
`TempDatabase`-backed fixture, and real Tokio time. A local helper may classify the one permitted
conflict only if it is used by both tests; otherwise keep the assertions inline.

## Failure contract

Task panics and join failures fail the test. `DB_UNREACHABLE`, raw SQLite errors, `SQLITE_BUSY`,
authentication failures, different-body conflicts, and any non-idempotency conflict fail the test.
Assertions are delayed until all task results have been collected so one failed result does not
strand barrier participants or hide the durable post-race observations.

## Verification

- Run each new test directly and through `just test-repeat voom-control-plane <filter> 25`.
- Verify the tests bite by temporarily removing the reservation insert's
  `ON CONFLICT ... DO NOTHING`, observing a focused new test fail, restoring the production file,
  and rerunning it green.
- Run `just fmt-check`, `just lint`, `just check-test-layout`,
  `just check-paused-time-db`, `just check-transaction-openers`, and `just test` before shipping.
- No target architecture is declared; the host is x86_64 and the tests are architecture-neutral.

## Durable workflow checkpoint

- Branch: `feat/concurrent-idempotency-test-579`
- Base branch: `main`
- Scope token: `q579-c2e0fbc8`
- Open findings and deferrals: none at design creation
