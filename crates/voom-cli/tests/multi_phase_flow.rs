//! Multi-phase `compliance execute` run + `compliance report --job-id` read-back
//! through the `voom` CLI, against the real `voom-ffmpeg-worker` + real ffprobe
//! stack (issue #166).
//!
//! Fake workers cannot commit a second mutation phase — the fake transcoder's
//! audio result hardcodes null stream facts that satisfy neither the planner's
//! preservation-fact gate nor the host's preserved-facts commit check — so a
//! genuine two-committed-phase run requires the real ffmpeg worker. Real ffmpeg
//! output embeds run-/version-varying `bitrate`/`duration`, so this is a
//! field-assertion test, not an `insta` golden (the same reason
//! `crates/voom-control-plane/tests/phase_barrier_flow.rs` asserts fields).
//!
//! A two-`transcode video` phase policy is the proven two-commit shape: phase 0
//! transcodes the scanned h264 to hevc and commits; phase 1 re-plans against the
//! committed artifact and re-transcodes because the probe reports the container
//! as `matroska,webm`, not the canonical `mkv` (ADR-0007 normalization quirk),
//! committing a second time. Both phases land a `Committed` per-`(file, phase)`
//! row, and `compliance report --job-id` reads the durable two-phase chain back.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests fail loudly and preserve paths for diagnosis"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use tempfile::NamedTempFile;
use voom_control_plane::ControlPlane;
use voom_control_plane::scan::{ScanPathInput, ScanReportFileStatus};
use voom_core::FileVersionId;
use voom_policy::{MediaSnapshotInput, PolicyInputSetDraft, PolicyInputSourceKind, TargetRef};
use voom_store::test_support::sqlite_url_for;
use voom_test_support::worker::{TestWorkerConfig, TestWorkerLaunch, target_debug_binary};

/// `compliance execute` drives a two-phase transcode policy to completion through
/// the CLI, and `compliance report --job-id` reads the durable two-phase chain
/// back: two `completed` phases, two `committed` per-file rows, phase 1 rooted at
/// phase 0's produced version, and the post-run read returns the same chain with
/// `latest_phase_index` pointing at phase 1.
#[tokio::test]
async fn multi_phase_execute_then_report_by_job_id() {
    let _ffprobe_guard = hide_stale_fake_ffprobe_sibling();
    cargo_build("voom-ffprobe-worker");
    cargo_build("voom-verify-artifact-worker");
    cargo_build("voom-ffmpeg-worker");

    let tmp = tempfile::TempDir::new().unwrap();
    let source = tmp.path().join("Movie.mp4");
    generate_h264_fixture(&source);

    let db = NamedTempFile::new().unwrap();
    let url = sqlite_url_for(db.path());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = ControlPlane::open_with_pool(pool, std::sync::Arc::new(voom_core::SystemClock))
        .await
        .unwrap();

    let file = scan_one(&cp, &source).await;
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
        .create_policy_input_set(single_file_input(file))
        .await
        .unwrap();
    let version_id = policy.version.id.0;
    let input_id = input.id.0;

    let mut worker = TranscodeWorkerLaunch::start(&cp).await.unwrap();
    let out_dir = tmp.path().join("out");
    let staging_root = tmp.path().join("stage");
    let execute = run_voom(
        &url,
        &[
            "compliance",
            "execute",
            "--policy-version-id",
            &version_id.to_string(),
            "--input-set-id",
            &input_id.to_string(),
            "--staging-root",
            &staging_root.display().to_string(),
            "--output-dir",
            &out_dir.display().to_string(),
        ],
    );
    worker.shutdown().unwrap();

    let execute_json = assert_execute_committed_two_phases(&url, &execute).await;
    let job_id = execute_json["data"]["summary"]["job_id"].as_u64().unwrap();
    let run_phases = execute_json["data"]["phases"].as_array().unwrap();

    assert_report_reads_back_chain(&url, job_id, run_phases);
}

/// `execute` exits 0 with two `completed` phases, two `committed` per-file rows,
/// and phase 1 rooted at phase 0's produced version. Returns the parsed envelope.
async fn assert_execute_committed_two_phases(url: &str, execute: &std::process::Output) -> Value {
    assert_eq!(
        execute.status.code(),
        Some(0),
        "execute must succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&execute.stdout),
        String::from_utf8_lossy(&execute.stderr)
    );
    let execute_json = envelope(&execute.stdout);
    assert_eq!(execute_json["command"], "compliance");
    assert_eq!(execute_json["status"], "ok");

    let phases = execute_json["data"]["phases"].as_array().unwrap();
    assert_eq!(phases.len(), 2, "two phases recorded: {phases:?}");
    assert_eq!(phases[0]["phase_name"], "normalize");
    assert_eq!(phases[0]["outcome"], "completed");
    assert_eq!(phases[1]["phase_name"], "reverify");
    assert_eq!(phases[1]["outcome"], "completed");

    let file_phases = execute_json["data"]["file_phases"].as_array().unwrap();
    assert_eq!(file_phases.len(), 2, "one committed row per phase");
    assert!(file_phases.iter().all(|fp| fp["outcome"] == "committed"));
    let produced_v1 = file_phase_at(file_phases, 0)["produced_file_version_id"]
        .as_u64()
        .unwrap();
    let produced_v2 = file_phase_at(file_phases, 1)["produced_file_version_id"]
        .as_u64()
        .unwrap();
    assert_ne!(
        produced_v1, produced_v2,
        "each phase produces a distinct version"
    );
    assert_eq!(
        produced_from(url, FileVersionId(produced_v2)).await,
        Some(i64::try_from(produced_v1).unwrap()),
        "phase 1 must run against the version phase 0 produced"
    );
    execute_json
}

