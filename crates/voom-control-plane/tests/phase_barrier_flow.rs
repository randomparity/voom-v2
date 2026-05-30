#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test setup should fail loudly with direct assertions"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;
use tempfile::NamedTempFile;
use voom_control_plane::ControlPlane;
use voom_control_plane::cases::compliance::ComplianceExecutionOptions;
use voom_control_plane::scan::{ScanPathInput, ScanReportFileStatus};
use voom_control_plane::workflow::coordinator::CoordinatorOutcome;
use voom_core::{FileVersionId, MediaSnapshotId};
use voom_policy::{
    MediaSnapshotInput, PolicyInputSetDraft, PolicyInputSourceKind, TargetRef, load_policy_fixture,
};
use voom_store::repo::workflow_summaries::{
    FilePhaseOutcome, FilePhaseSummary, PhaseOutcome, SqliteWorkflowSummaryRepo,
    WorkflowSummaryRepo,
};
use voom_test_support::worker::{
    TestWorkerConfig, TestWorkerLaunch, cargo_build_package, target_debug_binary,
};

/// The phase-barrier coordinator drives one `plan_phase` per phase across every
/// active file, fanning the phase's planned nodes out across the files in a
/// single owned job. This exercises the dispatch + inline-commit path end to
/// end: two scanned h264 files each transcode to hevc in the one `normalize`
/// phase, both commit, and the coordinator records a `Committed` per-`(file,
/// phase)` row for each — with distinct branch ids, distinct ticket ids, and
/// real produced references — plus a `Completed` phase row and a job-grain
/// summary whose dispatch count covers both files.
#[tokio::test]
async fn phase_barrier_commits_every_file_in_a_single_phase() {
    // The post-commit result probe runs REAL ffprobe against the committed
    // output; hide any canned `ffprobe` stub installed by sibling tests.
    let _ffprobe_guard = hide_stale_fake_ffprobe_sibling();
    cargo_build_package("voom-ffprobe-worker").unwrap();
    cargo_build_package("voom-verify-artifact-worker").unwrap();
    cargo_build_package("voom-ffmpeg-worker").unwrap();

    let tmp = tempfile::TempDir::new().unwrap();
    let source_one = tmp.path().join("Movie1.mp4");
    let source_two = tmp.path().join("Movie2.mp4");
    generate_h264_fixture(&source_one);
    generate_h264_fixture(&source_two);

    let db = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = ControlPlane::open_with_pool(pool, std::sync::Arc::new(voom_core::SystemClock))
        .await
        .unwrap();

    let file_one = scan_one(&cp, &source_one).await;
    let file_two = scan_one(&cp, &source_two).await;

    let policy = cp
        .create_policy_document(
            "video-transcode-hevc",
            &load_policy_fixture("fixtures/policies/video-transcode-hevc.voom").unwrap(),
        )
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set(two_file_input(&[
            ("movie-one", file_one),
            ("movie-two", file_two),
        ]))
        .await
        .unwrap();

    let mut worker = TranscodeWorkerLaunch::start(&cp).await.unwrap();
    let out_dir = tmp.path().join("out");
    let outcome = cp
        .run_phase_barrier(
            policy.version.id,
            input.id,
            ComplianceExecutionOptions {
                transcode_staging_root: tmp.path().join("stage"),
                transcode_target_dir: out_dir.clone(),
                ..ComplianceExecutionOptions::default()
            },
        )
        .await;
    worker.shutdown().unwrap();
    let outcome = outcome.unwrap();

    assert_phase_completed(&outcome, &out_dir);
    assert_rows_durable(&url, outcome.job_id).await;
}

