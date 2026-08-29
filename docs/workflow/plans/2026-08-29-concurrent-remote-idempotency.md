# Concurrent remote idempotency implementation plan

## Goal

Add deterministic contention tests proving that same-key remote acquire and complete deliveries
produce one durable mutation and only caller-visible replay or clean in-progress conflict results.

The tests exercise `ControlPlane` methods, because ADR 0091 requires proof across reservation,
domain mutation, response completion, and commit. They reuse the existing on-disk SQLite
`RemoteFixture`; no production API or helper is added.

Tech stack: Rust, Tokio multi-thread tests, sqlx SQLite, existing `voom-test-support` fixtures.

## Global constraints

- Branch: `feat/concurrent-idempotency-test-579`; base: `main`; scope token:
  `q579-c2e0fbc8`.
- Host architecture: x86_64; target architectures: none declared; relationship:
  no-target-declared.
- Modify only
  `crates/voom-control-plane/src/cases/execution/remote_execution/mod_test.rs`, plus the existing
  design records. Production edits are transient only for the bite check and must be restored.
- Run independently initialized races with two and six contenders, each using a
  `tokio::sync::Barrier` sized to the contenders plus the test,
  `#[tokio::test(flavor = "multi_thread", worker_threads = 4)]`, the existing real on-disk SQLite
  fixture, and real Tokio time.
- Accept only a successful stored outcome or `VoomError::Conflict` whose message contains
  `idempotency key is already in progress`; every other error fails.
- Guardrails: `just fmt-check`, `just lint`, `just check-test-layout`,
  `just check-paused-time-db`, `just check-transaction-openers`, `just test`, and finally `just ci`.

## Task 1 — Prove concurrent acquire idempotency

Files:

- Modify and test:
  `crates/voom-control-plane/src/cases/execution/remote_execution/mod_test.rs`.

Interfaces:

- Consume `RemoteFixture::ready_ticket`, `RemoteFixture::acquire_input`,
  `ControlPlane::remote_acquire`, `RemoteAcquireOutcome::Leased`,
  `RemoteLeaseDispatch::{lease_id,ticket_id,scheduler_decision_id}`, `count`, and
  `SchedulerDecisionFilter::default()` exactly as they exist in the test module.
- Produce one new test named `concurrent_same_key_remote_acquire_mutates_once`.
- Task 2 reuses only the test's barrier/task pattern; it does not call a new helper.

Steps:

1. Add the multi-thread Tokio test and iterate over `[2_usize, 6]`. For each count create a fresh
   fixture and ready ticket, capture its pre-race `attempt`, build `Arc<Barrier>` with
   `contenders + 1`, and spawn that many tasks. Each task clones the `ControlPlane`, input, and
   barrier, waits, then calls `remote_acquire`.
2. For each count release the barrier from the test, await all handles, and classify every result
   without asserting until all handles have joined. Collect leased dispatches. Count only the
   exact in-progress conflict as acceptable; panic after collection for any other error or
   outcome.
3. For each count assert at least one leased result, every leased result is identical, and every
   dispatch names the prepared ticket. Query durable state and assert one lease row, one
   scheduler-decision row, one ticket attempt increment, and one `LeaseAcquired` event. Assert the
   sole decision id equals every successful dispatch's `scheduler_decision_id`.
4. Run
   `cargo test -p voom-control-plane concurrent_same_key_remote_acquire_mutates_once`; expect one
   passed test.
5. Commit the focused test as `test: race concurrent remote acquire replays` after the guardrails
   relevant to this file are green.

Acceptance:

- Both two and six calls begin from their own barrier release against fresh fixtures.
- No database error is accepted or converted to a conflict.
- Durable lease, ticket-attempt, event, and scheduler-decision observations each prove one
  mutation while successful callers share the same stored outcome.

## Task 2 — Prove concurrent complete idempotency

Files:

- Modify and test:
  `crates/voom-control-plane/src/cases/execution/remote_execution/mod_test.rs`.

Interfaces:

- Consume `leased_fixture`, `fixture_lease_id`, `RemoteFixture::complete_input`,
  `ControlPlane::remote_complete`, `RemoteCompleteOutcome`, `count`, and the existing lease,
  ticket, and artifact-access-plan repository accessors.
- Produce one test named `concurrent_same_key_remote_complete_mutates_once`.
- No later implementation task consumes a new code interface.

Steps:

1. Add the multi-thread test and iterate over `[2_usize, 6]`. For each count start from a fresh
   `leased_fixture`, capture its lease id, clone one same-key/same-hash completion input into that
   many barrier-released tasks, and call `remote_complete` from each task.
2. For each count join every task before asserting. Collect successful outcomes and exact
   in-progress conflicts; record every other error as an unexpected result that fails the test
   after collection.
3. For each count assert at least one success, all successes equal the first outcome, and every
   success names the original lease. Assert the lease has one release timestamp, its ticket is
   `Succeeded`, its artifact plan is `Consumed`, and `LeaseReleased` plus `TicketSucceeded` event
   counts are one.
4. Run
   `cargo test -p voom-control-plane concurrent_same_key_remote_complete_mutates_once`; expect one
   passed test.
5. Commit as `test: race concurrent remote complete replays` after focused guardrails are green.

Acceptance:

- Both two and six completion callers start together against fresh held leases and keys.
- Every loser is a stored success replay or the clean in-progress conflict.
- Durable completion state and events prove the completion mutation happened once.

## Task 3 — Prove bite and stability

Files:

- Temporarily modify, then restore before any commit:
  `crates/voom-store/src/repo/execution/remote_idempotency.rs`.
- Verify the two committed tests in
  `crates/voom-control-plane/src/cases/execution/remote_execution/mod_test.rs`.

Interfaces:

- The temporary fault removes only the literal `ON CONFLICT(node_id, route_key,
  worker_scope_id, idempotency_key) DO NOTHING` clause from `reserve_or_replay_in_tx`.
- The final branch must retain the original production query byte-for-byte.

Steps:

1. Save the production file's blob id with `git hash-object`, remove only the conflict clause via
   `apply_patch`, and run each focused new test. Expect failure caused by a duplicate-key database
   error; a pass means the test does not bite and Task 1 or 2 must be corrected.
2. Restore the exact clause with `apply_patch`, require `git diff --exit-code --
   crates/voom-store/src/repo/execution/remote_idempotency.rs`, and verify its blob id matches the
   saved id.
3. Run `just test-repeat voom-control-plane concurrent_same_key_remote_acquire_mutates_once 25`
   and `just test-repeat voom-control-plane concurrent_same_key_remote_complete_mutates_once 25`;
   expect `no failure in 25 runs` from both.
4. Run the Global Constraints guardrails and `just ci`; expect exit 0 with no skipped checks beyond
   recipe-declared inapplicable hooks. Commit only any test correction needed by this proof, never
   the fault.

## Durable workflow checkpoint

- Current phase after this plan: design review, then oathbind scope audit.
- Open findings: none after the corrected specification confirming pass.
- Deferrals: none.
- ADR review: approve in one iteration. Spec review: one accepted-fixed finding, then approve.
