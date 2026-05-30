---
status: accepted
date: 2026-05-29
deciders: [VOOM core]
---

# 0006 — Durable workflow-summary schema and repository shape

## Context

Sprint 16 (`docs/superpowers/specs/2026-05-29-voom-sprint-16-design.md`, §4)
persists per-phase workflow summaries using a two-grain model that, with the
job-level parent, is three tables: a job row, a per-phase child, and a
per-`(file, phase)` grandchild. There is no reports table — each phase's
compliance report folds into its per-phase summary row. Issue #161 delivers the
schema (migration) and `SqliteWorkflowSummaryRepo`; the coordinator that writes
these rows during a run is #162 and later.

The spec settles *what* is stored at each grain (§4). What it leaves open is the
concrete relational shape and the Rust repository surface. Decisions to pin
before the coordinator is built against this surface:

1. **Layering.** `voom-store` sits below `voom-control-plane`
   (`AGENTS.md` → Crate layering), so the repo cannot import
   `WorkflowRunSummary`/`OperationSummary` (control-plane types). What do the
   repo's input/row structs look like?
2. **Foreign-key spine.** Children are written *incrementally* as each file
   commits, before the job-level parent's final counters are known. What do the
   three tables reference, and with what delete behaviour?
3. **List- and document-valued fields.** A per-`(file, phase)` row carries a
   *set* of ticket IDs; the parent carries a `per_operation` rollup; a per-phase
   row carries a compliance report. How are these encoded?
4. **`elapsed` encoding.** `WorkflowRunSummary.elapsed` is a `Duration`. How is
   it stored losslessly?
5. **Per-`(file, phase)` outcome vocabulary.** The spec pins the per-phase
   vocabulary (`completed | partially-committed | skipped | blocked`) but not
   the per-file one.

## Decision

### Three tables, keyed off `jobs`, not off each other

- `workflow_summaries` — job grain. PK `job_id` (one summary per job), FK
  `job_id → jobs(id) ON DELETE CASCADE`.
- `workflow_phase_summaries` — per-phase grain. Autoincrement `id`, natural key
  `UNIQUE (job_id, phase_ordinal)`, FK `job_id → jobs(id) ON DELETE CASCADE`.
- `workflow_file_phase_summaries` — per-`(file, phase)` grain. Autoincrement
  `id`, natural key `UNIQUE (job_id, phase_ordinal, branch_id)`, FK
  `job_id → jobs(id) ON DELETE CASCADE`.

All three reference `jobs(id)` directly. Children do **not** FK the parent
`workflow_summaries` row, because per-`(file, phase)` and per-phase rows are
written incrementally *as each file's phase artifact commits* (§4, §6), which is
before the job-level parent (with final counters and `elapsed`) is written at job
end. A child-FK-to-parent would force the parent to exist first and break the
incremental-write invariant the spec requires. `ON DELETE CASCADE` to `jobs`
means a job's whole summary tree is reclaimed with the job.

### Scalar produced-artifact references are real FKs; the ticket *set* is JSON

A per-`(file, phase)` row's singular produced references —
`produced_file_version_id → file_versions(id)`,
`produced_file_location_id → file_locations(id)`,
`artifact_handle_id → artifact_handles(id)`,
`reprobe_snapshot_id → media_snapshots(id)` — are real FK columns with
`ON DELETE RESTRICT`, matching how `file_versions` is referenced elsewhere
(`0012_staged_artifact_commit.sql`). They are nullable: a file that did not
advance produces none.

`ticket_ids` is a *set* per row and therefore cannot be a column FK. It is stored
as a JSON array of integers in a `TEXT` column with a `json_valid` CHECK, the
same content-addressed-document convention the codebase already uses for
`report`/`explanation_json` columns. Referential integrity for the ticket set is
the coordinator's responsibility, not a column constraint.

### `per_operation` rollup and the compliance report are stored as JSON documents

`workflow_summaries.per_operation` and `workflow_phase_summaries.report` are
`TEXT` columns with `json_valid` CHECKs holding opaque JSON. The store does not
model `OperationKind`-keyed `OperationSummary` (a control-plane type it cannot
import, decision 1) nor the `ComplianceReport` shape (a `voom-plan` type). The
repo's input structs take `serde_json::Value`; the caller (control-plane)
serializes its own types in. This keeps the store decoupled and matches the
spec's "report JSON (or its hash)" framing (§4): the column holds whichever the
caller provides.