/// `compliance report --job-id` reads the durable two-phase chain back unchanged:
/// same phases, `latest_phase_index` at phase 1, folded report ids preserved.
fn assert_report_reads_back_chain(url: &str, job_id: u64, run_phases: &[Value]) {
    let report = run_voom(
        url,
        &["compliance", "report", "--job-id", &job_id.to_string()],
    );
    assert_eq!(
        report.status.code(),
        Some(0),
        "report --job-id must succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&report.stdout),
        String::from_utf8_lossy(&report.stderr)
    );
    let report_json = envelope(&report.stdout);
    assert_eq!(report_json["status"], "ok");
    assert_eq!(report_json["data"]["summary"]["job_id"], job_id);
    let report_phases = report_json["data"]["phases"].as_array().unwrap();
    assert_eq!(
        report_phases.len(),
        2,
        "post-run read returns the full chain"
    );
    assert_eq!(report_phases[0]["phase_name"], "normalize");
    assert_eq!(report_phases[1]["phase_name"], "reverify");
    assert_eq!(
        report_json["data"]["latest_phase_index"], 1,
        "latest index points at the highest-ordinal phase"
    );
    assert!(
        report_phases.iter().all(|p| p["report_id"].is_string()),
        "each phase carries its folded report id"
    );
    assert_eq!(
        report_phases[0]["report_id"], run_phases[0]["report_id"],
        "read-back phase 0 report id matches the run"
    );
    assert_eq!(
        report_json["data"]["file_phases"].as_array().unwrap().len(),
        2,
        "post-run read returns both committed file rows"
    );
}

fn file_phase_at(file_phases: &[Value], ordinal: u64) -> &Value {
    file_phases
        .iter()
        .find(|fp| fp["phase_ordinal"].as_u64() == Some(ordinal))
        .unwrap_or_else(|| panic!("missing file-phase row for ordinal {ordinal}"))
}

fn run_voom(url: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_voom"))
        .arg("--database-url")
        .arg(url)
        .args(args)
        .output()
        .unwrap()
}

fn envelope(stdout: &[u8]) -> Value {
    let stdout = String::from_utf8(stdout.to_vec()).unwrap();
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be one JSON envelope; got {stdout:?}: {e}"))
}

#[derive(Clone, Copy)]
struct ScannedFile {
    file_version_id: voom_core::FileVersionId,
    media_snapshot_id: Option<voom_core::MediaSnapshotId>,
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

fn single_file_input(file: ScannedFile) -> PolicyInputSetDraft {
    PolicyInputSetDraft {
        slug: "cli-multi-phase".to_owned(),
        display_name: "cli-multi-phase".to_owned(),
        schema_version: 1,
        source_kind: PolicyInputSourceKind::Test,
        created_at: time::OffsetDateTime::UNIX_EPOCH,
        description: None,
        fixture_labels: vec!["movie".to_owned()],
        synthetic_targets: Vec::new(),
        media_snapshots: vec![MediaSnapshotInput {
            ordinal: 1,
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
        }],
        identity_evidence: Vec::new(),
        bundle_targets: Vec::new(),
        quality_profiles: Vec::new(),
        issues: Vec::new(),
    }
}

/// The `produced_from_version_id` (chain parent) recorded for a file version,
/// read directly so the test pins the durable lineage column.
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

fn cargo_build(package: &str) {
    voom_test_support::worker::cargo_build_package(package).unwrap();
}

/// Hide the canned test-helper `ffprobe` sibling so the bundled probe worker runs
/// real ffprobe (see `phase_barrier_flow.rs` for the rationale). The static mutex
/// serializes real-ffprobe cases in this binary.
fn hide_stale_fake_ffprobe_sibling() -> FfprobeSiblingGuard {
    static SERIALIZE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let lock = SERIALIZE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = target_debug_binary("ffprobe");
    let hidden = path.with_file_name("ffprobe.multi-phase-flow-hidden");
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
                    "cli-multi-phase-transcode",
                    "cli-multi-phase-e2e-secret",
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
