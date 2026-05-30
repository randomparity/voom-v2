# Per-(file, phase) Resume Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add per-`(file, phase)` resume reconciliation and resolve-time rejection of non-default `on_error` to the Sprint 16 phase-barrier coordinator, so a crashed/failed multi-phase run can be re-driven without re-mutating already-advanced files.

**Architecture:** Reuse the existing coordinator (`run_phase_barrier`, `dispatch_phase`, `finalize_phase`, `finalize_failed_phase`). Add (1) an `on_error` guard in the shared resolve prologue, (2) a per-file `resume_ordinal` on `PhaseFile` so the phase loop can start files at heterogeneous phases, and (3) a `resume_phase_barrier` entry point that reconciles each file against the most-recent failed job's per-`(file, phase)` rows (highest-recorded + 1, with a single-commit consistency backfill). New job per resume; prior job's rows are read-only reconciliation input (ADR-0009).

**Tech Stack:** Rust, tokio, sqlx/SQLite, `voom-control-plane` crate. Tests are sibling `*_test.rs` (unit) and `crates/voom-control-plane/tests/phase_barrier_flow.rs` (integration, real ffprobe).

**Spec:** `docs/superpowers/specs/2026-05-30-issue-165-per-file-phase-resume-design.md`
**ADR:** `docs/adr/0009-resume-opens-new-job-reconciles-prior-rows.md`

---

## File Structure

- Modify `crates/voom-control-plane/src/workflow/coordinator.rs`:
  - `PhaseFile`: add `resume_ordinal: u32` and `start_version_id: FileVersionId`.
  - `reject_unhandled_on_error` (new free fn): resolve-time `on_error` guard.
  - `initial_phase_files`: set the two new fields.
  - `run_phase_barrier_in_job` → split into `drive_phase_loop` (shared, heterogeneous-aware) called by a thin fresh wrapper.
  - `run_phase_barrier_with_runtimes`: call the guard before `open_job`.
  - `resume_phase_barrier` / `resume_phase_barrier_with_runtimes` (new pub entry points).
  - `resume_phase_barrier_in_job` + `reconcile_resume` (new private helpers).
- Modify `crates/voom-control-plane/src/workflow/coordinator_test.rs`: unit tests.
- Modify `crates/voom-control-plane/tests/phase_barrier_flow.rs`: resume integration test.

Verification commands (run from repo root):
- Single unit test: `cargo test -p voom-control-plane <test_name>`
- Lint: `cargo clippy -p voom-control-plane --all-targets --all-features -- -D warnings`
- Full CI: `just ci`

---

## Task 1: Reject non-default `on_error` at resolve time

**Files:**
- Modify: `crates/voom-control-plane/src/workflow/coordinator.rs`
- Test: `crates/voom-control-plane/src/workflow/coordinator_test.rs`

- [ ] **Step 1: Confirm no live golden drives a `continue`/`skip` policy** (spec §5 verification)

Run:
```bash
rg -n 'production-normalize|on_error' crates/voom-control-plane crates/voom-cli/tests
```
Expected: no coordinator/CLI execute or golden test loads a policy whose compiled `on_error` is `continue`/`skip`. Record the result in the Task 1 commit message. (As of writing, only `voom-policy` fixtures reference such policies.)

- [ ] **Step 2: Write the failing unit test**

Add to `coordinator_test.rs`. This uses an in-memory `CompiledPolicy` with a phase carrying `on_error: Continue`, exercising the guard helper directly (no DB):

```rust
use voom_policy::{CompiledPhase, CompiledPolicy, ErrorStrategy};

fn policy_with_on_error(strategy: Option<ErrorStrategy>) -> CompiledPolicy {
    let mut policy = CompiledPolicy::minimal_for_test("guarded", "src-hash-onerr");
    policy.phases = vec![CompiledPhase {
        name: "normalize".to_owned(),
        depends_on: Vec::new(),
        run_if: None,
        skip_if: None,
        on_error: strategy,
        operations: Vec::new(),
    }];
    policy.phase_order = vec!["normalize".to_owned()];
    policy
}

#[test]
fn reject_unhandled_on_error_rejects_continue() {
    let err = super::reject_unhandled_on_error(&policy_with_on_error(Some(ErrorStrategy::Continue)))
        .expect_err("continue must be rejected");
    assert_eq!(err.code(), "POLICY_VALIDATION_ERROR");
    assert!(err.to_string().contains("normalize"), "names the phase: {err}");
    assert!(err.to_string().contains("continue"), "names the strategy: {err}");
}

#[test]
fn reject_unhandled_on_error_rejects_skip() {
    let err = super::reject_unhandled_on_error(&policy_with_on_error(Some(ErrorStrategy::Skip)))
        .expect_err("skip must be rejected");
    assert_eq!(err.code(), "POLICY_VALIDATION_ERROR");
    assert!(err.to_string().contains("normalize"));
}

#[test]
fn reject_unhandled_on_error_allows_abort_and_unset() {
    assert!(super::reject_unhandled_on_error(&policy_with_on_error(Some(ErrorStrategy::Abort))).is_ok());
    assert!(super::reject_unhandled_on_error(&policy_with_on_error(None)).is_ok());
}
```

