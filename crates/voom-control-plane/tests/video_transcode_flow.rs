#![expect(
    clippy::unwrap_used,
    reason = "integration test setup should fail loudly with direct assertions"
)]

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::Duration;

use serde_json::json;
use tempfile::NamedTempFile;
use voom_control_plane::ControlPlane;
use voom_control_plane::cases::compliance::ComplianceExecutionOptions;
use voom_control_plane::scan::{ScanPathInput, ScanReportFileStatus};
use voom_core::{FileVersionId, MediaSnapshotId};
use voom_policy::{
    MediaSnapshotInput, PolicyInputSetDraft, PolicyInputSourceKind, TargetRef, load_policy_fixture,
};
use voom_store::repo::identity::{IdentityRepo, SqliteIdentityRepo};
use voom_store::repo::workers::{NewCapability, NewGrant, NewWorker, WorkerKind};

#[tokio::test]
async fn video_transcode_flow_verifies_commits_and_replans_result_as_no_op() {
    build_worker_binary("voom-ffprobe-worker");
    build_worker_binary("voom-verify-artifact-worker");
    build_worker_binary("voom-ffmpeg-worker");

    let tmp = tempfile::TempDir::new().unwrap();
    let source = tmp.path().join("Movie.mp4");
    generate_h264_fixture(&source);

    let db = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = ControlPlane::open_with_pool(pool, std::sync::Arc::new(voom_core::SystemClock))
        .await
        .unwrap();

    let scan = cp
        .scan_path(ScanPathInput {
            path: source.clone(),
        })
        .await
        .unwrap();
    assert_eq!(scan.summary.scanned_count(), 1);
    let scanned = scan
        .files
        .iter()
        .find(|file| file.status == ScanReportFileStatus::Scanned)
        .unwrap();
    let source_file_version_id = scanned.file_version_id.unwrap();

    let policy = cp
        .create_policy_document(
            "video-transcode-hevc",
            &load_policy_fixture("fixtures/policies/video-transcode-hevc.voom").unwrap(),
        )
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set(input_for(
            "movie-h264",
            source_file_version_id,
            scanned.media_snapshot_id,
            "mp4",
            "h264",
        ))
        .await
        .unwrap();

    let plan = cp
        .generate_compliance_report(policy.version.id, input.id)
        .await
        .unwrap();
    assert_eq!(plan.plan.nodes[0].operation_kind, "transcode_video");
    assert_eq!(plan.plan.nodes[0].status, voom_plan::NodeStatus::Planned);

    let mut worker = TranscodeWorkerLaunch::start(&cp).await.unwrap();
    let out_dir = tmp.path().join("out");
    let executed = cp
        .execute_compliance_policy_with_options(
            policy.version.id,
            input.id,
            ComplianceExecutionOptions {
                transcode_staging_root: tmp.path().join("stage"),
                transcode_target_dir: out_dir.clone(),
            },
        )
        .await
        .unwrap();
    worker.shutdown().unwrap();

    let (result_file_version_id, result_media_snapshot_id) =
        assert_transcode_execution_result(&url, &out_dir, &executed).await;
    assert_result_replans_as_no_op(
        &cp,
        policy.version.id,
        result_file_version_id,
        result_media_snapshot_id,
    )
    .await;
}

async fn assert_transcode_execution_result(
    url: &str,
    out_dir: &Path,
    executed: &voom_control_plane::cases::compliance::ComplianceExecuteData,
) -> (FileVersionId, MediaSnapshotId) {
    let ticket = executed
        .tickets
        .iter()
        .find(|ticket| ticket.operation == "transcode_video")
        .unwrap();
    let result = ticket.result.as_ref().unwrap();
    let result_file_version_id = FileVersionId(result["result_file_version_id"].as_u64().unwrap());
    let result_media_snapshot_id =
        MediaSnapshotId(result["result_media_snapshot_id"].as_u64().unwrap());
    assert!(result["staged_artifact_handle_id"].as_u64().unwrap() > 0);
    assert!(result["verification_id"].as_u64().unwrap() > 0);
    assert!(result["commit_record_id"].as_u64().unwrap() > 0);
    assert!(out_dir.join("Movie.hevc.mkv").is_file());

    let snapshots = SqliteIdentityRepo::new(voom_store::connect(url).await.unwrap())
        .list_media_snapshots_by_version(result_file_version_id)
        .await
        .unwrap();
    let result_snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.id == result_media_snapshot_id)
        .unwrap();
    assert_eq!(result_snapshot.payload["container"], "mkv");
    assert_eq!(result_snapshot.payload["video_codec"], "hevc");
    (result_file_version_id, result_media_snapshot_id)
}

async fn assert_result_replans_as_no_op(
    cp: &ControlPlane,
    policy_version_id: voom_core::PolicyVersionId,
    result_file_version_id: FileVersionId,
    result_media_snapshot_id: MediaSnapshotId,
) {
    let result_input = cp
        .create_policy_input_set(input_for(
            "movie-hevc",
            result_file_version_id,
            Some(result_media_snapshot_id),
            "mkv",
            "hevc",
        ))
        .await
        .unwrap();
    let result_plan = cp
        .generate_compliance_report(policy_version_id, result_input.id)
        .await
        .unwrap();
    assert_eq!(
        result_plan.plan.nodes[0].status,
        voom_plan::NodeStatus::NoOp
    );
}

