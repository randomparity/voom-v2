# Compliance Run/Report CLI Surface (#166) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a read-only `compliance report --job-id` post-run mode that returns a completed job's durable per-phase chain, and prove + snapshot a multi-phase `compliance execute` flow end-to-end through the CLI.

**Architecture:** A new control-plane read method `read_compliance_run_report(job_id)` reads the durable `workflow_summaries` rows (`get_summary` / `phases_for_job` / `file_phases_for_job`, already shipped in #169) and assembles a `ComplianceRunReportData` view reusing the existing `WorkflowSummaryView` / `PhaseSummaryView` / `FilePhaseSummaryView` DTOs. The CLI `report` command gains an optional `--job-id` that, via clap `conflicts_with`/`requires` plus a handler validation arm, switches between the existing regenerate-preview path and the new durable-read path. A proof-of-commit gate selects a two-phase fake-worker policy before the golden flow is snapshotted.

**Tech Stack:** Rust, clap, sqlx/SQLite, `insta` golden snapshots, `voom-fakes` workers, the bundled fake ffprobe. Guardrail: `just ci` (fmt-check, clippy `-D warnings`, check-test-layout, test, doc, deny, audit).

**Spec:** `docs/superpowers/specs/2026-05-30-issue-166-compliance-run-report-cli.md`
**ADR:** `docs/adr/0010-compliance-report-job-read-mode.md`

**Repo conventions (AGENTS.md) that bind every task:**
- Unit tests live in a sibling `<source>_test.rs` linked via `#[cfg(test)] #[path = "<source>_test.rs"] mod tests;` — never an inline `mod tests {}` in `src/`. `just check-test-layout` enforces this.
- Suppress lints with `#[expect(clippy::…, reason = "…")]`, never `#[allow]`.
- `_in_tx` re-reads go through the tx handle; this plan adds no `_in_tx` methods (the read method is read-only, no transaction).
- Run a single test: `cargo test -p <crate> <name>`. Review insta with `cargo insta review`.
- Commit messages end with the `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>` trailer.

---

## File Structure

- `crates/voom-control-plane/src/cases/compliance.rs` — **modify**: add `ComplianceRunReportData` struct, `latest_phase_index` field logic, and `ControlPlane::read_compliance_run_report`. (Currently 749 lines; the additions are ~70 lines. If it crosses the 100-line-function / file-clarity bar, keep the new method small and free functions at file scope.)
- `crates/voom-control-plane/src/cases/compliance_test.rs` — **modify**: handler unit tests for the read method (ordered chain, latest index, NotFound, zero-phase).
- `crates/voom-cli/src/cli.rs` — **modify**: change `ComplianceCommand::Report` to carry three `Option<u64>` args with clap `conflicts_with`/`requires`.
- `crates/voom-cli/src/commands/compliance.rs` — **modify**: add the run-report view DTO mapping + `report` handler dispatch (preview vs job-id vs BAD_ARGS), and a `ComplianceRunReportData` CLI-facing serialize type.
- `crates/voom-cli/src/commands/compliance_test.rs` — **modify**: unit-test the argument-combination validation helper.
- `crates/voom-cli/src/main.rs` — **modify**: update the `ComplianceCommand::Report` destructure in `dispatch_compliance`.
- `crates/voom-policy/fixtures/policies/remux-then-audio.voom` — **create** (name/body confirmed by Task 1 gate): the two-phase mutation fixture.
- `crates/voom-cli/tests/compliance_envelope.rs` — **modify**: the proof-of-commit gate test, the multi-phase golden flow, the `report --job-id` unknown-job test, and the BAD_ARGS argument-combination test.
- `crates/voom-cli/tests/snapshots/` — **create**: new `.snap` files (via `cargo insta`).

---

## Task 1: Proof-of-commit gate — prove a two-phase fake policy commits twice through the CLI

**This task gates all golden work. No snapshot is written until its test is green.** It establishes the concrete (fixture, workers, prober, seeding snapshot) tuple the rest of the plan uses. Per the spec §5, the leading candidate is `remux → transcode audio to opus` with `fake-remuxer` + `fake-transcoder` + the fake ffprobe; the fallback is the real-ffmpeg stack (field assertions, not insta).

**Files:**
- Create: `crates/voom-policy/fixtures/policies/remux-then-audio.voom`
- Modify: `crates/voom-cli/tests/compliance_envelope.rs`

- [ ] **Step 1: Write the fixture**

`crates/voom-policy/fixtures/policies/remux-then-audio.voom`:

```text
policy "remux-then-audio" {
  phase remux {
    container mkv
  }
  phase audio {
    depends_on: [remux]
    transcode audio to opus
  }
}
```

- [ ] **Step 2: Confirm the fixture parses + resolves (no run yet)**

Add a temporary `#[test]` to `compliance_envelope.rs` (it will be deleted in Step 6 once the gate test exists):

```rust
#[test]
fn gate_fixture_parses() {
    let source = voom_policy::load_policy_fixture("fixtures/policies/remux-then-audio.voom")
        .expect("fixture must parse");
    assert!(source.contains("phase remux"));
    assert!(source.contains("phase audio"));
}
```

Run: `cargo test -p voom-cli --test compliance_envelope gate_fixture_parses`
Expected: PASS. If `transcode audio to opus` (no `where`) does not parse, or the
compiler rejects it, fall back per spec §5 — add a stream-index/kind selector that
matches the fixed `basic-mp4.json` audio stream (index 1, kind audio), e.g.
`transcode audio to opus` with whatever minimal selector the grammar requires, and
re-run. Record the working form in the fixture.

- [ ] **Step 3: Write the gate test (plain assertions, NOT insta)**

Add to `compliance_envelope.rs`. This reuses the seeding + worker-launch helpers
already in the file (`seed_scanned_remux`, `RemuxProviderLaunch`,
`TestWorkerLaunch`, `compliance_execute_command_with_dirs`, `fake_ffprobe_bin`).
The seed must use the `remux-then-audio` policy and a snapshot carrying a container
≠ mkv plus an audio stream, so both phases plan. Add a sibling
`seed_scanned_remux_then_audio` modeled on `seed_scanned_remux` but using the new
policy and an audio-bearing snapshot (the `seed_scanned_audio` snapshot payload is
a good template — it has eng-tagged opus audio + a non-mkv source container set via
`create_policy_input_set_from_scan(container: "mp4")`).

```rust
#[tokio::test]
async fn gate_remux_then_audio_commits_both_phases() {
    let seeded = seed_scanned_remux_then_audio().await;
    let mut remux = RemuxProviderLaunch::start(&seeded.url).await.unwrap();
    let mut audio = AudioProviderLaunch::start(&seeded.url).await.unwrap();

    let root = seeded.dir.path().canonicalize().unwrap();
    let staging_root = root.join("stage");
    let output_dir = root.join("out");
    let ffprobe_bin = fake_ffprobe_bin(&root);
    let output = compliance_execute_command_with_dirs(
        &seeded.url,
        seeded.version_id,
        seeded.input_id,
        &staging_root,
        &output_dir,
        &ffprobe_bin,
    );
    remux.shutdown().unwrap();
    audio.shutdown().unwrap();

    let json = envelope(output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "both phases must commit; stdout={} stderr={}",
        serde_json::to_string_pretty(&json).unwrap(),
        String::from_utf8_lossy(&output.stderr)
    );
    let phases = json["data"]["phases"].as_array().unwrap();
    assert_eq!(phases.len(), 2, "two phases recorded");
    assert_eq!(phases[0]["phase_name"], "remux");
    assert_eq!(phases[0]["outcome"], "completed");
    assert_eq!(phases[1]["phase_name"], "audio");
    assert_eq!(phases[1]["outcome"], "completed");
    let file_phases = json["data"]["file_phases"].as_array().unwrap();
    assert_eq!(file_phases.len(), 2, "one committed row per phase");
    assert!(file_phases.iter().all(|fp| fp["outcome"] == "committed"));
}
```

Add an `AudioProviderLaunch` helper next to `RemuxProviderLaunch` (same shape,
launching `fake-transcoder` with capability `transcode_audio`):

```rust
struct AudioProviderLaunch {
    inner: TestWorkerLaunch,
}

impl AudioProviderLaunch {
    async fn start(url: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let pool = voom_store::connect(url).await?;
        let cp = voom_control_plane::ControlPlane::open_with_pool(
            pool,
            std::sync::Arc::new(voom_core::SystemClock),
        )
        .await?;
        Ok(Self {
            inner: TestWorkerLaunch::start(
                &cp,
                TestWorkerConfig::synthetic(
                    cargo_bin_or_build("voom-fakes", "fake-transcoder")?,
                    "cli-compliance-audio-gate",
                    "cli-compliance-audio-gate-secret",
                    "transcode_audio",
                ),
            )
            .await?,
        })
    }

    fn shutdown(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.inner.shutdown()
    }
}
```

- [ ] **Step 4: Run the gate test**

Run: `cargo test -p voom-cli --test compliance_envelope gate_remux_then_audio_commits_both_phases -- --nocapture`
Expected: PASS (two committed phases).

**If it FAILS** (e.g. phase 2 records `skipped` or the job exits 2): diagnose with
`--nocapture` output. Common causes and fixes, in order:
- Phase 2 `skipped`: the audio re-probe already satisfies `opus` — confirm the fake
  ffprobe reports `aac` (it does, `basic-mp4.json`); if it reports opus, change the
  target codec so it diverges.
- Phase 2 `blocked`: the audio selector matched nothing against the re-probe stream
  — adjust the fixture selector to match `basic-mp4.json`'s audio stream.
- Job exits 2 at phase 1: the remux commit path failed — verify the seed container ≠
  mkv so remux is planned, and that `fake-remuxer` is launched.
- If no fake pairing reaches two commits after these fixes, **switch to the
  real-ffmpeg fallback** (spec §5 candidate 2): model the gate + golden on
  `crates/voom-control-plane/tests/phase_barrier_flow.rs` (real `voom-ffmpeg-worker`,
  `generate_h264_fixture`, `hide_stale_fake_ffprobe_sibling`), and make the
  multi-phase coverage **field assertions, not insta**. Note this switch in the PR.

- [ ] **Step 5: Delete the temporary `gate_fixture_parses` test from Step 2**

It is subsumed by the gate test. Remove it.

- [ ] **Step 6: Commit**

```bash
git add crates/voom-policy/fixtures/policies/remux-then-audio.voom crates/voom-cli/tests/compliance_envelope.rs
git commit -m "test(cli): prove remux->audio two-phase policy commits through CLI (#166)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Control-plane `read_compliance_run_report` — failing test first

**Files:**
- Modify: `crates/voom-control-plane/src/cases/compliance.rs`
- Test: `crates/voom-control-plane/src/cases/compliance_test.rs`

- [ ] **Step 1: Write the failing test for the not-found case**

Add to `compliance_test.rs` (uses the existing `cp()` test helper that opens a
control plane over a temp SQLite — confirm its name in the file; the coordinator
tests use `cp().await` returning `(ControlPlane, TempDir)`):

```rust
#[tokio::test]
async fn read_compliance_run_report_unknown_job_is_not_found() {
    let (cp, _tmp) = cp().await;
    let err = cp
        .read_compliance_run_report(voom_core::JobId(999_999))
        .await
        .expect_err("unknown job must be NotFound");
    assert!(matches!(err, voom_core::VoomError::NotFound(_)), "got {err:?}");
}
```

- [ ] **Step 2: Run it to verify it fails to compile (method missing)**

Run: `cargo test -p voom-control-plane read_compliance_run_report_unknown_job_is_not_found`
Expected: FAIL — `no method named read_compliance_run_report`.

- [ ] **Step 3: Add the DTO and method (minimal)**

In `compliance.rs`, add after the `FilePhaseSummaryView` impl block:

```rust
/// Read-only view of a completed run's durable workflow summary: the job-grain
/// counters, the ordered per-phase chain (each carrying its folded report), the
/// per-`(file, phase)` rows, and an index into `phases` of the latest (highest
/// ordinal) phase. An index, not a duplicated row, so the latest report has one
/// wire representation (ADR-0010).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ComplianceRunReportData {
    pub summary: WorkflowSummaryView,
    pub phases: Vec<PhaseSummaryView>,
    pub file_phases: Vec<FilePhaseSummaryView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_phase_index: Option<usize>,
}
```

Add the method inside the existing `impl ControlPlane { … }` block:

```rust
    /// Read a completed phase-barrier run's durable summary by job id.
    ///
    /// Read-only: opens no transaction, submits no tickets, and regenerates no
    /// report — it returns the reports the run already folded into the rows
    /// (ADR-0008/0010). Returns `NotFound` when no summary row exists for the
    /// job.
    ///
    /// # Errors
    /// `NotFound` when the job has no workflow summary; database errors from the
    /// repo reads otherwise.
    pub async fn read_compliance_run_report(
        &self,
        job_id: voom_core::JobId,
    ) -> Result<ComplianceRunReportData, VoomError> {
        let repo = self.workflow_summaries();
        let summary = repo.get_summary(job_id).await?.ok_or_else(|| {
            VoomError::NotFound(format!("workflow summary for job {job_id} not found"))
        })?;
        let phases: Vec<PhaseSummaryView> = repo
            .phases_for_job(job_id)
            .await?
            .iter()
            .map(PhaseSummaryView::from)
            .collect();
        let file_phases = repo
            .file_phases_for_job(job_id)
            .await?
            .iter()
            .map(FilePhaseSummaryView::from)
            .collect();
        let latest_phase_index = phases.len().checked_sub(1);
        Ok(ComplianceRunReportData {
            summary: WorkflowSummaryView::from(&summary),
            phases,
            file_phases,
            latest_phase_index,
        })
    }
