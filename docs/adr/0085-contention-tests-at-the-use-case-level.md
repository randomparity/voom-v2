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

Writing the first such test surfaced an asymmetry the epic's framing missed.
Readiness is enforced **once** on the node-local path and **twice** on the
remote path:

- Local: `SqliteLeaseRepo::acquire_guarded` reads the ticket only to check its
  `kind` (`crates/voom-store/src/repo/execution/leases.rs:399-413`), then runs
  the CAS at `:415-424`. The CAS is the sole readiness gate, and it runs
  *before* the worker-eligibility (`:441`) and capacity (`:452`) checks, so a
  loser reaches it and receives `TicketNotReady`.
- Remote: `ControlPlane::remote_acquire` opens one `BEGIN IMMEDIATE`
  transaction (`.../remote_execution/acquire.rs:59`) spanning both the
  ready-ticket snapshot and the CAS. The snapshot
  (`crates/voom-store/src/repo/execution/tickets.rs:1114-1136`) carries the
  same four predicates as the CAS — `state = 'ready'`, `next_eligible_at <=`,
  `attempt < max_attempts`, job-open. The write lock is held from `BEGIN`, so
  nothing commits between a loser's snapshot and its own CAS: the loser finds
  the snapshot empty, takes the `tickets.is_empty()` branch (`acquire.rs:215-233`)
  and returns `Idle`, never executing the CAS.

That asymmetry decides what each test can prove, and a record was needed
because the obvious assertion set is satisfied by a test that proves nothing:
`COUNT(*) FROM leases WHERE state = 'held' == 1` is also what a run observes
when every loser was turned away by capacity or eligibility and never contended
for the ticket at all.

## Decision

Contention tests for ticket claiming live at the control-plane use-case level,
against a real on-disk WAL SQLite database, driven by the use case a caller
really invokes — `ControlPlane::try_acquire_lease` locally,
`ControlPlane::remote_acquire` remotely. Claimers are released together by a
`tokio::sync::Barrier` on a multi-threaded tokio runtime.

Each test asserts three things, not one:

1. **Safety** — exactly one claimer acquires, and `COUNT(*) FROM leases WHERE
   state = 'held'` is 1.
2. **The CAS's own side effect** — the ticket's `attempt` incremented exactly
   once, asserted relative to a pre-race read. Nothing else in the acquire flow
   increments `attempt` and `epoch` (`leases.rs:415-417`), which makes this the
   only observation unique to the transition itself; it reads 2 the instant a
   second transition is admitted.
3. **Why each loser lost** — by the outcome the losing path actually produces,
   never by elimination. This differs per path, and the difference is the
   record's substance:

   | path | loser outcome | discriminator |
   |---|---|---|
   | local | `LeaseAcquireOutcome::TicketNotReady` | the outcome itself |
   | remote | `RemoteAcquireOutcome::Idle` | the decision's `decision_kind = Idle` |

   The remote discriminator is `decision_kind`, **not** the reason code.
   `SchedulerReasonCode::NoReadyTicket` has two producers — the empty-snapshot
   elimination (`crates/voom-scheduler/src/lib.rs:178-183`) and
   `outcome_reason_code(TicketNotReady)` (`acquire.rs:1367-1369`) — so asserting
   the reason alone cannot tell a CAS loss from an elimination. A loser observed
   with a capacity or eligibility reason fails the test outright.

Concurrent claimers stay at or below the pool's `max_connections`
(`crates/voom-store/src/pool.rs:62`); this ADR uses 6 against 8. Claimers beyond
that queue for a connection rather than contending, so they add wall-clock
without adding contention.

## Consequences

- **The two paths bite differently, and a test author must know which.** On the
  local path, deleting `state = 'ready'` from the CAS admits a second claimer
  and the test reddens. On the remote path it does **not**: the snapshot's
  identical filter still holds, so a bite check there must weaken *both* layers.
  A remote test is a composite exclusivity test, not a CAS test.
- The loser-reason assertion is what keeps a passing test meaningful, and it is
  the part a later edit is most likely to drop as redundant. It is written here
  so its removal is a visible decision.
- **Reachability depends on transaction mode, which #552 is about to change en
  masse.** `remote_acquire`'s window closed when the M6 fix converted it to
  `BEGIN IMMEDIATE` (see `.../remote_execution/mod_test.rs:764-769`); ADR 0072's
  changed-gate branch presumably predates that. #552 converts further
  read-then-write sites, shrinking what is raceable. #580 and #581 both assume
  two transactions can interleave on the same rows and should check each target
  path's `BEGIN` mode before the test is designed.
- These tests are not pool-saturation coverage. Exceeding the pool is a
  different failure mode with a different expected outcome (#580).
- Nothing enforces the claimer bound or the assertion set mechanically. Both are
  prose plus a comment naming the pool.

## Considered & rejected

- **Race at the `voom-store` repository level, calling `try_acquire_in_tx`
  directly.** verified: the caller owns the transaction there — it takes
  `&mut Transaction` and opens only a savepoint (`leases.rs:334-350`) — so the
  test would supply its own `BEGIN`, making the transaction mode the test's
  choice rather than the code's. Given the consequence above, transaction mode
  is exactly what a contention test must not get to pick.
- **Race through the HTTP surface in `voom-api`.** verified: the densest
  protocol tests there are in-process `tower::oneshot` with no socket
  (`crates/voom-api/tests/remote_execution_route.rs`, `oneshot` at lines
  90/96/171/489/883; no `TcpListener`/`reqwest`/`serve(` hits), so transport adds
  no contention the use-case level lacks. Real multi-process transport is #581.
- **Assert only the held-lease count.** judgment: satisfied by a run in which no
  claimer ever contended for the ticket, which is the failure this record exists
  to prevent.
- **Assert the loser's `SchedulerReasonCode` rather than `decision_kind` on the
  remote path.** verified: `NoReadyTicket` is emitted by both the empty-snapshot
  path (`crates/voom-scheduler/src/lib.rs:178-183`, surfaced at
  `acquire.rs:215-233`) and the changed-gate path (`acquire.rs:1367-1369`), so the
  assertion would pass in the case it is meant to exclude.
- **Use a deterministic concurrency harness (loom, turmoil, madsim).** verified:
  `rg -n '^name = "(loom|turmoil|madsim)"' Cargo.lock` returns no match, and epic
  #577 records deterministic distributed simulation as a non-goal until the
  harness and CI lane exist. The question is SQLite transaction semantics, which
  a simulated scheduler does not execute.
- **Set the claimer count from the host's CPU count.** judgment: the binding
  constraint is the connection pool, a fixed 8 regardless of host, and a
  host-varying count makes a flake unreproducible on the machine that saw it.
- **Do nothing and rely on the existing concurrent tests.** verified: they race
  N claimers over N distinct tickets under a capacity limit (the two ranges cited
  in Context) and assert at most one lease per worker, so no ticket is ever
  contended.