Confirm `CompiledPolicy::minimal_for_test` is `#[cfg(test)]`-visible from this crate (it is `pub`, gated `#[cfg(test)]` in `voom-policy`). If it is not reachable across crates, build the policy with `CompiledPhase` literals against a `CompiledPolicy { .. }` struct literal using its public fields instead (all fields shown in `compiled.rs:9-20`).

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p voom-control-plane reject_unhandled_on_error`
Expected: FAIL — `reject_unhandled_on_error` not found.

- [ ] **Step 4: Implement the guard helper**

Add near the other free functions in `coordinator.rs` (e.g. after `phase_outcome`):

```rust
/// Reject a policy whose any `phase_order` phase declares a non-default
/// `on_error` strategy. `continue`/`skip` are deferred this sprint (spec §11);
/// honoring them partially would be indistinguishable at runtime from real
/// handling, so they are rejected at resolve time before any job opens.
fn reject_unhandled_on_error(policy: &voom_policy::CompiledPolicy) -> Result<(), VoomError> {
    for phase_name in &policy.phase_order {
        let Some(phase) = policy.phases.iter().find(|p| p.name == *phase_name) else {
            continue;
        };
        match phase.on_error {
            None | Some(voom_policy::ErrorStrategy::Abort) => {}
            Some(strategy) => {
                let label = match strategy {
                    voom_policy::ErrorStrategy::Continue => "continue",
                    voom_policy::ErrorStrategy::Skip => "skip",
                    voom_policy::ErrorStrategy::Abort => unreachable!(),
                };
                return Err(VoomError::PolicyValidationError(format!(
                    "phase `{phase_name}` declares on_error `{label}`, which is not supported \
                     this sprint (only the default abort); see Sprint 16 §11"
                )));
            }
        }
    }
    Ok(())
}
```

Add `ErrorStrategy` to the `voom_policy` import line if you prefer unqualified use; qualified paths above need no import change.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p voom-control-plane reject_unhandled_on_error`
Expected: PASS (3 tests).

- [ ] **Step 6: Wire the guard into the fresh resolve prologue**

In `run_phase_barrier_with_runtimes`, immediately after `let policy = self.compiled_policy_for_version(&inputs.version).await?;` add:

```rust
        reject_unhandled_on_error(&policy)?;
```

(`?` converts `VoomError` → `CoordinatorError` via the existing `From` impl; the guard precedes `open_job`, so no job is opened on rejection.)

- [ ] **Step 7: Lint + run the coordinator suite**

Run: `cargo clippy -p voom-control-plane --all-targets --all-features -- -D warnings && cargo test -p voom-control-plane`
Expected: clean, all green.

- [ ] **Step 8: Commit**

```bash
git add crates/voom-control-plane/src/workflow/coordinator.rs crates/voom-control-plane/src/workflow/coordinator_test.rs
git commit -m "feat(control-plane): reject non-default on_error at resolve time

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Add per-file `resume_ordinal` and generalize the phase loop

This makes the loop start files at heterogeneous phases while keeping the fresh
run (all `resume_ordinal = 0`) byte-for-byte identical.

**Files:**
- Modify: `crates/voom-control-plane/src/workflow/coordinator.rs`
- Test: `crates/voom-control-plane/src/workflow/coordinator_test.rs`

- [ ] **Step 1: Extend `PhaseFile`**

```rust
struct PhaseFile {
    asset_id: FileAssetId,
    version_id: FileVersionId,
    /// The input-set starting version (chain root for this run); the backfill
    /// consistency guard compares the current tip against this when no committed
    /// row is visible.
    start_version_id: FileVersionId,
    snapshot: MediaSnapshot,
    branch_id: String,
    ordinal: u32,
    /// First phase ordinal this file participates in (`0` for a fresh run; set by
    /// resume reconciliation). Files pass through phases below this untouched.
    resume_ordinal: u32,
}
```

- [ ] **Step 2: Set the new fields in `initial_phase_files`**

In `initial_phase_files`, the loop already has `version_id` (the input-set starting version) and computes `tip`. Set:

```rust
            files.push(PhaseFile {
                asset_id: version.file_asset_id,
                version_id: tip.id,
                start_version_id: *version_id,
                snapshot,
                branch_id: branch_id.clone(),
                ordinal: u32::try_from(index + 1)
                    .map_err(|e| VoomError::Internal(format!("file ordinal overflow: {e}")))?,
                resume_ordinal: 0,
            });
```

- [ ] **Step 3: Run to confirm the crate still builds (existing tests cover fresh behavior)**

Run: `cargo test -p voom-control-plane run_phase_barrier`
Expected: PASS — adding fields with `resume_ordinal: 0` does not change behavior yet (the loop ignores the field). This is the safety net for the refactor.

- [ ] **Step 4: Extract `drive_phase_loop` from `run_phase_barrier_in_job`, made heterogeneous-aware**

Replace `run_phase_barrier_in_job` with a thin wrapper plus the generalized loop. The wrapper keeps the existing signature/callsite:

```rust
    #[expect(
        clippy::too_many_arguments,
        reason = "one owned job's run state: policy, context, draft, branch ids, options, runtimes"
    )]
    async fn run_phase_barrier_in_job(
        &self,
        job_id: JobId,
        policy: &voom_policy::CompiledPolicy,
        context: &PlanningContext,
        base_draft: PolicyInputSetDraft,
        branch_ids: &[(FileVersionId, String)],
        options: ComplianceExecutionOptions,
        runtimes: WorkerRuntimeRegistry,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        if branch_ids.is_empty() || policy.phase_order.is_empty() {
            return Ok(self.finalize_zero_phase_run(job_id, Vec::new()).await?);
        }
        let files = self.initial_phase_files(branch_ids).await?;
        self.drive_phase_loop(job_id, policy, context, base_draft, files, Vec::new(), options, runtimes)
            .await
    }
