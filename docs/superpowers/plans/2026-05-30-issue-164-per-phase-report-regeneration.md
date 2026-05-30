# Per-Phase Compliance Report Regeneration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make each per-phase workflow-summary row record a compliance report regenerated against that phase's *post-commit refreshed facts* (the produced artifact), instead of the pre-dispatch report computed against the facts entering the phase.

**Architecture:** The phase loop in `run_phase_barrier_in_job` keeps generating the pre-dispatch report to *drive dispatch* (the bridge needs it). After `finalize_phase` commits artifacts and advances chain tips, a new read-only pass re-reads every file that entered the phase at its current chain tip, re-projects it, re-plans the *same* phase, and regenerates the report; that regenerated report is what gets written to `NewPhaseSummary.report`. No schema change, no new dispatch path. See `docs/adr/0008-per-phase-report-regenerated-against-refreshed-facts.md`.

**Tech Stack:** Rust, tokio, sqlx/SQLite, `voom-plan` (`plan_phase`, `generate_compliance_report`), `voom-control-plane` coordinator. Tests: `cargo test`, `insta` snapshots, bundled ffmpeg/ffprobe workers under `cargo test --workspace`.

---

## Background the implementer needs

- The bug: `crates/voom-control-plane/src/workflow/coordinator.rs:434` generates `report` from the plan that *drove* the phase (planned against each file's chain-tip snapshot **entering** the phase), then writes it to the phase row at `:472-477`. By ADR-0007 the entering snapshot is the *prior* phase's output, so phase *k*'s row records the wrong phase's facts (off by one); the final phase's produced artifact is never recorded.
- The fix re-reads facts **after** `finalize_phase` (coordinator.rs:460-462), which has already committed artifacts and advanced surviving files' chain tips. A file that committed re-reads at its new produced version + re-probe snapshot; a skipped/no-op/blocked file re-reads at its unchanged tip.
- The report set is **every file that entered the phase**, not the post-`finalize_phase` survivors (blocked files are dropped from the working set but must stay in the report so their planner diagnostic — the only durable record of a mid-chain block — survives; ADR-0008).
- Existing helpers already in `coordinator.rs` scope: `active_version_with_snapshot(&self.identity, asset_id) -> Result<Option<(FileVersion, MediaSnapshot)>, VoomError>` (returns chain tip + latest snapshot, or `None` if the asset has no live version/snapshot), `project_media_snapshot_input(ordinal: u32, &MediaSnapshot) -> MediaSnapshotInput`, `phase_draft(&PolicyInputSetDraft, &[PhaseFile]) -> PolicyInputSetDraft`. `PhaseReport { report_id: String, report: serde_json::Value }` and `PhaseFile { asset_id, version_id, snapshot, branch_id, ordinal }` are defined in the same file.
- `report_id` is deterministic (preimage strips `generated_at`/`plan_hash`/`plan_id`, `voom-plan/src/hash.rs:50-52`); the stored report JSON contains run-specific DB rowids (file-version/input-set/ticket ids), which the CLI goldens already redact.

## File structure

- Modify: `crates/voom-control-plane/src/workflow/coordinator.rs` — add `regenerate_phase_report`, capture the entered set, write the regenerated report to the phase row.
- Modify (test): `crates/voom-control-plane/tests/phase_barrier_flow.rs` — shift the chain test's report assertions to #164 semantics (each phase's report reflects *its own* produced artifact).
- Modify (test): `crates/voom-control-plane/src/workflow/coordinator_test.rs` — add a guard test that an all-blocked phase still records a report carrying the blocked file's diagnostic.
- Regenerate (test fixtures): `crates/voom-cli/tests/snapshots/*` — the `compliance execute` goldens whose per-phase report content shifts.

---

### Task 1: Shift the chain integration test to post-commit report semantics (failing test)

**Files:**
- Test: `crates/voom-control-plane/tests/phase_barrier_flow.rs:260-281` (inside `phase_barrier_chains_committed_artifact_into_the_next_phase`) and `:287-340` (`assert_reprobe_and_lineage_chain`)

- [ ] **Step 1: Rewrite the phase-0 report assertion to reflect phase 0's produced artifact**