trait ScanSummaryExt {
    fn scanned_count(&self) -> u64;
}

impl ScanSummaryExt for voom_control_plane::scan::ScanSummary {
    fn scanned_count(&self) -> u64 {
        self.ingested
    }
}

fn input_for(
    slug: &str,
    file_version_id: FileVersionId,
    media_snapshot_id: Option<MediaSnapshotId>,
    container: &str,
    video_codec: &str,
) -> PolicyInputSetDraft {
    PolicyInputSetDraft {
        slug: slug.to_owned(),
        display_name: slug.to_owned(),
        schema_version: 1,
        source_kind: PolicyInputSourceKind::Test,
        created_at: time::OffsetDateTime::UNIX_EPOCH,
        description: None,
        fixture_labels: vec![format!("video-transcode-flow-{slug}")],
        synthetic_targets: Vec::new(),
        media_snapshots: vec![MediaSnapshotInput {
            ordinal: 1,
            target: TargetRef::FileVersion {
                id: file_version_id,
            },
            container: Some(container.to_owned()),
            stream_summary: json!({"video_stream_count": 1}),
            video_codec: Some(video_codec.to_owned()),
            width: Some(32),
            height: Some(32),
            hdr: None,
            bitrate: None,
            duration_millis: Some(1000),
            audio_languages: Vec::new(),
            subtitle_languages: Vec::new(),
            health_flags: Vec::new(),
            existing_media_snapshot_id: media_snapshot_id,
        }],
        identity_evidence: Vec::new(),
        bundle_targets: Vec::new(),
        quality_profiles: Vec::new(),
        issues: Vec::new(),
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
    child: Child,
    stdin: Option<ChildStdin>,
}

impl TranscodeWorkerLaunch {
    async fn start(cp: &ControlPlane) -> Result<Self, Box<dyn std::error::Error>> {
        let secret = "control-plane-transcode-e2e-secret";
        let worker = cp
            .register_worker(NewWorker {
                name: "e2e-ffmpeg-transcode".to_owned(),
                kind: WorkerKind::Synthetic,
                registered_at: cp.clock().now(),
                node_id: None,
            })
            .await?;
        let mut child = Command::new(worker_binary("voom-ffmpeg-worker"))
            .env("VOOM_WORKER_SECRET", secret)
            .env("VOOM_WORKER_ID", worker.id.0.to_string())
            .env("VOOM_WORKER_EPOCH", "0")
            .env("VOOM_WORKER_BIND", "127.0.0.1:0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child.stdin.take();
        let bound = read_bound_addr(&mut child)?;
        cp.record_capability(NewCapability {
            worker_id: worker.id,
            operation: "transcode_video".to_owned(),
            codecs: Vec::new(),
            hardware: Vec::new(),
            artifact_access: Vec::new(),
            extra: json!({
                "endpoint": bound.to_string(),
                "secret": secret,
            }),
        })
        .await?;
        cp.record_grant(NewGrant {
            worker_id: worker.id,
            can_execute: vec!["transcode_video".to_owned()],
            can_access_read: Vec::new(),
            can_access_write: Vec::new(),
            denies: Vec::new(),
            max_parallel: json!({ "transcode_video": 1 }),
        })
        .await?;
        Ok(Self { child, stdin })
    }

    fn shutdown(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        drop(self.stdin.take());
        let started = std::time::Instant::now();
        loop {
            if let Some(status) = self.child.try_wait()? {
                if status.success() {
                    return Ok(());
                }
                return Err(Box::new(std::io::Error::other(format!(
                    "voom-ffmpeg-worker exited with {status}"
                ))));
            }
            if started.elapsed() > Duration::from_secs(5) {
                let _ = self.child.kill();
                return Err(Box::new(std::io::Error::other(
                    "voom-ffmpeg-worker cleanup timed out",
                )));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn read_bound_addr(child: &mut Child) -> Result<std::net::SocketAddr, Box<dyn std::error::Error>> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("worker stdout missing"))?;
    let mut lines = std::io::BufReader::new(stdout).lines();
    let line = lines
        .next()
        .transpose()?
        .ok_or_else(|| std::io::Error::other("worker exited before bind line"))?;
    Ok(line
        .strip_prefix("BOUND addr=")
        .ok_or_else(|| std::io::Error::other(format!("malformed bind line: {line}")))?
        .parse::<std::net::SocketAddr>()?)
}

fn build_worker_binary(package: &str) {
    let status = Command::new("cargo")
        .args(["build", "-p", package])
        .current_dir(workspace_root())
        .status()
        .unwrap();
    assert!(status.success(), "failed to build {package}: {status}");
}

fn worker_binary(name: &str) -> PathBuf {
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    workspace_root()
        .join("target")
        .join("debug")
        .join(format!("{name}{suffix}"))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}
