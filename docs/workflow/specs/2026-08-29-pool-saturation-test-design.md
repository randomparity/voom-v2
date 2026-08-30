# Pool-saturation test design

## Scope and goal

Issue #580 requires a deterministic regression test that drives more than eight concurrent lease
callers while a `BEGIN IMMEDIATE` writer holds SQLite's write lock, proves queued lease holders do
not expire spuriously, and proves the system converges after the writer releases. The permitted
surface is the lease repository test module, its existing fixtures, and this workflow's design
records. Production pool settings, transaction behavior, lease contracts, dependencies, the
general stress harness, and scheduled CI lanes are excluded.

The governing decision is
[ADR 0093](../../adr/0093-test-pool-saturation-with-queued-heartbeats.md).

## Design

Add one multi-thread Tokio test to
`crates/voom-store/src/repo/execution/leases_test.rs`:

1. Use the existing on-disk `setup()` fixture and acquire one held lease with a deadline later than
   the test's fixed heartbeat timestamp.
2. Open a `BEGIN IMMEDIATE` transaction from the same pool and retain it as the slow writer.
3. Spawn twelve public heartbeat calls. A thirteen-party barrier releases the tasks together. Each
   task pins and polls its heartbeat future once, increments `first_pending` only for `Pending`,
   then awaits the same future.
4. With a bounded timeout, wait until all twelve first polls are pending, `pool.size() == 8`, and
   `pool.num_idle() == 0`. Capture rather than immediately assert the observation and finished-task
   count. SQLx counts a slot in `size()` after reserving its capacity permit, potentially before a
   new connection finishes opening, so this proves eight active or reserved slots rather than eight
   physical checkouts. With the held writer owning one slot, at most seven heartbeat callers can
   hold or reserve the remaining capacity; at least five must await admission. This does not claim
   that the other seven calls have reached SQLite's write lock; issue #588 owns that observability
   gap.
5. Attempt to commit the writer and retain its `Result` without `?`, `unwrap`, `expect`, or an
   assertion; the commit consumes the transaction on either outcome. Join every task regardless of
   the commit result. Only then assert that commit succeeded, assert the captured saturation
   observation, and require every heartbeat to return the same held lease identity without a
   database or conflict error.
6. Read the lease and assert it remains held, its deadline is at least the original deadline, its
   last-heartbeat timestamp equals the fixed supplied value, and its epoch increased exactly
   twelve times. Issue one more heartbeat and require its epoch to advance, proving convergence
   after the saturated queue drains.

The test uses real Tokio time because it touches SQLite. The timeout bounds only the diagnostic
wait for the observable saturated state; its failure message reports pool size, idle connections,
and finished-task count. No production timeout is consumed because the writer releases as soon as
the state is observed.

## Failure contract

A task panic, join error, `DB_UNREACHABLE`, lease conflict, a first poll that completes while the
writer is held, failure to observe twelve pending first polls or all eight pool slots active or reserved,
a shortened deadline, an expired or released lease, or an incorrect epoch fails the test. All
spawned tasks are joined after writer release before any commit-result,
captured-observation, or task-result assertion can fire, so a red test does not strand lock
waiters.

## Verification

- Run the focused test and require it to pass in under ten seconds.
- Run it through `just test-repeat voom-store pool_saturation 25` and require all repetitions to
  pass.
- Verify the test bites by temporarily reducing the caller count to seven, observe the explicit
  caller-count-versus-non-writer-slots assertion fail after cleanup, restore twelve, and rerun
  green.
- Run `just fmt-check`, `just lint`, `just check-test-layout`, `just check-paused-time-db`,
  `just check-transaction-openers`, and `just test` before shipping.
- No target architecture is declared; the host is x86_64 and the test is architecture-neutral.

## Durable workflow checkpoint

- Branch: `feat/pool-saturation-test-580`
- Base branch: `main`
- Scope token: `q580-fc6e0258`
- Guardrails: `just fmt-check`; `just lint`; `just check-test-layout`;
  `just check-paused-time-db`; `just check-transaction-openers`; `just test`; `just ci`
- Open findings and deferrals: SQLite lock-wait observability remains with #588. Issue comment
  `5465825708` authorizes narrowing #580 to deterministic pool-admission and convergence coverage;
  all original scope exclusions remain in force.
