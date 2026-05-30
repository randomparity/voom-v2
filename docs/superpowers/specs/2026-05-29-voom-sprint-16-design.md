---
name: voom-sprint-16-design
description: Sprint 16 design for coherent multi-phase real-media policy execution — a phase-barrier coordinator over a multi-file input set, append-only active-version chaining, staged-result probing at phase boundaries, bounded per-phase replanning, per-phase compliance reports folded into a durable per-phase workflow summary, and a scan/plan/execute/report CLI surface.
status: draft
date: 2026-05-29
sprint: 16
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-25-voom-sprint-12-design.md
  - docs/superpowers/specs/2026-05-25-voom-sprint-13-design.md
  - docs/superpowers/specs/2026-05-26-voom-sprint-14-design.md
  - docs/superpowers/specs/2026-05-28-voom-sprint-15-design.md
---

# VOOM Sprint 16 - Real-Media Policy Workflow Completion

## 1. Goal

Sprint 16 makes multi-phase real-media policy execution coherent from CLI scan
through report. Sprints 12-15 delivered each real mutation operation in
isolation — video transcode, container remux and track selection, audio
transcode and extract, named video profiles — and each runs end-to-end through
durable tickets, out-of-process workers, staged-artifact verification, and
host-owned commit. What does not yet work is running several mutation phases
against the *same* file so that the artifact produced by one phase is the input
to the next.

The static pipeline is already phase-aware. The policy DSL declares named phases
(`PolicyAst.phases`), the compiler preserves them with dependencies and
conditions (`CompiledPolicy.phases`, `CompiledPolicy.phase_order`,
`CompiledPhase { name, depends_on, run_if, skip_if, on_error, operations }`),
the planner walks `phase_order` and tags every `ExecutionPlan` node with its
`phase_name`, and the compliance report carries `phase_name` per check. The gap
is purely at runtime: the planner today expands **all** phases up front against
the **original** observed media state, and the workflow executor runs the
resulting tickets without ever feeding a phase's produced artifact back into the
next phase's planning.

Sprint 16 closes that gap. A control-plane coordinator drives the existing
executor one phase at a time across the whole input set — phases are barriers
across files. For each file it plans one phase against that file's current
snapshot, runs and commits the phase's artifact, probes the staged result
(byte-identical to the committed target) and records the snapshot against the
committed `FileVersion`, advances the file's active version, then re-invokes the
planner for the next phase against the refreshed snapshot. The compliance report
is regenerated per phase and folded into a durable per-phase workflow summary
that is persisted incrementally as each file's phase artifact commits, and the CLI
exposes the whole flow through the existing `compliance` command family.

## 2. Scope

Sprint 16 delivers:

- **A multi-file phase-barrier coordinator.** A control-plane coordinator runs
  the existing executor one phase at a time across the whole multi-file input set
  (`PolicyInputSet.media_snapshots`); each phase is a barrier across files. Each
  `media_snapshots` entry is one file, rooted at the `FileVersion` that snapshot
  keys (`voom-store/src/repo/policy_inputs.rs:35`); the coordinator's starting
  active version for a file is that entry's version, and there is **at most one
  active snapshot per file at job start** (the coordinator selects the chain tip
  per file). When a
  mutation phase commits a staged artifact for a file, that artifact's
  `FileVersion`/`FileLocation` becomes the file's active version — the input the
  next phase plans and executes against — threading produced lineage forward. A
  single file with one declared phase behaves exactly as Sprints 12-15.
- **Probing the staged result at phase boundaries.** After a mutation phase
  commits, the coordinator probes the staged result (byte-identical to the
  committed target) and records a refreshed `MediaSnapshot` against the committed
  `FileVersion`. This reuses `probe_staged_result` / the post-commit snapshot
  record (`transcode/commit.rs:149-177`); no second post-commit probe path is
  added.
- **Bounded per-phase replanning.** Before each phase after the first, the
  control plane re-invokes the planner against the refreshed snapshot, so
  `run_if`/`skip_if` and per-operation compliance re-evaluate against the
  artifact the prior phase produced. Replanning may only refine operations
  within phases already declared in `phase_order`; it can never introduce a new
  phase. The bound is therefore the declared phase count, with no intra-phase
  retry loop. A phase that cannot be planned after re-probe records an
  inspectable blocked issue and stops the workflow for that file.
