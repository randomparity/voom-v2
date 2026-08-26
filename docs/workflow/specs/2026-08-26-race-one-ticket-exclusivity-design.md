# Race N claimers at one ticket — design

Issue: [#578](https://github.com/randomparity/voom-v2/issues/578)
Epic: [#577](https://github.com/randomparity/voom-v2/issues/577)
ADR: [0085](../../adr/0085-contention-tests-at-the-use-case-level.md)

## Goal

Prove, under real contention, that at most one worker can hold a lease on one
ticket — on the node-local claim path and on the multi-node remote claim path.

## Background

Claim exclusivity rests on one compare-and-swap in
`SqliteLeaseRepo::acquire_guarded`
(`crates/voom-store/src/repo/execution/leases.rs:415-424`):

```sql
UPDATE tickets
   SET state = 'leased', state_changed_at = ?, attempt = attempt + 1,
       epoch = epoch + 1
 WHERE id = ? AND state = 'ready' AND next_eligible_at <= ?
       AND attempt < max_attempts
       AND (job_id IS NULL OR EXISTS (
             SELECT 1 FROM jobs WHERE jobs.id = tickets.job_id
                                  AND jobs.state = 'open'))
```

`rows_affected() == 0` returns `LeaseAcquireOutcome::TicketNotReady`. The
statement runs inside the savepoint `try_acquire_in_tx` opens
(`leases.rs:334-350`); every non-`Acquired` outcome rolls that savepoint back, so
a claimer that loses must leave `attempt` and `epoch` untouched.

No test races two claimers at one ticket. The existing concurrent tests
(`crates/voom-control-plane/src/cases/execution/leases_test.rs:1246-1290`,
`crates/voom-control-plane/src/cases/execution/remote_execution/mod_test.rs:759-813`)
race N claimers over N *distinct* tickets under a capacity limit and assert "at
most one lease per worker". Dropping `state = 'ready'` from the CAS would not
redden any of them.

## Requirements

| # | Requirement | Source |
|---|---|---|
| R1 | A test races 6 workers on one node at one ready ticket via `ControlPlane::try_acquire_lease`. | #578 Change (1) |
| R2 | A test races 6 workers across 6 registered nodes at one ready ticket via `ControlPlane::remote_acquire`. | #578 Change (2) |
| R3 | Each test asserts exactly one claimer acquires. | #578 Change |
| R4 | Each test asserts every loser settles cleanly with zero errors. | #578 Change |
| R5 | Each test asserts `COUNT(*) FROM leases WHERE state = 'held' == 1`. | #578 Change |
| R6 | Each test asserts the ticket's `attempt` incremented exactly once. | #578 Change |
| R7 | Each test asserts every loser lost **at the CAS**, not at an earlier gate. | ADR 0085; necessary consequence of R3–R6 (see *Why R7 is necessary*) |
| R8 | Both tests run in the default `just test` suite with no `#[ignore]`. | #578 Acceptance |
| R9 | Both tests are verified to bite: weaken the CAS locally, observe failure, revert. | #578 Acceptance |
| R10 | Both tests are stable under `just test-repeat` at `COUNT=25` and under both `just test-serial` and `just test-parallel`. | #578 Acceptance |
| R11 | `just ci` is green. | `AGENTS.md` § Commands |

### Why R7 is necessary

R3–R6 are all satisfied by a run in which every loser was rejected by a
capacity or eligibility gate and never executed the CAS at all: one claimer
acquires, the rest settle cleanly, one lease is held, `attempt` is 1. Such a run
proves nothing about exclusivity while being indistinguishable from one that
does. No reasonable implementation of R3–R6 proves what #578 asks for without
R7, which is what makes it a necessary consequence of the sourced criteria
rather than an added promise.

## Non-goals

Owned elsewhere and deliberately not addressed here: concurrent
same-idempotency-key delivery (#579); pool saturation beyond `max_connections`
(#580); a multi-runner × multi-node stress harness, ticket-conservation
assertions, and socket-level or multi-process transport (#581); a scheduled
constrained-resource CI lane (#582); ENOSPC coverage (#583); a property-based
state-machine suite (#584). Deterministic distributed simulation and performance
budgets are epic #577 non-goals.

## Global constraints

- **Claimer count is 6**, against the pool's `max_connections = 8`
  (`crates/voom-store/src/pool.rs:62,67`). Per ADR 0083 the acquire paths open
  `BEGIN IMMEDIATE` and hold their pooled connection across the whole write-lock
  wait, so a count above the pool measures `acquire_timeout` (45s,
  `pool.rs:76`) instead of the CAS.
- **`busy_timeout` is 30s** (`pool.rs:41`); a serialized claimer waits, it does
  not fail.
- **No `tokio::time::pause`/`advance` in either test file.** ADR 0012, enforced
  by `just check-paused-time-db`. Domain time comes from the caller-supplied
  `now` and from the fixture clock.
- **Sibling test layout.** ADR 0004, enforced by `just check-test-layout`. Both
  target files already exist and are already linked by `#[path]`; no new
  linkage is needed.
- **Tests run on the pinned `.test-tmp/` root.** ADR 0079. `TempDatabase`
  handles this; no test may name its own temp directory.
- **Clippy runs `--all-targets --all-features -- -D warnings`**, and
  `[workspace.lints]` denies `panic`/`unwrap`/`expect` in production code. Test
  code in these files already uses `.unwrap()` freely; match the surrounding
  style.

## Design

### Test A — node-local claimers

File: `crates/voom-control-plane/src/cases/execution/leases_test.rs`
Name: `concurrent_local_acquire_at_one_ticket_leases_exactly_once`

Fixture: the existing `cp()` helper (`crates/voom-control-plane/src/cases/mod_test.rs:22`),
which builds a real on-disk WAL database via `TempDatabase` and a `SystemClock`.
Domain time is supplied per call as `T0` (`OffsetDateTime::UNIX_EPOCH`), matching
every other test in the file.

Setup:

1. One ticket from the file's `ticket("noop", 2)` helper, then
   `mark_ready_if_unblocked(id, T0)`.
2. Six workers from the file's `eligible_worker(&cp, name, &operation)` helper —
   distinct workers, each with the capability and grant for the operation.
3. `grant_capacity(&cp, &worker, &operation, 1)` on each. One is enough for this
   single ticket, so **no worker can be turned away for capacity**; that is what
   forces every claimer to reach the CAS and is what R7 then asserts.

Race: `tokio::sync::Barrier::new(6)`, one `tokio::spawn` per worker, each
awaiting the barrier and then calling
`cp.try_acquire_lease(NewLease { ticket_id, worker_id, ttl: 60s, now: T0 })`.

`try_acquire_lease` rather than `acquire_lease`: it returns the typed
`LeaseAcquireOutcome`, so a loser is an `Ok` value that can be *classified*.
`acquire_lease` maps the same outcome through `into_lease_result()` into an
error, which erases the distinction R7 depends on.

Assertions:

- Exactly one `LeaseAcquireOutcome::Acquired`.
- Every other outcome is `TicketNotReady { ticket_id }` carrying the raced
  ticket's id — **R7 for this path**. A `CapacityFull` or `WorkerIneligible`
  outcome fails the test with a message saying the claimer never reached the
  CAS.
- Zero `Err`.
- `SELECT COUNT(*) FROM leases` is 1 and `... WHERE state = 'held'` is 1.
- The single lease's `worker_id` equals the winner's.
- Ticket `state = 'leased'` and `attempt = 1`. Epoch is asserted relative to a
  pre-race read — `epoch_after == epoch_before + 1` — because the absolute value
  is 2, not 1: `mark_ready_if_unblocked` already bumps epoch from its `DEFAULT 0`
  (`crates/voom-store/src/repo/execution/tickets.rs:570`,
  `migrations/0001_schema.sql:61`). The relative form also states the property
  the test means, which is "incremented exactly once".
- Exactly one `lease.acquired` event, via the file's `count()` helper.

### Test B — remote claimers across nodes

File: `crates/voom-control-plane/src/cases/execution/remote_execution/mod_test.rs`
Name: `concurrent_remote_acquire_across_nodes_at_one_ticket_leases_exactly_once`

The existing `fixture_with_options` (`mod_test.rs:2102`) registers exactly one
node and one worker and hard-codes a single incarnation id, so it cannot express
this race. A new file-local helper `multi_node_remote_fixture(node_count)` builds
`node_count` independent nodes, each with its own registration token, its own
active incarnation, and one ready remote worker holding the capability and grant
for the raced operation.

Incarnation ids are 32 hex characters and must differ per node; they are derived
from the node index so the fixture stays deterministic.

The helper lives in `mod_test.rs` rather than `voom-test-support` because it has
exactly one caller. #581 builds the real multi-node harness and can promote it
then (`AGENTS.md` Rule 3).

Setup: one ready ticket via the existing `RemoteFixture`-shaped payload. That
payload does not parse as a `WorkflowTicketPayload` — `parse_ticket` requires
`operation` and `rendered_payload` fields it does not carry
(`crates/voom-control-plane/src/workflow/plan/ticket_payload.rs:118-152`) — so
`resolve_ticket_owner_locality_in_tx` classifies it `NoDeclaration` and the
owner-local gate does not reject non-owner nodes. Without that, five of six
nodes would be gated out before reaching the CAS and the race would not happen.

Race: `tokio::sync::Barrier::new(6)`, one task per node, each calling
`cp.remote_acquire(...)` with a per-node idempotency key and request hash.

Assertions:

- Exactly one `RemoteAcquireOutcome::Leased`.
- The other five are `RemoteAcquireOutcome::NoCandidate`; zero `Idle`.
- Zero `Err`.
- Each losing `NoCandidate`'s durable scheduler decision carries
  `SchedulerReasonCode::NoReadyTicket` — **R7 for this path**. That reason is
  produced only by `outcome_reason_code(TicketNotReady)`
  (`remote_execution/acquire.rs:1369`), so it is direct evidence the claimer
  executed the CAS and lost. A `WorkerCapacityFull` or `NodeCapacityFull` reason
  fails the test.
- `SELECT COUNT(*) FROM leases` is 1 and `... WHERE state = 'held'` is 1.
- Ticket `state = 'leased'`, `attempt = 1`.

Each node has its own limit (default 1, `scheduler_node_limits.node_limit_in_tx`
returns 1 for an absent row) and its own `active_count_for_node`, so the winner's
lease does not consume any loser's node capacity. The capacity recheck in
`recheck_selected_remote_capacity_in_tx` therefore passes for every loser and
they all reach the CAS.

## Failure handling

These tests are written after the code they cover, so a red assertion is a
finding about the code until proven otherwise. If either test fails:

1. Do not adjust the assertion. Run `$detect-curse` and establish the cause.
2. If the cause is a defect in the production path, fix the production path —
   `crates/voom-store/src/repo/execution/leases.rs` or the control-plane
   execution cases — within the surface the charter permits.
3. If the cause is a defect in the test's own setup, fix the setup and record
   what the wrong setup made the test appear to prove.

Softening an assertion to match observed behaviour is the failure mode this
section exists to prevent: it converts a test that found a bug into a test that
documents one.

## Verification

| Requirement | How it is verified |
|---|---|
| R1–R7 | `cargo test -p voom-control-plane concurrent_local_acquire_at_one_ticket_leases_exactly_once` and `... concurrent_remote_acquire_across_nodes_at_one_ticket_leases_exactly_once` |
| R8 | Neither test carries `#[ignore]`; both are reached by `just test` |
| R9 | Delete `AND state = 'ready'` from the CAS in `leases.rs`, run both tests, observe both fail on `held == 2`, restore the predicate, observe both pass |
| R10 | `just test-repeat voom-control-plane at_one_ticket 25`, then `just test-serial` and `just test-parallel` scoped to `voom-control-plane` |
| R11 | `just ci` |

R9's expected magnitude is two held leases, not six: the CAS retains
`attempt < max_attempts` and the raced ticket has `max_attempts = 2`, so the
third claimer's weakened UPDATE still matches zero rows. `held == 1` reddens at
two, which satisfies the criterion.

## Risks

- **A new contention test is itself a flake risk.** R10 is the mitigation, and
  it is the acceptance criterion most likely to fail first.
- **Six serialized `BEGIN IMMEDIATE` transactions per test** add wall-clock to
  `just test`. Both tests are short, but the coverage job runs
  `--test-threads=1` and pays them serially.
- **The remote fixture duplicates registration logic** already present in
  `fixture_with_options`. Accepted for now; #581 is where the two converge.
