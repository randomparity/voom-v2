---
name: issue-165-per-file-phase-resume-design
description: Design for issue #165 — per-(file, phase) resume reconciliation and partial-barrier finalization for the phase-barrier coordinator, plus resolve-time rejection of non-default on_error strategies. Refines Sprint 16 spec §6/§8.
status: draft
date: 2026-05-30
issue: 165
references:
  - docs/superpowers/specs/2026-05-29-voom-sprint-16-design.md
  - docs/adr/0009-resume-opens-new-job-reconciles-prior-rows.md
  - docs/adr/0007-phase-barrier-coordinator.md
  - docs/adr/0006-workflow-summary-schema.md
---

# Issue #165 — Per-(file, phase) resume and partial-barrier reconciliation

## 1. Goal

Close the resume half of the phase-barrier contract (Sprint 16 §8). The
coordinator (#162) already drives phases as barriers, commits each file's phase
artifact inline, persists a per-`(file, phase)` summary row, and — on a whole-job
failure mid-barrier — backfills a `Committed` row for every file that advanced
before the failure (`finalize_failed_phase`). What is missing:

1. **Resume.** Re-running the policy against the same input set after a crash or a
   failed job must re-enter, per file, the first phase whose artifact is not yet
   committed, and must never re-mutate a file already advanced past a phase.
2. **Backfill on resume.** A file whose phase-*k* tickets committed but whose
   per-`(file, phase)` row is missing (a crash between the inline commit and the
   row write) is finalized by re-probing the already-committed artifact and
   writing that row — no re-mutation.
3. **`on_error` guard.** A non-default `CompiledPhase.on_error` (`continue` /
   `skip`) is rejected at resolve time so the deferred-handling limitation (§11)
   cannot silently regress.

Non-goals (unchanged from Sprint 16 §11): per-file isolation of *ticket*
failures, rollback/active-version reset, honoring `continue`/`skip`, independent
per-file phase cursors beyond what resume needs.

## 2. Existing state (already shipped, not re-implemented)

- The phase loop, inline commit, re-probe, and per-`(file, phase)` row writes
  (`finalize_phase` / `finalize_file`) — #162, ADR-0007.
- Mid-barrier finalization on **job failure** (`finalize_failed_phase`): every
  file that committed inline before the failure gets a `Committed` row, the job
  is `failed`, and the partial outcome is returned in `CoordinatorError.partial`
  — #162. Issue #165 bullet 3 ("on job failure mid-barrier, finalize any file
  that committed…") is therefore **already satisfied**; this design pins it with
  a regression test and builds resume on top of it.
- Per-phase report regeneration against refreshed facts — #164, ADR-0008.

## 3. Resume entry point and reconciliation

Per ADR-0009, resume opens a **new** job and reconciles against the prior job's
rows, addressed by an explicit `prior_job_id`.

```
ControlPlane::resume_phase_barrier(
    prior_job_id, policy_version_id, input_set_id, options,
) -> Result<CoordinatorOutcome, CoordinatorError>
ControlPlane::resume_phase_barrier_with_runtimes(.., runtimes) -> ...   // test seam
```

Both share the existing resolve prologue with `run_phase_barrier`
(`load_current_accepted_policy_and_input`, `compiled_policy_for_version`,
`active_branch_ids`) plus the new `on_error` guard (§5), then open the job and
enter the shared phase loop with per-file `resume_ordinal` set from
reconciliation.

### 3.1 Per-file reconciliation

Read `file_phases_for_job(prior_job_id)` once and index rows by
`(branch_id, phase_ordinal)`. For each active file (current chain tip resolved by
`initial_phase_files`, branch id derived as in the fresh run):

1. **Terminal check.** If any prior row for the file has outcome `Blocked`, the
   file already aborted-for-file (§8). Exclude it from resume entirely.
2. **Resume ordinal.** `resume_ordinal` = the smallest ordinal in
   `[0, phase_count)` with no prior row for the file. If every phase has a row,
   the file is complete — drop it.
3. **Backfill detection.** Let `recorded_tip` = the produced version of the
   file's highest `Committed` prior row, or its starting version if none. If the
   current chain tip ≠ `recorded_tip`, a phase committed inline without a row.
   Backfill a `Committed` row at `resume_ordinal` (re-probe the current tip via
   `ProducedRefs::resolve`, empty `ticket_ids`), then `resume_ordinal += 1`.
   Invariant: inline commit and row write are adjacent, so at most one row is
   lost; the missing phase is exactly `resume_ordinal`.

The file enters the phase loop with its computed `resume_ordinal` and its
current chain-tip snapshot.

### 3.2 Heterogeneous per-file start in the phase loop

`PhaseFile` carries `resume_ordinal: u32` (`0` for a fresh run). The phase loop
generalizes:

- At phase `p` (ordinal), the *entering* set is the active files with
  `resume_ordinal <= p`. The phase draft, plan, classification, dispatch, and
  `finalize_phase` operate **only** on the entering set.
- Files with `resume_ordinal > p` pass through untouched: no plan, no row for
  phase `p`, kept active so they rejoin at their own resume phase.
- The loop walks `phase_order` once; it `break`s only when no active files
  remain (an all-pass-through phase still advances `p`).
- A phase whose entering set is empty writes no phase-grain row (no work
  happened at that phase in this run) and regenerates no report.

A fresh `run_phase_barrier` sets every `resume_ordinal = 0`, so every file enters
every phase from phase 0 — identical to today's behavior.

## 4. Idempotency contract

- Fully-recorded `(file, phase)` pairs are never re-run (the recorded-row check
  drops them below `resume_ordinal`).
- The committed artifact (append-only chain tip) is the active version the next
  phase plans against, so resume needs no rollback.
- Replanning is **not** the idempotency signal: ADR-0007 notes a container
  transform re-runs on replanning against its own product. The recorded-row check
  is what prevents re-mutation, so a recorded container phase is not re-run on
  resume.
- Backfill writes a durable `Committed` row from the already-committed bytes; it
  dispatches nothing.

## 5. `on_error` rejection at resolve time

A pure helper `reject_unhandled_on_error(policy) -> Result<(), VoomError>` runs
after `compiled_policy_for_version` and before `open_job`, in both fresh and
resume paths. For each phase name in `phase_order`, look up its `CompiledPhase`
and reject `Some(ErrorStrategy::Continue)` / `Some(ErrorStrategy::Skip)` with
`VoomError::PolicyValidationError` (code `POLICY_VALIDATION_ERROR`) naming the
phase and strategy. `None` and `Some(Abort)` pass (default whole-job abort,
unchanged). Because the guard precedes `open_job`, a rejected policy opens **no**
job (asserted, mirroring the branch-collision guard).

## 6. Error handling and edges

- **Resume of a non-existent / mismatched `prior_job_id`:** reading
  `file_phases_for_job` of an unknown job returns no rows; every file then
  reconciles to `resume_ordinal = 0` and resume degrades to a fresh run against
  the current chain tips. This is safe (committed artifacts are still the active
  version) and is the documented behavior, not an error.
- **A file present in the prior rows but absent from the current input set, or
  vice versa:** reconciliation is per current-input-set file; prior rows for
  files not in the current set are ignored, and current files with no prior rows
  start at `resume_ordinal = 0`.
- **Backfill when the highest `Committed` row's produced version equals the
  current tip:** no gap, no backfill; `resume_ordinal` is the first missing
  phase as computed.
- **`on_error` on a phase not in `phase_order`:** unreachable — the guard walks
  `phase_order`, the authoritative execution order; a phase absent from it never
  runs and is irrelevant.
- **Branch-id collision on resume:** rejected before the job opens by the
  existing `active_branch_ids` check, same as the fresh run.

## 7. Testing

Unit (`coordinator_test.rs`, handler-level, no real dispatch):

- `on_error` `continue` and `skip` each rejected with `POLICY_VALIDATION_ERROR`,
  naming the phase; **no job opened**. `abort` and unset accepted (reach the loop).
- Reconciliation: a file with prior `Committed` rows through phase *k* resumes at
  *k+1*; a file with a prior `Blocked` row is excluded; a file with all phases
  recorded is dropped; an unknown `prior_job_id` degrades to a fresh run.
- Backfill: a committed-tip-without-row file gets a `Committed` row backfilled at
  the first missing ordinal with empty `ticket_ids`, and is not re-dispatched.

Integration (`tests/phase_barrier_flow.rs`, real ffprobe on staged output, run
only under `cargo test --workspace`):

- **Partial-barrier-failure + resume (acceptance):** two files, branch A commits
  at phase *k*, branch B's ticket fails (whole job fails). Assert the failed
  job's summary records A `Committed` at *k* and no B row at *k*. Resume against
  the failed job id; assert A is **not** re-mutated (no new A version, phase *k*
  not in A's resumed rows) and B re-enters phase *k* and commits, under a new
  job id.
- **Regression for already-shipped finalization:** the existing
  `phase_barrier_records_committed_sibling_when_a_file_fails` stays green.

## 8. Acceptance criteria (issue #165)

- Partial-barrier-failure + resume: A not re-mutated (phase *k* skipped), B
  re-enters phase *k*. ✔ via the integration test above.
- `on_error`: a non-default strategy is rejected so the limitation cannot
  silently regress. ✔ via the unit tests above.
- `just ci` passes; guardrails green at every commit.