- **Per-phase compliance reports folded into the summary.** After each phase
  commits and re-probes, the compliance report is regenerated against the
  refreshed facts. Reports remain on-demand/content-addressed; each phase's
  `report_id` and report JSON (or its hash) is recorded in that phase's
  workflow-summary child row keyed by `(job_id, phase_ordinal)`. Lineage is the
  ordered per-phase rows, not a stored pointer. The deterministic report identity
  the current generator produces is unchanged.
- **Durable per-phase workflow summaries.** A new durable summary persists the
  existing `WorkflowRunSummary` counters plus a per-phase rollup linking each
  phase to its tickets, produced artifacts, re-probe snapshots, and compliance
  report. Phase progress itself is already durable in existing rows (a phase is
  complete when its tickets are all `succeeded`); each per-`(file, phase)` summary
  row is persisted as that file's phase artifact commits and is retrievable by job
  through the CLI.
- **Scan/plan/execute/report CLI surface.** The CLI surface is scan -> plan ->
  execute (run) -> report. `plan` dry-run plus `compliance report` serve as the
  pre-run preview; `compliance execute` grows into the multi-phase run and report
  surface that exposes the durable summary and closes #149. Golden-output
  (`insta`) fixtures cover the full scan -> plan -> execute -> report flow for a
  real multi-phase policy.
- **Sprint 16 closeout evidence** tying policy phases to tickets, artifacts,
  re-probe snapshots, reports, and CLI outputs in a closeout matrix.

Sprint 16 explicitly does not deliver:

- Backup worker, sidecar asset ingest, or bundle/sidecar CLI views (Sprint 17).
- Filesystem watcher, background scheduler loop, or any daemon loop (Sprints
  18-20).
- Web UI, plugin SDK, or production packaging.
- New mutation operations or DSL grammar. Phases, dependencies, and conditions
  already exist in the language; Sprint 16 changes only how the runtime consumes
  them.
