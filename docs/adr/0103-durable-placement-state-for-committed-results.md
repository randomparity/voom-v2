# 0103 — Committed results carry a durable placement state

## Status

Accepted

## Context

Issue #663 (epic) found that nothing durable says whether a committed result has left its
staging root. A committed record whose result location sits under a staging root may be
waiting for the workflow to move it to an output root, may have been left there on purpose
(a manual `voom artifact commit`, which ADR 0097's "Later decision: commit resolves the
staging root" allows), or may already have been moved. The store cannot tell these apart.
The retire guard (#678) and any diagnostic for a never-moved chain tip (#679) both need
that fact. The operator chose to build it as durable state (#663, 2026-10-02 checkpoint).

Three earlier decisions bound the answer:

- ADR 0074 gives `artifact_commit_records.promotion_started_at` a meaning: the start of
  the node-local staging-to-target commit. The new state must not reuse "promotion".
- ADR 0050 makes the control plane byte-blind and calls the coordinator's filesystem move
  (`workflow/coordinator/promotion.rs`) transitional, pending #416–#425, and not to be
  extended (`0050:264-268`).
- #622 was closed not planned. Promotion skips a chain tip that matches no working dir
  with a bare `continue`, and that same `continue` is the documented idempotent resume
  path. Its close rested on two grounds. First, an observable would fire on every resume
  because nothing told "already moved" from "never matched". Second, adding one would grow
  the transitional path.

## Decision

1. **The state.** Each `artifact_commit_records` row carries `placement_intent`
   (`staged` or `retained`, set at prepare, never changed) and `placement_state`:
   - `NULL` while the record is not `committed`;
   - `staged`: committed, and the caller expects a move to an output root;
   - `retained`: committed and deliberately left at its commit address;
   - `placed`: moved to an output root. Only a `staged` result can become `placed`.

   Commit sets `placement_state` to the record's `placement_intent` in the same statement
   that marks it committed. The caller supplies the intent through
   `CommitArtifactInput::placement_intent`. Manual `voom artifact commit` passes
   `retained`. Every producer of workflow results passes `staged`, including the
   node-owned path that #416–#425 builds. Promotion refuses a non-`staged` record and
   fails the run. A recovery successor copies its predecessor record's intent.
2. **Enforcement.** Migration 0044 adds both columns with `ALTER TABLE ADD COLUMN`,
   backfills existing rows, and enforces the per-state rule with two `BEFORE` triggers.
   The rule: intent present; state non-NULL exactly when committed and equal to the
   intent, except a `staged` intent may read `placed`; intent immutable; `placed`
   terminal. The triggers check one row, the same scope as the table's CHECK.
3. **Terminal states outside the move.** A superseded intermediate that the coordinator
   reclaims stays `staged`, and its result location is retired. "Staged with a retired
   result location" means withdrawn: no bytes remain and no move will follow. Sidecar
   outputs carry commit records and follow `staged → placed`. A scoped location with no
   committed record has no placement state, so the move succeeds and writes nothing.
   This is defensive: no in-tree producer emits such a location today.
4. **Ruling on ADR 0050's transitional path.** The control plane may persist this state.
   It is a lifecycle fact about a durable record, not an observation of bytes. The
   transitional move writes `placed` only inside its existing address-update transaction,
   with a compare-and-set from `staged`. It refuses, before moving bytes, a result whose
   record is not `staged`. `promotion_plan()` gains no capability. The node-owned
   successor (#425) must write the same `staged → placed` transition in the transaction
   that records the new address.
5. **#622 superseded.** Ground one no longer holds: a skipped tip whose record is `placed`
   was already moved, and one that is still `staged` never was. Ground two no longer holds
   either. The state belongs to the commit lifecycle, which #425 inherits, not to the
   transitional path. #622's reconsideration condition (b) also points at a discriminator.
   This ADR therefore authorizes #679, on this surface only: when promotion skips a live
   chain tip that matches no working dir and whose record is `staged`, it emits one
   warning-level log naming the commit record and the file location. `placed`, `retained`
   and record-less tips stay silent. The run's outcome, resume behavior, and the
   event taxonomy do not change.
6. **#678's guard contract.** Retiring a root is refused while a `committed` record whose
   placement is `staged` or `retained` has its live result location on that root. A
   `placed` result never blocks. It lives on the configured output root, which may be the
   same root when a root is its own output default. Retiring a root while it holds placed
   outputs is the output-root question that #663 excludes, and the stranding in the
   coincident case is accepted under that exclusion. A withdrawn intermediate never blocks,
   because its location is retired. A later decision owns any guard for output roots.

## Consequences

- Upgrading existing databases needs no preflight refusal. The backfill classifies by the
  working-dir convention promotion itself matches: a commit-time `target_path` with a
  `.committed` component is `staged`, otherwise `retained`. A `staged` committed row whose
  live result locator no longer has a `.committed` component is `placed`. A manual commit
  aimed inside `.committed/` is backfilled `staged`. That is harmless because no
  promotion scope contains it.
- A workflow resumed across the upgrade still promotes. Its unmoved tips were backfilled
  `staged`.
- Readers that ask "what still awaits a move" must join a live result location. `staged`
  alone also matches withdrawn intermediates.
- The window in which bytes are moved before the address transaction remains as it was.
  The new write adds no step before the move other than a read.
- Every insert into `artifact_commit_records` must name `placement_intent`. Test fixtures
  that insert rows directly change with this ADR.

## Considered & rejected

- **Rebuild `artifact_commit_records` with a table CHECK.** verified: in SQLite 3.51.2
  with `PRAGMA foreign_keys=ON`, `DROP TABLE` of a parent row referenced through
  `ON DELETE RESTRICT` fails with `FOREIGN KEY constraint failed (19)` (scratch
  reproduction of a parent/child pair during design of #677). `RESTRICT` acts at once, so
  `PRAGMA defer_foreign_keys` does not help. Three tables reference this one
  (`0001:1281`, `0001:1572`, `0043:40`). Migrations run in one transaction, where
  `PRAGMA foreign_keys` cannot be toggled (`0001_schema.sql:4-5`). judgment: copying all
  three children as well (0043 shows one child rebuild) would triple the migration and
  add no guarantee that the triggers lack.
- **Add the per-state rule as a column CHECK on `ADD COLUMN`.** verified: the same SQLite
  rejects `ALTER TABLE p ADD COLUMN y TEXT CHECK ((state='committed') = (y IS NOT NULL))`
  with `CHECK constraint failed` while a committed row exists. The constraint is tested
  against existing rows before any backfill can run.
- **A child table `artifact_result_placements`.** judgment: "a committed record has a
  placement" spans two tables, so it can be enforced at neither insert nor update, and
  every reader pays a join. The column-plus-trigger form enforces both directions per row.
- **Keep the intent on `artifact_commit_intents`.** judgment: intents are transient fence
  state for the node (`0043` header). The intent belongs beside `target_path` on the
  record, which is where recovery already reads its successor input.
- **Infer the intent from the target path at runtime.** judgment: issue #677 asks for a
  deliberate, durable statement, and path conventions are a guess. The convention is used
  only once, to classify legacy rows.
- **A fifth state for reclaimed intermediates.** judgment: #677 names the four states. A
  retired result location already says "withdrawn" to the only consumers (#678, #679).
- **Do nothing; keep #622 closed.** judgment: the operator chose durable state for #663.
  Without it, #678 cannot say what blocks a retirement.