```

Note: `WorkflowSummaryRepo` is already imported at the top of `coordinator.rs`; in
`compliance.rs` add `use voom_store::repo::workflow_summaries::WorkflowSummaryRepo;`
to the existing `use` block if the trait methods are not in scope (the
`get_summary`/`phases_for_job` calls require the trait in scope). Verify with the
compiler.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p voom-control-plane read_compliance_run_report_unknown_job_is_not_found`
Expected: PASS.

- [ ] **Step 5: Run clippy on the crate**

Run: `cargo clippy -p voom-control-plane --all-targets --all-features -- -D warnings`
Expected: clean. If `JobId`'s `Display` is missing for the format string, use
`job_id.0` instead.

- [ ] **Step 6: Commit**

```bash
git add crates/voom-control-plane/src/cases/compliance.rs crates/voom-control-plane/src/cases/compliance_test.rs
git commit -m "feat(control-plane): add read_compliance_run_report durable read (#166)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Control-plane read method — chain ordering, latest index, and zero-phase tests

**Files:**
- Test: `crates/voom-control-plane/src/cases/compliance_test.rs`

- [ ] **Step 1: Write the zero-phase test (a run with no file targets)**

The shipped coordinator test `run_phase_barrier_with_no_file_targets_succeeds_with_zero_phase_summary`
shows the pattern: a compliant-baseline input set whose targets are synthetic
yields a job with a summary row but zero phase rows. Reuse it:

```rust
#[tokio::test]
async fn read_compliance_run_report_zero_phase_job_is_ok_and_empty() {
    let (cp, _tmp) = cp().await;
    let source = load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap();
    let created = cp
        .create_policy_document("container-metadata", &source)
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set(load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap())
        .await
        .unwrap();
    let outcome = cp
        .run_phase_barrier(created.version.id, input.id, ComplianceExecutionOptions::default())
        .await
        .unwrap();

    let view = cp.read_compliance_run_report(outcome.job_id).await.unwrap();
    assert_eq!(view.summary.job_id, outcome.job_id.0);
    assert!(view.phases.is_empty(), "no file targets => no phase rows");
    assert!(view.file_phases.is_empty());
    assert_eq!(view.latest_phase_index, None);
}
```

- [ ] **Step 2: Run it to verify it passes**

Run: `cargo test -p voom-control-plane read_compliance_run_report_zero_phase_job_is_ok_and_empty`
Expected: PASS (the read method is already implemented; this test pins the
zero-phase contract). If imports are missing, mirror the `use` lines the sibling
coordinator-style tests in `compliance_test.rs` already use (`load_policy_fixture`,
`load_fixture`, `FixtureName`, `ComplianceExecutionOptions`).

- [ ] **Step 3: Write the multi-phase ordering + latest-index test**

This needs a job with ≥1 phase row. Rather than launch real workers in a
control-plane unit test, write the rows directly through the repo (the pattern the
shipped `control_plane_persists_workflow_summary_over_shared_pool` test uses:
`open_job` then `insert_summary` / `upsert_phase_summary`). Insert a summary plus
two phase rows out of ordinal order to prove the method returns them ascending and
picks the last as latest:

```rust
#[tokio::test]
async fn read_compliance_run_report_orders_phases_and_points_at_latest() {
    use voom_store::repo::workflow_summaries::{
        NewPhaseSummary, NewWorkflowSummary, PhaseOutcome, PhaseReport, WorkflowSummaryRepo,
    };
    let (cp, _tmp) = cp().await;
    let job = cp
        .open_job(voom_store::repo::jobs::NewJob {
            kind: "synthetic.workflow".to_owned(),
            priority: 0,
            created_at: T0,
        })
        .await
        .unwrap();
    cp.workflow_summaries()
        .insert_summary(
            NewWorkflowSummary {
                job_id: job.id,
                branch_count: 1,
                ticket_count: 2,
                dispatch_count: 2,
                retry_count: 0,
                failure_count: 0,
                peak_active_workflow_leases: 1,
                elapsed: std::time::Duration::from_millis(1),
                per_operation: serde_json::json!({}),
            },
            T0,
        )
        .await
        .unwrap();
    // Insert ordinal 1 before ordinal 0 to prove the read sorts ascending.
    for (ordinal, name) in [(1u32, "audio"), (0u32, "remux")] {
        cp.workflow_summaries()
            .upsert_phase_summary(
                NewPhaseSummary {
                    job_id: job.id,
                    phase_ordinal: ordinal,
                    phase_name: name.to_owned(),
                    outcome: PhaseOutcome::Completed,
                    report: Some(PhaseReport {
                        report_id: format!("report_{name}"),
                        report: serde_json::json!({"report_id": format!("report_{name}")}),
                    }),
                },
                T0,
            )
            .await
            .unwrap();
    }

    let view = cp.read_compliance_run_report(job.id).await.unwrap();
    assert_eq!(view.phases.len(), 2);
    assert_eq!(view.phases[0].phase_ordinal, 0);
    assert_eq!(view.phases[0].phase_name, "remux");
    assert_eq!(view.phases[1].phase_ordinal, 1);
    assert_eq!(view.phases[1].phase_name, "audio");
    assert_eq!(view.latest_phase_index, Some(1));
    assert_eq!(
        view.phases[view.latest_phase_index.unwrap()].report_id.as_deref(),
        Some("report_audio"),
        "latest index points at the highest-ordinal phase's report"
    );
}
```

Note: confirm the exact field names on `NewPhaseSummary` / `NewWorkflowSummary`
against `workflow_summaries.rs` (this plan used `phase_ordinal`, `phase_name`,
`outcome`, `report`, and the `WorkflowSummary` counters verified in the spec). `T0`
is the test-module time constant the sibling tests use; if absent, use
`time::OffsetDateTime::UNIX_EPOCH`. Confirm `open_job` and `NewJob`'s field set
against the shipped test.

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p voom-control-plane read_compliance_run_report_orders_phases_and_points_at_latest`
Expected: PASS.