/// Every active file committed in the one `normalize` phase: a `Committed`
/// per-file row with real produced references and disjoint ticket attribution,
/// a `Completed` phase row, an on-disk output per file, and a job summary whose
/// dispatch count covers both files.
fn assert_phase_completed(outcome: &CoordinatorOutcome, out_dir: &Path) {
    assert_eq!(outcome.phases.len(), 1);
    assert_eq!(outcome.phases[0].phase_name, "normalize");
    assert_eq!(outcome.phases[0].outcome, PhaseOutcome::Completed);

    assert_eq!(outcome.file_phases.len(), 2);
    let mut branch_ids: Vec<&str> = outcome
        .file_phases
        .iter()
        .map(|row| row.branch_id.as_str())
        .collect();
    branch_ids.sort_unstable();
    assert_eq!(branch_ids, vec!["Movie1", "Movie2"]);

    let mut all_ticket_ids = Vec::new();
    for row in &outcome.file_phases {
        assert_eq!(row.outcome, FilePhaseOutcome::Committed);
        assert!(
            row.produced_file_version_id.is_some(),
            "committed row must record the produced file version"
        );
        assert!(
            row.produced_file_location_id.is_some(),
            "committed row must record the produced file location"
        );
        assert!(
            row.reprobe_snapshot_id.is_some(),
            "committed row must record the post-commit reprobe snapshot"
        );
        assert!(
            !row.ticket_ids.is_empty(),
            "committed row must attribute its transcode tickets"
        );
        all_ticket_ids.extend(row.ticket_ids.iter().copied());
    }
    // Each file's tickets are attributed to exactly one branch (no overlap).
    let unique: std::collections::HashSet<_> = all_ticket_ids.iter().collect();
    assert_eq!(
        unique.len(),
        all_ticket_ids.len(),
        "ticket ids must not be shared across file-phase rows"
    );

    assert!(out_dir.join("Movie1.default-hevc.hevc.mkv").is_file());
    assert!(out_dir.join("Movie2.default-hevc.hevc.mkv").is_file());

    assert!(
        outcome.summary.dispatch_count >= 2,
        "job summary dispatch_count {} should cover both files",
        outcome.summary.dispatch_count
    );

    // ADR-0008: the regenerated report covers *every* file that entered the phase
    // — here, one check per committed file targeting that file's produced
    // version. Asserting both produced versions appear pins the full-entered-set
    // semantics at >1 file (the single-file chain/blocked tests cannot).
    let report = outcome.phases[0]
        .report
        .as_ref()
        .expect("a completed phase records a regenerated report");
    let mut report_targets: Vec<u64> = report.report["checks"]
        .as_array()
        .expect("the report carries a checks array")
        .iter()
        .map(|check| {
            check["target"]["id"]
                .as_u64()
                .expect("each check targets a version")
        })
        .collect();
    report_targets.sort_unstable();
    let mut produced_versions: Vec<u64> = outcome
        .file_phases
        .iter()
        .map(|row| {
            row.produced_file_version_id
                .expect("committed row records its produced version")
                .0
        })
        .collect();
    produced_versions.sort_unstable();
    assert_eq!(
        report_targets, produced_versions,
        "the phase report must contain a check targeting each file's produced version"
    );
}

/// The rows are durable, not just returned in memory: re-read them through a
/// fresh repo over the same database.
async fn assert_rows_durable(url: &str, job_id: voom_core::JobId) {
    let repo = SqliteWorkflowSummaryRepo::new(voom_store::connect(url).await.unwrap());
    let durable_files = repo.file_phases_for_job(job_id).await.unwrap();
    assert_eq!(durable_files.len(), 2);
    assert!(
        durable_files
            .iter()
            .all(|row| row.outcome == FilePhaseOutcome::Committed)
    );
    // Issue #163: each committed file's refreshed snapshot is keyed to the
    // version that file produced (the single-phase case of the chain test).
    for row in &durable_files {
        let produced = row
            .produced_file_version_id
            .expect("committed row records its produced version");
        let snapshot = row
            .reprobe_snapshot_id
            .expect("committed row records its reprobe snapshot");
        assert_eq!(
            snapshots_for_version(url, produced).await,
            vec![i64::try_from(snapshot.0).unwrap()],
            "the committed file's reprobe snapshot must key to the version it produced"
        );
    }
    let durable_phases = repo.phases_for_job(job_id).await.unwrap();
    assert_eq!(durable_phases.len(), 1);
    assert_eq!(durable_phases[0].outcome, PhaseOutcome::Completed);
}

