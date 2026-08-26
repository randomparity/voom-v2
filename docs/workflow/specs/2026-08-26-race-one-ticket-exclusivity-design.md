# Race N claimers at one ticket — design

Issue: [#578](https://github.com/randomparity/voom-v2/issues/578)
Epic: [#577](https://github.com/randomparity/voom-v2/issues/577)
ADR: [0085](../../adr/0085-contention-tests-at-the-use-case-level.md)

## Goal

Prove, under real contention, that at most one worker can hold a lease on one
ticket — on the node-local claim path and on the multi-node remote claim path.

## Background

No test races two claimers at one ticket. The existing concurrent tests
(`crates/voom-control-plane/src/cases/execution/leases_test.rs:1246-1290`,
`crates/voom-control-plane/src/cases/execution/remote_execution/mod_test.rs:759-813`)
race N claimers over N *distinct* tickets under a capacity limit and assert "at
most one lease per worker", so no ticket is ever contended.

### Readiness is enforced once locally and twice remotely

This asymmetry, established while designing this change and reported on #578 and
#577, decides what each test can prove. ADR 0085 records the decision that
follows from it.

**Both paths open `BEGIN IMMEDIATE`** per ADR 0083 — `try_acquire_lease` at
`crates/voom-control-plane/src/cases/execution/leases.rs:50-58` and
`remote_acquire` at `.../remote_execution/acquire.rs:59`, both via
`begin_immediate_tx` (`crates/voom-control-plane/src/cases/mod.rs:52-58`). So
neither test executes a readiness gate concurrently: SQLite's single write lock
serializes all six claimers, and each transaction begins only after the previous
commits. What these tests prove is that the gate rejects a ticket another
transaction has **already committed** as leased. That is the exclusivity property
that matters; it is not concurrent execution of the gate, and neither test should
be described as racing the CAS in that sense.

The difference between the paths is solely how many times readiness is checked.

The compare-and-swap in `SqliteLeaseRepo::acquire_guarded`
(`crates/voom-store/src/repo/execution/leases.rs:415-424`) is:

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

`rows_affected() == 0` yields `LeaseAcquireOutcome::TicketNotReady`. It runs
inside the savepoint `try_acquire_in_tx` opens (`leases.rs:334-350`); every
non-`Acquired` outcome rolls that savepoint back, so a loser leaves `attempt` and
`epoch` untouched.

**Local path — the CAS is the only readiness gate.** `acquire_guarded` reads the
ticket only to check its `kind` (`leases.rs:399-413`), then runs the CAS, and only
*then* checks worker eligibility (`:441`) and capacity (`:452`). A concurrent
loser reaches the CAS and receives `TicketNotReady`.

**Remote path — the CAS is unreachable as a race.**
`ControlPlane::remote_acquire` opens a single `BEGIN IMMEDIATE` transaction
(`.../remote_execution/acquire.rs:59`) spanning both the ready-ticket snapshot
and the CAS. The snapshot
(`crates/voom-store/src/repo/execution/tickets.rs:1114-1136`) filters on the same
four predicates as the CAS: `state = 'ready'`, `next_eligible_at <=`,
`attempt < max_attempts`, job-open. The write lock is held from `BEGIN`, so
nothing commits between a loser's snapshot and its own CAS. With one contended
ticket the loser's snapshot is **empty**, it takes the `tickets.is_empty()`
branch (`acquire.rs:215-233`), and returns `RemoteAcquireOutcome::Idle` without
executing the CAS.

Two things follow that #578's original Change section got wrong, and this spec
corrects:

- a remote loser settles as `Idle`, not `NoCandidate`; and
- weakening the CAS alone does **not** redden a multi-node test, because the
  snapshot's identical filter still holds.

`SchedulerReasonCode::NoReadyTicket` cannot discriminate either: it is produced
both by the empty-snapshot elimination (`crates/voom-scheduler/src/lib.rs:178-183`)
and by `outcome_reason_code(TicketNotReady)` (`acquire.rs:1367-1369`, reached at
`:529`). `decision_kind = Idle` is the discriminator: its sole producer is the
empty-candidate branch at `acquire.rs:229`.

### What each remote assertion detects

| weakening | `held` | remote loser | Test B |
|---|---|---|---|
| CAS `state = 'ready'` alone | 1 | `Idle` | green — undetected |
| snapshot `state = 'ready'` alone | 1 | `NoCandidate` | red, on the loser assertion |
| both | 2 | `NoCandidate` | red, on safety and the loser assertion |

The middle row is the reason R6 is load-bearing rather than hygiene. Remove
`state = 'ready'` from the snapshot only: a loser's transaction — which begins
after the winner commits — now sees the ticket in `leased`, so
`tickets.is_empty()` is false, the `Idle` return at `acquire.rs:229` is never
reached, candidates are non-empty, and because Test B uses six distinct workers
on six nodes the loser's own worker has capacity, so scoring returns `Selected`.
The CAS then correctly rejects, and the outcome is `NoCandidate`
(`acquire.rs:543`). Exactly one lease is still held and `attempt` still reads one
more than before — safety is intact, and only the loser assertion notices.

R6 is therefore not a safety assertion; it binds current control flow. A future
change that legitimately widens the snapshot while leaving the CAS authoritative
will redden Test B for a non-safety reason. That is the intended tripwire: update
this spec and ADR 0085 rather than delete the assertion.

Test B cannot detect a CAS-only regression. Test A can, and both paths call the
same `acquire_guarded`, so the pair covers the CAS.

## Requirements

| # | Requirement | Source |
|---|---|---|
| R1 | A test races 6 workers on one node at one ready ticket via `ControlPlane::try_acquire_lease`. | #578 Change (1) |
| R2 | A test races 6 workers across 6 registered nodes at one ready ticket via `ControlPlane::remote_acquire`. | #578 Change (2) |
| R3 | Each test asserts exactly one claimer acquires, with zero errors. | #578 Change |
| R4 | Each test asserts `COUNT(*) FROM leases WHERE state = 'held' == 1`. | #578 Change |
| R5 | Each test asserts the ticket's `attempt` incremented exactly once, relative to a pre-race read. | #578 Change; ADR 0085 Decision (2) |
| R6 | Test A asserts every loser is `LeaseAcquireOutcome::TicketNotReady`. Test B asserts every loser is `RemoteAcquireOutcome::Idle` whose scheduler decision has `decision_kind = Idle`. | #578 Change, corrected by the asymmetry above; operator decision 2026-08-26 |
| R7 | Each test fails if any loser was eliminated on capacity or eligibility rather than ticket readiness. | Necessary consequence of R3–R6 (see below) |
| R8 | Both tests run in the default `just test` suite with no `#[ignore]`. | #578 Acceptance |
| R9 | Test A is verified to bite by weakening the CAS alone. Test B is verified against all three arms of the table above, with the observed result matching each row. | #578 Acceptance, amended by operator decision 2026-08-26 and corrected by review |
| R10 | Both tests stable under `just test-repeat` at `COUNT=25` and under both `just test-serial` and `just test-parallel`. | #578 Acceptance |
| R11 | `just ci` is green. | `AGENTS.md` § Commands |

### Why R7 is necessary

R3–R5 are all satisfied by a run in which every loser was rejected by a capacity
or eligibility gate and never contended for the ticket: one claimer acquires, one
lease is held, `attempt` is 1. Such a run proves nothing while being
indistinguishable from one that does. No reasonable implementation of R3–R5
proves what #578 asks for without R7.

## Non-goals

Owned elsewhere: concurrent same-idempotency-key delivery (#579); pool saturation
beyond `max_connections` (#580); a multi-runner × multi-node stress harness,
ticket-conservation assertions, and socket-level or multi-process transport
(#581); a scheduled constrained-resource CI lane (#582); ENOSPC coverage (#583);
a property-based state-machine suite (#584). Enforcing or extending
`BEGIN IMMEDIATE` coverage is #552. Whether ADR 0072's changed-gate branch is now
dead code is flagged, not audited here. Deterministic distributed simulation and
performance budgets are epic #577 non-goals.

## Global constraints

- **Claimer count is 6**, against the pool's `max_connections = 8`
  (`crates/voom-store/src/pool.rs:62,67`). Claimers beyond the pool queue for a
  connection rather than contending, adding wall-clock without adding contention.
- **`busy_timeout` is 30s** (`pool.rs:41`), `acquire_timeout` 45s (`pool.rs:76`);
  a serialized claimer waits, it does not fail.
- **No `tokio::time::pause`/`advance` in either test file.** ADR 0012, enforced
  by `just check-paused-time-db`. Domain time comes from the caller-supplied
  `now` and from the fixture clock.
- **Sibling test layout.** ADR 0004, `just check-test-layout`. Both target files
  already exist and are already `#[path]`-linked; no new linkage is needed.
- **Tests run on the pinned `.test-tmp/` root.** ADR 0079; `TempDatabase` handles
  it. No test names its own temp directory.
- **Clippy runs `--all-targets --all-features -- -D warnings`.** Test code in
  these files already uses `.unwrap()` freely; match the surrounding style.

## Design

### Test A — node-local claimers

File: `crates/voom-control-plane/src/cases/execution/leases_test.rs`
Name: `concurrent_local_acquire_at_one_ticket_leases_exactly_once`

Fixture: the existing `cp()` helper (`crates/voom-control-plane/src/cases/mod_test.rs:22`) —
a real on-disk WAL database via `TempDatabase`, with a `SystemClock`. Domain time
is supplied per call as `T0` (`OffsetDateTime::UNIX_EPOCH`), as elsewhere in the
file.

Setup:

1. One ticket from the file's `ticket("noop", 2)` helper, then
   `mark_ready_if_unblocked(id, T0)`. Read its `attempt` and `epoch` before the
   race.
2. Six workers from `eligible_worker(&cp, name, &operation)` — distinct workers,
   each with the capability and grant.
3. `grant_capacity(&cp, &worker, &operation, 1)` on each. One is enough for this
   single ticket, so **no worker can be turned away for capacity** — which is
   what forces every claimer to the CAS and is what R7 then asserts.

   The capacity grant is load-bearing for R5 as well, not only R7. The CAS runs
   inside a savepoint that `try_acquire_in_tx` rolls back on every non-`Acquired`
   outcome (`.../leases.rs:346`, `:359-370`), and the CAS runs *before* the
   eligibility (`:442`) and capacity (`:452`) checks. So a claimer whose CAS
   succeeds but which is then rejected on capacity leaves `attempt` back where it
   started — the increment assertion would read 1 while the CAS was broken.
   Granting every worker capacity removes that masking path.

Race: `tokio::sync::Barrier::new(6)`, one `tokio::spawn` per worker, each awaiting
the barrier then calling
`cp.try_acquire_lease(NewLease { ticket_id, worker_id, ttl: 60s, now: T0 })`.

`try_acquire_lease` rather than `acquire_lease`: it returns the typed
`LeaseAcquireOutcome`, so a loser is an `Ok` value that can be classified.
`acquire_lease` maps the same outcome through `into_lease_result()` into an error,
erasing the distinction R6 and R7 depend on.

Assertions:

- Exactly one `LeaseAcquireOutcome::Acquired`.
- Every other outcome is `TicketNotReady { ticket_id }` carrying the raced
  ticket's id — R6 and R7 for this path. A `CapacityFull` or `WorkerIneligible`
  outcome fails with a message saying the claimer never reached the CAS.
- Zero `Err`.
- `SELECT COUNT(*) FROM leases` is 1 and `... WHERE state = 'held'` is 1.
- The single lease's `worker_id` equals the winner's.
- Ticket `state = 'leased'`, `attempt == attempt_before + 1`, and
  `epoch == epoch_before + 1`. The relative form states the property meant, and
  avoids depending on the absolute values — `mark_ready_if_unblocked` already
  bumps epoch from its `DEFAULT 0` (`.../tickets.rs:570`,
  `migrations/0001_schema.sql:61`).
- Exactly one `lease.acquired` event, via the file's `count()` helper.

### Test B — remote claimers across nodes

File: `crates/voom-control-plane/src/cases/execution/remote_execution/mod_test.rs`
Name: `concurrent_remote_acquire_across_nodes_at_one_ticket_leases_exactly_once`

This test proves **multi-node exclusivity**, not the CAS. Per the asymmetry above
the CAS is unreachable as a race here; exclusivity is held by two layers and the
test proves the composite.

The existing `fixture_with_options` (`mod_test.rs:2102`) registers exactly one
node and one worker and hard-codes a single incarnation id, so it cannot express
this race. A new file-local helper `multi_node_remote_fixture(node_count)` builds
`node_count` independent nodes, each with its own registration token, its own
active incarnation, and one ready remote worker holding the capability and grant.
Incarnation ids are 32 lowercase hex characters
(`crates/voom-core/src/taxonomy/ids.rs:109-122`) and are derived from the node
index so the fixture stays deterministic.

The helper lives in `mod_test.rs` rather than `voom-test-support` because it has
exactly one caller. #581 builds the real multi-node harness and can promote it
then (`AGENTS.md` Rule 3).

Setup: one ready ticket with the existing `RemoteFixture::ready_ticket` payload
shape. That payload does not parse as a `WorkflowTicketPayload` — `parse_ticket`
requires `operation` and `rendered_payload` fields it does not carry
(`crates/voom-control-plane/src/workflow/plan/ticket_payload.rs:118-152`) — so
`resolve_ticket_owner_locality_in_tx` classifies it `NoDeclaration` and the
owner-local gate does not reject non-owner nodes. Without that, five of six nodes
would be gated out before contending at all.

Race: `tokio::sync::Barrier::new(6)`, one task per node, each calling
`cp.remote_acquire(...)` with a per-node idempotency key and request hash.

Assertions:

- Exactly one `RemoteAcquireOutcome::Leased`.
- The other five are `RemoteAcquireOutcome::Idle`; zero `NoCandidate`.
- Each loser's scheduler decision, fetched by the `scheduler_decision_id` the
  `Idle` outcome carries, has `decision_kind = SchedulerDecisionKind::Idle` —
  R6 and R7 for this path. A `NoCandidate` decision, or any decision carrying
  `WorkerCapacityFull` or `NodeCapacityFull`, fails the test: it would mean the
  loser was eliminated on capacity rather than ticket readiness.
- Zero `Err`.
- `SELECT COUNT(*) FROM leases` is 1 and `... WHERE state = 'held'` is 1.
- Ticket `state = 'leased'` and `attempt == attempt_before + 1`.

Each node has its own limit (default 1 —
`scheduler_node_limits.node_limit_in_tx` returns 1 for an absent row) and its own
`active_count_for_node`, so the winner's lease consumes no loser's node capacity
and no capacity gate fires before the readiness check.

## Failure handling

These tests are written after the code they cover, so a red assertion is a
finding about the code until proven otherwise. If either test fails:

1. Do not adjust the assertion. Run `$detect-curse` and establish the cause.
2. If the cause is a production defect, fix the production path — within the
   surface the charter permits.
3. If the cause is the test's own setup, fix the setup and record what the wrong
   setup made the test appear to prove.

Softening an assertion to match observed behaviour is the failure mode this
section exists to prevent: it turns a test that found a bug into one that
documents it. The asymmetry in *Background* was found exactly this way, at design
time rather than after the fact.

## Verification

| Requirement | How it is verified |
|---|---|
| R1–R7 | `cargo test -p voom-control-plane --all-features at_one_ticket_leases_exactly_once` |
| R8 | Neither test carries `#[ignore]`; both are reached by `just test` |
| R9 (Test A) | Delete `AND state = 'ready'` from the CAS in `leases.rs`, run Test A, observe failure, restore, observe pass |
| R9 (Test B) | Three runs matching the table in *Background*: (i) CAS predicate deleted alone — expected green; (ii) snapshot `state = 'ready'` deleted alone — expected red on the loser assertion, with `held` still 1; (iii) both deleted — expected red on safety and the loser assertion, `held == 2`. Restore and re-run |
| R10 | `just test-repeat voom-control-plane at_one_ticket_leases_exactly_once 25`, then `just test-serial` and `just test-parallel` |
| R11 | `just ci` |

R9's expected magnitude on Test A is two held leases, not six: the CAS retains
`attempt < max_attempts` and the raced ticket has `max_attempts = 2`, so a third
claimer's weakened `UPDATE` still matches zero rows. `held == 1` reddens at two.

All three arms are the evidence for ADR 0085's central consequence, and each must
be recorded — not only the final red. Arm (i) staying green is what shows the CAS
is not the remote path's load-bearing gate; arm (ii) going red *while `held`
stays 1* is what shows the loser assertion detects a regression the safety
assertion cannot.

## Risks

- **A new contention test is itself a flake risk.** R10 is the mitigation and the
  criterion most likely to fail first.
- **Six serialized `BEGIN IMMEDIATE` transactions per test** add wall-clock; the
  coverage job runs `--test-threads=1` and pays them serially. Measured baseline
  on this host: the existing `concurrent_` tests run in 1.7s prebuilt.
- **Test B's value shrinks if #552 converts more paths.** Its assertions describe
  the current two-layer structure; a future change to either layer should update
  this spec and ADR 0085 rather than silently weaken the test.
- **The remote fixture duplicates registration logic** already in
  `fixture_with_options`. Accepted; #581 is where the two converge.
