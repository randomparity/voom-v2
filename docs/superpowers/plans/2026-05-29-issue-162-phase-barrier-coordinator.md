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

## Cross-phase accounting & invariants (load-bearing)

These follow from reusing one `job_id` across phases (ADR-0007) and the executor's
job-scoped queries. They are not optional — code that ignores them produces a wrong
durable summary or hangs.

- **Counts are job-cumulative, not per-phase.** `refresh_counts`
  (`executor.rs:1080-1117`), `workflow_finished`, and `first_failed_ticket_error`
  all query `tickets WHERE job_id = ?`. The `WorkflowRunSummary` returned by
  `run_plan_in_job` after phase *k* therefore reflects phases *1..k* cumulatively.
  - The end-of-run job-grain `insert_summary` consumes this cumulative summary
    directly (cumulative == whole job — correct).
  - **Any phase-grain or file-phase count must be a per-phase delta**, never the
    returned summary's totals. Per-`(file, phase)` rows carry their own explicit
    `ticket_ids` (the only tickets that phase/branch owns). Per-phase `per_operation`
    is computed by snapshotting cumulative counts immediately before the phase and
    subtracting, or by aggregating that phase's `ticket_ids`. A test with ≥2 phases
    must assert phase-2's row is not the sum of phases 1+2.
- **Every phase-*k* ticket is terminal before phase *k+1* starts.** `workflow_id`
  is `workflow-{job_id}` (`executor.rs:274`), identical across phases, and
  `workflow_finished` counts non-terminal tickets job-wide. If any phase leaves a
  ticket in `pending/ready/leased`, the next phase's loop never finishes (hang) or
  spins on `retry_delay`. The coordinator asserts all phase-*k* tickets are terminal
  after `run_plan_in_job` returns (success path) before projecting phase *k+1*; a
  skipped/NoOp phase that mints no ticket is fine, but a phase that mints a ticket
  must drive it to terminal. Covered by the skipped-phase test.
- **`branch_id` is unique per `(job, phase)` — enforced, not assumed.**
  `upsert_file_phase_summary` is `ON CONFLICT (job_id, phase_ordinal, branch_id)
  DO NOTHING` (`workflow_summaries.rs:399`); its own comment (389-392) states it
  *relies on* `branch_id` uniqueness. Two input files sharing a path stem collide
  and the second file's row is silently dropped. The coordinator detects duplicate
  `branch_id`s across the active file set **at job start** and fails fast with a
  specific error — it never relies on `DO NOTHING` to paper over a collision.

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
  - **One intentional behaviour change on the failure path:** today the terminal
    branch (`executor.rs:311-320`) calls `fail_job` and returns `Err` immediately,
    dropping the `active` `JoinSet` (line 291) and aborting in-flight sibling
    dispatches. The runner must instead **drain** `active` (await `join_next` to
    completion) before `fail_job`, so every dispatch that was going to commit
    inline has landed (or definitively not) before the runner returns. This makes
    the coordinator's chain-tip diff (Phase 3 step 4) race-free: no dispatch is in
    flight when tips are inspected. Verify no existing test asserts abort-on-
    failure timing; the final job state (`failed`) is unchanged.
  - Add crate-visible `submit_and_run_in_job(&self, job_id, plan)` = thin wrapper
    over `run_plan_in_job` (validates the plan first; on validate error fails the
    job since it already exists). The coordinator calls this per phase and calls
    `succeed_job` itself after the last phase.
- **Test first:** existing executor tests stay green (single-call behaviour
  identical — success still ends in `succeed_job`); add a test asserting two
  sequential `submit_and_run_in_job` calls with the same `job_id` accumulate
  tickets, the job stays `open` until the caller succeeds it, and the second
  call's returned summary `ticket_count` is the **cumulative** total (counts are
  job-scoped — this documents the delta requirement for the phase-grain rows in
  Phase 3). Assert all tickets from the first call are terminal before the second
  call returns (the cross-phase terminal invariant). Add a failure-path test: a
  plan with one failing ticket and ≥2 concurrent sibling dispatches
  (`max_in_flight_dispatches > 1`) asserts the runner returns `Err` only after
  every sibling dispatch has reached a terminal state (the drain contract).
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
- Load `policy_version_id` → `CompiledPolicy` (source of `compiled.phase_order`).
- **Before opening the job:** derive each active file's `branch_id` and fail fast
  with a specific error if any two collide (the per-`(file, phase)` upsert is
  `DO NOTHING` and would silently drop the loser — see cross-phase invariants).
- **Job cleanup contract:** once the job is open, *every* error path — not just an
  in-phase ticket failure — must call `fail_job(job_id, …)` before returning. The
  runner already fails the job on ticket failure; coordinator-side errors after
  `open_job` (snapshot projection, a `plan_phase` hard `Err` per ADR-0005, the
  bridge call, report regeneration, a summary upsert) would otherwise orphan the
  job in `open`. Wrap the post-`open_job` body so any `Err` finalizes the job as
  `failed`. (Empty `phase_order` or empty active set → no phases run → `succeed_job`
  + a zero-phase `insert_summary`; covered by an empty-state test.)