In `phase_barrier_chains_committed_artifact_into_the_next_phase`, replace the existing phase-1 report block (current lines 260-278, which assert phase 1 targets phase 0's produced version) with a phase-**0** assertion. After the existing `let produced_version = phase0_commit.produced_file_version_id.expect(...)` (around line 255-257), add:

```rust
    // Issue #164: the report recorded for a phase reflects that phase's own
    // produced artifact, regenerated after commit + re-probe — not the facts
    // that entered the phase. Phase 0 transcoded h264 -> hevc and committed a new
    // FileVersion; its recorded report must therefore target that produced
    // version and observe the committed hevc codec. Before #164 this row held the
    // pre-dispatch report (target = the scanned h264 version, observed h264).
    let phase0 = outcome.phases[0]
        .report
        .as_ref()
        .expect("phase 0 has a report");
    assert_eq!(phase0.report["input"]["slug"], "phase-barrier-two-file");
    let phase0_check = &phase0.report["checks"][0];
    assert_eq!(
        phase0_check["target"]["id"].as_u64().unwrap(),
        produced_version.0,
        "phase 0's report must target the FileVersion phase 0 produced"
    );
    assert_eq!(
        phase0_check["observed_state"]["video_codec"], "hevc",
        "phase 0's report must observe its committed hevc artifact"
    );
```

- [ ] **Step 2: Update the phase-1 report assertion to phase 1's produced artifact**

In `assert_reprobe_and_lineage_chain` (the helper already resolves `phase1_commit` and its produced V2), after it computes `phase1_commit` / its produced version, add an assertion that phase 1's recorded report targets V2 (phase 1's own produced version) and observes hevc. Locate the `let phase1_commit = outcome ...` binding (around line 305) and the produced-V2 id it yields; add, using that id (named `produced_v2` below — match the helper's actual binding name):

```rust
    // Issue #164: phase 1's recorded report reflects phase 1's produced artifact
    // (V2), regenerated after its commit — not phase 0's output (V1).
    let phase1 = outcome.phases[1]
        .report
        .as_ref()
        .expect("phase 1 has a report");
    let phase1_check = &phase1.report["checks"][0];
    assert_eq!(
        phase1_check["target"]["id"].as_u64().unwrap(),
        produced_v2.0,
        "phase 1's report must target the FileVersion phase 1 produced"
    );
    assert_eq!(
        phase1_check["observed_state"]["video_codec"], "hevc",
        "phase 1's report must observe its committed hevc artifact"
    );
```

If the helper does not already bind the produced-V2 id, add `let produced_v2 = phase1_commit.produced_file_version_id.expect("phase 1 committed row records its produced version");` before the block.

- [ ] **Step 3: Run the test to verify it fails against current code**

Run: `cargo test -p voom-control-plane --test phase_barrier_flow phase_barrier_chains_committed_artifact_into_the_next_phase -- --nocapture`
Expected: FAIL — the phase-0 assertion fails because the current code records the pre-dispatch report (`target` is the scanned h264 version, `observed_state.video_codec` is `"h264"`, not `produced_version` / `"hevc"`).

> Note: this test launches the bundled ffmpeg/ffprobe workers; it builds them on first run and only runs under a full `cargo test` (not unit-only). Do not commit yet — the test is red by design.

---

### Task 2: Regenerate the phase report against post-commit refreshed facts

**Files:**
- Modify: `crates/voom-control-plane/src/workflow/coordinator.rs` — loop body at `:418-483` and a new private method near `finalize_phase` (`:652`).

- [ ] **Step 1: Add the `regenerate_phase_report` method**

Add this `impl ControlPlane` method adjacent to `finalize_phase` in `coordinator.rs` (it reuses `active_version_with_snapshot`, `project_media_snapshot_input`, and `PhaseReport`, all already in scope):

```rust
    /// Regenerate the per-phase compliance report against the phase's refreshed
    /// facts (ADR-0008): re-read every file that *entered* the phase at its
    /// current chain tip (committed files at their produced version + re-probe
    /// snapshot, others unchanged), re-project, re-plan the same phase, and
    /// generate the report. Read-only: no tickets, no version advance, no phase.
    async fn regenerate_phase_report(
        &self,
        policy: &voom_policy::CompiledPolicy,
        context: &PlanningContext,
        base_draft: &PolicyInputSetDraft,
        phase_name: &str,
        entered: &[(FileAssetId, u32)],
    ) -> Result<PhaseReport, VoomError> {
        let mut snapshots = Vec::with_capacity(entered.len());
        for (asset_id, ordinal) in entered {
            let (_tip, snapshot) = active_version_with_snapshot(&self.identity, *asset_id)
                .await?
                .ok_or_else(|| {
                    VoomError::Internal(format!(
                        "phase file asset {asset_id} lost its snapshot during report regeneration"
                    ))
                })?;
            snapshots.push(project_media_snapshot_input(*ordinal, &snapshot));
        }
        let mut draft = base_draft.clone();
        draft.media_snapshots = snapshots;
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
        Ok(PhaseReport {
            report_id: report.report_id.clone(),
            report: serde_json::to_value(&report)
                .map_err(|e| VoomError::Internal(format!("phase report encode: {e}")))?,
        })
    }
```

- [ ] **Step 2: Capture the entered set before `finalize_phase` and write the regenerated report**

In `run_phase_barrier_in_job`, in the loop body, capture the entered set immediately before the `finalize_phase` call (`:460`), and replace the inline `report: Some(PhaseReport { ... })` in the `upsert_phase_summary` call (`:472-477`) with the regenerated report.

Change this region (current `:457-482`):

```rust
            if run.is_some() {
                last_run = run;
            }
            let rows = self
                .finalize_phase(job_id, phase_ordinal, &mut files, &dispositions)
                .await?;
            let outcome = phase_outcome(&rows.iter().map(|row| row.outcome).collect::<Vec<_>>());
            file_phases.extend(rows);
            let phase_row = self
                .workflow_summaries()
                .upsert_phase_summary(
                    NewPhaseSummary {
                        job_id,
                        phase_ordinal,
                        phase_name: phase_name.clone(),
                        report: Some(PhaseReport {
                            report_id: report.report_id.clone(),
                            report: serde_json::to_value(&report).map_err(|e| {
                                VoomError::Internal(format!("phase report encode: {e}"))
                            })?,
                        }),
                        outcome,
                    },
                    self.clock().now(),
                )
                .await?;
            phases.push(phase_row);
```

to:

```rust
            if run.is_some() {
                last_run = run;
            }
            let entered: Vec<(FileAssetId, u32)> =
                files.iter().map(|file| (file.asset_id, file.ordinal)).collect();
            let rows = self
                .finalize_phase(job_id, phase_ordinal, &mut files, &dispositions)
                .await?;
            let outcome = phase_outcome(&rows.iter().map(|row| row.outcome).collect::<Vec<_>>());
            file_phases.extend(rows);
            let report = self
                .regenerate_phase_report(policy, context, &base_draft, phase_name, &entered)
                .await?;
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
```

Note: the pre-dispatch `let report = voom_plan::generate_compliance_report(&plan)...` at `:434` and its use in `dispatch_phase(..., &report, ...)` at `:439` stay unchanged — that report still drives dispatch. The new `let report = self.regenerate_phase_report(...)` shadows it after dispatch, which is intentional (the pre-dispatch value is no longer needed past `dispatch_phase`).

- [ ] **Step 3: Build and lint**

Run: `just fmt && cargo build -p voom-control-plane && just lint`
Expected: clean — no warnings, no clippy errors. (`regenerate_phase_report` has 5 params, under the positional limit; `policy`/`context`/`base_draft` are borrows already held by the loop.)

- [ ] **Step 4: Run the chain test to verify it now passes**

Run: `cargo test -p voom-control-plane --test phase_barrier_flow phase_barrier_chains_committed_artifact_into_the_next_phase`
Expected: PASS — both phase-0 and phase-1 report assertions now hold.

- [ ] **Step 5: Run the full control-plane test crate**

Run: `cargo test -p voom-control-plane`
Expected: PASS. If `crates/voom-control-plane/src/workflow/coordinator_test.rs` or `compliance_execute`/`video_transcode_flow` assert per-phase report content, update those assertions to the produced-artifact facts (the report now reflects the committed artifact, not the entering snapshot). Re-run until green.

- [ ] **Step 6: Regenerate the CLI golden snapshots whose per-phase report shifted**

Run: `cargo test -p voom-cli` first to see which `compliance execute` insta snapshots changed.
Then: `cargo insta review` and accept only the diffs where the per-phase `report` block now shows the produced artifact's facts (codec/container/target version) and reject anything unexpected. Re-run `cargo test -p voom-cli` until green.
Expected: the accepted snapshot diffs are confined to per-phase `report` content; redacted ids (produced_*/reprobe/ticket_ids) are unchanged.

- [ ] **Step 7: Commit**

```bash
git add crates/voom-control-plane/src/workflow/coordinator.rs \
        crates/voom-control-plane/tests/phase_barrier_flow.rs \
        crates/voom-cli/tests/snapshots
git commit -m "feat(control-plane): regenerate per-phase report against refreshed facts

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

(Also stage `crates/voom-control-plane/src/workflow/coordinator_test.rs` if Step 5 required edits there.)

---

### Task 3: Guard test — an all-blocked phase still records a report with the blocked diagnostic

This pins ADR-0008's safety property (the rejected survivors-only/`None` alternative would have dropped it). The file blocks (snapshot without a container), nothing commits; the regenerated report must still be `Some` and carry the blocked file's planner diagnostic.

**Files:**
- Test: `crates/voom-control-plane/src/workflow/coordinator_test.rs` — extend `run_phase_barrier_drops_unplannable_file_as_blocked` (`:289-333`).

- [ ] **Step 1: Add report assertions to the blocked-file handler test**

After the existing assertions in `run_phase_barrier_drops_unplannable_file_as_blocked` (after the `all(|row| row.outcome != FilePhaseOutcome::Committed)` block, around line 333), add:

```rust
    // Issue #164 / ADR-0008: even an all-blocked phase (nothing committed) must
    // still record a report, and that report must carry the blocked file's
    // diagnostic — the per-(file, phase) row has no diagnostic field, so the
    // report is the only durable record of *why* the file blocked. Recording
    // `None` here (the rejected survivors-only design) would lose it.
    let phase = outcome.phases.first().expect("a phase row was recorded");
    let report = phase.report.as_ref().expect("blocked phase still records a report");
    assert!(
        !report.report["diagnostics"].as_array().unwrap().is_empty(),
        "blocked phase report must carry the planner diagnostic, got {:?}",
        report.report["diagnostics"]
    );
```

- [ ] **Step 2: Run the guard test**

Run: `cargo test -p voom-control-plane run_phase_barrier_drops_unplannable_file_as_blocked`
Expected: PASS. (The all-blocked phase commits nothing, so the regenerated report equals the pre-dispatch report — `Some` with the planner's "snapshot container is unknown" diagnostic.)

> If the `diagnostics` array is empty for this fixture, the block is surfaced as a `Blocked` *check* rather than a top-level diagnostic. In that case assert on the check instead: `assert_eq!(report.report["checks"][0]["status"], "blocked");`. Inspect the actual report JSON with `-- --nocapture` and a temporary `eprintln!` if needed, then keep whichever assertion matches the real shape (do not assert both).

- [ ] **Step 3: Commit**

```bash
git add crates/voom-control-plane/src/workflow/coordinator_test.rs
git commit -m "test(control-plane): all-blocked phase still records report diagnostic

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Full-suite verification

**Files:** none (verification only).

- [ ] **Step 1: Run the full workspace test suite**

Run: `cargo test --workspace --all-features`
Expected: PASS — including the multi-phase ffprobe-on-staged-output integration tests (`phase_barrier_flow`, `video_transcode_flow`, etc.). Fix any remaining report-content assertions and re-run.

- [ ] **Step 2: Run the exact CI suite locally**

Run: `just ci`
Expected: PASS — `fmt-check`, `lint` (clippy `-D warnings`), `check-test-layout`, `test`, `doc`, `deny`, `audit` all green.

- [ ] **Step 3: Commit any fixups**

If Steps 1-2 required changes, commit them with an imperative subject and the `Co-Authored-By` trailer. If nothing changed, skip.

---

## Self-review

- **Spec coverage:** #164 scope item 1 (regenerate after commit/re-probe) → Task 2 Steps 1-2. Scope item 2 (record `report_id` + JSON in the per-phase row) → Task 2 Step 2 writes `PhaseReport { report_id, report }` to `NewPhaseSummary.report`. Scope item 3 (deterministic identity unchanged) → no change to `compliance_report.rs`; `regenerate_phase_report` calls the unchanged `generate_compliance_report`. Acceptance (per-phase regeneration recorded; deterministic identity preserved) → Task 1 (produced-artifact assertions), Task 3 (blocked diagnostic preserved), Task 4 (`just ci`). ADR-0008 blocked-file coverage → Task 1 (full entered set), Task 3 (all-blocked guard).
- **Placeholder scan:** every code step shows the literal code; the one conditional (Task 3 Step 2 fallback) names the exact alternative assertion and how to decide. No TBD/TODO.
- **Type consistency:** `regenerate_phase_report(&self, policy: &CompiledPolicy, context: &PlanningContext, base_draft: &PolicyInputSetDraft, phase_name: &str, entered: &[(FileAssetId, u32)]) -> Result<PhaseReport, VoomError>` is referenced identically in Task 2 Step 2. `PhaseReport { report_id, report }`, `PhaseFile.asset_id`/`.ordinal`, `active_version_with_snapshot` return shape, and `FilePhaseOutcome` all match the names verified in `coordinator.rs`/`workflow_summaries.rs`.
