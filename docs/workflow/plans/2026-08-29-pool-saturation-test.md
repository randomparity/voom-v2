# Pool-saturation test implementation plan

## Goal

Add a deterministic lease-repository regression test proving that more callers than the on-disk
SQLite pool can serve queue safely behind a held writer and converge after release.

ADR 0093 selects twelve public heartbeats against one held lease. The test proves the pool is fully
occupied and at least five heartbeat callers are waiting for admission, releases the writer before
asserting any captured failure, and verifies durable lease state afterward. It makes no claim that
the other calls reached SQLite's write lock; issue #588 owns that observability.

Tech stack: Rust, Tokio multi-thread tests, sqlx SQLite, existing `voom-test-support` fixtures.

## Global constraints

- Branch: `feat/pool-saturation-test-580`; base: `main`; scope token: `q580-fc6e0258`.
- Host architecture: x86_64; target architectures: none declared; relationship:
  no-target-declared.
- Modify only `crates/voom-store/src/repo/execution/leases_test.rs`, plus the committed design
  records. Production edits are transient only for the bite check and must be restored.
- Use the existing real on-disk `setup()` fixture and real Tokio time. Never pause or advance
  Tokio time around the SQLite pool.
- Use twelve public heartbeat callers plus one held `BEGIN IMMEDIATE` writer. Do not shorten
  production lock-wait or pool-acquire budgets and do not add a pool constructor.
- Poll every heartbeat future once after the barrier and count only first polls that return
  `Pending`. Capture the full-pool observation and writer commit result, consume the writer, and
  join all tasks before asserting any captured failure.
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

1. Keep the current twelve-caller barrier and first-poll synchronization. Bounded-wait for all
   twelve first polls to return `Pending`, `pool.size() == 8`, and `pool.num_idle() == 0`. Capture
   the observation and finished-task count without asserting.
2. Commit the held writer and capture its result, then join all twelve tasks regardless of any
   captured failure. Only afterward assert the writer result, observations, that `CALLERS` exceeds
   the seven connections available beside the writer, zero premature finishes, and each heartbeat
   result. Assert the stored lease and final uncontended heartbeat exactly as in the current test.

3. Run
   `cargo test -p voom-store pool_saturation_queues_heartbeats_until_writer_releases`; expect one
   passed test and elapsed test time below ten seconds. If the compiler identifies an existing
   interface mismatch, correct only the borrowed signature or type named by that diagnostic and
   keep the behavior above unchanged.
4. Verify the test bites: temporarily set `CALLERS` to `7`, rerun the same focused command, and
   require failure at the `callers must exceed ... non-writer connections` assertion after every
   spawned task has joined. Restore `CALLERS` to `12` with `apply_patch`, rerun the focused command,
   and require it green. Do not commit the controlled fault.
5. Run
   `just test-repeat voom-store pool_saturation_queues_heartbeats_until_writer_releases 25`; expect
   all 25 repetitions to pass and each iteration to stay below the issue's ten-second default-suite
   threshold.
6. Run `just fmt-check`, `just lint`, `just check-test-layout`, `just check-paused-time-db`,
   `just check-transaction-openers`, and `just test`; expect every command to exit zero with no
   skipped test reported by the focused or repeated runs.
7. No further test-code commit is required if the implementation already matches this narrowed
   plan. Commit only the corrected design records after review and verification.

Acceptance:

- The test observes twelve public heartbeat futures `Pending` on their first poll, eight
  checked-out connections, zero idle connections, zero completed heartbeats, and more callers than
  the seven non-writer slots. These facts prove at least five callers wait for pool admission.
- Writer commit is captured, all task handles are joined, and only then can assertions panic.
- Every heartbeat succeeds; the lease stays held, does not shorten its deadline, records the fixed
  heartbeat time, and increments its epoch once per caller.
- A later uncontended heartbeat succeeds and advances the epoch once more.
- The controlled seven-caller fault fails, the focused test stays below ten seconds, and 25
  repeated runs pass.

## Durable workflow checkpoint

- Current phase: operator-approved scope narrowing; re-review precedes final verification.
- Branch: `feat/pool-saturation-test-580`; base branch: `main`.
- Scope token: `q580-fc6e0258`.
- Open findings and deferrals: SQLite lock-stage observability remains with #588. Issue comment
  `5465825708` authorizes the narrowed pool-admission and convergence proof; all original
  exclusions remain unchanged.
