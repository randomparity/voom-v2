# Pool-saturation test implementation plan

## Goal

Add a deterministic lease-repository regression test proving that more callers than the on-disk
SQLite pool can serve queue safely behind a held writer and converge after release.

ADR 0093 selects seven transaction-scoped heartbeats plus five public heartbeats against one held
lease. The staged test proves seven writes are waiting behind the writer and five callers are
waiting for pool admission, releases the writer before asserting any captured failure, and verifies
durable lease state afterward.

Tech stack: Rust, Tokio multi-thread tests, sqlx SQLite, existing `voom-test-support` fixtures.

## Global constraints

- Branch: `feat/pool-saturation-test-580`; base: `main`; scope token: `q580-fc6e0258`.
- Host architecture: x86_64; target architectures: none declared; relationship:
  no-target-declared.
- Modify only `crates/voom-store/src/repo/execution/leases_test.rs`, plus the committed design
  records. Production edits are transient only for the bite check and must be restored.
- Use the existing real on-disk `setup()` fixture and real Tokio time. Never pause or advance
  Tokio time around the SQLite pool.
- Use seven admitted and five queued heartbeat callers plus one held `BEGIN IMMEDIATE` writer. Do
  not shorten production lock-wait or pool-acquire budgets and do not add a pool constructor.
- Open seven deferred transactions before starting the five public calls. Poll the public calls
  once to prove pool-admission waits, then gate the seven `heartbeat_in_tx` writes and poll them
  once to prove SQLite writer waits. Capture observations and writer commit result, consume the
  writer, and join all tasks before asserting any captured failure.
- Guardrails: `cargo test -p voom-store pool_saturation_queues_heartbeats_until_writer_releases`;
  `just test-repeat voom-store pool_saturation_queues_heartbeats_until_writer_releases 25`;
  `just fmt-check`; `just lint`; `just check-test-layout`; `just check-paused-time-db`;
  `just check-transaction-openers`; `just test`; and finally `just ci`.

## Task 1 — Prove saturation, liveness, and recovery

Files:

- Modify and test: `crates/voom-store/src/repo/execution/leases_test.rs`.

Interfaces:

- Consume the existing `setup()`, `SqliteLeaseRepo::acquire`, `SqliteLeaseRepo::heartbeat`,
  `SqliteLeaseRepo::get`, `NewLease`, `LeaseState`, `T0`, and on-disk `SqlitePool` returned by the
  fixture.
- Consume sqlx 0.8.6 `Pool::size() -> u32` and `Pool::num_idle() -> usize`, verified in the pinned
  dependency source.
- Produce one test named `pool_saturation_queues_heartbeats_until_writer_releases`.
- No production or later-task interface is added.

Steps:

1. Replace the current one-stage test with a staged proof. Define `ADMITTED = 7`, `QUEUED = 5`,
   and `CALLERS = ADMITTED + QUEUED`. After opening the held writer, spawn seven tasks that each
   open a deferred test transaction, increment `transactions_open`, and wait on a cloned
   `watch<bool>` receiver. Bounded-wait for all seven transactions and a full pool.
2. Spawn five public-heartbeat tasks. Each pins its heartbeat, polls it exactly once, increments
   `admission_pending` only for `Pending`, and then awaits the same future. Bounded-wait for all
   five; at this stage every pool connection is already owned.
3. Send `true` on the watch gate. Each admitted task pins `heartbeat_in_tx` against its already-open
   transaction, polls it exactly once, increments `write_pending` only for `Pending`, awaits the
   same future, and commits. Bounded-wait for all seven pending writes. Capture each timeout and
   the finished-task counts without asserting.
4. Commit the held writer and capture its result, then join all twelve tasks regardless of any
   captured failure. Only afterward assert the writer result, observations, `CALLERS > pool.size()`,
   zero premature finishes, and each heartbeat result. Assert the stored lease and final
   uncontended heartbeat exactly as in the current test.

5. Run
   `cargo test -p voom-store pool_saturation_queues_heartbeats_until_writer_releases`; expect one
   passed test and elapsed test time below ten seconds. If the compiler identifies an existing
   interface mismatch, correct only the borrowed signature or type named by that diagnostic and
   keep the behavior above unchanged.
6. Verify the test bites: temporarily set `QUEUED` to `0`, rerun the same focused command, and
   require failure at the `callers must exceed ... pool connections` assertion after every
   spawned task has joined. Restore `QUEUED` to `5` with `apply_patch`, rerun the focused command,
   and require it green. Do not commit the controlled fault.
7. Run
   `just test-repeat voom-store pool_saturation_queues_heartbeats_until_writer_releases 25`; expect
   all 25 repetitions to pass and each iteration to stay below the issue's ten-second default-suite
   threshold.
8. Run `just fmt-check`, `just lint`, `just check-test-layout`, `just check-paused-time-db`,
   `just check-transaction-openers`, and `just test`; expect every command to exit zero with no
   skipped test reported by the focused or repeated runs.
9. Commit the focused correction as `test: prove both saturation wait layers` after the guardrails
   are green.

Acceptance:

- Seven deferred transactions occupy the non-writer connections before five public calls start.
- The test observes five public heartbeat futures pending at pool admission and seven
  transaction-scoped heartbeat writes pending behind the SQLite writer, with eight checked-out
  connections, zero idle connections, zero completed heartbeats, and more callers than pool slots.
- Writer commit is captured, all task handles are joined, and only then can assertions panic.
- Every heartbeat succeeds; the lease stays held, does not shorten its deadline, records the fixed
  heartbeat time, and increments its epoch once per caller.
- A later uncontended heartbeat succeeds and advances the epoch once more.
- The controlled zero-queued-caller fault fails, the focused test stays below ten seconds, and 25
  repeated runs pass.

## Durable workflow checkpoint

- Current phase: operator-approved staged-proof design amendment; re-review and scope audit precede
  the fix wave.
- Branch: `feat/pool-saturation-test-580`; base branch: `main`.
- Scope token: `q580-fc6e0258`.
- Open findings and deferrals: none. Issue comment `5465708409` authorizes only the staged
  seven-write/five-admission test proof; all original exclusions remain unchanged.
