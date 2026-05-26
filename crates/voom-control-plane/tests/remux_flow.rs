#![expect(
    clippy::panic,
    clippy::unwrap_used,
    reason = "integration test setup should fail loudly with direct assertions"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::NamedTempFile;
use voom_control_plane::ControlPlane;
use voom_control_plane::cases::compliance::ComplianceExecutionOptions;
use voom_control_plane::cases::policy_inputs::PolicyInputFromScanInput;
use voom_control_plane::scan::{ScanPathInput, ScanReportFileStatus};
use voom_core::{FileLocationId, FileVersionId, MediaSnapshotId};
use voom_store::repo::identity::{IdentityRepo, SqliteIdentityRepo};
use voom_test_support::worker::{
    TestWorkerConfig, TestWorkerLaunch, cargo_build_package, target_debug_binary,
};

const REMUX_POLICY: &str = r#"
policy "remux track selection" {
  phase normalize {
    container mkv
    keep audio where lang in [eng, und]
    remove subtitle where forced
    order tracks [video, audio, subtitle]
    defaults audio: first
    defaults subtitle: none
  }
}
"#;

#[tokio::test]
async fn remux_flow_verifies_commits_and_records_result_snapshot() {
    require_command("ffmpeg", &["-version"]);
    require_command("ffprobe", &["-version"]);
    require_command("mkvmerge", &["--version"]);
    cargo_build_package("voom-ffprobe-worker").unwrap();
    cargo_build_package("voom-verify-artifact-worker").unwrap();
    cargo_build_package("voom-mkvtoolnix-worker").unwrap();
    let _ffprobe_guard = hide_stale_fake_ffprobe_sibling();

    let tmp = tempfile::TempDir::new().unwrap();
    let source = tmp.path().join("Movie.mkv");
    generate_remux_fixture(&source);

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
    let source_media_snapshot_id = scanned.media_snapshot_id.unwrap();
    assert_scanned_stream_facts(&url, source_file_version_id, source_media_snapshot_id).await;

    let policy = cp
        .create_policy_document("remux-track-selection", REMUX_POLICY)
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "movie-remux-track-selection".to_owned(),
            file_version_id: source_file_version_id,
            media_snapshot_id: source_media_snapshot_id,
            container: "mkv".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap();

    let plan = cp
        .generate_compliance_report(policy.version.id, input.input_set_id)
        .await
        .unwrap();
    assert_eq!(plan.plan.nodes.len(), 1);
    assert_eq!(plan.plan.nodes[0].operation_kind, "remux");
    assert_eq!(plan.plan.nodes[0].status, voom_plan::NodeStatus::Planned);
    assert_eq!(
        plan.plan.nodes[0].operation_payload["source_media_snapshot_id"],
        source_media_snapshot_id.0
    );

    let mut worker = RemuxWorkerLaunch::start(&cp).await.unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let out_dir = root.join("out");
    let executed = cp
        .execute_compliance_policy_with_options(
            policy.version.id,
            input.input_set_id,
            ComplianceExecutionOptions {
                remux_staging_root: root.join("stage"),
                remux_target_dir: out_dir.clone(),
                ..ComplianceExecutionOptions::default()
            },
        )
        .await
        .unwrap();
    worker.shutdown().unwrap();

    assert_remux_execution_result(&url, &out_dir, &executed).await;
}

trait ScanSummaryExt {
    fn scanned_count(&self) -> u64;
}

impl ScanSummaryExt for voom_control_plane::scan::ScanSummary {
    fn scanned_count(&self) -> u64 {
        self.ingested
    }
}

