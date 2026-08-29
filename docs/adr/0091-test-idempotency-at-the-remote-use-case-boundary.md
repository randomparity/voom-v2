# 0091 — Test idempotency at the remote use-case boundary

## Status

Accepted (2026-08-29)

## Context

Remote acquire and complete reserve an idempotency key, perform domain mutations, store the
response, and commit all three in one `BEGIN IMMEDIATE` transaction. Existing tests replay a
request only after the first request commits. They prove response reuse but do not execute the
same-key delivery race that retrying node agents can create (#579).

A repository-only race can prove that one idempotency row is inserted, but cannot prove that the
lease or completion mutation guarded by that row occurs once. Conversely, response assertions
alone can pass while duplicate durable effects occur.

## Decision

Test concurrent same-key delivery at the `ControlPlane` remote-use-case boundary. Release a fixed
set of tasks together with `tokio::sync::Barrier` on a multi-thread runtime and use the existing
on-disk SQLite fixture.

Exercise both `remote_acquire` and `remote_complete`. For each race, accept a task only when it
returns the stored successful outcome or the documented clean `CONFLICT` for an in-progress key;
any database error, including surfaced `SQLITE_BUSY`, fails the test. After every task settles,
assert the operation's durable mutation and events occurred exactly once. Also assert all
successful outcomes identify the same lease.

Run each path once with two contenders and again with six contenders. Two is the minimal race;
six covers repeated losers without exceeding the pool's eight-connection bound. Each cardinality
uses a fresh fixture. The tests remain in the default suite and use real time; they do not pause
Tokio time around SQLite.

## Consequences

- The tests cover the full reserve → mutate → complete span through caller-visible behavior.
- Current `BEGIN IMMEDIATE` transaction ownership normally serializes contenders before the
  reservation insert, so losers replay the committed response. The assertions also permit the
  clean in-progress conflict promised by the idempotency contract if transaction ordering changes.
- The reserve conflict clause is load-bearing: removing it makes a serialized loser surface a
  uniqueness-backed database error, so the test must fail during the required bite check.
- This is contention correctness coverage, not pool-saturation or performance coverage.

## Considered & rejected

- **Race `reserve_or_replay_in_tx` directly.** judgment: it proves row ownership but cannot prove
  the guarded domain mutation occurs once across the transaction span.
- **Force one transaction to pause after reservation.** judgment: a test-only interception point
  would add production or abstraction surface solely to manufacture the optional in-progress
  outcome.
- **Require every loser to return a replay.** verified: both remote paths open with
  `begin_read_then_write` before reserving (`acquire.rs` and `complete.rs` on `main` at
  `16ee12bfde14e9f8378d2e5dac966d6e4bfa9d21`), so that matches current ordering but would overbind
  the test beyond issue #579's explicit replay-or-conflict contract.
- **Keep the sequential tests only.** judgment: they never execute the delivery race named by the
  issue and cannot establish the requested concurrency guarantee.