/// The barrier re-plans each phase against the artifact the prior phase
/// produced. A two-phase policy transcodes the file in phase 0 (h264 -> hevc,
/// committing a new chain tip), then phase 1 projects that committed artifact
/// into the planner: its report targets phase 0's produced `FileVersion` and
/// observes the committed hevc codec. If the coordinator had not advanced the
/// chain tip, phase 1 would instead target the original version and observe
/// h264 — so the phase-1 report proves the chain advance. (The repeated
/// transcode here is intentional fixture shaping; per ADR-0007 the planner
/// re-runs it because the probe reports the container as `matroska,webm`, not
/// the canonical `mkv` — orthogonal to the chain-advance behavior under test.)
#[tokio::test]
async fn phase_barrier_chains_committed_artifact_into_the_next_phase() {
    let _ffprobe_guard = hide_stale_fake_ffprobe_sibling();
    cargo_build_package("voom-ffprobe-worker").unwrap();
    cargo_build_package("voom-verify-artifact-worker").unwrap();
    cargo_build_package("voom-ffmpeg-worker").unwrap();

    let tmp = tempfile::TempDir::new().unwrap();
    let source = tmp.path().join("Chain.mp4");
    generate_h264_fixture(&source);

    let db = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = ControlPlane::open_with_pool(pool, std::sync::Arc::new(voom_core::SystemClock))
        .await
        .unwrap();

    let file = scan_one(&cp, &source).await;
    let scanned_version = file.file_version_id;
    let policy = cp
        .create_policy_document(
            "video-transcode-hevc-twice",
            "policy \"video transcode hevc twice\" {\n  \
               phase normalize { transcode video to hevc }\n  \
               phase reverify { depends_on: [normalize] transcode video to hevc }\n}",
        )
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set(two_file_input(&[("chain", file)]))
        .await
        .unwrap();

    let mut worker = TranscodeWorkerLaunch::start(&cp).await.unwrap();
    let out_dir = tmp.path().join("out");
    let outcome = cp
        .run_phase_barrier(
            policy.version.id,
            input.id,
            ComplianceExecutionOptions {
                transcode_staging_root: tmp.path().join("stage"),
                transcode_target_dir: out_dir.clone(),
                ..ComplianceExecutionOptions::default()
            },
        )
        .await;
    worker.shutdown().unwrap();
    let outcome = outcome.unwrap();

    // Phase 0 transcodes h264 and commits a new hevc FileVersion.
    assert_eq!(outcome.phases.len(), 2);
    assert_eq!(outcome.phases[0].phase_name, "normalize");
    assert_eq!(outcome.phases[0].outcome, PhaseOutcome::Completed);
    let phase0_commit = outcome
        .file_phases
        .iter()
        .find(|row| row.phase_ordinal == 0 && row.outcome == FilePhaseOutcome::Committed)
        .expect("phase 0 commits the transcoded file");
    let produced_version = phase0_commit
        .produced_file_version_id
        .expect("committed row records the produced version");
    assert!(out_dir.join("Chain.default-hevc.hevc.mkv").is_file());

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
    // Deterministic identity preserved (#164 acceptance): the regenerated report
    // carries a content-addressed report_id, and the row's report_id column
    // matches the identity embedded in the report JSON.
    assert!(
        !phase0.report_id.is_empty(),
        "the recorded phase report carries a content-addressed report_id"
    );
    assert_eq!(
        phase0.report_id, phase0.report["report_id"],
        "the row's report_id column matches the embedded report identity"
    );

    assert_reprobe_and_lineage_chain(&url, &outcome, scanned_version, phase0_commit).await;
}

