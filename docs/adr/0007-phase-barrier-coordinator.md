# ADR 0007 — Phase-barrier coordinator owns one job and drives the existing executor

- Status: Accepted
- Date: 2026-05-29
- Issue: #162 (Sprint 16 §3/§6)
- Related: ADR-0005 (`plan_phase`), ADR-0006 (workflow-summary schema)

## Context

Sprint 16 needs a control-plane coordinator that runs the existing workflow
executor one phase at a time across a `PolicyInputSet`, with phases as barriers
across files (spec `docs/superpowers/specs/2026-05-29-voom-sprint-16-design.md`).
Two interface decisions are left open by the spec and the dependency ADRs:

1. **Job ownership across phases.** `WorkflowExecutor::submit_and_run`
   (`crates/voom-control-plane/src/workflow/executor.rs:244`) opens a *new* job
   (`open_job`, kind `synthetic.workflow`) on every call. The durable summary
   (ADR-0006, spec §4) is keyed by a *single* `job_id` for the whole run, and
   reads are `phases_for_job(job_id)` / `file_phases_for_job(job_id)`. Running N
   phases via N `submit_and_run` calls would mint N jobs and fragment the
   summary.

2. **Per-phase plan construction.** Today `workflow_plan_from_compliance(&ExecutionPlan, &ComplianceReport)`
   (`workflow/policy_bridge.rs`) bridges a whole-policy plan into a flat DAG of
   `policy-node_{id}` workflow nodes. The coordinator must instead feed the
   executor one phase at a time, re-planned against refreshed snapshots.

## Decision

**The coordinator owns exactly one job for the whole multi-phase run and reuses
the existing executor, one phase per `submit_and_run`-equivalent call.**

- Extract the post-`open_job` body of `submit_and_run` into a private
  `run_plan_in_job(&self, job_id: JobId, plan: WorkflowPlan) -> Result<WorkflowRunSummary, WorkflowRunError>`.
  `submit_and_run` becomes: `open_job` → `run_plan_in_job`. Existing single-call
  behaviour and tests are unchanged.
- Add a crate-visible entry point the coordinator calls per phase that takes the
  coordinator-owned `job_id` and the phase's `WorkflowPlan`, delegating to
  `run_plan_in_job`. The coordinator opens the job once (kind `synthetic.workflow`)
  and threads it through every phase.
- **One `plan_phase` call per phase, not per file.** `plan_phase(request, phase_name)`
  already plans a phase across *all* `request.input.media_snapshots`. The
  coordinator projects every still-active file's current active-version snapshot
  into one `PolicyInputSetDraft`, calls `plan_phase` once, and bridges the
  resulting per-phase `ExecutionPlan` (only the named phase's nodes, no
  inter-phase edges per ADR-0005) into one `WorkflowPlan` via the existing
  bridge. Per-file granularity lives in the *outcome* handling, not the planning
  call.
- **Active version = chain tip.** After a phase's branch commits inline (commit
  + `probe_staged_result` + `record_result_snapshot_payload` already run in the
  dispatch path), the coordinator reads each file's new active version
  (`file_versions` WHERE `file_asset_id=? AND retired_at IS NULL ORDER BY id DESC LIMIT 1`)
  and that version's refreshed `MediaSnapshot`, projects it into the next phase's
  request, and writes the per-`(file, phase)` summary row.

## Consequences

- Single `job_id` keys all three summary grains; `phases_for_job` /
  `file_phases_for_job` return the whole run. No sub-job fan-out.
- The executor gains no new execution path: phases reuse `run_plan_in_job`, the
  same ticket DAG, dispatch, inline commit, and probe.
- A whole-job failure mid-barrier leaves committed files advanced (append-only,
  not transactional). Per-`(file, phase)` rows are written as each branch
  commits, so the durable summary records which files advanced even on terminal
  failure (spec §6, §8). Resume is per-`(file, phase)`.
- Unplannable targets surface as `Blocked` plan nodes + diagnostics from the
  single `plan_phase` call; the coordinator drops those files (abort-for-file)
  while planned siblings proceed.

## Considered & rejected

- **One job per phase, summary keyed to a coordinator job.** Rejected: ADR-0006
  keys every grain to `jobs(id)` with `ON DELETE CASCADE` and the reads take one
  `job_id`; a separate coordinator-job id would need a second job table concept
  and break `phases_for_job`. More moving parts for no gain.
- **`plan_phase` per file, merge plans.** Rejected: `plan_phase` is already
  whole-input-set per phase; calling it per file would re-derive `plan_id`/
  `plan_hash` per file and force a plan-merge step the bridge doesn't need.
- **A second commit/probe path in the coordinator.** Rejected by spec §2/§6 —
  reuse `probe_staged_result` / `record_result_snapshot_payload`; add no second
  probe.
- **A new phase-cursor table.** Rejected: a phase is complete when its tickets
  are all `succeeded` (ADR-0006, spec §4); the per-`(file, phase)` rows are the
  durable rollup. No new cursor.