- [ ] **Step 5: Run the whole crate's compliance tests + clippy**

Run: `cargo test -p voom-control-plane cases::compliance`
Run: `cargo clippy -p voom-control-plane --all-targets --all-features -- -D warnings`
Expected: PASS / clean.

- [ ] **Step 6: Commit**

```bash
git add crates/voom-control-plane/src/cases/compliance_test.rs
git commit -m "test(control-plane): pin run-report ordering, latest index, zero-phase (#166)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: CLI `report` argument model — three optional args with clap guards

**Files:**
- Modify: `crates/voom-cli/src/cli.rs:113-139` (the `ComplianceCommand::Report` variant)

- [ ] **Step 1: Change the `Report` variant to three optional args with guards**

Replace the existing `Report { policy_version_id: u64, input_set_id: u64 }` arm:

```rust
    /// Generate a compliance report from durable policy and input rows
    /// (preview: `--policy-version-id` + `--input-set-id`), or read a completed
    /// run's durable per-phase chain (`--job-id`). Exactly one mode.
    Report {
        #[arg(long, requires = "input_set_id", conflicts_with = "job_id")]
        policy_version_id: Option<u64>,
        #[arg(long, requires = "policy_version_id", conflicts_with = "job_id")]
        input_set_id: Option<u64>,
        #[arg(long, conflicts_with_all = ["policy_version_id", "input_set_id"])]
        job_id: Option<u64>,
    },