`workflow_phase_summaries.report_id` is the content-addressed report identity
(`voom-plan` derives it from the report preimage; a `TEXT` hash). `report_id`
and `report` are either both present or both NULL (a CHECK enforces this): a
skipped or blocked phase regenerates no report.

### `elapsed` is stored as integer nanoseconds

`workflow_summaries.elapsed_ns INTEGER NOT NULL CHECK (elapsed_ns >= 0)`. The
repo maps `Duration ↔ u64` nanoseconds, lossless for any realistic run (the u64
ceiling is ~584 years). `throughput_per_second` is **not** persisted — it is
derivable from `dispatch_count` and `elapsed` and the issue's counter list omits
it.

### Per-`(file, phase)` outcome vocabulary: `committed | skipped | blocked`

Per-file rows are written when a file advances, so `committed` is the value the
commit-time path produces (and the only one the #161 acceptance exercises).
`skipped` and `blocked` are included so the coordinator can record a
non-advancing file explicitly without a schema change. A CHECK ties the
produced-artifact columns to the outcome: `committed` requires
`produced_file_version_id`, `produced_file_location_id`, and `reprobe_snapshot_id`
to be present; `skipped`/`blocked` require all four produced references NULL.
(`artifact_handle_id` is nullable even for `committed` — a remux/transcode commit
produces an artifact handle, but the column is not forced, to avoid over-pinning
ahead of the coordinator.)

A `committed` row therefore always carries its re-probe snapshot. The invariant
that satisfies this is: **a per-`(file, phase)` row is written only after both
the phase artifact commits *and* its staged result is re-probed.** A crash in the
commit→re-probe window leaves no row for that file; that window is recovered by
the idempotent backfill below (the file's committed artifact is already its active
version, so re-probe + write is a pure record-keeping step, never a re-mutation).

### Idempotent child writes (append-only, first-write-wins)

The two child grains are written by two coordinator paths that can target the
*same* natural key: the per-`(file, phase)` row is written when a file advances,
**and again** by mid-failure finalization (§6) or post-crash resume (§8) when the
first write was lost; the per-phase row is written when a phase's files are all
resolved **and again** when its report is regenerated on resume (§8). The spec
calls this an "idempotent backfill" where "fully recorded `(file, phase)` pairs
are never re-run" (§8).

Child inserts are therefore **idempotent first-write-wins**, not plain inserts:
`INSERT … ON CONFLICT(<natural key>) DO NOTHING`. A conflicting write is a no-op
and the method returns the already-stored row. This is safe because the rows are
append-only and deterministic — a regenerated per-phase report has the same
content-addressed `report_id` (same committed facts ⇒ same report), and a
re-finalized per-file row carries the same committed lineage. To let the
coordinator implement "never re-run fully recorded pairs" without relying on a
conflict, the repo also exposes targeted existence reads
(`get_phase_summary`, `get_file_phase_summary` below). The job-level parent stays
a single end-of-run insert (one writer, one row).

### Repository surface

`SqliteWorkflowSummaryRepo` follows the existing conventions (`connect`/`init`
separation — the table is created only by a migration, never by the repo; an
`_in_tx` variant per writer plus a pool-wrapping variant that `begin`/`commit`s):

- `insert_summary` / `insert_summary_in_tx(NewWorkflowSummary)` — job-level, a
  single end-of-run insert.
- `upsert_phase_summary` / `upsert_phase_summary_in_tx(NewPhaseSummary) → PhaseSummary`
  — idempotent first-write-wins on `(job_id, phase_ordinal)`.
- `upsert_file_phase_summary` / `upsert_file_phase_summary_in_tx(NewFilePhaseSummary) → FilePhaseSummary`
  — idempotent first-write-wins on `(job_id, phase_ordinal, branch_id)`.
- `get_summary(JobId) → Option<WorkflowSummary>`
- `get_phase_summary(JobId, phase_ordinal) → Option<PhaseSummary>`
- `get_file_phase_summary(JobId, phase_ordinal, branch_id) → Option<FilePhaseSummary>`
- `phases_for_job(JobId) → Vec<PhaseSummary>` ordered by `phase_ordinal`
- `file_phases_for_job(JobId) → Vec<FilePhaseSummary>` ordered by
  `(phase_ordinal, branch_id)`

A child `upsert_*` method whose `ON CONFLICT DO NOTHING` matched an existing row
re-reads that row **through the same tx handle** (a pool read would query a
different connection and miss an uncommitted sibling write) and returns it; a
fresh insert constructs its return value from the input plus
`last_insert_rowid()`. The job-level `insert_summary` is a plain insert (one row
per job). Pool reads run on the pool. The two child grains are append-only and
idempotent (above); the job-level row is written once with the run's final
counters.

## Consequences

- The coordinator (#162) gets a durable surface it can write incrementally:
  per-`(file, phase)` rows at each commit, the per-phase row when a phase's files
  are all resolved, the job row at job end — with no ordering constraint between
  children and parent.
- A half-committed barrier is recorded exactly: only advanced files have rows,
  and the `committed` CHECK guarantees each carries its produced lineage.
- A terminally-failed job that never resumes (§6) may have child rows (the files
  that advanced) but **no job-level parent row**, since the parent is a single
  end-of-run insert. The child grains are therefore authoritative for "which
  files advanced"; a consumer must query `file_phases_for_job` even when
  `get_summary` returns `None`, rather than treat a missing parent as "no
  summary." Persisting a parent row on terminal failure is the coordinator's call
  (#162), not forced by this schema.
- Resume and mid-failure finalization can re-issue the same child write; the
  idempotent first-write-wins inserts make that a no-op, so neither path throws a
  UNIQUE violation, and "fully recorded pairs are never re-run" (§8) holds without
  the coordinator needing a read-before-write race guard.
- The store stays below control-plane: it depends on no control-plane or
  `voom-plan` type; rollup and report ride as JSON.
- The schema ships before its first writer (#162); it is a spec-advertised
  surface (§4), not dead code.
- `ON DELETE RESTRICT` on produced references means a summary row pins the
  `file_versions`/`file_locations`/`media_snapshots` it cites — they cannot be
  deleted out from under an inspectable summary.

## Alternatives Considered

- **Children FK the parent `workflow_summaries` row.** Rejected: per-`(file,
  phase)` rows are written incrementally before the job-level parent exists
  (§4, §6); requiring the parent first breaks the incremental-write invariant and
  would lose the record of files that advanced in a barrier that later failed
  mid-run.
- **Import `WorkflowRunSummary`/`OperationSummary` into the repo input.**
  Rejected: violates crate layering (`voom-store` is below
  `voom-control-plane`). The store takes primitive counters plus a
  `serde_json::Value` rollup; control-plane maps its own type in.
- **Normalize `per_operation` into a fourth table keyed by
  `(job_id, operation)`.** Rejected for this issue: the rollup is an opaque
  job-level document the spec calls a "rollup", not a queried relation; a table
  is premature (`AGENTS.md` Rule 3) and would import the operation vocabulary the
  store is kept free of. Reconsider if a caller needs per-operation queries.
- **Normalize `ticket_ids` into a join table.** Rejected: the set is a small
  per-row attribute the summary reports, not an independently queried relation;
  JSON matches the existing document-column convention and keeps the grandchild a
  single append-only row per `(file, phase)`.
- **Store a `report` table with `supersedes_report_id` lineage.** Rejected by the
  spec explicitly (§4): reports remain on-demand and content-addressed; lineage is
  the ordered per-phase rows, not a stored pointer.
- **Store `elapsed` as milliseconds / a formatted string.** Rejected: ms loses
  sub-millisecond fidelity the `Duration` carries, and a string needs parsing and
  cannot be range-CHECKed; integer nanoseconds is exact and lossless for any
  realistic run.
- **Persist `throughput_per_second`.** Rejected: derivable from `dispatch_count`
  and `elapsed`, and absent from the issue's counter list — storing it duplicates
  state that can drift.
- **Plain insert-only child writes (throw on conflict).** Rejected: the spec's
  finalize (§6) and resume (§8) paths can both write the same `(job_id,
  phase_ordinal, branch_id)` row, and the spec names this an "idempotent
  backfill." A plain insert against the UNIQUE natural key would throw on the
  second write, forcing every caller into a read-before-write race guard.
  First-write-wins `ON CONFLICT DO NOTHING` makes the second write a no-op
  directly, which is sound because the rows are append-only and deterministic.
- **Update-on-conflict (last-write-wins) for child rows.** Rejected: the rows are
  append-only records of what was committed; a regenerated per-phase report is
  byte-identical (same committed facts ⇒ same content-addressed `report_id`), so
  overwriting buys nothing and would let a buggy late write mutate a recorded
  fact. First-write-wins preserves the original record.
- **A single wide `outcome` vocabulary shared by phase and file grains.**
  Rejected: `partially-committed` is meaningful only at the phase grain (a phase
  where some files advanced and some did not); a file is `committed`, `skipped`,
  or `blocked`. Separate vocabularies keep each CHECK precise.
