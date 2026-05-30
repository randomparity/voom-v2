#![expect(
    clippy::unwrap_used,
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
    FilePhaseOutcome, PhaseOutcome, SqliteWorkflowSummaryRepo, WorkflowSummaryRepo,
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
    let durable_phases = repo.phases_for_job(job_id).await.unwrap();
    assert_eq!(durable_phases.len(), 1);
    assert_eq!(durable_phases[0].outcome, PhaseOutcome::Completed);
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