async fn assert_scanned_stream_facts(
    url: &str,
    file_version_id: FileVersionId,
    media_snapshot_id: MediaSnapshotId,
) {
    let snapshots = SqliteIdentityRepo::new(voom_store::connect(url).await.unwrap())
        .list_media_snapshots_by_version(file_version_id)
        .await
        .unwrap();
    let snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.id == media_snapshot_id)
        .unwrap();
    let streams = snapshot.payload["streams"].as_array().unwrap();
    assert_eq!(
        streams
            .iter()
            .filter(|stream| stream["kind"].as_str() == Some("video"))
            .count(),
        1
    );
    assert!(
        streams
            .iter()
            .filter(|stream| stream["kind"].as_str() == Some("audio"))
            .count()
            >= 2,
        "expected at least two audio streams, got {streams:?}"
    );
    assert!(
        streams
            .iter()
            .filter(|stream| stream["kind"].as_str() == Some("subtitle"))
            .count()
            >= 2
    );
    assert!(streams.iter().all(|stream| {
        stream["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("stream-"))
    }));
    assert!(streams.iter().any(|stream| {
        stream["kind"].as_str() == Some("subtitle") && stream["disposition"]["forced"] == true
    }));
}

async fn assert_remux_execution_result(
    url: &str,
    out_dir: &Path,
    executed: &voom_control_plane::cases::compliance::ComplianceExecuteData,
) {
    let ticket = executed
        .tickets
        .iter()
        .find(|ticket| ticket.operation == "remux")
        .unwrap();
    assert_eq!(ticket.state, "succeeded");
    let result = ticket.result.as_ref().unwrap();
    let staged_artifact_handle_id = result["staged_artifact_handle_id"].as_u64().unwrap();
    let verification_id = result["verification_id"].as_u64().unwrap();
    let commit_record_id = result["commit_record_id"].as_u64().unwrap();
    let result_file_version_id = FileVersionId(result["result_file_version_id"].as_u64().unwrap());
    let result_file_location_id =
        FileLocationId(result["result_file_location_id"].as_u64().unwrap());
    let result_media_snapshot_id =
        MediaSnapshotId(result["result_media_snapshot_id"].as_u64().unwrap());

    assert!(staged_artifact_handle_id > 0);
    assert!(verification_id > 0);
    assert!(commit_record_id > 0);
    assert!(out_dir.join("Movie.remux.mkv").is_file());

    let pool = voom_store::connect(url).await.unwrap();
    assert_row_exists(
        &pool,
        "SELECT COUNT(*) FROM artifact_handles WHERE id = ?",
        staged_artifact_handle_id,
    )
    .await;
    assert_row_exists(
        &pool,
        "SELECT COUNT(*) FROM artifact_verifications WHERE id = ?",
        verification_id,
    )
    .await;
    assert_row_exists(
        &pool,
        "SELECT COUNT(*) FROM artifact_commit_records WHERE id = ?",
        commit_record_id,
    )
    .await;
    assert_row_exists(
        &pool,
        "SELECT COUNT(*) FROM file_locations WHERE id = ?",
        result_file_location_id.0,
    )
    .await;

    let snapshots = SqliteIdentityRepo::new(pool)
        .list_media_snapshots_by_version(result_file_version_id)
        .await
        .unwrap();
    let result_snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.id == result_media_snapshot_id)
        .unwrap();
    assert!(result_snapshot.payload.get("snapshot_kind").is_none());
    assert_eq!(result_snapshot.payload["format"], "sprint10-v1");
    assert_eq!(result_snapshot.payload["probe"]["provider"], "ffprobe");
    assert_eq!(
        result_snapshot.payload["container"]["format_name"],
        "matroska,webm"
    );
    let streams = result_snapshot.payload["streams"].as_array().unwrap();
    assert!(streams.iter().all(|stream| {
        stream["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("stream-"))
    }));
    assert_eq!(
        streams
            .iter()
            .filter(|stream| stream["kind"].as_str() == Some("video"))
            .count(),
        1
    );
    assert!(streams.iter().any(|stream| {
        stream["kind"].as_str() == Some("audio")
            && stream["language"].as_str() == Some("eng")
            && stream["disposition"]["default"] == true
    }));
    assert!(!streams.iter().any(|stream| {
        stream["kind"].as_str() == Some("audio") && stream["language"].as_str() == Some("spa")
    }));
    assert!(streams.iter().any(|stream| {
        stream["kind"].as_str() == Some("subtitle")
            && stream["language"].as_str() == Some("eng")
            && stream["disposition"]["forced"] != true
    }));
    assert!(!streams.iter().any(|stream| {
        stream["kind"].as_str() == Some("subtitle") && stream["disposition"]["forced"] == true
    }));
}