/// Issue #163: the re-probe snapshot is keyed to the produced version, fed to
/// the next phase, with a correct `source_lineage` chain. Phase 1 re-transcodes
/// phase 0's output, committing a third version (V2) from V1, so the chain is
/// V0 (scan) -> V1 (phase 0) -> V2 (phase 1).
async fn assert_reprobe_and_lineage_chain(
    url: &str,
    outcome: &CoordinatorOutcome,
    scanned_version: FileVersionId,
    phase0_commit: &FilePhaseSummary,
) {
    let produced_v1 = phase0_commit
        .produced_file_version_id
        .expect("phase 0 committed row records its produced version");
    let phase0_snapshot = phase0_commit
        .reprobe_snapshot_id
        .expect("phase 0 committed row records its reprobe snapshot");
    // Phase 1 produces V2 only because the planner re-runs the transcode under
    // the ADR-0007 container-normalization quirk (probe reports `matroska,webm`,
    // not canonical `mkv`; see this test's docstring). If that quirk is ever
    // fixed, phase 1 becomes a NoOp and this lookup fails — the chain/lineage
    // behavior under test is unaffected; switch the policy's second phase to a
    // genuinely-needed mutation (e.g. remux) to keep producing V2.
    let phase1_commit = outcome
        .file_phases
        .iter()
        .find(|row| row.phase_ordinal == 1 && row.outcome == FilePhaseOutcome::Committed)
        .expect("phase 1 commits the re-transcoded file");
    let produced_v2 = phase1_commit
        .produced_file_version_id
        .expect("phase 1 committed row records its produced version");
    let phase1_snapshot = phase1_commit
        .reprobe_snapshot_id
        .expect("phase 1 committed row records its reprobe snapshot");

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

    // produced_from chain is append-only and never retires the source.
    assert_eq!(
        produced_from(url, produced_v1).await,
        Some(i64::try_from(scanned_version.0).unwrap()),
        "phase 0's artifact must descend from the scanned source version"
    );
    assert_eq!(
        produced_from(url, produced_v2).await,
        Some(i64::try_from(produced_v1.0).unwrap()),
        "phase 1's artifact must descend from phase 0's produced version"
    );

    // The refreshed snapshot is keyed to the produced version and is the only
    // (hence chain-tip) snapshot on it, so it is exactly what the coordinator
    // projects as the next phase's planning input. Combined with the V0->V1->V2
    // produced_from chain asserted above, this proves phase 0's reprobe snapshot
    // is the fact fed forward into phase 1's planner.
    assert_eq!(
        snapshots_for_version(url, produced_v1).await,
        vec![i64::try_from(phase0_snapshot.0).unwrap()],
        "phase 0's produced version carries exactly its reprobe snapshot"
    );
    assert_eq!(
        snapshots_for_version(url, produced_v2).await,
        vec![i64::try_from(phase1_snapshot.0).unwrap()],
        "phase 1's produced version carries exactly its reprobe snapshot"
    );

    // source_lineage is correct across the chain: phase 0 transcoded the scan,
    // phase 1 transcoded phase 0's produced artifact (not the original scan).
    assert_eq!(
        transcode_lineage_sources(url).await,
        vec![
            i64::try_from(scanned_version.0).unwrap(),
            i64::try_from(produced_v1.0).unwrap(),
        ],
        "each transcode's source_lineage must point at the prior phase's version"
    );
}

