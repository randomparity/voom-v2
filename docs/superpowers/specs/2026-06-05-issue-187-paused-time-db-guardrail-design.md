# Issue #187 — Guard against pairing `tokio::time::pause()` with the real SQLite pool in tests

- **Status:** Draft
- **Date:** 2026-06-05
- **Issue:** #187
- **ADR:** [0012 — Guard paused-time + real SQLite pool in tests](../../adr/0012-paused-time-db-pool-guard.md)

## Problem

A test that pairs tokio's paused virtual clock (`tokio::time::pause()` /
`tokio::time::advance()`) with a **real `SqlitePool`** can fail spuriously.

When tokio time is paused, the runtime auto-advances virtual time to the next
pending timer whenever it has **no runnable task**. `sqlx` runs SQLite on a
blocking thread pool, so while an `await` is parked waiting on that blocking
thread (connection open or a query under CPU starvation) the async runtime is
idle and the paused clock jumps forward — past the pool's `acquire_timeout`
(sqlx default 30s). The DB call then returns `DbUnreachable` ("pool timed out
while waiting for an open connection") even though nothing is wrong.

This is a **clock-domain mismatch**: tokio's *test* clock (virtual, auto
advancing) is conflated with the *wall clock* against which sqlx measures
`acquire_timeout` on its blocking thread.

The failure is environmental — it surfaced ~9/10 full-suite runs under 48-core
saturation and is `busy_timeout`-independent. PR #186 fixed the one affected
test
(`await_with_lease_heartbeats_refreshes_workflow_lease_while_future_runs` in
`crates/voom-control-plane/src/workflow/execution/executor_test.rs`) by running
its heartbeat loop in real time while keeping the freshness assertion
deterministic via the injected `ManualClock`. Nothing prevents the trap from
being reintroduced.

## Goal

Prevent paused-time + real-pool tests from reintroducing this flake, and record
the decision where test authors will see it.

## Non-goals

- Re-fixing any current test. PR #186 already de-flaked the only affected test;
  the suite is green today.
- Changing the production pool configuration or `acquire_timeout`. The issue
  notes a smaller test-only `acquire_timeout` would make the failure *clearer*
  but does **not** fix the auto-advance race, so it is out of scope.
- Banning `tokio::time::pause()` outright. It is legitimate and in use where no
  DB is involved (`crates/voom-control-plane/src/scan/worker_test.rs` pauses
  around a process-launch timeout with no pool).
- Banning the injected domain `Clock` / `ManualClock`. `ManualClock::advance`
  is the *prescribed* way to drive domain time in DB-touching tests and must
  not be flagged.

## Current state (verified 2026-06-05)

`rg "tokio::time::pause|tokio::time::advance"` across `crates/` returns exactly
one test file:

| File | `tokio::time::pause`/`advance` | references `SqlitePool`/`ControlPlane` |
|---|---|---|
| `crates/voom-control-plane/src/scan/worker_test.rs` | yes | **no** |

`fixture.clock.advance(...)` / `clock.advance(...)` calls elsewhere
(`executor_test.rs`, `registry_test.rs`, `clock_test_support_test.rs`) are the
**domain** `ManualClock`, not tokio, and are unrelated to the trap.

Implication: a check scoped to the **co-occurrence** of (a) a tokio paused-time
call and (b) a DB-pool reference *in the same `*_test.rs` file* flags **nothing
today** — `worker_test.rs` has the pause but no pool, so it is excluded by
scope, not by an allowlist. CI stays green on adoption.

## Decision summary

Adopt **both** layers (see [ADR 0012](../../adr/0012-paused-time-db-pool-guard.md)):

1. **Convention** — a written rule in `AGENTS.md` (the testing section that test
   authors already consult): do not pair `tokio::time::pause()`/`advance()` with
   a real `SqlitePool`; drive DB-touching tests on real time and control domain
   time via the injected `Clock` (`ManualClock`).

2. **Scoped check** — a new `scripts/check-paused-time-db.sh`, wired into
   `just ci` (and therefore the pre-commit/CI suite), that fails when a single
   `crates/*/src/**/*_test.rs` file contains **both** a tokio paused-time call
   **and** a DB-pool reference. The check uses `ast-grep` for the paused-time
   call so it inspects real Rust syntax-tree items (not comments/strings),
   matching the precedent set by `check-test-layout.sh`.