```

Now add `drive_phase_loop`. It is the old loop body with two changes: it accepts `files` + `seed_file_phases` as parameters, and each phase partitions `files` into the *entering* set (`resume_ordinal <= phase_ordinal`) and pass-through, operating only on the entering set:

```rust
    #[expect(
        clippy::too_many_arguments,
        reason = "one owned job's run state plus the pre-seeded resume rows"
    )]
    async fn drive_phase_loop(
        &self,
        job_id: JobId,
        policy: &voom_policy::CompiledPolicy,
        context: &PlanningContext,
        base_draft: PolicyInputSetDraft,
        mut files: Vec<PhaseFile>,
        seed_file_phases: Vec<FilePhaseSummary>,
        options: ComplianceExecutionOptions,
        runtimes: WorkerRuntimeRegistry,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        if files.is_empty() || policy.phase_order.is_empty() {
            return Ok(self.finalize_zero_phase_run(job_id, seed_file_phases).await?);
        }
        let executor = WorkflowExecutor::with_options(
            self.clone(),
            SingleWorkerPerKindSelector,
            runtimes,
            WorkflowExecutorOptions::from(options),
        );

        let mut phases = Vec::new();
        let mut file_phases = seed_file_phases;
        let mut last_run = None;
        for (index, phase_name) in policy.phase_order.iter().enumerate() {
            if files.is_empty() {
                break;
            }
            let phase_ordinal = u32::try_from(index)
                .map_err(|e| VoomError::Internal(format!("phase ordinal overflow: {e}")))?;
            let (mut entering, passthrough): (Vec<PhaseFile>, Vec<PhaseFile>) = std::mem::take(&mut files)
                .into_iter()
                .partition(|file| file.resume_ordinal <= phase_ordinal);
            if entering.is_empty() {
                files = passthrough;
                continue;
            }
            let draft = phase_draft(&base_draft, &entering);
            let plan = voom_plan::plan_phase(
                PlanningRequest {
                    policy: policy.clone(),
                    input: draft,
                    context: context.clone(),
                },
                phase_name,
            )
            .map_err(voom_plan::PlanGenerationError::into_voom_error)?;
            let report = voom_plan::generate_compliance_report(&plan)
                .map_err(voom_plan::ComplianceReportError::into_voom_error)?;
            let dispositions = classify_phase(&entering, &plan);

            let run = match self
                .dispatch_phase(&executor, job_id, &plan, &report, &dispositions)
                .await
            {
                Ok(run) => run,
                Err(failure) => {
                    return self
                        .finalize_failed_phase(
                            job_id,
                            phase_ordinal,
                            &entering,
                            &dispositions,
                            failure,
                            phases,
                            file_phases,
                        )
                        .await;
                }
            };
            if run.is_some() {
                last_run = run;
            }
            let (rows, refreshed) = self
                .finalize_phase(job_id, phase_ordinal, &mut entering, &dispositions)
                .await?;
            let outcome = phase_outcome(&rows.iter().map(|row| row.outcome).collect::<Vec<_>>());
            file_phases.extend(rows);
            let report =
                regenerate_phase_report(policy, context, &base_draft, phase_name, &refreshed)?;
            let phase_row = self
                .workflow_summaries()
                .upsert_phase_summary(
                    NewPhaseSummary {
                        job_id,
                        phase_ordinal,
                        phase_name: phase_name.clone(),
                        report: Some(report),
                        outcome,
                    },
                    self.clock().now(),
                )
                .await?;
            phases.push(phase_row);

            // Recombine the survivors of this phase with the files that have not
            // yet reached their resume phase.
            files = entering;
            files.extend(passthrough);
        }

        let now = self.clock().now();
        self.succeed_job(job_id, now).await?;
        let summary = self
            .workflow_summaries()
            .insert_summary(job_grain_summary(job_id, last_run.as_ref()), now)
            .await?;
        Ok(CoordinatorOutcome {
            job_id,
            summary,
            phases,
            file_phases,
        })
    }
```

- [ ] **Step 5: Update `finalize_zero_phase_run` to carry seed rows**

Change its signature to accept the pre-seeded rows so a resume that backfilled rows but has nothing left to run still returns them:

```rust
    async fn finalize_zero_phase_run(
        &self,
        job_id: JobId,
        seed_file_phases: Vec<FilePhaseSummary>,
    ) -> Result<CoordinatorOutcome, VoomError> {
        let now = self.clock().now();
        self.succeed_job(job_id, now).await?;
        let summary = self
            .workflow_summaries()
            .insert_summary(
                NewWorkflowSummary {
                    job_id,
                    branch_count: 0,
                    ticket_count: 0,
                    dispatch_count: 0,
                    retry_count: 0,
                    failure_count: 0,
                    peak_active_workflow_leases: 0,
                    elapsed: Duration::ZERO,
                    per_operation: json!({}),
                },
                now,
            )
            .await?;
        Ok(CoordinatorOutcome {
            job_id,
            summary,
            phases: Vec::new(),
            file_phases: seed_file_phases,
        })
    }
```

The only existing caller is `run_phase_barrier_in_job` (updated in Step 4 to pass `Vec::new()`).

- [ ] **Step 6: Run the full coordinator + integration suite to prove fresh behavior is unchanged**

Run: `cargo test -p voom-control-plane`
Expected: PASS — all existing coordinator unit tests and the `run_phase_barrier_*` tests still pass; the partition with all `resume_ordinal = 0` yields `entering = all files`, `passthrough = empty` every phase.

- [ ] **Step 7: Add a heterogeneous-start unit test (no real dispatch)**

This proves a file with `resume_ordinal > 0` is skipped for the early phase. Use the all-blocked path so no worker is needed: seed two files, set one's `resume_ordinal` past phase 0 by driving through `reconcile_resume` is not yet available, so instead test the partition behavior via a focused test on `drive_phase_loop` is heavy. Defer the behavioral assertion to the Task 4 reconciliation tests and the Task 5 integration test; here, assert only that the fresh path is unaffected by re-running an existing representative test by name:

Run: `cargo test -p voom-control-plane run_phase_barrier_drops_unplannable_file_as_blocked`
Expected: PASS.

- [ ] **Step 8: Lint + commit**

```bash
cargo clippy -p voom-control-plane --all-targets --all-features -- -D warnings
git add crates/voom-control-plane/src/workflow/coordinator.rs
git commit -m "refactor(control-plane): drive phase loop with per-file resume ordinals

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Reconcile a file's resume ordinal from prior rows (+ backfill)

