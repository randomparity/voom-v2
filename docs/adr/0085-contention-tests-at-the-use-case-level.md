# 0085 — Contention tests race at the control-plane use-case level

## Status

Accepted (2026-08-26)

## Context

Ticket claim exclusivity — at most one worker holds a lease on a ticket — rests
on a single compare-and-swap in `SqliteLeaseRepo::acquire_guarded`
(`crates/voom-store/src/repo/execution/leases.rs:415-424`), inside the savepoint
`try_acquire_in_tx` opens. Nothing raced multiple claimers at one ticket. Every
concurrent test in the suite races N claimers over N *distinct* tickets against a
capacity limit, so it proves "at most one lease per worker" and would stay green
if the ticket CAS lost its `state = 'ready'` predicate (#578, and finding 1 of
epic #577).

Two questions had to be settled before writing the first such test, and both
recur for every contention test the epic adds after it (#579, #580, #581).

The first is how many concurrent claimers a contention test may use. The pool is
sized at `max_connections = 8` (`crates/voom-store/src/pool.rs`), and per ADR
0083 the acquire paths open `BEGIN IMMEDIATE`, so each in-flight claimer holds
its pooled connection across the whole write-lock wait rather than only across
its own writes. A test with more claimers than connections stops measuring the
CAS and starts measuring `acquire_timeout`.

The second is what such a test must assert. `COUNT(*) FROM leases WHERE
state = 'held' == 1` is the safety property, but it is also what a test observes
when every loser was turned away by an eligibility or capacity gate and never
reached the CAS at all. That test is green, proves nothing, and looks exactly
like one that works.

## Decision

Contention tests for ticket claiming live at the control-plane use-case level,
against a real on-disk WAL SQLite database, and they assert both the safety
property and each loser's reason for losing.

Concretely:

- The entry point under test is the use case a caller really invokes —
  `ControlPlane::try_acquire_lease` for a node-local claimer,
  `ControlPlane::remote_acquire` for a node-owned remote one — not the store
  repository beneath it. The CAS is only reachable through the transaction,
  savepoint, gate, and event machinery those use cases wrap, and a repo-level
  race would exercise the CAS without any of it.
- Claimers are released together by a `tokio::sync::Barrier` on a
  `#[tokio::test(flavor = "multi_thread")]` runtime.
- **The concurrent claimer count stays at or below the pool's
  `max_connections`, with headroom.** This ADR sets 6 against the pool's 8.
- **Every loser is asserted to have lost at the CAS**, by the outcome the CAS
  produces rather than by elimination: `LeaseAcquireOutcome::TicketNotReady` on
  the local path, and a durable scheduler decision carrying
  `SchedulerReasonCode::NoReadyTicket` on the remote path. A test that observes
  a loser rejected for capacity or eligibility has proved nothing about
  exclusivity and fails.

## Consequences

- A weakening of the ticket CAS reddens a test. Dropping `state = 'ready'`
  admits a second claimer, so `held` becomes 2 against an asserted 1.
- The loser-reason assertion is the part that keeps a passing test meaningful,
  and it is the part a later edit is most likely to drop as redundant. It is
  written here so its removal is a visible decision.
- The claimer bound means these tests cannot also serve as pool-saturation
  coverage. That is deliberate and is #580's subject: exceeding the pool is a
  different failure mode with a different expected outcome, and mixing the two
  makes neither diagnosable.
- Nothing enforces the bound mechanically. It is a number in a test file with a
  comment naming the pool, not a guard.
- Six claimers on a serialized write lock cost roughly six serialized
  transactions per test, which is why the bound is a ceiling and not a target.

## Considered & rejected

- **Race at the `voom-store` repository level, calling `try_acquire_in_tx`
  directly.** verified: the caller owns the transaction there —
  `try_acquire_in_tx` takes `&mut Transaction` and opens only a savepoint
  (`crates/voom-store/src/repo/execution/leases.rs:334-350`) — so the test would
  supply its own `BEGIN`, and the `BEGIN IMMEDIATE` that ADR 0083 requires of
  the real callers would be the test's choice rather than the code's. The
  production transaction mode is precisely what a contention test must not get
  to pick.
- **Race through the HTTP surface in `voom-api`.** verified: the densest
  protocol tests there are in-process `tower::oneshot` with no socket
  (`crates/voom-api/tests/remote_execution_route.rs`), so the transport adds no
  contention the use-case level lacks — only latency and a second failure
  vocabulary. Real multi-process transport is #581's subject.
- **Assert only `COUNT(*) FROM leases WHERE state = 'held' == 1`.** judgment:
  satisfied by a run in which no claimer ever reached the CAS, which is the
  failure mode this ADR exists to prevent.
- **Use a deterministic concurrency harness (loom, turmoil, madsim).**
  verified: none is in `Cargo.lock`, and epic #577 records deterministic
  distributed simulation as a non-goal to revisit only after the harness and CI
  lane exist. The exclusivity question is a SQLite transaction-semantics
  question, and a simulated scheduler does not execute SQLite's locking.
- **Set the claimer count from the host's CPU count.** judgment: the binding
  constraint is the connection pool, which is a fixed 8 regardless of host, and
  a host-varying count makes a flake unreproducible on the machine that saw it.
- **Do nothing and rely on the existing concurrent tests.** verified: they race
  N claimers over N distinct tickets under a capacity limit
  (`crates/voom-control-plane/src/cases/execution/leases_test.rs:1246-1290`,
  `crates/voom-control-plane/src/cases/execution/remote_execution/mod_test.rs:759-813`)
  and assert at most one lease per worker, so no ticket is ever contended and
  the CAS predicate is never the thing under test.