/// When one file's ticket fails mid-phase while a sibling commits inline, the
/// coordinator records the committed file's `Committed` per-`(file, phase)` row
/// before returning (ADR-0007): the executor drains every in-flight dispatch to
/// a terminal state, so the survivor's commit has landed, and that durable
/// record must survive the failed job. The failed file gets no row, the run
/// returns a `partial` outcome, and the job is `failed`.
#[tokio::test]
async fn phase_barrier_records_committed_sibling_when_a_file_fails() {
    let _ffprobe_guard = hide_stale_fake_ffprobe_sibling();
    cargo_build_package("voom-ffprobe-worker").unwrap();
    cargo_build_package("voom-verify-artifact-worker").unwrap();
    cargo_build_package("voom-ffmpeg-worker").unwrap();

    let tmp = tempfile::TempDir::new().unwrap();
    let good = tmp.path().join("Good.mp4");
    let doomed = tmp.path().join("Doomed.mp4");
    generate_h264_fixture(&good);
    generate_h264_fixture(&doomed);

    let db = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = ControlPlane::open_with_pool(pool, std::sync::Arc::new(voom_core::SystemClock))
        .await
        .unwrap();

    let good_file = scan_one(&cp, &good).await;
    let doomed_file = scan_one(&cp, &doomed).await;
    // Corrupt the doomed source AFTER scanning so its transcode fails on the
    // source-facts check (size/hash no longer match the scanned file version),
    // while the good file transcodes and commits inline.
    std::fs::write(&doomed, b"not a video anymore").unwrap();

    let policy = cp
        .create_policy_document(
            "video-transcode-hevc",
            &load_policy_fixture("fixtures/policies/video-transcode-hevc.voom").unwrap(),
        )
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set(two_file_input(&[
            ("good", good_file),
            ("doomed", doomed_file),
        ]))
        .await
        .unwrap();

    let mut worker = TranscodeWorkerLaunch::start(&cp).await.unwrap();
    let out_dir = tmp.path().join("out");
    let result = cp
        .run_phase_barrier(
            policy.version.id,
            input.id,
            ComplianceExecutionOptions {
                transcode_staging_root: tmp.path().join("stage"),
                transcode_target_dir: out_dir.clone(),
                ..ComplianceExecutionOptions::default()
            },
        )
        .await;
    worker.shutdown().unwrap();

    let err = result.expect_err("a failed file must fail the phase-barrier run");
    let partial = err
        .partial
        .expect("the committed sibling must be reported as a partial outcome");

    // Exactly the good file committed; the doomed file produced no row.
    assert_eq!(partial.file_phases.len(), 1);
    let committed = &partial.file_phases[0];
    assert_eq!(committed.branch_id, "Good");
    assert_eq!(committed.outcome, FilePhaseOutcome::Committed);
    assert!(committed.produced_file_version_id.is_some());
    assert!(committed.reprobe_snapshot_id.is_some());
    assert!(out_dir.join("Good.default-hevc.hevc.mkv").is_file());

    // The committed row is durable and the job is failed.
    let repo = SqliteWorkflowSummaryRepo::new(voom_store::connect(&url).await.unwrap());
    let durable = repo.file_phases_for_job(partial.job_id).await.unwrap();
    assert_eq!(durable.len(), 1);
    assert_eq!(durable[0].branch_id, "Good");
    assert_eq!(job_state(&url, partial.job_id).await, "failed");
}

