# Issue #187 — Guard against pairing `tokio::time::pause()` with the real SQLite pool in tests

- **Status:** Reviewed
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

The check operates on a **scan root** resolved CWD-relative (the `crates/*/src`
glob, exactly like `check-test-layout.sh`). This makes it testable: a harness
`cd`s into a temporary fixture tree laid out as `crates/<x>/src/<y>_test.rs` and
runs the check there, so fixtures never touch the real `crates/` tree.

For each `crates/*/src/**/*_test.rs` under the scan root:

1. **Paused-time signal** (`ast-grep`, `--lang rust`). Present if the file
   contains a `tokio::time::pause` / `tokio::time::advance` call written in
   **any** idiomatic call form, because the realistic *reintroduction* path is a
   `use` import, not the fully-qualified call. The check matches:
   - a scoped call `tokio::time::pause()` / `tokio::time::advance($$$)` (today's
     form in `worker_test.rs`); **or**
   - a scoped call `time::pause()` / `time::advance($$$)` (via `use tokio::time;`);
     **or**
   - a **bare** call `pause()` / `advance($$$)` **gated on** the file also
     containing a `use tokio::time` import (via `use tokio::time::{pause, …};`).

   Matching is on `ast-grep` call-expression nodes, so commented-out or
   stringified mentions never trip the check. The injected domain clock is a
   **method** call (`clock.advance(...)` / `fixture.clock.advance(...)`, an
   `&self` method on `ManualClock`), which is a different syntax node
   (field-expression callee) and is therefore never matched by any of the three
   forms above. The bare-call form is gated on a `use tokio::time` import so a
   hypothetical user-defined free `fn pause`/`advance` cannot trip it.
2. **DB-pool signal** (`ast-grep`, **exact identifier-node** match): a reference
   to the type identifier `SqlitePool` **or** `ControlPlane`. These are the two
   types through which control-plane tests reach the pool (`ControlPlane` owns
   `.pool`; `SqlitePool` is the raw handle), and both are named in the issue.
   Exact-node matching means near-miss identifiers (`SqlitePoolOptions`,
   `ControlPlaneConfig`, `ControlPlaneError`) do **not** match, and — being
   `ast-grep`, not text — neither do mentions inside comments or strings.
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
- **False negative — paused-time call via a renaming import** (`use
  tokio::time::pause as p; p()`). Not matched: the bare-call form keys on the
  function name `pause`/`advance`. This is an exotic shape with no precedent in
  the tree; the convention is the backstop. Listed for honesty, not designed
  around.
- **False positive — a file-local free `fn pause`/`advance` plus a `use
  tokio::time` import plus a pool reference.** The bare-call form is gated on a
  `use tokio::time` import precisely to make this unlikely, but a file that both
  imports `tokio::time` *and* defines its own `pause`/`advance` *and* references
  a pool would flag spuriously. No such file exists today; resolution is to
  rename the local function or split the file (same as the next item).
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
5. The check's own self-test runs as part of `just ci` (not merely as a file
   that exists), so a future edit that breaks the `ast-grep` patterns fails CI.
   *Check: removing/sabotaging the check's matching logic makes `just ci` go
   red via the self-test.* The check script also passes `shellcheck` and
   `shfmt -d` and starts with `set -euo pipefail`, matching
   `check-test-layout.sh`.

## Test strategy

The check is a shell script; it is tested by a sibling shell self-test
(`scripts/check-paused-time-db-selftest.sh`, started with `set -euo pipefail`)
that `cd`s into a per-case temporary fixture tree (`crates/<x>/src/<y>_test.rs`)
and runs the check there, asserting exit code per case. The self-test gets its
own `just` recipe that is added to the `ci` target, so it runs on every `just
ci` and in GitHub Actions — guaranteeing the patterns cannot silently rot.
Because the check resolves `crates/*/src` CWD-relative, the temp tree fully
isolates fixtures from the real `crates/` — the self-test writes only under a
`mktemp -d` directory it removes on exit, so a concurrent real `just ci` never
sees the fixtures. Cases:

- `tokio::time::pause()` (scoped) + `SqlitePool` → exit non-zero;
- `tokio::time::advance(..)` (scoped) + `ControlPlane` → exit non-zero;
- `use tokio::time::{pause, advance};` + bare `pause()` + `SqlitePool` → exit
  non-zero (the realistic reintroduction shape);
- `use tokio::time;` + `time::pause()` + `ControlPlane` → exit non-zero;
- paused-time only, no pool → exit 0 (the `worker_test.rs` shape);
- pool only, no paused-time → exit 0;
- `clock.advance(..)` (domain `ManualClock` method) + pool → exit 0 (method
  call, not a tokio free call);
- `SqlitePoolOptions` / `ControlPlaneConfig` near-miss + paused-time → exit 0
  (exact-identifier match must not over-match);
- a commented-out `// tokio::time::pause()` + pool → exit 0 (`ast-grep` ignores
  comments).

The implementation plan specifies the exact harness and `ast-grep` invocations.
