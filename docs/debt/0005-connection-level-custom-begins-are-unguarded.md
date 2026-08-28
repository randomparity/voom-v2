# 0005 — Connection-level custom `BEGIN` opens are unguarded

## Status

Open
review-by: 2026-11-27

## Concern

ADR 0087 fixes the cancellation window in `voom_store::tx`'s two `BEGIN IMMEDIATE`
openers, and `scripts/check-transaction-openers.sh` is what makes that fix reach every
production transaction — it fails a build in which production code calls `pool.begin*()`
outside `voom-store/src/tx.rs`.

That check cannot see an `acquire`-then-`begin_with` open. Its ast-grep rule
(`scripts/check-transaction-openers.sh:68-79`) matches `$POOL.begin()` and
`$POOL.begin_with($$$MODE)` under `constraints: POOL: regex: "(?i)pool"`. The receiver
constraint is deliberate — it is what stops a savepoint (`tx.begin()` on a live handle)
from matching a rule about pool-level openers — and it also makes a connection receiver
invisible.

`crates/voom-store/src/init.rs:54-56` is exactly that shape: `pool.acquire()` followed by
`conn.begin_with("BEGIN IMMEDIATE")`, on the same two-step `SqliteTransactionManager::begin`
path, with the same await point between the write lock and the `Transaction` construction.
`./scripts/check-transaction-openers.sh crates` reported `check-transaction-openers: OK
(378 files)`, exit 0, with it present (Fedora, Linux 7.1.8-200.fc44.x86_64, ast-grep as
installed by `just setup`, repo at `d130cebb`).

## Why deferred

**This record owns two things: the live `init.rs:54-56` site, and the recurrence guard.**

An earlier version of this record owned only the guard, because issue #592's plan intended
to route `run_migrations_on` through `begin_read_then_write` in the same change. That task
was cut: no completion criterion required it, and the charter grants
`crates/voom-store/` as *surface* — where a change may land — without authorizing a second
production rewrite, the supersession of a sentence of ADR 0068, and the loss of the
`acquire connection for migration` error context on the quest's own authority. The live
site therefore stays, and is owned here rather than left unattributed.

What the live site is worth, stated plainly so a later reader does not re-derive it: the
hazard is real but not reachable by the cancellation sources ADR 0087 identifies.
`run_migrations_on`'s only production caller is
`crates/voom-cli/src/commands/system/init.rs:16` — every other caller is a test, and
`init_on` is behind the `test` feature at `init.rs:31` — so it sits behind neither
`bounded_router`'s `TimeoutLayer` nor an axum client disconnect. A CLI invocation's
cancellation is SIGINT, which takes the process and the file lock with it. That is why it
is deferrable at all; it is not an argument that the shape is safe.

The recurrence guard is deferred for a second, independent reason: `scripts/` is outside
issue #592's frozen change surface
(https://github.com/randomparity/voom-v2/issues/592#issuecomment-5447684231, token
`q592-387107cd`), whose surface is the crates on the deadlocked path plus `docs/`.
Extending the rule is also not a one-line edit: the receiver constraint exists to separate
pool openers from savepoints, and a rule that matches `conn.begin_with` has to keep that
separation — most likely by matching the statement shape rather than the receiver name,
which needs its own selftest case in `check-transaction-openers-selftest`.

## Non-regression boundary

Issue #592's change must not add a production `conn.begin_with` or
`conn.begin_with`-equivalent custom-statement open, and must not weaken
`check-transaction-openers.sh`. After it lands, this predicate returns matches in exactly
two files — `crates/voom-store/src/tx.rs` and `crates/voom-store/src/init.rs` — and no
others:

```
rg -n 'begin_with' crates --type rust \
  -g '!**/tests/**' -g '!**/*_test.rs' -g '!crates/voom-test-support/**'
```

The support-crate exclusion is not a loosening. `crates/voom-test-support/src/`
holds three custom-statement opens (`commit_node.rs:89,110`,
`staging_seed.rs:59`) which `check-transaction-openers.sh` exempts through its
`grep -Ev "/(voom-test-support|voom-fakes|voom-fake-support|voom-conformance)/"`
filter — a different mechanism from its `! -name '*_test.rs' ! -path '*/tests/*'`
filter. An earlier wording of this section said "only `tx.rs` and in test files",
which those three falsify, making the boundary unable to distinguish success from
failure.

`init.rs` appears in the expected set because the live site stays — see *Why deferred*.
A predicate demanding `tx.rs` alone would go red on the tree issue #592 actually
produces, which is a boundary that fails for the wrong reason. This section is stated at
file granularity because line numbers do not survive the change: `tx.rs`'s two opener
lines are replaced by a single `pool.begin_with(statement)` inside `begin_detached`, at a
different number.

## What would resolve it

A second pattern in `scripts/check-transaction-openers.sh` that matches a custom-statement
`begin_with` on a connection receiver while still exempting savepoints, plus a
`check-transaction-openers-selftest` case that fails without it. Done when a fixture
containing `let mut conn = pool.acquire().await?; conn.begin_with("BEGIN IMMEDIATE")` in a
production path makes `just check-transaction-openers` exit non-zero, and the selftest
proves it.

## Provenance

target: docs/adr/0087-cancellation-safe-begin-immediate.md
target: scripts/check-transaction-openers.sh
Raised by `$gauntlet` during `$trial-loop` on ADR 0087, iteration 2, 2026-08-27, under
`$quest` for issue #592 (scope token `q592-387107cd`).
tracker: #592