async fn assert_row_exists(pool: &sqlx::SqlitePool, sql: &str, id: u64) {
    let id = i64::try_from(id).unwrap();
    let count: i64 = sqlx::query_scalar(sql)
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

fn require_command(program: &str, args: &[&str]) {
    let output = Command::new(program).args(args).output().unwrap_or_else(|err| {
        panic!(
            "required media tool `{program}` is unavailable; install it for Sprint 13 remux integration tests: {err}"
        )
    });
    assert!(
        output.status.success(),
        "required media tool `{program}` failed setup check with {}: stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn hide_stale_fake_ffprobe_sibling() -> Option<FfprobeSiblingGuard> {
    let path = target_debug_binary("ffprobe");
    let bytes = std::fs::read(&path).ok()?;
    if !bytes
        .windows(b"ffprobe version test-helper".len())
        .any(|window| window == b"ffprobe version test-helper")
    {
        return None;
    }
    let hidden = path.with_file_name(format!("ffprobe.remux-flow-hidden-{}", std::process::id()));
    std::fs::rename(&path, &hidden).unwrap_or_else(|err| {
        panic!(
            "cannot hide stale test-helper ffprobe sibling {} before real scan: {err}",
            path.display()
        )
    });
    Some(FfprobeSiblingGuard { path, hidden })
}

struct FfprobeSiblingGuard {
    path: PathBuf,
    hidden: PathBuf,
}

impl Drop for FfprobeSiblingGuard {
    fn drop(&mut self) {
        if self.hidden.exists() && !self.path.exists() {
            let _ = std::fs::rename(&self.hidden, &self.path);
        }
    }
}

fn generate_remux_fixture(path: &Path) {
    let dir = path.parent().unwrap();
    let subtitle = dir.join("english.srt");
    let forced_subtitle = dir.join("forced.srt");
    std::fs::write(
        &subtitle,
        "1\n00:00:00,000 --> 00:00:00,900\nEnglish subtitle\n",
    )
    .unwrap();
    std::fs::write(
        &forced_subtitle,
        "1\n00:00:00,000 --> 00:00:00,900\nForced subtitle\n",
    )
    .unwrap();

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
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=660:sample_rate=48000",
            "-i",
            subtitle.to_str().unwrap(),
            "-i",
            forced_subtitle.to_str().unwrap(),
            "-t",
            "1",
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-map",
            "2:a:0",
            "-map",
            "3:s:0",
            "-map",
            "4:s:0",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-c:s",
            "srt",
            "-metadata:s:a:0",
            "language=eng",
            "-metadata:s:a:1",
            "language=spa",
            "-metadata:s:s:0",
            "language=eng",
            "-metadata:s:s:1",
            "language=spa",
            "-disposition:s:1",
            "forced",
            path.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(
        status.success(),
        "ffmpeg remux fixture generation failed: {status}"
    );
}

struct RemuxWorkerLaunch {
    inner: TestWorkerLaunch,
}

impl RemuxWorkerLaunch {
    async fn start(cp: &ControlPlane) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            inner: TestWorkerLaunch::start(
                cp,
                TestWorkerConfig::synthetic(
                    target_debug_binary("voom-mkvtoolnix-worker"),
                    "e2e-mkvtoolnix-remux",
                    "control-plane-remux-e2e-secret",
                    "remux",
                ),
            )
            .await?,
        })
    }

    fn shutdown(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.inner.shutdown()
    }
}