/// Partial-barrier failure + resume (issue #165, spec §8). Two files transcode in
/// one phase: `Good` commits, `Doomed` (corrupted after scan) fails and fails the
/// whole job. After restoring `Doomed`'s bytes, resuming against the failed job
/// re-enters `Doomed` (it commits) without re-mutating `Good`, under a new job.
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
    // Corrupt the doomed source after scanning so its transcode fails the
    // source-facts check, while the good file commits inline.
    std::fs::write(&doomed, b"not a video anymore").unwrap();

    let policy = cp
        .create_policy_document(
            "video-transcode-hevc",
            &load_policy_fixture("fixtures/policies/video-transcode-hevc.voom").unwrap(),
        )
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set(two_file_input(&[
            ("good", good_file),
            ("doomed", doomed_file),
        ]))
        .await
        .unwrap();
    let out_dir = tmp.path().join("out");
    let options = ComplianceExecutionOptions {
        transcode_staging_root: tmp.path().join("stage"),
        transcode_target_dir: out_dir.clone(),
        ..ComplianceExecutionOptions::default()
    };

    // One worker serves both the failing run and the resume (re-registering the
    // same worker name would hit a UNIQUE constraint).
    let mut worker = TranscodeWorkerLaunch::start(&cp).await.unwrap();

    // First run: Doomed fails, Good commits, the whole job fails.
    let failed = cp
        .run_phase_barrier(policy.version.id, input.id, options.clone())
        .await
        .expect_err("the corrupt file must fail the run");
    let partial = failed
        .partial
        .expect("Good must be recorded as a committed partial");
    let good_committed = partial
        .file_phases
        .iter()
        .find(|r| r.branch_id == "Good")
        .expect("Good committed");
    let good_v1 = good_committed
        .produced_file_version_id
        .expect("Good produced a version");
    assert_eq!(job_state(&url, partial.job_id).await, "failed");

    // Restore Doomed to its scanned bytes so its re-planned transcode can commit.
    std::fs::copy(&doomed_bak, &doomed).unwrap();

    // Resume against the failed job.
    let outcome = cp
        .resume_phase_barrier(partial.job_id, policy.version.id, input.id, options)
        .await
        .expect("resume succeeds once the doomed file is valid");
    worker.shutdown().unwrap();

    assert_ne!(outcome.job_id, partial.job_id, "resume opens a new job");
    assert_eq!(job_state(&url, outcome.job_id).await, "succeeded");

    // Good is complete (single-phase policy), so reconciliation drops it: no Good
    // row in the resumed job, and certainly none producing a version past good_v1.
    assert!(
        outcome.file_phases.iter().all(|r| r.branch_id != "Good"
            || r.produced_file_version_id.is_none()
            || r.produced_file_version_id == Some(good_v1)),
        "Good must not be re-mutated on resume: {:?}",
        outcome.file_phases
    );

    // Doomed re-entered and committed under the new job.
    let doomed_committed = outcome
        .file_phases
        .iter()
        .find(|r| r.branch_id == "Doomed" && r.outcome == FilePhaseOutcome::Committed)
        .expect("Doomed commits on resume");
    assert!(doomed_committed.produced_file_version_id.is_some());
    assert!(out_dir.join("Doomed.default-hevc.hevc.mkv").is_file());
}