```

This makes clap reject: job-id together with either preview arg, and one preview
arg without its partner. The "none at all" case is not expressible in clap here
(both groups are optional), so the handler validates it (Task 5).

- [ ] **Step 2: Verify the crate still compiles (other arms reference the old shape)**

Run: `cargo build -p voom-cli 2>&1 | head -30`
Expected: FAIL — `main.rs` and `commands/compliance.rs` still destructure the old
`Report { policy_version_id, input_set_id }`. That is fixed in Tasks 5–6. (This
step documents the expected breakage so the engineer is not surprised.)

- [ ] **Step 3: Do not commit yet** — the crate does not build. Proceed to Task 5;
commit at the end of Task 6 when the crate builds and tests pass.

---

## Task 5: CLI `report` handler — dispatch preview vs job-id vs BAD_ARGS

**Files:**
- Modify: `crates/voom-cli/src/commands/compliance.rs`
- Modify: `crates/voom-cli/src/main.rs:346-349` (the `Report` destructure)
- Test: `crates/voom-cli/src/commands/compliance_test.rs`

- [ ] **Step 1: Add the run-report CLI DTO and the new handler to `compliance.rs`**

At the top, extend the type aliases:

```rust
pub type RunReportData = voom_control_plane::cases::compliance::ComplianceRunReportData;
```

Add the validation helper + handlers. Replace the existing `report` fn with a
dispatcher, and add `report_job` / keep the preview body as `report_preview`:

```rust
/// Parsed `compliance report` mode after argument validation.
enum ReportMode {
    Preview { policy_version_id: u64, input_set_id: u64 },
    Run { job_id: u64 },
}

