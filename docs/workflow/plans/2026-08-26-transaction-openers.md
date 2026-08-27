# Transaction openers are named helpers — implementation plan

Issue #552 · spec: `docs/workflow/specs/2026-08-26-transaction-openers-design.md` · ADR 0086
Governing rule: ADR 0083. Contention-test convention: ADR 0085.

Guardrails: `just ci` (fmt-check, lint, check-test-layout, check-paused-time-db,
check-control-plane-sql-boundary, check-check-constraint-bypass,
check-payload-deny-unknown, check-adr-index, select-ffmpeg-asset-selftest,
run-constrained-selftest, test, doc, deny, audit). The ADR index gate is coupled
and CI-hard-gated, so ADR 0086's row lands in this PR.

Conventions: AGENTS.md — sibling `*_test.rs` via `#[path]`; never pair
`tokio::time::pause` with a real `SqlitePool`; tests run on the pinned
`.test-tmp/` root.

## Global constraints

- **No new dependency.** `ast-grep` is already a `just setup` tool and a
  `check-paused-time-db` dependency. No Python, no new crate, no migration.
- **Wire `just ci` last.** The `prek` hooks run on every commit, so wiring the
  check in while 186 unconverted openers stand would fail every subsequent
  commit including the ones that fix them. T6 is last; until then the check is
  run directly.
- **The classification is evidence, not authority.** A scratchpad tool
  classified all 186 transactions. Every conversion is confirmed by reading the
  body before the name is chosen. The tool is not committed.

## T1 — The vocabulary and the guardrail

Files: `crates/voom-store/src/tx.rs` (new), `crates/voom-store/src/lib.rs`
(`pub mod tx;`), `scripts/check-transaction-openers.sh` (new),
`scripts/check-transaction-openers-selftest.sh` (new), `justfile` (the two bare
recipes — **not** added to `ci` here).

`tx.rs` holds both helpers from the spec, each mapping to its `BEGIN` mode
and wrapping the error with the caller's `context`. Each carries a doc comment
saying when to use it and what goes wrong otherwise, so the choice is answerable
at the call site without opening ADR 0083.

The check is one `ast-grep` rule over production sources, excluding `tx.rs`.

Verify: `./scripts/check-transaction-openers.sh` exits 1 and lists ~186 sites;
`./scripts/check-transaction-openers-selftest.sh` prints OK and exits 0. Break
any fixture's expectation and the selftest exits 1 naming it.

## T2 — Convert `voom-store` (86 transactions)

Files: `crates/voom-store/src/repo/**`, including deletion of the seven ad-hoc
helpers as their callers move: `begin` in `repo/library/mod.rs`,
`repo/media/backups.rs`, `repo/execution/workflow_summaries.rs`,
`repo/execution/workflow_progress.rs`; `begin_immediate` in
`repo/policy/policies.rs`; `begin_tx` in `repo/media/use_leases.rs`;
`begin_gate_tx` in `repo/media/commit_safety_gate.rs`.

One commit per repository module, not per file. The `prek` hook runs `just lint`
and the workspace test suite on every commit — measured at over two minutes —
so 59 per-file commits would spend hours in hooks for no extra bisect
resolution. A module is still a coherent, revertible unit. Each commit names the
transactions it converts and the shape it claims for each.

**Read before naming.** For each site, confirm from the source what its first
statement does on that handle, following `*_in_tx` callees. A site whose reading
contradicts the classification is converted to what the reading says, and the
discrepancy noted in the commit message.

The four ADR 0083 sites are in this task except `ControlPlane::force_release_lease`
(T3): `SqliteLeaseRepo::fail`, `SqliteLeaseRepo::force_release`,
`SqliteTicketRepo::mark_ready_if_unblocked` — all `begin_read_then_write`.

Verify per commit: `cargo test -p voom-store`. At the end, the check reports no
`voom-store` sites.

## T3 — Convert `voom-control-plane` (106 transactions)

Files: `crates/voom-control-plane/src/**`, including deletion of `begin_tx` and
`begin_immediate_tx` from `cases/mod.rs`. `commit_tx` stays — it is not an
opener. `local_worker.rs:452`'s direct `pool.begin()` is the one leak past the
existing helpers and converts like any other site.

Same discipline as T2: one commit per module, read before naming.
`ControlPlane::force_release_lease` becomes `begin_read_then_write`.

Verify per commit: `cargo test -p voom-control-plane`. At the end,
`./scripts/check-transaction-openers.sh` exits 0 over the workspace.

## T4 — The contention tests

Files: `crates/voom-store/src/repo/execution/leases_test.rs`,
`crates/voom-control-plane/src/cases/execution/leases_test.rs`.

Replace each 200 ms timed release with the spec's five-step ordered sequence.
Step 4 **awaits and unwraps a finished treatment** rather than asserting a bare
boolean — without that, criterion 5's `database is locked` evidence is
unreachable, because a reverted opener makes the treatment finish before step 4
and the test never reaches step 5. `tokio::sync` primitives only,
multi-threaded runtime, per ADR 0085. No production code gains a hook.

Both tests keep their existing assertions on the report and the ticket state.

Verify: `just test-repeat voom-store expire_due_waits_out_a_concurrent_writer 20`
and the `voom-control-plane` equivalent — 20/20 green each. Then open
`expire_due` with `begin_write_first` and run `cargo test -p voom-store
expire_due_waits_out_a_concurrent_writer`: it must fail naming
`database is locked`. Undo that change — it must not be committed.

## T5 — The first-run report

Files: `docs/workflow/specs/2026-08-26-transaction-openers-first-run.md` (new).

The check's clean run, the count of transactions by helper, and the two
revert-and-observe runs from T4 (criteria 2 and 5). This is criterion 3's report
and the evidence ADR 0086's `verified:` grounds cite.

Written after T4, so criterion 5's evidence is a run of the *new* tests — a run
recorded earlier would capture the 200 ms versions, which is the false-green this
change removes.

Verify: the counts match a fresh check run; the criterion-5 section quotes output
containing `database is locked`.

## T6 — Wire it into the guardrails

Files: `justfile` (both recipes into `ci`), `.pre-commit-config.yaml` (the hook
pair, delegating to the recipes like its siblings), `AGENTS.md` (an entry beside
`check-paused-time-db`).

`.pre-commit-config.yaml` neighbours PR #553's `check-check-constraint-bypass`
hook pair; the conflict if both land is textual only.

Verify: `prek run --all-files` passes; **`just ci` green on the final tree** —
this is criterion 6, and it is the run that matters, because T4 added two
multi-threaded contention tests that hold a write lock while the rest of the
binary runs on the shared pinned root. A filtered `test-repeat` exercises none of
that interaction.

## Rollback

T1 is additive. T2/T3 are call-site renames with no behavior change except at
the 24 sites whose mode actually changes — those are the #546 fix and are worth
keeping on ADR 0083 alone. If the check proves unworkable, reverting T1 and T6
leaves the vocabulary and the conversions in place.