async fn job_state(url: &str, job_id: voom_core::JobId) -> String {
    let pool = voom_store::connect(url).await.unwrap();
    sqlx::query_scalar::<_, String>("SELECT state FROM jobs WHERE id = ?")
        .bind(i64::try_from(job_id.0).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap()
}

/// The `produced_from_version_id` (chain parent) recorded for a file version,
/// read directly so the test pins the durable lineage column, not an in-memory
/// projection.
async fn produced_from(url: &str, version: FileVersionId) -> Option<i64> {
    let pool = voom_store::connect(url).await.unwrap();
    sqlx::query_scalar::<_, Option<i64>>(
        "SELECT produced_from_version_id FROM file_versions WHERE id = ?",
    )
    .bind(i64::try_from(version.0).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap()
}

/// Every media-snapshot id keyed to a file version, in id order. A produced
/// version carries exactly one — its post-commit reprobe snapshot.
async fn snapshots_for_version(url: &str, version: FileVersionId) -> Vec<i64> {
    let pool = voom_store::connect(url).await.unwrap();
    sqlx::query_scalar::<_, i64>(
        "SELECT id FROM media_snapshots WHERE file_version_id = ? ORDER BY id ASC",
    )
    .bind(i64::try_from(version.0).unwrap())
    .fetch_all(&pool)
    .await
    .unwrap()
}

/// The `source_file_version_id` recorded in each `transcode_video` artifact
/// handle's `source_lineage`, in creation order — the source each mutation
/// transcoded.
async fn transcode_lineage_sources(url: &str) -> Vec<i64> {
    let pool = voom_store::connect(url).await.unwrap();
    sqlx::query_scalar::<_, i64>(
        "SELECT json_extract(source_lineage, '$.source_file_version_id') \
         FROM artifact_handles \
         WHERE json_extract(source_lineage, '$.operation') = 'transcode_video' \
         ORDER BY id ASC",
    )
    .fetch_all(&pool)
    .await
    .unwrap()
}

async fn scan_one(cp: &ControlPlane, source: &Path) -> ScannedFile {
    let scan = cp
        .scan_path(ScanPathInput {
            path: source.to_owned(),
        })
        .await
        .unwrap();
    let scanned = scan
        .files
        .iter()
        .find(|file| file.status == ScanReportFileStatus::Scanned)
        .unwrap();
    ScannedFile {
        file_version_id: scanned.file_version_id.unwrap(),
        media_snapshot_id: scanned.media_snapshot_id,
    }
}

#[derive(Clone, Copy)]
struct ScannedFile {
    file_version_id: FileVersionId,
    media_snapshot_id: Option<MediaSnapshotId>,
}

fn two_file_input(files: &[(&str, ScannedFile)]) -> PolicyInputSetDraft {
    let media_snapshots = files
        .iter()
        .enumerate()
        .map(|(index, (_slug, file))| MediaSnapshotInput {
            ordinal: u32::try_from(index + 1).unwrap(),
            target: TargetRef::FileVersion {
                id: file.file_version_id,
            },
            container: Some("mp4".to_owned()),
            stream_summary: json!({"video_stream_count": 1}),
            video_codec: Some("h264".to_owned()),
            width: Some(32),
            height: Some(32),
            hdr: None,
            bitrate: None,
            duration_millis: Some(1000),
            audio_languages: Vec::new(),
            subtitle_languages: Vec::new(),
            health_flags: Vec::new(),
            existing_media_snapshot_id: file.media_snapshot_id,
        })
        .collect();
    PolicyInputSetDraft {
        slug: "phase-barrier-two-file".to_owned(),
        display_name: "phase-barrier-two-file".to_owned(),
        schema_version: 1,
        source_kind: PolicyInputSourceKind::Test,
        created_at: time::OffsetDateTime::UNIX_EPOCH,
        description: None,
        fixture_labels: files.iter().map(|(slug, _)| (*slug).to_owned()).collect(),
        synthetic_targets: Vec::new(),
        media_snapshots,
        identity_evidence: Vec::new(),
        bundle_targets: Vec::new(),
        quality_profiles: Vec::new(),
        issues: Vec::new(),
    }
}

/// Hide the canned test-helper `ffprobe` sibling so the bundled probe worker
/// runs real ffprobe (see `video_transcode_flow.rs` for the rationale). The
/// static mutex serializes any real-ffprobe cases in this binary.
fn hide_stale_fake_ffprobe_sibling() -> FfprobeSiblingGuard {
    static SERIALIZE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let lock = SERIALIZE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = target_debug_binary("ffprobe");
    let hidden = path.with_file_name("ffprobe.phase-barrier-flow-hidden");
    let is_stub = std::fs::read(&path).is_ok_and(|bytes| {
        bytes
            .windows(b"ffprobe version test-helper".len())
            .any(|window| window == b"ffprobe version test-helper")
    });
    if is_stub {
        std::fs::rename(&path, &hidden).unwrap();
    }
    FfprobeSiblingGuard {
        path,
        hidden,
        restore: is_stub,
        _lock: lock,
    }
}

struct FfprobeSiblingGuard {
    path: PathBuf,
    hidden: PathBuf,
    restore: bool,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Drop for FfprobeSiblingGuard {
    fn drop(&mut self) {
        if self.restore && self.hidden.exists() && !self.path.exists() {
            let _ = std::fs::rename(&self.hidden, &self.path);
        }
    }
}

fn generate_h264_fixture(path: &Path) {
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=32x32:rate=1",
            "-t",
            "1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            path.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(
        status.success(),
        "ffmpeg fixture generation failed: {status}"
    );
}

struct TranscodeWorkerLaunch {
    inner: TestWorkerLaunch,
}

impl TranscodeWorkerLaunch {
    async fn start(cp: &ControlPlane) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            inner: TestWorkerLaunch::start(
                cp,
                TestWorkerConfig::synthetic(
                    target_debug_binary("voom-ffmpeg-worker"),
                    "e2e-phase-barrier-transcode",
                    "control-plane-phase-barrier-e2e-secret",
                    "transcode_video",
                ),
            )
            .await?,
        })
    }

    fn shutdown(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.inner.shutdown()
    }
}