/// Validate the `report` argument combination. clap already rejects job-id with a
/// preview arg and a lone preview arg; this catches the "none supplied" case clap
/// cannot express, returning a `BAD_ARGS` message.
fn parse_report_mode(
    policy_version_id: Option<u64>,
    input_set_id: Option<u64>,
    job_id: Option<u64>,
) -> Result<ReportMode, String> {
    match (policy_version_id, input_set_id, job_id) {
        (Some(policy_version_id), Some(input_set_id), None) => Ok(ReportMode::Preview {
            policy_version_id,
            input_set_id,
        }),
        (None, None, Some(job_id)) => Ok(ReportMode::Run { job_id }),
        _ => Err("compliance report requires either --policy-version-id with \
                  --input-set-id (preview) or --job-id (post-run read)"
            .to_owned()),
    }
}

pub async fn report(
    database_url: &str,
    local: Local,
    policy_version_id: Option<u64>,
    input_set_id: Option<u64>,
    job_id: Option<u64>,
) -> io::Result<i32> {
    let mode = match parse_report_mode(policy_version_id, input_set_id, job_id) {
        Ok(mode) => mode,
        Err(message) => {
            emit_err(
                "compliance",
                voom_core::ErrorCode::BadArgs.as_str(),
                message,
                None,
                Some(local),
            )?;
            return Ok(1);
        }
    };
    match mode {
        ReportMode::Preview {
            policy_version_id,
            input_set_id,
        } => report_preview(database_url, local, policy_version_id, input_set_id).await,
        ReportMode::Run { job_id } => report_run(database_url, local, job_id).await,
    }
}

async fn report_preview(
    database_url: &str,
    local: Local,
    policy_version_id: u64,
    input_set_id: u64,
) -> io::Result<i32> {
    let cp = match ControlPlane::open(database_url).await {
        Ok(cp) => cp,
        Err(err) => {
            emit_err("compliance", err.code(), err.to_string(), None, Some(local))?;
            return Ok(2);
        }
    };
    match cp
        .generate_compliance_report(
            PolicyVersionId(policy_version_id),
            PolicyInputSetId(input_set_id),
        )
        .await
    {
        Ok(data) => emit_ok("compliance", data, Some(local), Vec::new()).map(|()| 0),
        Err(err) => {
            emit_err("compliance", err.code(), err.to_string(), None, Some(local))?;
            Ok(2)
        }
    }
}