**Files:**
- Modify: `crates/voom-control-plane/src/workflow/coordinator.rs`
- Test: `crates/voom-control-plane/src/workflow/coordinator_test.rs`

- [ ] **Step 1: Write the failing unit tests for `reconcile_resume`**

These build durable prior rows via the workflow-summaries repo, then assert the
computed `resume_ordinal` per file. Add helpers + tests to `coordinator_test.rs`.
Use the existing `seed_version` / `latest_snapshot` helpers and the repo's
`upsert_file_phase_summary`.

```rust
use voom_store::repo::workflow_summaries::{NewFilePhaseSummary, WorkflowSummaryRepo};

async fn open_workflow_job(cp: &crate::ControlPlane) -> JobId {
    use voom_store::repo::jobs::NewJob;
    cp.open_job(NewJob {
        kind: "synthetic.workflow".to_owned(),
        priority: 0,
        created_at: T0,
    })
    .await
    .unwrap()
    .id
}

async fn record_file_phase(
    cp: &crate::ControlPlane,
    job_id: JobId,
    phase_ordinal: u32,
    branch_id: &str,
    outcome: FilePhaseOutcome,
    produced_version: Option<FileVersionId>,
) {
    cp.workflow_summaries()
        .upsert_file_phase_summary(
            NewFilePhaseSummary {
                job_id,
                phase_ordinal,
                branch_id: branch_id.to_owned(),
                ticket_ids: Vec::new(),
                produced_file_version_id: produced_version,
                produced_file_location_id: None,
                artifact_handle_id: None,
                reprobe_snapshot_id: None,
                outcome,
            },
            T0,
        )
        .await
        .unwrap();
}
```

Test: a file with `Committed` rows through phase 1 resumes at 2; a file with a
`Blocked` top row is excluded; an all-recorded file is dropped. Because
`reconcile_resume` needs the file's chain tip, drive these through a small
constructed `PhaseFile` set. Expose a thin test-only wrapper if needed:

```rust
#[tokio::test]
async fn reconcile_resume_resumes_after_highest_recorded_phase() {
    let (cp, _tmp) = cp().await;
    let prior = open_workflow_job(&cp).await;
    let v = seed_version(&cp, "/lib/r/movie.mkv", "hash-r1", reprobe_payload("h264")).await;
    record_file_phase(&cp, prior, 0, "movie", FilePhaseOutcome::Committed, Some(v)).await;
    record_file_phase(&cp, prior, 1, "movie", FilePhaseOutcome::Committed, Some(v)).await;

    let files = cp.initial_phase_files(&[(v, "movie".to_owned())]).await.unwrap();
    let new_job = open_workflow_job(&cp).await;
    let (survivors, backfilled) = cp
        .reconcile_resume(prior, new_job, files, /*phase_count*/ 4)
        .await
        .unwrap();

    assert_eq!(survivors.len(), 1);
    assert_eq!(survivors[0].resume_ordinal, 2, "highest recorded (1) + 1");
    assert!(backfilled.is_empty(), "tip == recorded committed version, no backfill");
}

#[tokio::test]
async fn reconcile_resume_excludes_blocked_file() {
    let (cp, _tmp) = cp().await;
    let prior = open_workflow_job(&cp).await;
    let v = seed_version(&cp, "/lib/b/movie.mkv", "hash-b1", reprobe_payload("h264")).await;
    record_file_phase(&cp, prior, 0, "movie", FilePhaseOutcome::Blocked, None).await;

    let files = cp.initial_phase_files(&[(v, "movie".to_owned())]).await.unwrap();
    let new_job = open_workflow_job(&cp).await;
    let (survivors, backfilled) = cp.reconcile_resume(prior, new_job, files, 4).await.unwrap();

    assert!(survivors.is_empty(), "a blocked file is terminal");
    assert!(backfilled.is_empty());
}

#[tokio::test]
async fn reconcile_resume_drops_fully_recorded_file() {
    let (cp, _tmp) = cp().await;
    let prior = open_workflow_job(&cp).await;
    let v = seed_version(&cp, "/lib/c/movie.mkv", "hash-c1", reprobe_payload("h264")).await;
    for ordinal in 0..2 {
        record_file_phase(&cp, prior, ordinal, "movie", FilePhaseOutcome::Committed, Some(v)).await;
    }
    let files = cp.initial_phase_files(&[(v, "movie".to_owned())]).await.unwrap();
    let new_job = open_workflow_job(&cp).await;
    let (survivors, _) = cp.reconcile_resume(prior, new_job, files, /*phase_count*/ 2).await.unwrap();
    assert!(survivors.is_empty(), "resume_ordinal (2) >= phase_count (2) => complete");
}
```

