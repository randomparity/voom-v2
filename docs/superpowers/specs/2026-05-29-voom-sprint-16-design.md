---
name: voom-sprint-16-design
description: Sprint 16 design for coherent multi-phase real-media policy execution — phase-aware artifact chaining, re-probing at phase boundaries, bounded per-phase replanning, per-phase compliance-report regeneration, durable per-phase workflow summaries, and a full scan/evaluate/plan/run/report CLI surface.
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

Sprint 16 closes that gap. The control plane drives a phase-by-phase loop —
plan one phase against the current snapshot, run and commit its artifact,
re-probe the committed artifact, register it as the next phase's input file
version, then re-invoke the planner for the next phase against the refreshed
snapshot. The compliance report is regenerated per phase, a durable per-phase
workflow summary records the run, and the CLI exposes the whole flow through the
existing `compliance` command family.

## 2. Scope

Sprint 16 delivers:

- **Phase-aware artifact chaining in the workflow executor.** The executor walks
  `CompiledPolicy.phase_order`. When a mutation phase commits a staged artifact,
  the executor registers that artifact's `FileVersion`/`FileLocation` as the
  input the next phase plans and executes against, threading produced lineage
  forward. A file with one declared phase behaves exactly as Sprints 12-15.
- **Re-probing at phase boundaries.** After a mutation phase commits, the
  executor dispatches the existing bundled-ffprobe probe path against the
  committed artifact and persists a refreshed `MediaSnapshot` keyed to the new
  `FileVersion`. This reuses `probe_staged_result` / `verify_probe_facts` rather
  than introducing a second probe path.
- **Bounded per-phase replanning.** Before each phase after the first, the
  control plane re-invokes the planner against the refreshed snapshot, so
  `run_if`/`skip_if` and per-operation compliance re-evaluate against the
  artifact the prior phase produced. Replanning may only refine operations
  within phases already declared in `phase_order`; it can never introduce a new
  phase. The bound is therefore the declared phase count, with no intra-phase
  retry loop. A phase that cannot be planned after re-probe records an
  inspectable blocked issue and stops the workflow for that file.
- **Per-phase compliance-report regeneration.** After each phase commits and
  re-probes, the compliance report is regenerated against the refreshed facts.
  The latest report is persisted with a lineage pointer to the prior phase's
  report, preserving the deterministic report identity the current generator
  produces.
- **Durable per-phase workflow summaries.** A new durable summary persists the
  existing `WorkflowRunSummary` counters plus a per-phase rollup linking each
  phase to its tickets, produced artifacts, re-probe snapshots, and report
  version. The summary is retrievable by job through the CLI.
- **Full scan/evaluate/plan/run/report CLI surface.** The `compliance` command
  family gains `evaluate` (re-probe + replan preview against current artifacts,
  no commit) and grows `execute` into the multi-phase run and report surface,
  exposing the durable summary. Golden-output (`insta`) fixtures cover the full
  scan -> evaluate -> plan -> run -> report flow for a real multi-phase policy.
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
  for phase in compiled.phase_order:           # bounded by phase count
    snapshot_in = latest snapshot for current FileVersion
    plan_phase  = planner.plan(compiled, phase, snapshot_in)   # re-plan
    if plan_phase unplannable:
      record blocked issue; stop file
    run plan_phase tickets -> staged artifact
    commit artifact -> FileVersion(v_n+1) + FileLocation
    re-probe committed artifact -> MediaSnapshot(s_n+1) keyed to v_n+1
    regenerate compliance report (lineage -> prior report)
    advance current FileVersion to v_n+1
  persist WorkflowRunSummary (counters + per-phase rollup)
  -> report envelope + summary
