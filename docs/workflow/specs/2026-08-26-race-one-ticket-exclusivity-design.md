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

**ADR 0085 § Context owns the mechanism**: that both paths open `BEGIN IMMEDIATE`
per ADR 0083 and therefore serialize on SQLite's single write lock; the CAS
statement itself; the snapshot's matching predicates; the savepoint rollback that
leaves a loser's `attempt` untouched; and the line citation for each. None of it
is restated here. Every one of those is a claim about production code that #552 is
expected to change, and a claim kept in two places drifts — the record that must
not drift is the one that is not edited.

What this spec needs from that analysis is three consequences, because they are
what decide the requirements:

1. **A remote loser settles `Idle`, not `NoCandidate`.** Its snapshot is already
   empty, so it never reaches the CAS. #578's Change section predicted
   `TicketNotReady`/`NoCandidate`; that holds for the local path only, and R6
   asserts what the code actually produces (criterion 3a).
2. **Weakening the CAS alone does not redden a multi-node test**, because the
   snapshot's identical filter still holds. R9 is therefore stated per layer, and
   Test B is not credited with covering the CAS. Test A is, and both paths call
   the same `acquire_guarded`, so the pair covers it.
3. **`SchedulerReasonCode::NoReadyTicket` cannot discriminate.** It has two
   producers (`crates/voom-scheduler/src/lib.rs:178-183` and
   `acquire.rs:1367-1369`, reached at `:529`), so R6 names `decision_kind` rather
   than the reason code — and pins the reason code additionally, as a guard on
   control flow that may change.

### What each remote assertion detects

**ADR 0085 § Consequences owns the table of what each weakening does, and the
observed messages from running all three.** What follows is only how each row
binds a requirement.

- **Snapshot predicate alone** — why R6 is load-bearing rather than hygiene.
  Exclusivity survives that weakening intact: one lease held, `attempt` up by one,
  every safety assertion green. Only the loser assertion notices. So R6 is not a
  safety assertion; it binds current control flow, and a change that legitimately
  widens the snapshot while leaving the CAS authoritative will redden Test B for a
  non-safety reason. Update ADR 0085 then, rather than deleting the assertion.
- **Both predicates** — where R5 earns its place. A second claimer leases and it
  is `attempt` reading two that catches it; every genuine loser is still `Idle`,
  so R6 does not fire.
- **CAS predicate alone** — undetected here by design; consequence 2 above.

**R6's tripwire has a precondition the tests must assert.** Both raced tickets
require `max_attempts >= 2`, asserted explicitly rather than left to the fixture's
current value. The two tests need that floor for *different* reasons, which
ADR 0085 § Consequences records together with a warning against conflating them.

**`decision_kind = Idle` is not on its own enough for R7.** It says only that the
candidate set came back empty, and ADR 0085 § Consequences enumerates the three
things that empty it — one of which yields a fully vacuous pass wearing the right
outcome. Test B therefore also pins each loser's `operation_set` and the total
scheduler-decision count, which exclude the other two. Even with both, the remote
discriminator is weaker than the local path's `TicketNotReady`, which names the
CAS itself rather than the emptiness of a set.

## Requirements

| # | Requirement | Source |
|---|---|---|
| R1 | A test races 6 workers on one node at one ready ticket via `ControlPlane::try_acquire_lease`. | #578 Change (1) |
| R2 | A test races 6 workers across 6 registered nodes at one ready ticket via `ControlPlane::remote_acquire`. | #578 Change (2) |
| R3 | Each test asserts exactly one claimer acquires, with zero errors. | #578 Change |
| R4 | Each test asserts `COUNT(*) FROM leases WHERE state = 'held' == 1`, and that the one held row is the lease the winning claimer was handed — both tests by lease id and worker. | #578 Change; tie added by review |
| R5 | Each test asserts the ticket's `attempt` incremented exactly once, relative to a pre-race read. | #578 Change; ADR 0085 Decision (2) |
| R6 | Test A asserts every loser is `LeaseAcquireOutcome::TicketNotReady`. Test B asserts every loser is `RemoteAcquireOutcome::Idle` whose scheduler decision has `decision_kind = Idle`, whose explanation's `operation_set` contains the raced operation, and with exactly one scheduler decision per claimer. | #578 Change, corrected by the asymmetry above; operator decision 2026-08-26; anti-vacuity clauses added by review |
| R7 | Each test fails if any loser was eliminated on capacity or eligibility rather than ticket readiness. | Necessary consequence of R3–R6 (see below) |
| R8 | Both tests run in the default `just test` suite with no `#[ignore]`. | #578 Acceptance |
| R9 | Test A is verified to bite by weakening the CAS alone. Test B is verified against all three arms of the table in ADR 0085 § Consequences, with the observed result matching each row. | #578 Acceptance, amended by operator decision 2026-08-26 and corrected by review |
| R10 | Both tests stable under `just test-repeat` at `COUNT=25` and under both `just test-serial` and `just test-parallel`. | #578 Acceptance |
| R11 | `just ci` is green. | `AGENTS.md` § Commands |

### Why R7 is necessary

R3–R5 are all satisfied by a run in which every loser was rejected before it ever
contended for the ticket: one claimer acquires, one lease is held, `attempt` is 1.
Such a run proves nothing while being indistinguishable from one that does.

**That vacuous run is reachable on the remote path and not on the local one, and
R7 earns its place differently on each.**