Note: `initial_phase_files`, `reconcile_resume`, `open_job`, and `workflow_summaries()` must be reachable from the test module. They are methods on `ControlPlane` within the same crate; `initial_phase_files`/`reconcile_resume` are private but the sibling test module (`mod tests`) can call them via `cp.` since it is a child module of `coordinator`. (`open_job` is already `pub`; `workflow_summaries()` is `pub(crate)` or add it if missing — check the existing call in `coordinator.rs`.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p voom-control-plane reconcile_resume`
Expected: FAIL — `reconcile_resume` not found.

- [ ] **Step 3: Implement `reconcile_resume`**

Add to the `impl ControlPlane` block in `coordinator.rs`:

```rust
    /// Compute each active file's `resume_ordinal` from the most-recent failed
    /// job's per-`(file, phase)` rows (spec §3.1). Drops files that are terminal
    /// (`Blocked` at their highest recorded phase) or complete
    /// (`resume_ordinal >= phase_count`). Backfills a `Committed` row for any file
    /// whose chain tip advanced past its highest recorded committed version
    /// (a crash between the inline commit and the row write). Returns the
    /// surviving files (with `resume_ordinal` set) and the rows it backfilled.
    async fn reconcile_resume(
        &self,
        prior_job_id: JobId,
        job_id: JobId,
        files: Vec<PhaseFile>,
        phase_count: u32,
    ) -> Result<(Vec<PhaseFile>, Vec<FilePhaseSummary>), VoomError> {
        let prior = self.workflow_summaries().file_phases_for_job(prior_job_id).await?;
        let mut survivors = Vec::with_capacity(files.len());
        let mut backfilled = Vec::new();
        for mut file in files {
            let rows: Vec<&FilePhaseSummary> =
                prior.iter().filter(|row| row.branch_id == file.branch_id).collect();
            let highest = rows.iter().max_by_key(|row| row.phase_ordinal);
            if let Some(top) = highest {
                if top.outcome == FilePhaseOutcome::Blocked {
                    continue; // terminal: aborted-for-file under the prior run
                }
            }
            let mut resume_ordinal = highest.map_or(0, |top| top.phase_ordinal + 1);

            // Consistency backfill: default the recorded tip to the input-set
            // starting version when no committed row is visible.
            let recorded_tip = rows
                .iter()
                .filter(|row| row.outcome == FilePhaseOutcome::Committed)
                .max_by_key(|row| row.phase_ordinal)
                .and_then(|row| row.produced_file_version_id)
                .unwrap_or(file.start_version_id);
            if file.version_id != recorded_tip {
                let tip = self
                    .identity
                    .get_file_version(file.version_id)
                    .await?
                    .ok_or_else(|| {
                        VoomError::Internal(format!(
                            "resume: chain tip {} vanished for {}",
                            file.version_id, file.branch_id
                        ))
                    })?;
                let produced = ProducedRefs::resolve(self, &tip, &file.snapshot).await?;
                let row = self
                    .write_file_row(
                        job_id,
                        resume_ordinal,
                        &file,
                        FilePhaseOutcome::Committed,
                        &[],
                        Some(produced),
                    )
                    .await?;
                backfilled.push(row);
                resume_ordinal += 1;
            }

            if resume_ordinal >= phase_count {
                continue; // complete: nothing left to run
            }
            file.resume_ordinal = resume_ordinal;
            survivors.push(file);
        }
        Ok((survivors, backfilled))
    }
```

- [ ] **Step 4: Run the reconciliation tests**

Run: `cargo test -p voom-control-plane reconcile_resume`
Expected: PASS (3 tests).

- [ ] **Step 5: Add the backfill + chained-resume unit tests**

```rust
#[tokio::test]
async fn reconcile_resume_backfills_committed_tip_without_row() {
    let (cp, _tmp) = cp().await;
    let prior = open_workflow_job(&cp).await;
    // Seed a file, then advance its chain tip by committing a second version
    // WITHOUT recording a phase row (crash between commit and row write).
    let v0 = seed_version(&cp, "/lib/d/movie.mkv", "hash-d0", reprobe_payload("h264")).await;
    record_file_phase(&cp, prior, 0, "movie", FilePhaseOutcome::Committed, Some(v0)).await;
    let v1 = advance_chain_tip(&cp, v0, "hash-d1", reprobe_payload("hevc")).await; // helper below

    let files = cp.initial_phase_files(&[(v0, "movie".to_owned())]).await.unwrap();
    let new_job = open_workflow_job(&cp).await;
    let (survivors, backfilled) = cp.reconcile_resume(prior, new_job, files, 4).await.unwrap();

    assert_eq!(backfilled.len(), 1, "the un-rowed phase-1 commit is backfilled");
    assert_eq!(backfilled[0].phase_ordinal, 1);
    assert_eq!(backfilled[0].outcome, FilePhaseOutcome::Committed);
    assert_eq!(backfilled[0].produced_file_version_id, Some(v1));
    assert!(backfilled[0].ticket_ids.is_empty());
    assert_eq!(survivors[0].resume_ordinal, 2, "resume past the backfilled phase");
}

#[tokio::test]
async fn reconcile_resume_zero_rows_backfills_advanced_tip() {
    let (cp, _tmp) = cp().await;
    let prior = open_workflow_job(&cp).await; // no rows at all under this job
    let v0 = seed_version(&cp, "/lib/e/movie.mkv", "hash-e0", reprobe_payload("h264")).await;
    let v1 = advance_chain_tip(&cp, v0, "hash-e1", reprobe_payload("hevc")).await;

    let files = cp.initial_phase_files(&[(v0, "movie".to_owned())]).await.unwrap();
    let new_job = open_workflow_job(&cp).await;
    let (survivors, backfilled) = cp.reconcile_resume(prior, new_job, files, 4).await.unwrap();
    assert_eq!(backfilled.len(), 1, "advanced-without-rows is backfilled at ordinal 0");
    assert_eq!(backfilled[0].phase_ordinal, 0);
    assert_eq!(survivors[0].resume_ordinal, 1);
}
```

Add the `advance_chain_tip` test helper. It mirrors the scan/commit path used by
`seed_version` but appends a new `FileVersion` to the same asset with a recorded
snapshot, so `active_version_with_snapshot` returns the new tip. Model it on the
body of `seed_version` (read it first) — create a `NewFileVersion` with
`ProducedBy::…` referencing `v0`, persist it via `self.identity`, then record a
snapshot with the given payload. Keep it small and deterministic (clock `T0`).

- [ ] **Step 6: Run, lint, commit**

```bash
cargo test -p voom-control-plane reconcile_resume
cargo clippy -p voom-control-plane --all-targets --all-features -- -D warnings
git add crates/voom-control-plane/src/workflow/coordinator.rs crates/voom-control-plane/src/workflow/coordinator_test.rs
git commit -m "feat(control-plane): reconcile per-file resume ordinal from prior rows

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: `resume_phase_barrier` entry points

**Files:**
- Modify: `crates/voom-control-plane/src/workflow/coordinator.rs`
- Test: `crates/voom-control-plane/src/workflow/coordinator_test.rs`

- [ ] **Step 1: Write the failing `NotFound` unit test**

```rust
#[tokio::test]
async fn resume_phase_barrier_rejects_unknown_prior_job() {
    let (cp, _tmp) = cp().await;
    let source = load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap();
    let created = cp.create_policy_document("container-metadata", &source).await.unwrap();
    let v = seed_version(&cp, "/lib/u/movie.mkv", "hash-u1", reprobe_payload("h264")).await;
    let s = latest_snapshot(&cp, v).await;
    let input = cp.create_policy_input_set(file_draft("unknown-prior", &[s])).await.unwrap();

    let err = cp
        .resume_phase_barrier(
            JobId(999_999),
            created.version.id,
            input.id,
            ComplianceExecutionOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.source.code(), "NOT_FOUND");
    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs").fetch_one(&cp.pool).await.unwrap();
    assert_eq!(jobs, 0, "no job opens when the prior job is unknown");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p voom-control-plane resume_phase_barrier_rejects_unknown_prior_job`
Expected: FAIL — `resume_phase_barrier` not found.

- [ ] **Step 3: Implement the entry points**

Add to the `impl ControlPlane` block, mirroring `run_phase_barrier` /
`run_phase_barrier_with_runtimes`:

```rust
    /// Resume a crashed or failed phase-barrier run (issue #165, spec §3/§8).
    /// Opens a **new** job and reconciles each file against `prior_job_id`'s
    /// per-`(file, phase)` rows (ADR-0009). Pass the **most-recently-failed**
    /// run's job id (the latest `CoordinatorError.partial.job_id`).
    ///
    /// # Errors
    /// Returns [`CoordinatorError`] when `prior_job_id` does not exist, durable
    /// inputs are missing, the policy declares an unsupported `on_error`, or a
    /// phase's tickets fail.
    pub async fn resume_phase_barrier(
        &self,
        prior_job_id: JobId,
        policy_version_id: PolicyVersionId,
        input_set_id: PolicyInputSetId,
        options: ComplianceExecutionOptions,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        let runtimes = self.policy_runtime_registry().await?;
        self.resume_phase_barrier_with_runtimes(
            prior_job_id, policy_version_id, input_set_id, options, runtimes,
        )
        .await
    }

    /// [`Self::resume_phase_barrier`] with an injected worker-runtime registry.
    ///
    /// # Errors
    /// See [`Self::resume_phase_barrier`].
    pub async fn resume_phase_barrier_with_runtimes(
        &self,
        prior_job_id: JobId,
        policy_version_id: PolicyVersionId,
        input_set_id: PolicyInputSetId,
        options: ComplianceExecutionOptions,
        runtimes: WorkerRuntimeRegistry,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        use voom_store::repo::jobs::JobRepo;
        if self.jobs().get(prior_job_id).await?.is_none() {
            return Err(VoomError::NotFound(format!(
                "resume: prior job {prior_job_id} does not exist"
            ))
            .into());
        }
        let inputs = self
            .load_current_accepted_policy_and_input(policy_version_id, input_set_id)
            .await?;
        let policy = self.compiled_policy_for_version(&inputs.version).await?;
        reject_unhandled_on_error(&policy)?;
        let active: Vec<FileVersionId> = inputs
            .input
            .media_snapshots
            .iter()
            .filter_map(|snapshot| match snapshot.target {
                PolicyInputTargetRef::FileVersion { id } => Some(id),
                _ => None,
            })
            .collect();
        let base_draft = input_set_to_draft(inputs.input);
        let context = PlanningContext {
            policy_version_id: Some(policy_version_id),
            policy_input_set_id: Some(input_set_id),
            ..PlanningContext::default()
        };
        let branch_ids = self.active_branch_ids(&active).await?;

        let now = self.clock().now();
        let job = self
            .open_job(NewJob {
                kind: WORKFLOW_JOB_KIND.to_owned(),
                priority: 0,
                created_at: now,
            })
            .await?;

        match self
            .resume_phase_barrier_in_job(
                job.id, prior_job_id, &policy, &context, base_draft, &branch_ids, options, runtimes,
            )
            .await
        {
            Ok(outcome) => Ok(outcome),
            Err(err) => {
                let _ = self.fail_job(job.id, err.source.to_string(), self.clock().now()).await;
                Err(err)
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "resume threads the prior job id through the same owned-job run state"
    )]
    async fn resume_phase_barrier_in_job(
        &self,
        job_id: JobId,
        prior_job_id: JobId,
        policy: &voom_policy::CompiledPolicy,
        context: &PlanningContext,
        base_draft: PolicyInputSetDraft,
        branch_ids: &[(FileVersionId, String)],
        options: ComplianceExecutionOptions,
        runtimes: WorkerRuntimeRegistry,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        if branch_ids.is_empty() || policy.phase_order.is_empty() {
            return Ok(self.finalize_zero_phase_run(job_id, Vec::new()).await?);
        }
        let files = self.initial_phase_files(branch_ids).await?;
        let phase_count = u32::try_from(policy.phase_order.len())
            .map_err(|e| VoomError::Internal(format!("phase count overflow: {e}")))?;
        let (files, backfilled) =
            self.reconcile_resume(prior_job_id, job_id, files, phase_count).await?;
        self.drive_phase_loop(job_id, policy, context, base_draft, files, backfilled, options, runtimes)
            .await
    }
```

- [ ] **Step 4: Run the NotFound test**

Run: `cargo test -p voom-control-plane resume_phase_barrier_rejects_unknown_prior_job`
Expected: PASS.

- [ ] **Step 5: Add an `on_error`-reject-on-resume unit test**

```rust
#[tokio::test]
async fn resume_phase_barrier_rejects_unhandled_on_error_before_opening_job() {
    // Build/accept a policy whose compiled phase carries on_error: continue,
    // then assert resume rejects it with POLICY_VALIDATION_ERROR and opens no job.
    // Reuse production-normalize-reduced.voom (policy `ln`, on_error: continue).
    let (cp, _tmp) = cp().await;
    let source = load_policy_fixture("fixtures/policies/production-normalize-reduced.voom").unwrap();
    let created = cp.create_policy_document("ln", &source).await.unwrap();
    let v = seed_version(&cp, "/lib/o/movie.mkv", "hash-o1", reprobe_payload("h264")).await;
    let s = latest_snapshot(&cp, v).await;
    let input = cp.create_policy_input_set(file_draft("on-error", &[s])).await.unwrap();
    let prior = open_workflow_job(&cp).await;

    let err = cp
        .resume_phase_barrier(prior, created.version.id, input.id, ComplianceExecutionOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.source.code(), "POLICY_VALIDATION_ERROR");
    let open_jobs: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE state = 'open'").fetch_one(&cp.pool).await.unwrap();
    assert_eq!(open_jobs, 0, "the on_error reject precedes open_job");
}
```

If `create_policy_document("ln", …)` does not auto-accept the version, accept it
the same way the existing coordinator tests do (check how `container-metadata`
tests obtain `created.version.id` as the current accepted version; replicate any
accept step). If the `ln` fixture cannot be loaded/accepted cleanly, fall back to
the in-memory `policy_with_on_error` helper from Task 1 driven through a
test-only seam — but prefer the fixture path so the compatibility claim is real.

- [ ] **Step 6: Run, lint, commit**

```bash
cargo test -p voom-control-plane resume_phase_barrier
cargo clippy -p voom-control-plane --all-targets --all-features -- -D warnings
git add crates/voom-control-plane/src/workflow/coordinator.rs crates/voom-control-plane/src/workflow/coordinator_test.rs
git commit -m "feat(control-plane): add resume_phase_barrier entry point

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: Integration acceptance test — partial-barrier failure + resume

**Files:**
- Modify: `crates/voom-control-plane/tests/phase_barrier_flow.rs`

- [ ] **Step 1: Write the acceptance integration test**

Model it on `phase_barrier_records_committed_sibling_when_a_file_fails` (read that
test in full first; reuse its helpers: `scan_one`, `two_file_input`,
`generate_h264_fixture`, `TranscodeWorkerLaunch`, `hide_stale_fake_ffprobe_sibling`,
`cargo_build_package`, `job_state`, `produced_from`). The new test:

1. Scan `Good.mp4` and `Doomed.mp4`; corrupt `Doomed.mp4` after scanning so its
   transcode fails (exactly as the existing test).
2. `run_phase_barrier(...)` → expect `Err`; capture `partial.job_id` (the failed
   job) and assert `Good` committed, `Doomed` produced no row.
3. **Restore** `Doomed.mp4` to a valid video so its re-planned transcode can
   commit on resume: re-generate the fixture AND re-scan is NOT needed — the
   source-facts check compares against the scanned `FileVersion`; instead write
   back the original bytes. Simplest deterministic approach: before corrupting in
   step 1, copy the generated `Doomed.mp4` to `Doomed.bak`; in this step copy it
   back. This restores byte-identical content so the source-facts check passes.
4. Capture `Good`'s committed version id from `partial.file_phases` (call it
   `good_v1`).
5. `resume_phase_barrier(partial.job_id, policy.version.id, input.id, options)` →
   expect `Ok(outcome)`.
6. Assert:
   - `outcome.job_id != partial.job_id` (a new job owns the resume).
   - `job_state(url, outcome.job_id) == "succeeded"`.
   - `Good` is **not** re-mutated: no `Good` file-phase row in `outcome.file_phases`
     has a `produced_file_version_id` newer than `good_v1`; equivalently, the
     `file_versions` count for Good's asset is unchanged from after step 2.
   - `Doomed` re-entered and committed: `outcome.file_phases` contains a
     `Committed` row with `branch_id == "Doomed"` and a `produced_file_version_id`,
     and the expected output artifact exists on disk.

Concretely (adapt names/paths to the existing helpers):

```rust
#[tokio::test]
async fn phase_barrier_resumes_failed_file_without_remutating_committed_sibling() {
    let _ffprobe_guard = hide_stale_fake_ffprobe_sibling();
    cargo_build_package("voom-ffprobe-worker").unwrap();
    cargo_build_package("voom-verify-artifact-worker").unwrap();
    cargo_build_package("voom-ffmpeg-worker").unwrap();

    let tmp = tempfile::TempDir::new().unwrap();
    let good = tmp.path().join("Good.mp4");
    let doomed = tmp.path().join("Doomed.mp4");
    let doomed_bak = tmp.path().join("Doomed.bak");
    generate_h264_fixture(&good);
    generate_h264_fixture(&doomed);
    std::fs::copy(&doomed, &doomed_bak).unwrap();

    let db = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = ControlPlane::open_with_pool(pool, std::sync::Arc::new(voom_core::SystemClock))
        .await
        .unwrap();

    let good_file = scan_one(&cp, &good).await;
    let doomed_file = scan_one(&cp, &doomed).await;
    std::fs::write(&doomed, b"not a video anymore").unwrap();

    let policy = cp
        .create_policy_document(
            "video-transcode-hevc",
            &load_policy_fixture("fixtures/policies/video-transcode-hevc.voom").unwrap(),
        )
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set(two_file_input(&[("good", good_file), ("doomed", doomed_file)]))
        .await
        .unwrap();
    let out_dir = tmp.path().join("out");
    let options = ComplianceExecutionOptions {
        transcode_staging_root: tmp.path().join("stage"),
        transcode_target_dir: out_dir.clone(),
        ..ComplianceExecutionOptions::default()
    };

    // First run: Doomed fails, Good commits, whole job fails.
    let mut worker = TranscodeWorkerLaunch::start(&cp).await.unwrap();
    let failed = cp
        .run_phase_barrier(policy.version.id, input.id, options.clone())
        .await
        .expect_err("the corrupt file must fail the run");
    worker.shutdown().unwrap();
    let partial = failed.partial.expect("Good must be recorded as a committed partial");
    let good_committed = partial
        .file_phases
        .iter()
        .find(|r| r.branch_id == "Good")
        .expect("Good committed");
    let good_v1 = good_committed.produced_file_version_id.expect("Good produced a version");

    // Restore Doomed to valid bytes so its re-planned transcode can commit.
    std::fs::copy(&doomed_bak, &doomed).unwrap();

    // Resume against the failed job.
    let mut worker = TranscodeWorkerLaunch::start(&cp).await.unwrap();
    let outcome = cp
        .resume_phase_barrier(partial.job_id, policy.version.id, input.id, options)
        .await
        .expect("resume succeeds once the doomed file is valid");
    worker.shutdown().unwrap();

    assert_ne!(outcome.job_id, partial.job_id, "resume opens a new job");
    assert_eq!(job_state(&url, outcome.job_id).await, "succeeded");

    // Good is not re-mutated: no resumed row produces a version past good_v1.
    let good_rows: Vec<_> = outcome
        .file_phases
        .iter()
        .filter(|r| r.branch_id == "Good")
        .collect();
    assert!(
        good_rows.iter().all(|r| match r.produced_file_version_id {
            Some(v) => v == good_v1,
            None => true,
        }),
        "Good must not advance past its phase-k artifact on resume: {good_rows:?}"
    );

    // Doomed re-entered and committed.
    let doomed_committed = outcome
        .file_phases
        .iter()
        .find(|r| r.branch_id == "Doomed" && r.outcome == FilePhaseOutcome::Committed)
        .expect("Doomed commits on resume");
    assert!(doomed_committed.produced_file_version_id.is_some());
    assert!(out_dir.join("Doomed.default-hevc.hevc.mkv").is_file());
}
```

The single-phase `video-transcode-hevc` policy means `Good` is fully complete
after the first run (phase 0 committed) and is dropped by reconciliation
(`resume_ordinal = 1 >= phase_count = 1`), so it produces **no** resumed row at
all — the assertion above (`all … == good_v1 || None`) holds vacuously, which is
the correct "not re-mutated" signal. Keep the assertion as written so it still
holds if the fixture later grows phases.

- [ ] **Step 2: Run the integration test**

Run: `cargo test -p voom-control-plane --test phase_barrier_flow phase_barrier_resumes_failed_file_without_remutating_committed_sibling -- --nocapture`
Expected: PASS. (Per the project test-layout rule, this launches the bundled
ffprobe/ffmpeg workers; ensure they build.)

- [ ] **Step 3: Run the full workspace test once to catch probe-path breaks**

Run: `cargo test --workspace`
Expected: PASS — the memory note on `project_dispatch_tests_real_media_probe`
warns that only the full workspace run exercises the real probe path. Re-run once
if a known-flaky test (`remote_lease_heartbeat_invalid_ttl…`,
`chaos_dispatch_timeout_maps_to_worker_timeout`) flakes.

- [ ] **Step 4: Commit**

```bash
git add crates/voom-control-plane/tests/phase_barrier_flow.rs
git commit -m "test(control-plane): resume re-enters failed file, spares committed sibling

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: Full guardrails

**Files:** none (verification only)

- [ ] **Step 1: Run the exact CI suite locally**

Run: `just ci`
Expected: `fmt-check`, `lint`, `check-test-layout`, `test`, `doc`, `deny`, `audit`
all pass. Fix every warning (zero-warnings policy); suppress only with
`#[expect(clippy::…, reason = "…")]` per the workspace lint config.

- [ ] **Step 2: Confirm the test-layout rule is satisfied**

Run: `just check-test-layout`
Expected: PASS — all new tests live in the sibling `coordinator_test.rs` /
`tests/phase_barrier_flow.rs`; no inline `#[cfg(test)] mod tests { … }` was added
to `src/`.

- [ ] **Step 3: Commit any fmt/lint fixups (if `just ci` changed files)**

```bash
git add -A
git commit -m "chore(control-plane): satisfy guardrails for resume

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review notes (spec coverage)

- Spec bullet 1 (resume per-(file,phase)): Tasks 2–4 (loop + reconciliation + entry point), Task 5 (acceptance).
- Spec bullet 2 (backfill on resume): Task 3 Step 5 unit tests + `reconcile_resume` backfill branch.
- Spec bullet 3 (mid-barrier finalization on job failure): already shipped (#162); Task 5's first run + the existing `phase_barrier_records_committed_sibling_when_a_file_fails` regression keep it green.
- Spec bullet 4 (`on_error` reject): Task 1 (helper + fresh wiring) + Task 4 Step 5 (resume wiring).
- Most-recent-job contract / chained resume (§3.3): Task 3 covers highest-recorded+1 and the single-commit backfill; the contract is a documented caller obligation, not enforced code.
- `prior_job_id` existence check (§3): Task 4 Steps 1–4.