The convention educates; the check enforces. Neither alone is sufficient: a
convention is silently ignorable, and a check without a written rationale leaves
a flagged author with no guidance on the fix.

## Detection design

For each `crates/*/src/**/*_test.rs`:

1. **Paused-time signal** (`ast-grep`, `--lang rust`): a call expression
   `tokio::time::pause()` **or** `tokio::time::advance($$$)`. Using the syntax
   tree means a commented-out or stringified mention does not trip the check.
   Fully-qualified `tokio::time::` is required so that an injected
   `clock.advance(...)` / `fixture.clock.advance(...)` (the domain
   `ManualClock`) is never matched.
2. **DB-pool signal** (`ast-grep`, identifier match): a reference to the type
   identifier `SqlitePool` **or** `ControlPlane`. These are the two types
   through which control-plane tests reach the pool (`ControlPlane` owns
   `.pool`; `SqlitePool` is the raw handle), and both are named in the issue.
3. A file is a **violation** iff signal (1) **and** signal (2) are both present.
   The check prints the file path, the offending construct, and a one-line
   pointer to the `AGENTS.md` rule, then exits non-zero.

### Why co-occurrence scoping rather than an allowlist

The issue offers two ways to keep `worker_test.rs` from breaking CI: flag it as
a "known exception" (an allowlist), or scope the check to avoid the false
positive. Scoping by co-occurrence is strictly better here: `worker_test.rs`
has no pool, so it is *not a true positive at all* — adding it to an allowlist
would wrongly imply it is a tolerated instance of the bad pattern. No allowlist
entry is created. (See ADR 0012 rejected alternatives for the escape-hatch
discussion.)

## Failure modes and edge cases

- **False positive — pause and pool in one file but different tests, both
  legitimate.** Possible in principle but unobserved today. The documented
  resolution is to split the unrelated paused-time test into its own
  `*_test.rs` (the codebase already favors one concern per sibling file), or, if
  genuinely unavoidable, the rationale for adding an escape hatch is recorded in
  ADR 0012 — deliberately *not* built now (no current need; speculative
  mechanism avoided).
- **False negative — a DB-touching test that reaches the pool via neither
  `SqlitePool` nor `ControlPlane`.** Control-plane tests construct the DB
  exclusively through `ControlPlane`/`SqlitePool` today, so the two signals
  cover the real surface. New indirection (e.g. a future repo handle that hides
  both names) would slip past; the convention remains the backstop and the
  signal list is cheap to extend.
- **`ast-grep` missing.** `check-test-layout.sh` already requires `ast-grep`
  and exits 2 with an install hint when absent; the new check reuses that exact
  contract so behavior is consistent.
- **No `*_test.rs` files / empty crates.** The loop simply finds nothing and
  the check passes with an `OK` line, mirroring `check-test-layout.sh`.

## Acceptance criteria (falsifiable)

1. `AGENTS.md` contains a rule, in the testing section, stating the
   pause-vs-pool prohibition and the prescribed alternative (real time + domain
   `Clock`). *Check: the section names both `tokio::time::pause` and
   `ManualClock`/injected `Clock`.*
2. `scripts/check-paused-time-db.sh` exists, is wired into `just ci`, and:
   - exits **0** on the current tree (verified: `just ci` green, no
     `worker_test.rs` false positive); and
   - exits **non-zero** on a fixture file containing both a `tokio::time::pause`
     call and a `SqlitePool`/`ControlPlane` reference (proven by an automated
     test, not by manual inspection).
3. The check does **not** flag `crates/voom-control-plane/src/scan/worker_test.rs`.
4. ADR 0012 records the decision with Status · Context · Decision · Consequences
   · Considered & rejected, is linked from this spec, and is listed in
   `docs/adr/README.md`.

## Test strategy

The check is a shell script; it is tested by a sibling bats-free harness — a
small shell test (or the check run against temporary fixture files) asserting:

- pause + `SqlitePool` → exit non-zero;
- pause + `ControlPlane` → exit non-zero;
- pause only (no pool) → exit 0 (the `worker_test.rs` shape);
- pool only (no pause) → exit 0;
- `clock.advance(...)` (domain clock) + pool → exit 0 (not a tokio call).

Fixtures live in temp dirs created by the test so they are never picked up by
the real `just ci` invocation. The implementation plan specifies the exact
harness.
