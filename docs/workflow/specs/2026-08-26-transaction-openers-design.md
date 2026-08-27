# Transaction openers are named helpers — design

Issue: [#552](https://github.com/randomparity/voom-v2/issues/552)
ADR: [0086](../../adr/0086-transaction-openers-are-named-helpers.md)
Governing rule: [ADR 0083](../../adr/0083-read-then-write-transactions-begin-immediate.md)
Contention-test convention: [ADR 0085](../../adr/0085-contention-tests-at-the-use-case-level.md)

## Goal

Make ADR 0083's rule enforceable by making every transaction opener state what
shape of transaction it opens, and add a guardrail that no other opener exists.

## Non-goals

- Verifying that a stated shape matches the transaction's body. ADR 0086 accepts
  deliberateness over verification and says why.
- Converting every deferred `pool.begin()` to `BEGIN IMMEDIATE` (ADR 0083).
- Raising `busy_timeout` or retrying on `SQLITE_BUSY` (ADR 0083).
- Closing the contention tests' residual vacuous-pass window (issue #588).

## Architecture

### The vocabulary

One new module, `crates/voom-store/src/tx.rs`, `pub` so `voom-control-plane`
shares it:

```rust
/// Open a transaction that reads before it writes.
///
/// Takes SQLite's write lock up front, so `busy_timeout` serializes writers.
/// A deferred `BEGIN` here fails with `SQLITE_BUSY` on the lock upgrade without
/// consulting the busy handler at all — see ADR 0083.
pub async fn begin_read_then_write(pool: &SqlitePool, context: &'static str)
    -> Result<Transaction<'_, Sqlite>, VoomError>;

/// Open a transaction whose first statement writes.
///
/// A deferred `BEGIN` is correct: the write lock is taken at the first
/// statement, so there is no upgrade to refuse.
pub async fn begin_write_first(pool: &SqlitePool, context: &'static str)
    -> Result<Transaction<'_, Sqlite>, VoomError>;

/// Open a transaction that only reads.
pub async fn begin_read_only(pool: &SqlitePool, context: &'static str)
    -> Result<Transaction<'_, Sqlite>, VoomError>;
```

`context` preserves the call-specific error text the existing sites already
pass to `VoomError::database_context`.

`begin_write_first` and `begin_read_only` both emit a plain `BEGIN`. They stay
separate because the name is the record of the author's claim — see ADR 0086.

### The guardrail

`scripts/check-transaction-openers.sh`: one `ast-grep` rule matching
`.begin()` / `.begin_with(…)` on a pool receiver, over production sources, with
`crates/voom-store/src/tx.rs` excluded. Any match is a violation naming the file,
line, and the three helpers.

A savepoint (`tx.begin()` on a live handle) is not a pool-level opener and is not
matched: the rule keys on the receiver being a pool, not on the method name.

### What is removed

- `voom-store`'s seven ad-hoc helpers: `begin` in `repo/library/mod.rs`,
  `repo/media/backups.rs`, `repo/execution/workflow_summaries.rs`, and
  `repo/execution/workflow_progress.rs`; `begin_immediate` in
  `repo/policy/policies.rs`; `begin_tx` in `repo/media/use_leases.rs`;
  `begin_gate_tx` in `repo/media/commit_safety_gate.rs`.
- `voom-control-plane`'s `begin_tx` and `begin_immediate_tx` in `cases/mod.rs`.
  `commit_tx` stays — it is not an opener.

## The migration

186 pool-level transactions get a named opener. Each one's shape comes from a
one-time classification, and each is **read before it is converted** — the
classification is evidence, not authority.

| shape | count | helper |
|---|---:|---|
| reads then writes | 24 deferred + 45 already immediate | `begin_read_then_write` |
| first statement writes | 67 | `begin_write_first` |
| read-only | 9 | `begin_read_only` |
| classification uncertain | 41 | read the body; no allow file exists to defer into |

The 41 uncertain ones are where the abandoned analyzer gave up. They are not a
residue to disposition here — there is nothing to disposition *into*, because the
new check has no allow file. Each is read and given a name like any other site.

The four sites ADR 0083 deferred to this issue are in the 24 by construction:
`SqliteLeaseRepo::fail`, `SqliteLeaseRepo::force_release`,
`SqliteTicketRepo::mark_ready_if_unblocked`, `ControlPlane::force_release_lease`.

## Also in scope: the contention tests' barrier

Both `expire_due_waits_out_a_concurrent_writer` tests (one per crate, PR #558)
release their competing writer on a 200 ms timer. A host slow enough for
`expire_due` to take >200 ms to reach its first `UPDATE` passes even against a
deferred `BEGIN` — a false-green.

Replace the timer with an ordered control/treatment sequence, `tokio::sync`
primitives only, multi-threaded runtime, per ADR 0085:

1. **Control, before.** A deferred `BEGIN` against the held lock must fail
   `database is locked` — proof the lock is held and the window is real.
2. **Start the treatment** and signal that it has been spawned.
3. **Control, after.** The deferred control arm must fail the same way again —
   proof the lock was *still* held once the treatment was under way.
4. **Assert the treatment has not finished.** A deferred `BEGIN` fails fast, so a
   treatment that already returned cannot have waited. **If it has finished,
   `await` and unwrap it inside the assertion**, so the panic carries the
   treatment's own error rather than a message about task state — that is what
   makes criterion 5's `database is locked` evidence reachable.
5. **Release** the writer; the treatment must succeed.

A window too short to contend makes a control arm succeed, which reddens the test
rather than passing it. There is no wall-clock budget for a slow host to outrun.

**Residual.** Nothing exposes "this connection is now waiting on the write lock",
so the test cannot prove the treatment reached its `BEGIN`. If it has not, steps
3 and 4 both pass, the writer is released, and the treatment runs uncontended and
succeeds — a vacuous pass, failing toward a **green treatment**. The window
shrinks from 200 ms to two fail-fast transaction round-trips. Owned by issue
[#588](https://github.com/randomparity/voom-v2/issues/588).

No production code gains a test-only hook: the control arm is test-local SQL
against the test's own pool.

## Error handling

The check exits 0 clean, 1 on violations (each printed `file:line` with the
helper names), 2 on a missing `ast-grep` or an unparseable file — a file
`ast-grep` cannot parse yields no nodes, which would pass vacuously.

**Anti-vacuity.** The rule's own matcher cannot measure whether it still works,
so the selftest asserts it against a fixture that must match. A rule that silently
stops matching is the failure mode a boundary check has, and the only one.

## Testing

**Selftest** (`check-transaction-openers-selftest.sh`), fixtures under a temp
root, each asserted in both directions:

| fixture | asserts |
|---|---|
| `direct_begin` | `pool.begin()` in production code → exit 1 |
| `direct_begin_with` | `pool.begin_with("BEGIN IMMEDIATE")` → exit 1 |
| `helper_call` | `begin_read_then_write(&self.pool, "ctx")` → exit 0 |
| `savepoint` | `tx.begin()` on a live handle → exit 0 |
| `opener_module` | the same `pool.begin()` inside `tx.rs` → exit 0 |
| `test_source` | `pool.begin()` in a `*_test.rs` → exit 0 |
| `unparseable_file` | exit 2, not a vacuous 0 |
| `matcher_alive` | a fixture that must match; if it does not, the rule has rotted |

**Regression proof (criterion 2).** Reverting #546's change to
`ControlPlane::expire_due` must redden a check. Under this design that is the
`voom-store` unit test, not the opener check — the opener check sees a helper
call either way, because the revert changes *which* helper. So criterion 2 is
satisfied by criterion 5's revert-and-observe: with `expire_due` opened by
`begin_write_first` instead of `begin_read_then_write`, both contention tests
fail naming #546's `database is locked` error. That run is recorded in the
first-run report.

This is a real narrowing against the issue's wording, and it is deliberate: the
issue assumed a check that verifies ordering. A boundary check cannot redden on a
changed mode, and ADR 0086 accepts that trade. The property "reverting #546
reddens CI" still holds, through the tests rather than the gate.

## Success criteria

1. `just check-transaction-openers` exists, delegates to a script, and runs in
   `just ci` and the `prek` hooks. — issue #552 criterion 1.
2. `just check-transaction-openers-selftest` passes, and every fixture flips when
   inverted. — issue #552 criterion 2, as narrowed above.
3. The check exits 0 over the workspace, with all 186 transactions opened by a
   named helper and no allow file. — issue #552 criterion 3.
4. The four ADR 0083 sites open with `begin_read_then_write`. — issue #552
   criterion 4.
5. Neither `expire_due_waits_out_a_concurrent_writer` test contains a timed
   release. Verified both ways:
   - **Holds:** `just test-repeat <crate> expire_due_waits_out_a_concurrent_writer 20`
     — 20 green iterations, each crate.
   - **Bites:** open `expire_due` with `begin_write_first` and run
     `cargo test -p voom-store expire_due_waits_out_a_concurrent_writer`. It must
     **fail**, naming `database is locked`. `test-repeat` is the wrong tool: it
     stops at the first failure and reports that as its own, so its exit code
     cannot separate "the guard bit" from "the recipe broke".
6. `just ci` green on the final tree.

## Threat model

Not security-relevant. No entry point, no authn/authz, tenancy, secret, or
permission grant; parses no untrusted input; adds no dependency — `ast-grep` is
already required by `just setup` and by `check-paused-time-db`. The check reads
repository sources under the developer's own account and writes nothing.

Recorded because the completeness rule asks for the judgment, not because a
boundary was found.