async fn report_run(database_url: &str, local: Local, job_id: u64) -> io::Result<i32> {
    let cp = match ControlPlane::open(database_url).await {
        Ok(cp) => cp,
        Err(err) => {
            emit_err("compliance", err.code(), err.to_string(), None, Some(local))?;
            return Ok(2);
        }
    };
    match cp.read_compliance_run_report(voom_core::JobId(job_id)).await {
        Ok(data) => emit_ok("compliance", data, Some(local), Vec::new()).map(|()| 0),
        Err(err) => {
            emit_err("compliance", err.code(), err.to_string(), None, Some(local))?;
            Ok(2)
        }
    }
}
```

Add `use voom_core::ErrorCode;` (or use the fully-qualified path as above) and
`use voom_core::JobId;` — confirm against the existing `use voom_core::{…}` line and
extend it rather than duplicating.

- [ ] **Step 2: Update the `main.rs` dispatch destructure**

Replace `crates/voom-cli/src/main.rs:346-349`:

```rust
        ComplianceCommand::Report {
            policy_version_id,
            input_set_id,
            job_id,
        } => {
            compliance::report(&cfg.database_url, local, policy_version_id, input_set_id, job_id)
                .await?
        }
```

- [ ] **Step 3: Write the validation unit test**

Add to `crates/voom-cli/src/commands/compliance_test.rs` (create the file with the
sibling-module wiring if it does not already host tests — confirm `compliance.rs`
ends with `#[cfg(test)] #[path = "compliance_test.rs"] mod tests;`, which it does):

```rust
use super::{parse_report_mode, ReportMode};

#[test]
fn parse_report_mode_accepts_preview_pair() {
    let mode = parse_report_mode(Some(1), Some(2), None).unwrap();
    assert!(matches!(mode, ReportMode::Preview { policy_version_id: 1, input_set_id: 2 }));
}

#[test]
fn parse_report_mode_accepts_job_id() {
    let mode = parse_report_mode(None, None, Some(7)).unwrap();
    assert!(matches!(mode, ReportMode::Run { job_id: 7 }));
}

#[test]
fn parse_report_mode_rejects_none() {
    assert!(parse_report_mode(None, None, None).is_err());
}

#[test]
fn parse_report_mode_rejects_all_three() {
    assert!(parse_report_mode(Some(1), Some(2), Some(3)).is_err());
}
```

`parse_report_mode` and `ReportMode` must be visible to the test module — they are
private fns in the same module, so `use super::{…}` works.

- [ ] **Step 4: Build + run the unit tests**

Run: `cargo test -p voom-cli --lib commands::compliance`
Expected: PASS. The crate now builds (Tasks 4–5 together restore it).

- [ ] **Step 5: Clippy the crate**

Run: `cargo clippy -p voom-cli --all-targets --all-features -- -D warnings`
Expected: clean. The `ReportMode` enum + helper are dead-code-free because `report`
uses them.

- [ ] **Step 6: Commit**

