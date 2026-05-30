# Plan — Issue #162: multi-file phase-barrier coordinator

Spec: `docs/superpowers/specs/2026-05-29-voom-sprint-16-design.md` (§3, §6, §8).
Decisions: ADR-0007 (job ownership + plan-per-phase), ADR-0005 (`plan_phase`),
ADR-0006 (`workflow_summaries`). Deps #160, #161 merged.

## Goal

A `compliance execute` run drives the existing executor one phase at a time over
a `PolicyInputSet`, re-planning each phase against the artifact the prior phase
produced, persisting a durable per-phase / per-`(file, phase)` workflow summary.
A single file with one phase behaves exactly as Sprints 12–15.

## Invariants to preserve

- No new execution path: phases reuse `submit_and_run`'s body via
  `run_plan_in_job`; commit + `probe_staged_result` + `record_result_snapshot_payload`
  are reused, no second probe path.
- `connect()`/`init()` separation: coordinator is read+execute, never migrates.
- Tickets route work; events record facts (ADR-0001).
- Workspace lints: `#[expect(..., reason=...)]` not `#[allow]`; `_in_tx` re-reads
  via `&mut **tx`; no `unwrap/expect/panic/todo`.
- Sibling unit tests (`<src>_test.rs` + `#[path]`); `just check-test-layout` green.

## Phases

### Phase 1 — Executor: extract per-phase runner (refactor, no behaviour change)
- In `workflow/executor.rs`, split `submit_and_run` (244–404):
  - `submit_and_run`: `validate` → `open_job` → `run_plan_in_job(job.id, plan, started)`
    → on `Ok`, `succeed_job` then return; on `Err`, return as today. (Net
    behaviour identical to current `submit_and_run`.)
  - `run_plan_in_job(&self, job_id, plan, started) -> Result<WorkflowRunSummary, WorkflowRunError>`
    holds `create_root_tickets` + the main loop, using `job_id` throughout.
    **CRITICAL:** it must **NOT** call `succeed_job` — on phase success it returns
    `Ok(summary)` and leaves the job open. `fail_job` on an in-phase ticket
    failure stays inside the runner (whole job fails, spec §8). Job success is the
    caller's responsibility.
  - Add crate-visible `submit_and_run_in_job(&self, job_id, plan)` = thin wrapper
    over `run_plan_in_job` (validates the plan first; on validate error fails the
    job since it already exists). The coordinator calls this per phase and calls
    `succeed_job` itself after the last phase.
- **Test first:** existing executor tests stay green (single-call behaviour
  identical — success still ends in `succeed_job`); add a test asserting two
  sequential `submit_and_run_in_job` calls with the same `job_id` accumulate
  tickets and the job stays `open` until the caller succeeds it.
- Guardrail: `cargo test -p voom-control-plane`, `just lint`. Commit.

### Phase 2 — Snapshot projection helper
- Add a helper that reads a file's active version (chain tip) and its latest
  `MediaSnapshot`, and builds a `MediaSnapshotInput` (voom-policy). Map
  `PolicyMediaSnapshotInput`/`MediaSnapshot.payload` → `MediaSnapshotInput`
  fields (container, video_codec, width, height, hdr, bitrate, duration_millis,
  audio/subtitle languages, health_flags, target = `FileVersion{id}`).
- **Test first:** project a known committed FileVersion+snapshot → assert the
  `MediaSnapshotInput` round-trips the facts; assert chain-tip selection picks
  the latest non-retired version.
- Commit.

### Phase 3 — Coordinator core (new module `workflow/coordinator.rs`)
- Entry: `run_phase_barrier(&self, policy_version_id, input_set_id, options) ->
  Result<CoordinatorOutcome, _>`. Opens one job. `active = all files in input set`.
- For `phase_name` in `compiled.phase_order`:
  1. Project each active file's current snapshot → one `PolicyInputSetDraft`;
     build `PlanningRequest`; `plan_phase(request, phase_name)`.
  2. Partition plan nodes by target: `Blocked` → drop file (record blocked
     issue + `PhaseOutcome`/`FilePhaseOutcome::Blocked` row); zero nodes →
     phase `Skipped`; `Planned` → proceed.
  3. Bridge planned nodes → `WorkflowPlan`; **override** the bridge's hardcoded
     `fan_out.max_files: 1` / `concurrency.max_in_flight_dispatches: 1`
     (`policy_bridge.rs:95-97`) with values driven by the active-file count so the
     phase runs across files concurrently; `submit_and_run_in_job(job_id, plan)`.
     (Single active file → 1/1, preserving Sprint 12–15 parity.)
  4. On ticket failure → whole job fails; **finalize** any branch that committed
     inline before the failure (re-probe already done in dispatch; backfill its
     per-`(file, phase)` row), then return error with partial summary.
  5. For each committed branch: read new active version + reprobe snapshot,
     advance active version, `upsert_file_phase_summary` (`Committed`).
  6. Regenerate the per-phase compliance report against refreshed facts;
     `upsert_phase_summary` with `report_id` + report JSON + `PhaseOutcome`.
- After all phases: `succeed_job(job_id)`, then `insert_summary` (job grain
  counters + `per_operation`). On any phase's ticket failure the runner already
  failed the job; the coordinator finalizes committed branches and returns the
  partial summary without succeeding.
- **Tests first** (unit, handler-level, injected providers): single-file/single-
  phase parity; artifact-chain (phase N+1 plans against phase N's FileVersion);
  re-probe snapshot fed forward; bounded replan (one pass/phase, no phase added);
  unplannable→blocked; skipped phase; partial-barrier-failure finalization.
- Commit per logical step.

### Phase 4 — Wire into `compliance execute`
- `execute_compliance_policy_with_options` calls the coordinator instead of the
  single-shot `execute_compliance_workflow`. Extend `ComplianceExecuteData` with
  the durable summary (phases + per-file rows) or expose via report surface.
- Keep `ComplianceExecuteError { source, partial }` semantics.
- **Tests:** case-level execute returns multi-phase summary; error path returns
  partial with advanced-file rows.

### Phase 5 — CLI golden output
- Extend `crates/voom-cli/tests/compliance_envelope.rs` with an insta snapshot
  for a real multi-phase scan→plan→execute→report flow (fixture media written by
  harness; redact job_id/paths). `cargo insta review`.

### Phase 6 — Closeout
- Sprint 16 closeout evidence matrix (phases→tickets→artifacts→snapshots→
  reports→CLI). `just ci` green.

## Verification gates
Each commit: `just fmt-check && just lint && cargo test -p voom-control-plane`.
Coordinator/probe-path changes additionally need full `cargo test --workspace`
(real ffprobe on staged output). Final: `just ci`.

## Risks / open checks for /challenge
- Confirm the dispatch path already records the reprobe snapshot against the
  *committed* FileVersion (so coordinator only reads, never re-probes) — verify
  in `transcode/audio/remux commit.rs` before Phase 3.
- Confirm `plan_phase`'s `Blocked` node carries enough target identity to map
  back to a `branch_id` for the per-file row.
- Confirm the per-phase compliance report generator accepts refreshed-snapshot
  input set without a stored input-set row (on-demand regen).
- `branch_id` uniqueness within `(job, phase)` when two input files share a path
  stem (binding.rs uses stem) — guard or document.