On the remote path it is live. `remote_acquire` derives the worker's candidate
operations first and `ready_for_operations_in_tx` returns early on an empty list
(`tickets.rs:1110`), so a claimer whose worker has no capability for the raced
operation never looks at a ticket at all — and settles `Idle`, the same outcome a
genuine loser produces. Six misconfigured workers would give Test B the exact
`1 Leased / 5 Idle` shape it asserts while nothing contended. Here R7 is the only
thing standing between a green test and no test, which is why Test B pins the
`operation_set` and the decision count rather than `decision_kind` alone.

On the local path it is not. `acquire_guarded` runs the ticket CAS *before* the
eligibility and capacity checks (`leases.rs:415-424` ahead of `:442` and `:452`),
so a local claimer cannot be eliminated ahead of the CAS: every loser reaches it
and receives `TicketNotReady`, which names the CAS itself rather than the
emptiness of a set. R7 on Test A is therefore a guard against that ordering being
changed, not against a hazard the current code presents — and Test A's assertion
for it is a match arm that is unreachable today, marked as such in the test.

No reasonable implementation of R3–R5 proves what #578 asks for without R7 on the
remote path; on the local path R7 is cheap insurance against the reordering that
would make Test A vacuous the way Test B could be.

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
  Each test carries `const _: () = assert!(CLAIMERS >= 2)` beside the declaration:
  every assertion in both tests is satisfied by a single-claimer run that never
  contends, so trimming the count for wall-clock would leave them green and
  vacuous. The floor is compile-time, so it fires where the trim would be made.
- **`busy_timeout` is 30s** (`pool.rs:41`), `acquire_timeout` 45s (`pool.rs:76`);
  a serialized claimer waits, it does not fail.
- **Both tests declare `#[tokio::test(flavor = "multi_thread", worker_threads = 4)]`.**
  This makes the six submissions genuinely concurrent, and matches the two
  existing contention tests in `remote_execution/mod_test.rs`. It is **not** what
  makes the assertions meaningful: per ADR 0085 § Context the claimers serialize
  on SQLite's single write lock either way, so a current-thread runtime would
  prove the same thing. Recorded here because it is the one precondition with no
  mechanical guard — each test therefore carries a comment at the attribute
  itself, where a wall-clock trim would be made.
- **A `tokio::sync::Barrier` releases the claimers**, not a sleep or a timestamp.
  Every claimer awaits it as its first act inside the spawned task, so the
  contention window does not depend on task spawn order or on wall-clock timing.
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
- The single held row's `id` and `worker_id` are the lease id and worker the
  winning claimer was handed.
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
  `Idle` outcome carries, has `decision_kind = SchedulerDecisionKind::Idle` **and**
  `reason_code = SchedulerReasonCode::NoReadyTicket` — R6 and R7 for this path. A
  `NoCandidate` outcome, or any other reason code, fails the test: it would mean
  the loser was eliminated somewhere other than ticket readiness. Pinning the
  reason code is redundant with the kind today (an `Idle` decision can only come
  from scoring an empty candidate slice, which is unconditionally `NoReadyTicket`),
  but it is the half of the outcome reported to #578 and #577, so it carries its
  own guard rather than resting on control flow that may change.
- Each loser's decision explanation carries the raced operation in its
  `operation_set`, and `SELECT COUNT(*) FROM scheduler_decisions` equals the
  claimer count. Without these, `decision_kind = Idle` admits the two vacuous
  runs described under *Why R7 is necessary*.
- Zero `Err`.
- `SELECT COUNT(*) FROM leases` is 1 and `... WHERE state = 'held'` is 1, and
  that held row's `id` and `worker_id` are the `lease_id` and `worker_id` the
  winning claimer was handed. A count alone would accept a held lease belonging
  to a claimer that never reported success.
- Ticket `state = 'leased'` and `attempt == attempt_before + 1`.

All observations are gathered into one summary string before the first assertion
runs. A Rust test aborts at its first failed assertion, so asserting as each fact
is read would make the arm-(ii) evidence — red on the loser assertion *while*
`held` is still 1 — unobservable in a single run.

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
| R9 (Test B) | Three runs matching the table in ADR 0085 § Consequences: (i) CAS predicate deleted alone — green; (ii) snapshot `state = 'ready'` deleted alone — red on R6, `held` still 1; (iii) both deleted — red on R3, `held == 2`, one loser observed as `Leased` and four as `Idle`. Restore and re-run. All three ran and matched |
| R10 | `just test-repeat voom-control-plane at_one_ticket_leases_exactly_once 25`, then `just test-serial` and `just test-parallel` |
| R11 | `just ci` |

R9's expected magnitude on Test A is two held leases, not six: the CAS retains
`attempt < max_attempts` and the raced ticket has `max_attempts = 2`, so a third
claimer's weakened `UPDATE` still matches zero rows. `held == 1` reddens at two.

All three arms are the evidence for ADR 0085's central consequence, and each must
be recorded — not only the final red. Arm (i) staying green is what shows the CAS
is not the remote path's load-bearing gate; arm (ii) going red *while `held`
stays 1* is what shows R6 detects a regression the safety assertion cannot; arm
(iii) is what shows R5 catches the case R6 does not.

**All arms were run on 2026-08-26 and every cell reproduced**; the observed
messages are recorded in ADR 0085 § Consequences, which owns them. The rule that
produced them still stands for any future re-run: if an arm stops reproducing,
the finding is against this spec and ADR 0085 — correct them, do not adjust the
test to match.

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