```bash
git add crates/voom-cli/src/cli.rs crates/voom-cli/src/main.rs crates/voom-cli/src/commands/compliance.rs crates/voom-cli/src/commands/compliance_test.rs
git commit -m "feat(cli): add compliance report --job-id post-run read mode (#166)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: CLI BAD_ARGS + unknown-job goldens for `report`

**Files:**
- Modify: `crates/voom-cli/tests/compliance_envelope.rs`

- [ ] **Step 1: Write the unknown-job NOT_FOUND test**

Add to `compliance_envelope.rs`:

```rust
#[tokio::test]
async fn report_unknown_job_id_uses_not_found() {
    let seeded = seed(FixtureName::SyntheticNoncompliantTranscodeNeeded).await;

    let output = Command::new(env!("CARGO_BIN_EXE_voom"))
        .args([
            "--database-url",
            &seeded.url,
            "compliance",
            "report",
            "--job-id",
            "999999",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let mut json = envelope(output.stdout);
    assert_eq!(json["error"]["code"], "NOT_FOUND");
    redact_local(&mut json);
    insta::assert_json_snapshot!("report_unknown_job_id_uses_not_found", json);
}
```

- [ ] **Step 2: Write the BAD_ARGS argument-combination test**

Two sub-cases: clap-level (job-id + preview arg → `command: "cli"`) and
handler-level (no args → `command: "compliance"`). Assert the code, not the command,
where they differ:

```rust
#[test]
fn report_with_no_selector_args_is_bad_args() {
    let bin = env!("CARGO_BIN_EXE_voom");
    // No --database-url needed: the BAD_ARGS rejection happens before any DB open.
    let output = Command::new(bin)
        .args(["--database-url", "sqlite::memory:", "compliance", "report"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let json = envelope(output.stdout);
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "BAD_ARGS");
}

#[test]
fn report_with_job_id_and_preview_arg_is_bad_args() {
    let bin = env!("CARGO_BIN_EXE_voom");
    let output = Command::new(bin)
        .args([
            "--database-url",
            "sqlite::memory:",
            "compliance",
            "report",
            "--job-id",
            "1",
            "--policy-version-id",
            "1",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "clap conflict => BAD_ARGS exit 1");
    let json = envelope(output.stdout);
    assert_eq!(json["error"]["code"], "BAD_ARGS");
}
```

These are plain assertions (not insta) because the clap error message text is a
clap-version detail; pinning the `code`/exit is the stable contract.

- [ ] **Step 3: Run the new tests, accepting the new snapshot**

Run: `cargo test -p voom-cli --test compliance_envelope report_unknown_job_id_uses_not_found report_with_no_selector_args_is_bad_args report_with_job_id_and_preview_arg_is_bad_args`
Expected: the two BAD_ARGS tests PASS; the NOT_FOUND test fails pending snapshot
acceptance.

Run: `cargo insta review` (accept `report_unknown_job_id_uses_not_found`), or
`INSTA_UPDATE=always cargo test -p voom-cli --test compliance_envelope report_unknown_job_id_uses_not_found` then re-run to confirm PASS.

- [ ] **Step 4: Re-run to confirm all three pass**

Run: `cargo test -p voom-cli --test compliance_envelope report_unknown_job_id report_with_`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-cli/tests/compliance_envelope.rs crates/voom-cli/tests/snapshots/
git commit -m "test(cli): cover report --job-id NOT_FOUND and BAD_ARGS combinations (#166)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: Multi-phase golden flow — plan → report(preview) → execute → report(--job-id)

**Files:**
- Modify: `crates/voom-cli/tests/compliance_envelope.rs`

Builds on Task 1's proven setup (`seed_scanned_remux_then_audio`, both worker
launches, fake ffprobe). If Task 1 fell back to real-ffmpeg, this task uses field
assertions instead of `insta` (per spec §5 Determinism) — note that branch when
implementing.

- [ ] **Step 1: Write the golden flow test**

```rust
#[tokio::test]
async fn multi_phase_flow_plan_report_execute_report_golden() {
    let seeded = seed_scanned_remux_then_audio().await;

    // 1. plan (preview)
    let plan_out = Command::new(env!("CARGO_BIN_EXE_voom"))
        .args([
            "--database-url", &seeded.url, "plan", "show",
            "--policy-version-id", &seeded.version_id.to_string(),
            "--input-set-id", &seeded.input_id.to_string(),
        ])
        .output()
        .unwrap();
    assert_eq!(plan_out.status.code(), Some(0));
    let mut plan_json = envelope(plan_out.stdout);
    redact_local(&mut plan_json);
    insta::assert_json_snapshot!("multi_phase_flow_plan_preview", plan_json);

    // 2. report (preview)
    let report_out =
        compliance_command(&seeded.url, "report", seeded.version_id, seeded.input_id);
    assert_eq!(report_out.status.code(), Some(0));
    let mut report_json = envelope(report_out.stdout);
    redact_local(&mut report_json);
    insta::assert_json_snapshot!("multi_phase_flow_report_preview", report_json);

    // 3. execute (the proven two-phase run)
    let mut remux = RemuxProviderLaunch::start(&seeded.url).await.unwrap();
    let mut audio = AudioProviderLaunch::start(&seeded.url).await.unwrap();
    let root = seeded.dir.path().canonicalize().unwrap();
    let staging_root = root.join("stage");
    let output_dir = root.join("out");
    let ffprobe_bin = fake_ffprobe_bin(&root);
    let exec_out = compliance_execute_command_with_dirs(
        &seeded.url, seeded.version_id, seeded.input_id,
        &staging_root, &output_dir, &ffprobe_bin,
    );
    remux.shutdown().unwrap();
    audio.shutdown().unwrap();
    assert_eq!(exec_out.status.code(), Some(0));
    let mut exec_json = envelope(exec_out.stdout);
    // Capture the job id BEFORE redaction for the post-run read.
    let job_id = exec_json["data"]["summary"]["job_id"].as_u64().unwrap();
    // Phase 1's produced version is the parent phase 2 chained from.
    let phases = exec_json["data"]["phases"].as_array().unwrap();
    assert_eq!(phases.len(), 2);
    assert_eq!(phases[0]["phase_name"], "remux");
    assert_eq!(phases[1]["phase_name"], "audio");
    let file_phases = exec_json["data"]["file_phases"].as_array().unwrap();
    assert_eq!(file_phases.len(), 2);
    assert!(file_phases.iter().all(|fp| fp["outcome"] == "committed"));
    redact_local(&mut exec_json);
    redact_execute_ids(&mut exec_json);
    insta::assert_json_snapshot!("multi_phase_flow_execute", exec_json);

    // 4. report (post-run, --job-id)
    let run_out = Command::new(env!("CARGO_BIN_EXE_voom"))
        .args([
            "--database-url", &seeded.url, "compliance", "report",
            "--job-id", &job_id.to_string(),
        ])
        .output()
        .unwrap();
    assert_eq!(
        run_out.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&run_out.stdout),
        String::from_utf8_lossy(&run_out.stderr)
    );
    let mut run_json = envelope(run_out.stdout);
    let run_phases = run_json["data"]["phases"].as_array().unwrap();
    assert_eq!(run_phases.len(), 2, "post-run read returns the full chain");
    assert_eq!(run_json["data"]["latest_phase_index"], 1);
    assert!(run_phases.iter().all(|p| p["report_id"].is_string()));
    redact_local(&mut run_json);
    redact_execute_ids(&mut run_json);
    insta::assert_json_snapshot!("multi_phase_flow_report_run", run_json);
}
```

Note: `redact_local` already redacts `data.summary.job_id`; for the post-run read
envelope, confirm the `job_id` under `data.summary.job_id` is redacted there too
(it is the same `WorkflowSummaryView` shape, so `redact_local` covers it). If the
plan/report preview envelopes carry volatile autoincrement ids, extend the
redaction helper as needed so the snapshots are stable.

- [ ] **Step 2: Run, accept snapshots, re-run**

Run: `cargo test -p voom-cli --test compliance_envelope multi_phase_flow_plan_report_execute_report_golden -- --nocapture`
Then: `cargo insta review` (inspect each of the 4 new snapshots — confirm the
execute envelope shows two `completed` phases and two `committed` file rows, and
the post-run read shows the same two-phase chain with `latest_phase_index: 1`).
Re-run to confirm PASS.

- [ ] **Step 3: Verify snapshot stability (run twice)**

Run the test twice in a row; both must PASS with no snapshot diff. If a value
churns, add it to the redaction helpers (it should not, given the fake ffprobe is
fixed, but produced-version ids are autoincrement and must be redacted — they are,
via `redact_execute_ids`).

- [ ] **Step 4: Commit**

```bash
git add crates/voom-cli/tests/compliance_envelope.rs crates/voom-cli/tests/snapshots/
git commit -m "test(cli): golden multi-phase scan->plan->execute->report flow (#166)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: Full guardrail pass + issue cross-references

**Files:**
- Modify: none (verification), unless `just ci` surfaces a fix.

- [ ] **Step 1: Run the full CI suite locally**

Run: `just ci`
Expected: PASS — `fmt-check`, `lint` (clippy `-D warnings`), `check-test-layout`,
`test` (workspace, all features), `doc`, `deny`, `audit` all green.

The workspace `test` step is what exercises the multi-phase flow (it launches the
prober on staged output), per the spec's test-layout note; `cargo test -p voom-cli`
alone may not. Run `just ci` (or at minimum `cargo test --workspace --all-features`)
before claiming done.

- [ ] **Step 2: Fix anything `just ci` surfaces**

If `fmt-check` fails: `just fmt`. If `check-test-layout` fails: ensure no inline
`#[cfg(test)] mod tests {}` was added in `src/` (all unit tests went to
`*_test.rs`). If clippy flags the new code: fix with `#[expect(…, reason = "…")]`
only as a last resort. Re-run `just ci` until green.

- [ ] **Step 3: Commit any guardrail fixes**

```bash
git add -A
git commit -m "chore(cli): satisfy guardrails for #166 surface

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

(Skip if Step 1 was already clean.)

---

## Self-Review (completed against the spec)

- **§2 read mode** → Tasks 2, 3, 5. **§3 argument contract** → Tasks 4, 5, 6.
  **§4 read method + `latest_phase_index`** → Tasks 2, 3. **§5 gate + fixture +
  golden** → Tasks 1, 7. **§6 error handling** (NOT_FOUND, BAD_ARGS, zero-phase) →
  Tasks 3, 6. **§7 testing** (handler units, CLI goldens, arg-parsing) → Tasks 3,
  5, 6, 7. **§8 acceptance** → Task 8 (`just ci`) + the per-task assertions.
- **No new durable schema / coordinator behavior / `voom transcode`** — confirmed:
  this plan only adds a read method, a CLI arg, a fixture, and tests.
- **Type consistency:** `ComplianceRunReportData` (control-plane) ↔ `RunReportData`
  alias (CLI); `latest_phase_index: Option<usize>` used identically in Tasks 2/3/7;
  `parse_report_mode`/`ReportMode` defined in Task 5, tested in Task 5; repo methods
  `get_summary`/`phases_for_job`/`file_phases_for_job` match `workflow_summaries.rs`.
- **Open verification points flagged inline** (confirm against source during
  implementation, do not assume): the `cp()` test helper name + return shape in
  `compliance_test.rs`; `NewWorkflowSummary`/`NewPhaseSummary`/`NewJob` field sets;
  whether `transcode audio to opus` parses without a `where` selector (Task 1 gate
  resolves this and may change the fixture); and whether `WorkflowSummaryRepo` needs
  importing into `compliance.rs` for the trait methods.
