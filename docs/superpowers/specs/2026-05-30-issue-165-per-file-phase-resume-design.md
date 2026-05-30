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

`resume_phase_barrier` additionally **verifies `prior_job_id` exists** (a
`synthetic.workflow` job) and fails with `VoomError::NotFound` otherwise, so a
typo'd id is rejected rather than silently reconciling against nothing. It does
**not** require the prior job to be in a terminal state: a crash (the primary §8
case) leaves the job stuck `Open` because the process died before `fail_job`
ran. Resume therefore assumes a **single writer** — the caller guarantees the
prior run is no longer executing. Automated liveness detection and recovery
(lease expiry, watcher) are deferred (Sprint 16 §11); this sprint the caller
passes the **most-recently-failed** run's job id verbatim from the latest
`CoordinatorError.partial.job_id` (§3.3 explains why the latest job, not an
earlier one in a resume chain). A `prior_job_id` that points to a *different* real
run is a caller-contract violation that the coordinator cannot detect (jobs do not
store policy/input identity, ADR-0006/0007);
the consistency guard in §3.1 step 3 still prevents the worst outcome
(re-mutating an already-advanced file) by backfilling rather than re-planning
from scratch whenever a file's chain tip has advanced past what the rows record.

### 3.1 Per-file reconciliation

Read `file_phases_for_job(prior_job_id)` once and index rows by
`(branch_id, phase_ordinal)`. For each active file (current chain tip resolved by
`initial_phase_files`, branch id derived as in the fresh run):

1. **Terminal check.** If the file's **highest** recorded prior row has outcome
   `Blocked`, the file already aborted-for-file (§8). Exclude it from resume
   entirely.
