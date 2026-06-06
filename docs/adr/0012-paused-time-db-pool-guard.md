---
status: accepted
date: 2026-06-05
deciders: [VOOM core]
---

# 0012 — Guard against pairing tokio paused time with the real SQLite pool in tests

## Context

While de-flaking the chaos/durable-workflow suite (#178, PR #186) we identified
a flake class: a test that pairs tokio's paused virtual clock
(`tokio::time::pause()` / `tokio::time::advance()`) with a real `SqlitePool`
fails spuriously.

When tokio time is paused, the runtime auto-advances virtual time to the next
pending timer whenever it has no runnable task. `sqlx` runs SQLite on a blocking
thread pool, so while an `await` is parked on that blocking thread (connection
open, or a query under CPU starvation) the async runtime is idle and the paused
clock jumps forward — past the pool's `acquire_timeout` (sqlx default 30s). The
DB call then returns `DbUnreachable` even though nothing is wrong. It is a
clock-domain mismatch: tokio's virtual *test* clock conflated with the *wall
clock* sqlx measures `acquire_timeout` against.

PR #186 fixed the single affected test
(`await_with_lease_heartbeats_refreshes_workflow_lease_while_future_runs`) by
running its loop in real time while keeping the assertion deterministic via the
injected `ManualClock`. The trap is easy to reintroduce; only a guardrail keeps
it out.

Today exactly one test uses tokio paused time —
`crates/voom-control-plane/src/scan/worker_test.rs` — and it pauses around a
process-launch timeout with **no** pool, so it is not an instance of the bad
pattern.

Design doc:
[`docs/superpowers/specs/2026-06-05-issue-187-paused-time-db-guardrail-design.md`](../superpowers/specs/2026-06-05-issue-187-paused-time-db-guardrail-design.md).

## Decision

Adopt two complementary layers:

1. **Convention.** `AGENTS.md` records the rule in its testing section: do not
   pair `tokio::time::pause()`/`advance()` with a real `SqlitePool`. Drive
   DB-touching tests on real time and control domain time via the injected
   `Clock` (`ManualClock`).

2. **Scoped check.** `scripts/check-paused-time-db.sh`, wired into `just ci`,
   scans both sibling unit tests (`crates/*/src/**/*_test.rs`) and integration
   tests (`crates/*/tests/**/*.rs`) — the trap is reachable in either — and
   fails when one file contains **both** a tokio paused-time call and a DB-pool
   reference (`SqlitePool` or `ControlPlane`,
   matched as exact identifier nodes). The paused-time signal matches the call
   in any idiomatic form — fully-qualified `tokio::time::pause()`/`advance(..)`,
   `time::pause()` via `use tokio::time;`, or a bare `pause()`/`advance(..)`
   gated on a `use tokio::time` import — because the realistic reintroduction is
   an import, not the fully-qualified call. It excludes the injected
   `ManualClock` (`clock.advance(..)` is an `&self` method call, a different
   syntax node). It uses `ast-grep` so it matches real syntax-tree items, not
   comments or string literals — the same tooling choice as
   `check-test-layout.sh`.

The check is scoped by **co-occurrence in a single file** rather than by an
allowlist. `worker_test.rs` has the pause but no pool, so it is excluded by
scope and stays green; no allowlist entry is created.

## Consequences

- A test that reintroduces the pause-plus-pool pattern fails `just ci` (locally
  via pre-commit and in CI) with a pointer to the `AGENTS.md` rule, before it
  can flake.
- `just ci` gains two fast shell steps: the check itself and its self-test
  (which runs the check against temporary fixtures). Both are `ast-grep` over a
  handful of files, comparable in cost to the existing `check-test-layout`. The
  self-test runs in CI so the matching patterns cannot silently rot.
- The signal set (`SqlitePool`, `ControlPlane`) covers how control-plane tests
  reach the pool today. A future test that hides the pool behind a different
  type slips past the check; the written convention remains the backstop and
  the signal list is a one-line edit to extend.
- No escape hatch ships. If a legitimate pause-plus-pool-in-one-file case ever
  arises, the documented first resort is to split the unrelated paused-time test
  into its own sibling `*_test.rs`; an inline-marker escape hatch is added only
  if a real case proves splitting insufficient.

## Considered & rejected

- **Convention only (no check).** Rejected: a written rule is silently
  ignorable, and this flake already slipped past human review once (#186 found
  it during verification, not review). Enforcement is the point.
- **Check only (no written convention).** Rejected: a flagged author needs to
  know *why* and what to do instead. The rule and the failure message reinforce
  each other.
- **Ban `tokio::time::pause()` outright.** Rejected: it is legitimate where no
  DB is involved (`worker_test.rs`). Banning it would force that test onto real
  time for no benefit.
- **Allowlist `worker_test.rs` as a known exception.** Rejected: it has no pool,
  so it is not a true positive. Allowlisting it would falsely imply it is a
  tolerated instance of the bad pattern. Co-occurrence scoping is more honest.
- **Ripgrep/text-pattern check.** Rejected for the paused-time signal for the
  same reason as ADR 0004: text patterns are fooled by comments, string
  literals, and disabled cfgs — exactly the silent failure mode a guardrail must
  avoid. `ast-grep` inspects the syntax tree.
- **Shrink the test pool's `acquire_timeout`.** Rejected as the *fix*: per the
  issue, a smaller timeout only makes the failure clearer; it does not stop the
  paused clock from auto-advancing past it. It would add production/test config
  divergence for no correctness gain.
- **Ship an inline escape-hatch marker now** (e.g.
  `// allow-paused-time-db: <reason>`). Rejected as speculative: there is no
  current case that needs it, and an unused escape hatch invites misuse.
  Deferred until a real case justifies it (recorded above).
- **Custom clippy-driver lint.** Rejected: orders of magnitude more code than an
  `ast-grep` shell script for the same outcome, matching the ADR 0004 reasoning.
