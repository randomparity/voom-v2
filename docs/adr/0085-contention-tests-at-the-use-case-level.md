# 0085 — Contention tests race at the control-plane use-case level

## Status

Accepted (2026-08-26)

## Context

Ticket claim exclusivity — at most one worker holds a lease on a ticket — was
never tested under contention. Every concurrent test in the suite races N
claimers over N *distinct* tickets against a capacity limit
(`crates/voom-control-plane/src/cases/execution/leases_test.rs:1246-1290`,
`crates/voom-control-plane/src/cases/execution/remote_execution/mod_test.rs:759-813`),
so no ticket is ever contended (#578, and finding 1 of epic #577).

Both claim paths open `BEGIN IMMEDIATE` per ADR 0083 —
`ControlPlane::try_acquire_lease` at
`crates/voom-control-plane/src/cases/execution/leases.rs:50-58` and
`ControlPlane::remote_acquire` at
`.../remote_execution/acquire.rs:59`, both through `begin_immediate_tx`
(`crates/voom-control-plane/src/cases/mod.rs:52-58`). So claimers do not execute
a readiness gate concurrently on either path: SQLite's single write lock
serializes them, and each claimer's transaction begins only after the previous
one commits. What a contention test proves is therefore that the gate rejects a
ticket another transaction has **already committed** as leased — which is the
exclusivity property that matters, but is not a concurrent execution of the gate.

The two paths differ in how many times readiness is checked, and that alone
decides what each test can detect:

- **Local — once.** `SqliteLeaseRepo::acquire_guarded` reads the ticket only to
  check its `kind` (`crates/voom-store/src/repo/execution/leases.rs:399-413`),
  then runs the compare-and-swap at `:415-424`, and only *then* checks worker
  eligibility (`:442`) and capacity (`:452`). There is no readiness pre-filter,
  so a loser executes the CAS and receives `TicketNotReady`.
- **Remote — twice.** `remote_acquire`'s ready-ticket snapshot
  (`crates/voom-store/src/repo/execution/tickets.rs:1114-1136`) carries the same
  four predicates as the CAS: `state = 'ready'`, `next_eligible_at <=`,
  `attempt < max_attempts`, job-open. A loser's snapshot is empty, so it takes
  the `tickets.is_empty()` branch (`acquire.rs:215-233`) and returns `Idle`
  without executing the CAS.

A record was needed because the obvious assertion set is satisfied by a test that
proves nothing: `COUNT(*) FROM leases WHERE state = 'held' == 1` is also what a
run observes when every loser was turned away by capacity or eligibility and
never contended for the ticket at all.

## Decision

Contention tests for ticket claiming live at the control-plane use-case level,
against a real on-disk WAL SQLite database, driven by the use case a caller
really invokes. Claimers are released together by a `tokio::sync::Barrier` on a
multi-threaded tokio runtime.

Each test asserts three things, not one:

1. **Safety** — exactly one claimer acquires, and `COUNT(*) FROM leases WHERE
   state = 'held'` is 1.
2. **The transition's own side effect** — the ticket's `attempt` incremented
   exactly once, relative to a pre-race read. `attempt = attempt + 1` has exactly
   one production site (`leases.rs:417`), so this is the observation unique to the
   transition. It detects a second transition that **commits**; a CAS that
   succeeds and is then rejected on a later gate rolls its savepoint back
   (`leases.rs:346`, `:359-370`) and leaves `attempt` unchanged. The fixture
   therefore grants every claimer capacity, so no later gate can fire and mask a
   second transition.
3. **Why each loser lost** — by the outcome the losing path actually produces,
   never by elimination:

   | path | loser outcome | discriminator |
   |---|---|---|
   | local | `LeaseAcquireOutcome::TicketNotReady` | the outcome itself |
   | remote | `RemoteAcquireOutcome::Idle` | `decision_kind = Idle`, **plus** the two clauses under Consequences |

   A loser observed with a capacity or eligibility reason fails the test outright.
   `decision_kind` alone does not carry the remote path; see the anti-vacuity
   consequence below for the two facts that must accompany it.

Concurrent claimers are a fixed count — 6 — chosen so every claimer holds a
pooled connection simultaneously (`max_connections = 8`,
`crates/voom-store/src/pool.rs:62`) and the runtime stays bounded.

## Consequences

- **The remote assertions detect different regressions, and only together.**
  The table below was predicted by code trace and is now **observed**: all three
  arms were run on 2026-08-26 against the committed tests, and every cell
  reproduced.

  | weakening | `held` | 5 remote losers | Test B |
  |---|---|---|---|
  | CAS `state = 'ready'` alone | 1 | 5× `Idle` | green — undetected |
  | snapshot `state = 'ready'` alone | 1 | 5× `NoCandidate` | red, on assertion 3 |
  | both | 2 | 1× `Leased`, 4× `Idle` | red, on assertions 1 and 2 |

  The first row is the load-bearing one, and it is the one a prediction could
  most easily have got wrong, so it was checked against its contrapositive: with
  the CAS predicate deleted and nothing else changed, Test B passes while the
  node-local Test A fails on `acquired=2 held=2 leases=2 state=Leased
  attempt=0->2 epoch=1->3 events=2`. One
  production edit, two tests, opposite verdicts — that is the two-layer structure
  this record asserts, observed rather than argued.

  Row 1: the snapshot's surviving `state = 'ready'` empties the loser's candidate
  set (`tickets.rs:1119-1128`), so it returns `Idle` at `acquire.rs:229`. Row 2:
  the snapshot returns the leased ticket, scoring selects it, and the CAS rejects
  it (`leases.rs:414-434`), surfacing as `NoCandidate` at `acquire.rs:543`.
  Row 3: with neither predicate present, claimer 2's CAS succeeds and it becomes a
  second winner; claimers 3–6 are then stopped by `attempt < max_attempts`, which
  both the snapshot and the CAS still carry, so their snapshot is empty and they
  return `Idle`.

  **Row 3's `held` is `min(claimers, max_attempts)`, not 2.** It reads 2 only
  because the raced ticket has `max_attempts = 2`. Row 3 reddens on assertions 1
  and 2 — *not* 3, since every genuine loser there is still `Idle`. Assertion 2 is
  corroborating rather than sole there: assertion 1 already fires on the second
  `Leased`. Its value is that it reads the ticket row directly, so it still fires
  if a future change stops surfacing a second acquisition as `Leased`.

  The spec points at this table rather than repeating it.

- **Both tests gather every observation before asserting on the race.** A Rust
  test aborts at its first failed assertion, so a test that asserts as it reads
  can only ever show one violated fact. The rows above are conclusions about
  *pairs* of facts — row 2 is "the loser assertion fired **while** exactly one
  lease was still held", row 3 is "a second claimer leased **and** `attempt`
  moved twice" — and neither is obtainable from a test that stops at the first
  one. So each test collects the outcome tallies, the lease rows, the decision
  count and the ticket's before/after state into one summary string, and
  interpolates it into every assertion message. Row 3's observed line reads
  `leased=2 idle=4 no_candidate=0 errors=0 held=2 leases=2 decisions=6
  state=Leased attempt=0->2 epoch=1->3`: assertion 1 is what fires, and
  assertion 2's violation is visible in the same message without being reached.

  This governs the **race** observations. The node-local test still classifies
  outcomes eagerly, asserting inside its collection loop when a claimer errors
  or is eliminated somewhere other than the CAS — those are setup failures
  rather than contention results, and the durable observations do not exist
  yet when they fire. Such a run therefore reports a bare message with no
  lease count or attempt delta; it still fails, which is what matters here.

  The middle row is why assertion 3 is load-bearing on the remote path rather than
  hygiene: it is the only detector of a snapshot regression, since exclusivity
  survives that weakening intact. It is not a safety assertion — it binds current
  control flow, so a future change that legitimately widens the snapshot while
  leaving the CAS authoritative reddens the test for a non-safety reason. That is
  the intended tripwire; a reader reaching it should update this record rather
  than delete the assertion.
- **That tripwire has a precondition, and the test must assert it.** The snapshot
  carries five predicates (`tickets.rs:1119-1129` — the fifth, `kind IN (...)`,
  begins at `:1129`), and the reasoning above is about `state = 'ready'` alone.
  If the raced ticket had `max_attempts = 1`, then
  after the winner's CAS the snapshot would be emptied by `attempt < max_attempts`
  instead, every loser would return `Idle` **whether or not** the snapshot's
  `state` predicate is present, and the middle row would go green — the detector
  disarmed by a one-character fixture edit, with nothing to signal it. The raced
  ticket therefore requires `max_attempts >= 2`, and the test asserts that
  explicitly rather than relying on the fixture's current value.

  **The node-local test needs the same floor for an unrelated reason, and the two
  must not be conflated.** There is no snapshot on that path; what matters there
  is the CAS's own `attempt < max_attempts` (`leases.rs:420`). At
  `max_attempts = 1` the winner's increment closes that predicate, so a CAS
  stripped of `state = 'ready'` still matches zero rows for the second claimer,
  every loser still returns `TicketNotReady`, and the local test goes green while
  detecting nothing. Under that weakening `held` is `min(claimers, max_attempts)`
  in general — the observed 2 above is that formula, not a constant. A reader who
  checks the remote rationale against the local test will find the snapshot
  irrelevant there and may conclude the assertion was copied; it was not, and
  each test states its own reason at the assertion.

- **`decision_kind = Idle` is not by itself an anti-vacuity assertion.** It says
  only that the candidate set came back empty, and three different things empty
  it:

  1. the ticket is no longer ready — the case the test means;
  2. the worker has no candidate operations at all, which short-circuits the
     snapshot query before it looks at any ticket
     (`ready_for_operations_in_tx` returns early on an empty operation list,
     `tickets.rs:1110-1112`); or
  3. an owner-local gate rejected every ready ticket, leaving `gated` empty
     (`acquire.rs:194-214`).

  Case 2 is the dangerous one: misconfigure five of six claimers and the run
  still shows one `Leased` and five `Idle`, one held lease, and `attempt`
  incremented once — the vacuous pass this record exists to prevent, wearing the
  right outcome. A remote contention test therefore pins two further facts: each
  loser's decision explanation carries the raced operation in its `operation_set`
  (the Idle branch stamps the worker's own operations there, `acquire.rs:221`),
  which excludes case 2; and the total scheduler-decision count equals the
  claimer count, which excludes case 3 by the absence of its
  `UnsupportedArtifactAccess` rows.

  Even so the remote discriminator remains weaker than the local path's
  `TicketNotReady`, which names the CAS itself rather than the emptiness of a set.
- **A remote test cannot detect a CAS-only regression.** The local test can, and
  both paths call the same `acquire_guarded`, so the pair covers the CAS's
  `state = 'ready'` predicate — the exclusivity gate — even though neither path
  covers it twice. It covers only that one of the CAS's four predicates. Deleting
  `attempt < max_attempts`, `next_eligible_at <= ?`, or the job-open `EXISTS`
  leaves both tests green: the surviving `state` predicate still rejects every
  loser, and the raced ticket's `job_id` is `NULL`, which makes the third clause
  inert in this fixture regardless. That is the right scope for a record about
  exclusivity, but it is not coverage of the statement.
- **Reachability depends on transaction mode, which #552 is about to change en
  masse.** `remote_acquire`'s window closed when the M6 fix converted it to
  `BEGIN IMMEDIATE` (see `.../remote_execution/mod_test.rs:761-769`). #552
  converts further read-then-write sites, shrinking what is raceable. #580 and
  #581 both assume two transactions can interleave on the same rows and should
  check each target path's `BEGIN` mode before the test is designed — including,
  per Context, the local path, whose mode is also `BEGIN IMMEDIATE`.
- These tests are not pool-saturation coverage. Exceeding the pool is a different
  failure mode with a different expected outcome (#580).
- The assertion set is prose; nothing enforces it mechanically. The claimer
  count is enforced: each test carries a `const _: () = assert!(CLAIMERS >= 2)`
  beside its declaration, because every assertion in both tests is satisfied by
  a single-claimer run that never contends, and trimming the count is the
  obvious response to wall-clock pressure.

## Considered & rejected

- **Race at the `voom-store` repository level, calling `try_acquire_in_tx`
  directly.** verified: the caller owns the transaction there — it takes
  `&mut Transaction` and opens only a savepoint (`leases.rs:334-350`) — so the
  test would supply its own `BEGIN`. Given that transaction mode is what decides
  whether a gate is reachable at all, it is exactly what a contention test must
  not get to pick.
- **Race through the HTTP surface in `voom-api`.** verified: the densest protocol
  tests there are in-process `tower::oneshot` with no socket —
  `rg -n 'oneshot' crates/voom-api/tests/remote_execution_route.rs` hits
  90/96/171/489/883, and `rg -n 'TcpListener|reqwest|serve\('` on the same file
  exits 1. Transport adds no contention the use-case level lacks. Real
  multi-process transport is #581.
- **Assert only the held-lease count.** judgment: satisfied by a run in which no
  claimer ever contended for the ticket, which is the failure this record exists
  to prevent. The table above additionally shows it is blind to a snapshot
  regression; that row was run on 2026-08-26 and reproduced.
- **Drive the six claims sequentially instead of concurrently.** judgment: every
  assertion the Decision lists is satisfied identically by six sequential calls,
  which cannot flake and need no barrier — and per Context the claimers serialize
  anyway, so the concurrency is not what makes the gate reject. Two things keep it:
  #578 asks for a race in those words, and concurrent submission additionally
  proves that `BEGIN IMMEDIATE` plus the 30s `busy_timeout` (`pool.rs:41`) yields
  zero errors rather than a leaked `SQLITE_BUSY` under a genuinely contended write
  lock. That second ground is weak on its own: the existing remote test already
  covers it at N=8 (`.../remote_execution/mod_test.rs:759-813`, comment at
  `:761-769`), which is higher concurrency than this one uses. So the honest
  ranking is that #578's wording is the reason and the error-freedom check is a
  by-product — a later reader weighing the flake budget against the value should
  know the concurrency here is doing less work than it appears to.
- **Assert the loser's `SchedulerReasonCode` rather than `decision_kind` on the
  remote path.** verified: `decision_kind = Idle` has a single producer.
  `decision_from_score` has three call sites (`acquire.rs:226`, `:248`, `:585`);
  only `:226` can carry `ScoreOutcome::Idle`, because `:240-242` turns an Idle
  score on non-empty candidates into a `VoomError::Internal` and `:585` is the
  Selected path. The kind is assigned by the `ScoreOutcome::Idle` arm at
  `acquire.rs:1021-1026`. So it is strictly stronger. The
  reason code is not: `NoReadyTicket` is emitted both there (via
  `crates/voom-scheduler/src/lib.rs:178-183`) and by
  `outcome_reason_code(TicketNotReady)` (`acquire.rs:1367-1369`, reached at
  `:529`), so it cannot separate an empty-snapshot elimination from a changed-gate
  rejection — which is precisely the distinction the middle row of the table turns
  on. It *would* exclude the capacity case, whose codes differ
  (`first_rejection_reason`, `crates/voom-scheduler/src/lib.rs:274-280`); that is
  not the case it fails at.
- **Use a deterministic concurrency harness (loom, turmoil, madsim).** verified:
  `rg -n '^name = "(loom|turmoil|madsim)"' Cargo.lock` returns no match, and epic
  #577 records deterministic distributed simulation as a non-goal until the
  harness and CI lane exist. The question is SQLite transaction semantics, which a
  simulated scheduler does not execute.
- **Set the claimer count from the host's CPU count.** judgment: a host-varying
  count makes a flake unreproducible on the machine that saw it.
- **Do nothing and rely on the existing concurrent tests.** verified: they race N
  claimers over N distinct tickets under a capacity limit (the two ranges cited in
  Context) and assert at most one lease per worker, so no ticket is ever
  contended.