2. **Resume ordinal.** `resume_ordinal` = the file's **highest recorded phase
   ordinal + 1**, or `0` if the file has no prior row. A committed/skipped row at
   ordinal *h* means the file passed every phase `≤ h`, so this is correct even
   when the visible rows are **not** a prefix from 0. If `resume_ordinal ≥
   phase_count`, the file is complete — drop it.

   *Why highest-plus-one, not smallest-missing.* Within any **single** job a
   file's rows are a contiguous range `[r, m]` where `r` is that run's resume
   point for the file (`0` for a fresh run): the phase loop adds a row for every
   entering file at every phase it participates in, until the file drops or the
   job ends. After a prior resume, the prior job's rows therefore start at `r >
   0`, not at 0. "Smallest ordinal with no row" would wrongly return `0` for such
   a job and re-enter a phase the file already passed; "highest recorded + 1"
   reads the contiguous tail correctly. See §3.3 for the chained-resume case this
   protects.
3. **Backfill detection (the consistency guard).** Let `recorded_tip` = the
   produced version of the file's highest `Committed` prior row, or **the file's
   input-set starting version if it has no `Committed` row**. If the current
   chain tip ≠ `recorded_tip`, a phase committed inline without a row. Backfill a
   `Committed` row at `resume_ordinal` (re-probe the current tip via
   `ProducedRefs::resolve`, empty `ticket_ids`), then `resume_ordinal += 1`. This
   one mechanism covers every "advanced-without-a-row" case, including a file
   with **zero** prior rows whose tip already advanced (a crash before the very
   first row write, or a stale/wrong `prior_job_id`): such a file is backfilled
   at ordinal 0 and resumes at 1 — it is **never** re-planned from scratch
   against its own product. So an already-advanced file is never re-mutated even
   when the prior rows are absent.

   *At-most-one-un-rowed-commit invariant.* The phase loop runs every entering
   file's inline commit during `dispatch_phase`, then writes **all** their rows
   in `finalize_phase`, then advances to the next phase. A crash can therefore
   lose at most the row(s) of the phase currently finalizing, never of an earlier
   phase; within the job the file's rows are a contiguous range ending at the
   last finalized phase, and the only possibly-missing row is the one just past
   the highest recorded ordinal — `resume_ordinal`. The backfilled artifact is
   unambiguously the product of that phase.

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
- A phase whose entering set is a **strict subset** of the run's files (some
  files re-enter, others already advanced past it) regenerates its report from
  the entering files' refreshed facts only — exactly as ADR-0008 already does
  for the fresh run, which regenerates from the files that entered the phase.
  The resumed job's phase-*p* report therefore covers fewer files than the prior
  job's phase-*p* report. This is intentional: the prior job's rows remain the
  durable record for the files that advanced earlier, and the resumed job's rows
  record what this run did. **Stitching the two jobs' per-phase reports into a
  single cross-job participant view is out of scope** (the durable summary is
  per-job by ADR-0006/0009); a reader reconstructs a file's full phase history by
  following its chain tip and the per-`(file, phase)` rows across both job ids.

A fresh `run_phase_barrier` sets every `resume_ordinal = 0`, so every file enters
every phase from phase 0 — the phase loop is identical to today's behavior. The
only change to the fresh path is the §5 `on_error` reject in its resolve
prologue.

### 3.3 Repeated (chained) resume

A resume can itself crash or fail, so resume must survive being run against a job
that is *already* a resume. **Caller contract: always pass the most-recently-failed
job id** (the latest `CoordinatorError.partial.job_id`). By construction that job
holds each file's contiguous tail `[r, m]` (§3.1 step 2: a single job records a
file's rows from that run's resume point onward), so:

- **Highest-recorded-plus-one** reads the tail exactly: a resumed job J2 holding
  `{F: phase 1 = Committed}` yields `resume_ordinal(F) = 2`, not `0`, so F is not
  re-entered at a phase it already passed. A depth-N chain stays correct because
  each resume passes the latest job, whose rows are F's newest tail.
- **The backfill** then only ever needs to cover the *single* within-that-job
  crash gap (commit landed, row not yet written; §3.1 step 3's at-most-one
  invariant), never multiple commits.

Passing an **older** job in a chain is a caller-contract violation, in the same
class as an unrelated job id (§6): an older job hides every commit a later sibling
recorded, and the single-commit backfill cannot absorb more than one, so a
container phase could be re-planned against its own product. The coordinator
cannot detect a stale-but-valid job id (jobs store no chain linkage); cross-job
per-file cursors that would make any job id in the chain safe are deferred (§11).
This sprint's guarantee is *no re-mutation under repeated resume when the caller
honors the most-recent-job contract*, verified by a two-failure unit test (§7).
A file that the passed job records as `Blocked` is terminal and excluded (§3.1
step 1); a `Blocked` file invisible to the passed job that is re-entered re-plans
to the same diagnostic and **commits nothing**, so even that residual case is a
redundant re-block, never re-mutation.

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

**This is an intentional fail-fast compatibility break.** The guard rejects on
`phase_order` membership alone, regardless of whether the offending phase would
actually run (its `run_if` may be false for every file). A policy that "worked"
only because its `continue`/`skip` phase was never reached now hard-fails at
resolve — which is the point: a silent no-op is indistinguishable at runtime
from real handling, so a future half-implementation of `continue` would regress
undetected (spec §9 "on_error tests … so the limitation cannot silently
regress"). One committed fixture already carries this shape —
`crates/voom-policy/fixtures/policies/production-normalize-reduced.voom` (policy
`ln`, `on_error: continue`) — but it is **not** wired through
`run_phase_barrier` / `execute_compliance_policy` by any current coordinator or
CLI golden test. The plan's verification step confirms no live execute/golden
path drives a `continue`/`skip` policy before the guard lands, so the reject
adds a test for a new failure mode rather than breaking an existing green one.

## 6. Error handling and edges

- **Resume of a non-existent `prior_job_id`:** rejected with `VoomError::NotFound`
  before the job opens (§3). Resume is for continuing a known prior run, not a
  cold start — a fresh start uses `run_phase_barrier`.
- **Resume of an existing but mismatched `prior_job_id`** (a real job from a
  different run whose rows do not key to these files): the coordinator cannot
  detect this (jobs store no policy/input identity). It does **not** silently
  re-plan an advanced file from scratch, because the §3.1 step-3 consistency
  guard backfills any file whose chain tip has advanced past its `recorded_tip`
  (which defaults to the input-set starting version when the file has no
  `Committed` row). So a file that advanced under the *real* prior run is
  backfilled and skipped rather than re-mutated, even when the supplied
  `prior_job_id` is wrong. A file that genuinely never advanced (tip == starting
  version) runs all phases from 0, which is correct. The only residual risk is a
  wrong id whose foreign rows collide by `branch_id` *and* mislead reconciliation
  toward skipping real work — a caller-contract violation called out in §3, not a
  silent data-loss path.
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
  recorded is dropped.
- `prior_job_id` that does not exist is rejected with `NOT_FOUND`, **no job
  opened**.
- Backfill: a committed-tip-without-row file (rows present through *k-1*, tip
  advanced) gets a `Committed` row backfilled at the first missing ordinal with
  empty `ticket_ids`, and is not re-dispatched.
- Backfill with **zero** prior rows: a file whose tip already advanced past its
  input-set starting version but has no rows is backfilled at ordinal 0 and not
  re-mutated (the consistency guard for a stale/wrong prior id).
- Chained resume (§3.3): a file whose only visible prior row is `Committed` at
  ordinal *h* (a resumed prior job, no rows `< h`) resumes at *h+1*, not 0 —
  asserting `highest-recorded + 1`, so the already-passed phases are not
  re-entered or re-mutated.
- Compatibility check (not a test, a pre-implementation grep, recorded in the
  plan): confirm no coordinator/CLI golden currently drives a `continue`/`skip`
  policy through `execute`, so the §5 reject introduces no pre-existing failure.

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