- Multi-output audio extraction (tracked separately as #99).
- Per-file failure isolation or independent per-file phase cursors. A file that
  fails or blocks fails its workflow exactly as today; sibling files already in
  the same job are not independently isolated.
- Phase re-entry: a phase is planned and run at most once per workflow. Adaptive
  re-encode loops and fixpoint replanning are out of scope.
- User-defined profile, policy, or input-set CRUD (Sprint 17).

## 3. Architecture

Sprint 16 turns the existing single-phase real path into a driven multi-phase
loop. The static layers are unchanged; the control-plane executor gains the
loop.

```text
voom scan --path <file>
  -> FileVersion(v0) + FileLocation + MediaSnapshot(s0)

voom compliance execute --policy-version-id <id> --input-set-id <id>
  active = set(input_set.media_snapshots)             # one entry per file; still progressing
  for phase in compiled.phase_order:                  # barrier across active files
    for file in active:
      snapshot_in = the active version's snapshot in file's produced-from chain
      plan_phase  = planner.plan_phase(compiled, phase, snapshot_in)   # re-plan
      if plan_phase unplannable:
        record blocked issue; active.remove(file)     # stop THAT file
    run remaining active files' phase tickets via submit_and_run   # existing DAG;
                                                                   # commit is inline per branch on ticket success
    for file whose phase ticket committed an artifact:
      probe staged result; advance file's active version   # over the already-committed v_k
      persist that (file, phase) summary row                # at commit, not batched at the barrier
  -> report envelope + summary
```

### A branch is one file; the coordinator drives the existing executor

A **branch is one file** (`workflow/binding.rs`): the executor already fans out
per-file branches with `branch_id` = path stem. The coordinator reuses the
existing `submit_and_run` plus the ticket-dependency DAG to run a phase across
the still-active files rather than introducing a new execution path. **Per-file
failure isolation is out of scope** — a file that fails or blocks fails its
workflow exactly as today, and sibling files already running in the same job are
not independently isolated.

The phase barrier is a **scheduling/planning boundary, not a transactional
one**. Commit is per-file and inline — each file's artifact is committed
host-side as soon as that branch's ticket succeeds — and a whole-job failure
does not roll back already-committed `FileVersion`s. A phase can therefore be
committed for some files and failed for others (file A advances to v_k, file B
fails, the whole job fails, A stays at v_k); per-file state is reconciled on
resume (§8).

### Phase boundary as the unit of replanning

A *phase boundary* is the point between two declared phases in `phase_order`.
It is the only place the plan is regenerated and the only place an artifact is
chained. Within a phase, execution is exactly the Sprint 12-15 ticket flow:
durable tickets, out-of-process worker, staged artifact, probe-before-commit,
host-owned commit. The executor never re-plans inside a phase and never plans a
phase that is not in `phase_order`.

### Plan-per-phase, not patch-the-plan

The planner is re-invoked per phase against the refreshed snapshot rather than
producing one whole-policy plan that is later patched. This keeps a single
planning code path (the existing `Planner`), keeps each phase's plan
deterministic from `(compiled policy, phase, snapshot)`, and makes
`run_if`/`skip_if` re-evaluation fall out naturally — a phase whose condition no
longer holds against the produced artifact is skipped, and a phase whose
operations are now satisfied produces a compliant (no-op) plan.

### Artifact lineage is the chain

Chaining is expressed through existing durable rows, not a new in-memory
structure. The **active version of a file is the latest non-retired
`file_versions` row in its produced-from chain** (`produced_from_version_id`,
`migrations/0003_identity.sql:36-54`). Each committed phase artifact becomes a
new `FileVersion` pointing at its source via `produced_from_version_id`; commit
is append-only and never retires the source (`artifact/commit.rs:730-795`). The
next phase's planner reads the snapshot of the active version, i.e. the artifact
the prior phase produced. Because nothing is retired this sprint, the active
version is simply the chain tip; the `retired_at` filter is forward-compat for
the deferred rollback work (§11) and is otherwise unexercised.

## 4. Data Model

Sprint 16 adds durable rows for per-phase workflow summaries (which carry each
phase's compliance report); it reuses the existing file-version, snapshot,
artifact, and ticket tables. Compliance reports are not stored in a table of
their own.

### Re-probe snapshots

Re-probing reuses the existing `MediaSnapshot` model and `scan::persist` path.
The only change is that a snapshot may now be keyed to a `FileVersion` produced
by a mutation phase rather than only to a scanned source. No schema change.

### Compliance report storage

Compliance reports remain on-demand and content-addressed: the generator derives
`report_id` from the report preimage (`voom-plan/src/compliance_report.rs:30-52`)
and that identity is unchanged. There is **no reports table and no
`supersedes_report_id` column**. Instead, each phase's `report_id` and the report
JSON (or its hash) are recorded in the per-phase workflow-summary child row keyed
by `(job_id, phase_ordinal)`. Report lineage is the ordered per-phase rows, not a
stored pointer.

### Durable workflow summary

A new `workflow_summaries` table (a job row, a per-phase child, and a
per-`(file, phase)` grandchild) persists:

- Job-level: `job_id`, the existing `WorkflowRunSummary` counters
  (`branch_count`, `ticket_count`, `dispatch_count`, `retry_count`,
  `failure_count`, `peak_active_workflow_leases`, `elapsed`), and the
  `per_operation` rollup.
- Per-phase, keyed `(job_id, phase_ordinal)`: `phase_name`, the phase's
  `report_id` and report JSON (or its hash) — a policy-level artifact covering
  the input set's refreshed facts at that phase — and the phase outcome
  (`completed` | `partially-committed` | `skipped` | `blocked`).
- Per-`(file, phase)`, keyed `(job_id, phase_ordinal, branch_id)`: that file's
  ticket IDs, produced `FileVersion`/`FileLocation`/artifact-handle IDs, re-probe
  snapshot ID, and per-file outcome. These rows are append-only and written as
  each file's phase artifact commits, so a half-committed barrier — some files
  advanced, others failed — is recorded exactly.

Phase progress itself needs no new cursor table: a phase is complete when its
tickets are all `succeeded` (`repo/tickets.rs`, `ticket_dependencies` with
kind=`phase`). Each per-`(file, phase)` summary row is a durable rollup over those
existing rows, persisted as that file's phase artifact commits and re-probes — not
batched at the barrier — so a crash or job failure mid-barrier leaves a record of
every file that advanced, not only of whole completed phases.

A `SqliteWorkflowSummaryRepo` follows the existing repository conventions
(`connect`/`init` separation, `_in_tx` re-reads through the tx handle).

## 5. Policy And Planning

No grammar, AST, or compiled-model changes. The planner is extended so it can be
invoked for a single phase against a supplied snapshot (the `plan_phase` entry
point and its failure contract are pinned by `docs/adr/0005-plan-phase-entry-point.md`):

- The planner already iterates `phase_order` and expands per phase. Sprint 16
  factors out a per-phase entry point that plans exactly one named phase against
  a caller-supplied planning input projected from the current snapshot.
- `run_if`/`skip_if` are evaluated against the refreshed snapshot at each
  boundary. A skipped phase produces no tickets and is recorded as `skipped` in
  the summary.
- An operation that cannot be planned against the refreshed artifact (for
  example a track selector that now matches nothing) yields a planning
  diagnostic that the executor turns into a blocked issue.

## 6. Control-Plane Execution

The workflow executor (`crates/voom-control-plane/src/workflow/`) gains the
phase loop described in Architecture. Key obligations:

- Run phases strictly in `phase_order`, as barriers across the input set. Honor
  `depends_on` already encoded in the order.
- Reuse the existing per-operation commit and probe the staged result
  (byte-identical to the committed target) against the committed `FileVersion`;
  do not add a second probe or commit path.
- Advance a file's active version only after that phase's artifact is committed
  and its staged result probed.
- Re-invoke the planner for the next phase against the refreshed snapshot.
- On the first **unplannable** phase (a planning diagnostic raised before any
  ticket is submitted), record a blocked issue and stop that file's remaining
  phases. This **abort-for-file** is coordinator-level and applies only to the
  unplannable case. An in-phase **ticket failure** during execution fails the
  whole job (all files in the barrier), exactly as Sprints 12-15 — per-file
  isolation of ticket failures is out of scope (§2). Non-default
  `CompiledPhase.on_error` strategies (e.g. continue-on-error) are **not honored
  this sprint** — they are either rejected at resolve time with a diagnostic or
  documented as no-ops; full `on_error` handling is deferred (§11).
- Persist each `(file, phase)` workflow-summary row as that file's phase artifact
  commits and re-probes (incrementally), not batched at the barrier, so a crash or
  job failure mid-barrier still leaves a durable record of every file that
  advanced — not only of whole completed phases — rather than computing the
  `WorkflowRunSummary` only at the end.
- When a job fails mid-barrier (terminal or retryable), finalize any file that
  committed its phase artifact inline before the failure: re-probe the committed
  artifact and backfill its `(file, phase)` summary row — the same idempotent
  backfill resume uses (§8). The durable summary therefore always records which
  files advanced within a partially-failed barrier, even when the job is
  terminally failed and never resumes.
- Because commit is per-file and inline and a whole-job failure does not roll
  back committed `FileVersion`s, a job-level failure may leave a partially-
  advanced input set (some files past phase k, others not). Resume reconciles
  this per file: the coordinator never re-runs a phase for a file already
  advanced past it (§8).

The out-of-process worker boundary is unchanged; phases still execute through
durable tickets and bundled workers.

## 7. Events And Reporting

- Per-phase compliance reports are regenerated and recorded in the per-phase
  workflow-summary rows as in Section 4. The CLI report surface returns the
  latest phase's report and can expose the per-phase chain by reading the ordered
  summary rows.
- Events continue to record facts only; the phase loop is driven by tickets and
  the coordinator's active-version advance, never by events (ADR-0001).
- The durable summary is the inspection surface tying phases to tickets,
  artifacts, snapshots, and reports.

## 8. Error Handling

- **Unplannable phase after re-probe:** record an inspectable blocked issue with
  the planner diagnostic and stop that file's workflow (abort-for-file). The last
  successfully committed phase's artifact stays the file's active version. **No
  rollback or quarantine is performed this sprint** (deferred, §11); a
  partially-applied policy therefore leaves a coherent, inspectable state — last
  good phase active plus a blocked issue — never a deleted or orphaned file. Not a
  retry.
- **Re-probe mismatch:** the existing `verify_probe_facts` guard already fails
  the commit before it lands; a mismatch fails the commit and therefore the
  **job** (all files in the barrier), as with any in-phase failure. The file's
  active version is not advanced.
- **Phase with no matching tracks/streams (e.g. preferred-language selector
  matches nothing):** must fail visibly as a blocked issue, never silently
  delete or pass through the file. The acceptance scenario pins this down
  (resolves #158).
- **Worker/ticket failures within a phase:** unchanged from Sprints 12-15 — an
  in-phase ticket failure fails the whole job (all files in the barrier), not just
  the file (durable retry/terminal classification). Per-file isolation of ticket
  failures is deferred (§11). Files that committed their phase artifact inline
  before the failure are finalized into the durable summary (§6), so even a
  terminally failed mid-barrier job — which never resumes — remains inspectable
  per file: the summary shows exactly which files advanced to the failed phase.
- **Resume after crash:** resume is per-`(file, phase)`, not phase-granular —
  the barrier is not transactional, so a phase may be committed for some files
  and not others (§3). On restart, for each file the coordinator re-enters the
  first phase whose artifact is **not yet committed for that file**. A file
  already advanced past phase k skips phase k — its committed artifact is the
  active version the next phase plans against. A file whose phase-k tickets
  succeeded but whose per-`(file, phase)` row is missing (a crash between commit
  and the summary write) is finalized by re-probing the already-committed artifact
  and writing that row — the same backfill §6 uses on job failure — no
  re-mutation; the per-phase report is regenerated once the phase's active files
  are all re-probed. Idempotency rests on the append-only committed artifacts
  being the active version the next phase plans against; fully recorded
  `(file, phase)` pairs are never re-run.

## 9. Testing

- **End-to-end workflow integration test:** a policy with phases combining video
  transcode, container remux + track selection, and audio mutation, plus
  verification and commit, executed against fixture media and inspected through
  the report and summary.
- **Artifact-chain tests:** assert phase N+1 plans and executes against the
  `FileVersion` phase N produced, with correct `source_lineage`.
- **Re-probe tests:** assert a refreshed snapshot keyed to the produced version
  is persisted and fed to the next phase.
- **Bounded-replan tests:** assert exactly one plan pass per declared phase, no
  phase added beyond `phase_order`, `run_if`/`skip_if` re-evaluation against the
  produced artifact, and a blocked issue on an unplannable phase.
- **Partial-barrier-failure + resume tests:** branch A commits at phase k, branch
  B fails (the whole job fails), then resume — assert A is **not** re-mutated
  (phase k is skipped for A because its committed artifact is already the active
  version) and B re-enters phase k.
- **on_error tests:** a phase declaring a non-default `on_error` is handled per
  the stated rule (rejected at resolve time or treated as a no-op), so the
  limitation cannot silently regress.
- **Compliance-report tests:** per-phase regeneration recorded in the per-phase
  summary rows; deterministic identity preserved.
- **Durable-summary tests:** schema + repo round-trip; the per-phase row links to
  the correct report and the per-`(file, phase)` rows link to the correct
  tickets/artifacts/snapshots; a half-committed barrier yields per-file rows only
  for the files that advanced.
- **CLI golden-output tests:** `insta` snapshots for the full scan -> plan ->
  execute -> report flow; `plan` dry-run + `compliance report` as the pre-run
  preview; multi-phase `compliance execute` with summary.
- **Documentation completeness scan** and `just ci`.

Per the project test-layout rule, full multi-phase runs that launch the bundled
ffprobe on staged output are only exercised by `cargo test --workspace`; the
fixture media must be written by the test harness.

## 10. Acceptance Criteria

- A multi-phase policy combining video transcode, remux/track selection, audio
  mutation, verification, and commit executes through `compliance execute` and
  is inspectable through CLI JSON envelopes.
- Each phase plans and executes against the artifact the prior phase produced and
  re-probed.
- Replanning is bounded by the declared phase count; no phase is added at
  runtime; an unplannable phase becomes an inspectable blocked issue.
- The compliance report reflects produced artifacts per phase with lineage.
- A durable workflow summary ties every phase to its tickets, artifacts,
  re-probe snapshots, and compliance report.
- A partially-applied policy leaves a coherent, inspectable state, never a deleted
  or orphaned file. For an unplannable phase the file's last good phase stays
  active with a blocked issue; for a job failure mid-barrier, every file that
  committed before the failure is recorded in the durable summary (§6
  finalization), so which files advanced is always inspectable.
- `just ci` passes.

## 11. Deferred Work

- Phase re-entry, adaptive re-encode loops, and fixpoint replanning.
- Rollback / active-version reset after a partially-applied policy.
- Per-file failure isolation and independent per-file phase cursors.
- Non-default `CompiledPhase.on_error` strategies (continue-on-error, etc.).
- Backup worker, sidecar ingest, and bundle/sidecar CLI views (Sprint 17).
- Daemon loops, watcher, scheduler, and recovery (Sprints 18-20).
- Web UI, plugin SDK, production packaging.
- Multi-output audio extraction (#99).
- Reconciliation of spec §8 CLI transcode-report framing is folded into the
  Sprint 16 `compliance execute`/report surface (#149); no separate `voom
  transcode` command is introduced.