- For `phase_name` in `compiled.phase_order`:
  1. Project each active file's current snapshot → one `PolicyInputSetDraft`;
     build `PlanningRequest`; `plan_phase(request, phase_name)`.
  2. Partition plan nodes **per file** by `NodeStatus` (ADR-0005 emits exactly
     one node status per target when the phase runs):
     - `Blocked` → drop file from the active set (abort-for-file); record blocked
       issue + `FilePhaseOutcome::Blocked` row.
     - `NoOp` (file already compliant for this phase) → **keep active, version
       unchanged, no dispatch**; record `FilePhaseOutcome::Skipped` row. (The
       bridge filters `Planned`-only, so `NoOp` mints no ticket — the coordinator
       must still keep the file active and account for it; omitting this bucket
       would either drop a compliant file or lose its row.)
     - `Planned` → proceed to dispatch.
     - **Zero nodes for the whole phase** (every target skipped via
       `run_if`/`skip_if`, ADR-0005) → phase `Skipped`; every active file stays
       active, version unchanged, `FilePhaseOutcome::Skipped`.
  3. Bridge planned nodes → `WorkflowPlan`; **override** the bridge's hardcoded
     `fan_out.max_files: 1` / `concurrency.max_in_flight_dispatches: 1`
     (`policy_bridge.rs:95-97`) with values driven by the active-file count so the
     phase runs across files concurrently; capture each active file's
     active-version id (needed by step 4); `submit_and_run_in_job(job_id, plan)`.
     (Single active file → 1/1, preserving Sprint 12–15 parity.)
  4. On ticket failure → whole job fails. `run_plan_in_job` returns
     `Err(WorkflowRunError { summary, source })` with counts but **no list of
     committed branches** (inline commit is worker-side in
     `transcode/audio/remux commit.rs`, invisible to the coordinator). Identify
     committed branches by diffing each active file's chain tip against the
     pre-phase active-version id captured in step 3: a file whose tip advanced
     committed inline. Backfill those files' per-`(file, phase)` `Committed` rows
     (re-probe already recorded in dispatch), then return the error with the
     partial summary — satisfying ADR-0007's "records which files advanced even on
     terminal failure." Files whose tip did not advance get no committed row.
     On this failure path the coordinator writes per-`(file, phase)` rows for
     committed branches but **no** phase-grain row for the failed phase (the four
     `PhaseOutcome` variants have no "failed" state) — the absent phase-grain row
     plus the `failed` job state denote the incomplete phase.
  5. For each committed branch: read new active version + reprobe snapshot,
     advance active version, `upsert_file_phase_summary` (`Committed`). Per-phase
     `per_operation` is a delta (see cross-phase accounting), not the cumulative
     returned summary.
  6. Regenerate the per-phase compliance report against refreshed facts;
     `upsert_phase_summary` with `report_id` + report JSON + `PhaseOutcome`.
     **Phase-grain `PhaseOutcome` rule** from the per-file outcome multiset:
     all files committed → `Completed`; any committed alongside any
     blocked/skipped → `PartiallyCommitted`; all blocked → `Blocked`; all skipped
     (incl. whole-phase skip and all-`NoOp`) → `Skipped`.
- After all phases: `succeed_job(job_id)`, then `insert_summary` (job grain
  counters + `per_operation`). On any phase's ticket failure the runner already
  failed the job; the coordinator finalizes committed branches and returns the
  partial summary without succeeding.
- **Tests first** (unit, handler-level, injected providers): single-file/single-
  phase parity; artifact-chain (phase N+1 plans against phase N's FileVersion);
  re-probe snapshot fed forward; bounded replan (one pass/phase, no phase added);
  unplannable→blocked; `NoOp`/compliant file stays active and carries forward;
  whole-phase skip; partial-barrier-failure finalization (concurrent siblings);
  coordinator-side error after `open_job` (e.g. injected `plan_phase` hard-error)
  leaves the job `failed`, not `open`; empty active set / empty `phase_order`
  succeeds with a zero-phase summary; mixed phase (commit + block) →
  `PhaseOutcome::PartiallyCommitted`.
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
- **Determinism:** the multi-file path runs with `max_in_flight_dispatches > 1`,
  so ticket/branch completion order is nondeterministic. Sort all per-ticket /
  per-branch / per-`(file, phase)` output by a stable key (`phase_ordinal`, then
  `branch_id`, then `node_id`) before rendering the snapshot, so the golden output
  is independent of dispatch order. The single-file case stays 1/1 (parity), so
  the ordering rule only affects the multi-file snapshot.

### Phase 6 — Closeout
- Sprint 16 closeout evidence matrix (phases→tickets→artifacts→snapshots→
  reports→CLI). `just ci` green.

## Verification gates
Each commit: `just fmt-check && just lint && cargo test -p voom-control-plane`.
Coordinator/probe-path changes additionally need full `cargo test --workspace`
(real ffprobe on staged output). Final: `just ci`.

## Risks / open checks to confirm before the dependent phase
- Confirm the dispatch path already records the reprobe snapshot against the
  *committed* FileVersion (so coordinator only reads, never re-probes) — verify
  in `transcode/audio/remux commit.rs` before Phase 3.
- Confirm `plan_phase`'s `Blocked` node carries enough target identity to map
  back to a `branch_id` for the per-file row.
- Confirm the per-phase compliance report generator accepts refreshed-snapshot
  input set without a stored input-set row (on-demand regen).

## Resolved by the cross-phase invariants section (no longer open)
- Per-phase vs cumulative counts → phase rows use deltas / explicit `ticket_ids`.
- Mid-barrier-failure committed-branch detection → active-version-id diff (Phase 3
  step 4).
- Next-phase hang on a non-terminal prior-phase ticket → terminal invariant +
  skipped-phase test.
- `branch_id` collision (same path stem) → fail-fast guard at job start (Phase 3),
  not reliance on the upsert's `DO NOTHING`.