```

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
structure. Each committed phase artifact is a new `FileVersion` with a
`source_lineage` recording the operation and source version. The executor's
"current version" cursor advances to the committed version, so the next phase's
planner reads the snapshot of the artifact the prior phase produced.

## 4. Data Model

Sprint 16 adds durable rows for summaries and report lineage; it reuses the
existing file-version, snapshot, artifact, ticket, and report tables.

### Re-probe snapshots

Re-probing reuses the existing `MediaSnapshot` model and `scan::persist` path.
The only change is that a snapshot may now be keyed to a `FileVersion` produced
by a mutation phase rather than only to a scanned source. No schema change.

### Compliance report lineage

The compliance report gains a nullable `supersedes_report_id` (or equivalent
lineage column) pointing at the prior phase's report for the same job. The
report's deterministic content hash and identity are unchanged; lineage is
metadata, not part of the hash.

### Durable workflow summary

A new `workflow_summaries` table (and a per-phase child table) persists:

- Job-level: `job_id`, the existing `WorkflowRunSummary` counters
  (`branch_count`, `ticket_count`, `dispatch_count`, `retry_count`,
  `failure_count`, `peak_active_workflow_leases`, `elapsed`), and the
  `per_operation` rollup.
- Per-phase: `phase_name`, ordinal, the phase's ticket IDs, produced
  `FileVersion`/`FileLocation`/artifact-handle IDs, re-probe snapshot ID,
  compliance report version, and phase outcome (`completed` | `skipped` |
  `blocked`).

A `SqliteWorkflowSummaryRepo` follows the existing repository conventions
(`connect`/`init` separation, `_in_tx` re-reads through the tx handle).

## 5. Policy And Planning

No grammar, AST, or compiled-model changes. The planner is extended so it can be
invoked for a single phase against a supplied snapshot:

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

- Run phases strictly in `phase_order`. Honor `depends_on` already encoded in
  the order.
- Reuse the existing per-operation commit and `probe_staged_result` paths; do
  not add a second probe or commit path.
- Advance the current-version cursor only after a phase's artifact is committed
  and re-probed.
- Re-invoke the planner for the next phase against the refreshed snapshot.
- Stop a file's workflow on the first unplannable phase, recording a blocked
  issue with the planning diagnostic; continue other files.
- Compute and persist the `WorkflowRunSummary` plus per-phase rollup at the end
  of the run.

The out-of-process worker boundary is unchanged; phases still execute through
durable tickets and bundled workers.

## 7. Events And Reporting

- Per-phase compliance reports are regenerated and persisted with lineage as in
  Section 4. The CLI report surface returns the latest report and can expose the
  per-phase chain.
- Events continue to record facts only; the phase loop is driven by tickets and
  the executor cursor, never by events (ADR-0001).
- The durable summary is the inspection surface tying phases to tickets,
  artifacts, snapshots, and reports.

## 8. Error Handling

- **Unplannable phase after re-probe:** record an inspectable blocked issue with
  the planner diagnostic; stop that file's workflow, leaving prior committed
  phases intact. Not a retry.
- **Re-probe mismatch:** the existing `verify_probe_facts` guard already fails
  the commit before it lands; a phase whose probe disagrees with worker-reported
  facts fails that phase and does not advance the cursor.
- **Phase with no matching tracks/streams (e.g. preferred-language selector
  matches nothing):** must fail visibly as a blocked issue, never silently
  delete or pass through the file. The acceptance scenario pins this down
  (resolves #158).
- **Worker/ticket failures within a phase:** unchanged from Sprints 12-15
  (durable retry/terminal classification).

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
- **Compliance-report tests:** per-phase regeneration with lineage; deterministic
  identity preserved.
- **Durable-summary tests:** schema + repo round-trip; per-phase rollup links to
  the correct tickets/artifacts/snapshots/reports.
- **CLI golden-output tests:** `insta` snapshots for the full scan -> evaluate ->
  plan -> run (execute) -> report flow; `compliance evaluate` preview with no
  commit; multi-phase `compliance execute` with summary.
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
  re-probe snapshots, and report version.
- `compliance evaluate` previews re-probe + replan without committing.
- `just ci` passes.

## 11. Deferred Work

- Phase re-entry, adaptive re-encode loops, and fixpoint replanning.
- Backup worker, sidecar ingest, and bundle/sidecar CLI views (Sprint 17).
- Daemon loops, watcher, scheduler, and recovery (Sprints 18-20).
- Web UI, plugin SDK, production packaging.
- Multi-output audio extraction (#99).
- Reconciliation of spec §8 CLI transcode-report framing is folded into the
  Sprint 16 `compliance execute`/report surface (#149); no separate `voom
  transcode` command is introduced.
