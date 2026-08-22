#![expect(
    clippy::panic,
    clippy::unwrap_used,
    reason = "integration test setup should fail loudly with direct assertions"
)]

use std::path::Path;
use std::process::Command;

use voom_control_plane::ControlPlane;
use voom_control_plane::policy::{ComplianceExecutionOptions, PolicyInputFromScanInput};
use voom_control_plane::scan::{RootScanOutcome, ScanReportFileStatus};
use voom_core::{FileLocationId, FileVersionId, MediaSnapshotId};
use voom_plan::PlanOperationKind;
use voom_store::repo::media::identity::{MediaSnapshotRepo, SqliteIdentityRepo};
use voom_test_support::TempDatabase;
use voom_test_support::worker::{
    TestWorkerConfig, TestWorkerLaunch, cargo_build_package, hide_stale_fake_ffprobe_sibling,
    target_debug_binary,
};

const REMUX_POLICY: &str = r#"
policy "remux track selection" {
  config {
    languages: ["spa", "eng"]
  }
  phase normalize {
    container mkv
    remove audio where commentary
    keep attachment where font
    remove subtitle where forced
    order tracks [video, audio, subtitle] where language == "spa"
    defaults audio: best
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
    let _ffprobe_guard = hide_stale_fake_ffprobe_sibling("remux-flow").unwrap();

    let tmp = tempfile::TempDir::new().unwrap();
    let source = tmp.path().join("Movie.mkv");
    generate_remux_fixture(&source);

    let db = TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    voom_store::test_support::seed_test_storage_root(&pool)
        .await
        .unwrap();
    voom_store::test_support::set_test_storage_root_path(&pool, tmp.path())
        .await
        .unwrap();
    // Background stand-in for the storage-owner agent (ADR 0074): drives the
    // fenced commit intent so non-blocked commits converge.
    voom_test_support::commit_node::install_and_spawn_driver(&pool);
    let cp = ControlPlane::open_with_pool(pool, std::sync::Arc::new(voom_core::SystemClock))
        .await
        .unwrap()
        .with_local_node_id(Some(voom_core::NodeId(9_000_001)));

    let outcome = cp
        .scan_library_root(voom_store::test_support::TEST_STORAGE_ROOT_ID)
        .await
        .unwrap();
    let RootScanOutcome::Scanned(scan) = outcome else {
        unreachable!("active local test root must scan")
    };
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
    assert_eq!(plan.plan.nodes[0].operation_kind, PlanOperationKind::Remux);
    assert_eq!(plan.plan.nodes[0].status, voom_plan::NodeStatus::Planned);
    assert_eq!(
        plan.plan.nodes[0].operation_payload["source_media_snapshot_id"],
        source_media_snapshot_id.0
    );
    assert_eq!(
        plan.plan.nodes[0].operation_payload["head_snapshot_stream_id"],
        plan.plan.nodes[0].operation_payload["defaults"][0]["selected_snapshot_stream_id"]
    );
    assert!(
        plan.plan.nodes[0].operation_payload["head_snapshot_stream_id"]
            .as_str()
            .is_some()
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

    let (result_version, result_snapshot) =
        assert_remux_execution_result(&url, &out_dir, &executed).await;
    assert_result_replans_from_authoritative_snapshot(
        &cp,
        policy.version.id,
        result_version,
        result_snapshot,
    )
    .await;
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
        1,
        "unexpected normalized source streams: {streams:#?}"
    );
    assert_eq!(
        streams
            .iter()
            .filter(|stream| stream["kind"].as_str() == Some("audio"))
            .count(),
        3,
        "unexpected source audio streams: {streams:?}"
    );
    assert!(
        streams
            .iter()
            .filter(|stream| stream["kind"].as_str() == Some("subtitle"))
            .count()
            >= 2
    );
    assert_eq!(
        streams
            .iter()
            .filter(|stream| stream["kind"].as_str() == Some("attachment"))
            .count(),
        2
    );
    assert!(streams.iter().all(|stream| {
        stream["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("stream-"))
    }));
    assert!(streams.iter().any(|stream| {
        stream["kind"].as_str() == Some("subtitle") && stream["disposition"]["forced"] == true
    }));
    assert!(streams.iter().any(|stream| {
        stream["kind"].as_str() == Some("audio") && stream["disposition"]["commentary"] == true
    }));
    assert!(streams.iter().any(|stream| {
        stream["kind"].as_str() == Some("audio") && stream["disposition"]["commentary"] == false
    }));
    for language in ["eng", "spa"] {
        assert!(streams.iter().any(|stream| {
            stream["kind"].as_str() == Some("audio")
                && stream["language"].as_str() == Some(language)
                && stream["disposition"]["commentary"] == false
        }));
    }
    assert!(streams.iter().any(|stream| {
        stream["kind"].as_str() == Some("attachment")
            && stream["filename"] == "OpenSans.ttf"
            && stream["mime_type"] == "font/ttf"
    }));
    assert!(streams.iter().any(|stream| {
        stream["kind"].as_str() == Some("attachment")
            && stream["filename"] == "cover.bin"
            && stream["mime_type"] == "application/octet-stream"
    }));
}

async fn assert_remux_execution_result(
    url: &str,
    out_dir: &Path,
    executed: &voom_control_plane::policy::ComplianceExecuteData,
) -> (FileVersionId, MediaSnapshotId) {
    let result = ticket_result(url, executed.summary.job_id, "remux").await;
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
    let output_path = out_dir.join("Movie.remux.mkv");
    assert!(output_path.is_file());
    assert_mkvmerge_attachment_inventory(&output_path);

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
    assert_remux_result_streams(streams);
    (result_file_version_id, result_media_snapshot_id)
}

fn assert_remux_result_streams(streams: &[serde_json::Value]) {
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
    assert_eq!(
        streams
            .iter()
            .map(|stream| stream["kind"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["audio", "video", "audio", "subtitle", "attachment"]
    );
    let audio = streams
        .iter()
        .filter(|stream| stream["kind"].as_str() == Some("audio"))
        .collect::<Vec<_>>();
    assert_eq!(audio.len(), 2);
    assert_eq!(audio[0]["language"], "spa");
    assert_eq!(audio[0]["disposition"]["commentary"], false);
    assert_eq!(audio[0]["disposition"]["default"], true);
    assert_eq!(audio[1]["language"], "eng");
    assert_eq!(audio[1]["disposition"]["commentary"], false);
    assert_eq!(audio[1]["disposition"]["default"], false);
    assert!(!streams.iter().any(|stream| {
        stream["kind"].as_str() == Some("audio") && stream["disposition"]["commentary"] == true
    }));
    assert!(streams.iter().any(|stream| {
        stream["kind"].as_str() == Some("subtitle")
            && stream["language"].as_str() == Some("eng")
            && stream["disposition"]["default"] == false
            && stream["disposition"]["forced"] == false
    }));
    assert!(!streams.iter().any(|stream| {
        stream["kind"].as_str() == Some("subtitle") && stream["disposition"]["forced"] == true
    }));
    let attachment = streams
        .iter()
        .filter(|stream| stream["kind"].as_str() == Some("attachment"))
        .collect::<Vec<_>>();
    assert_eq!(attachment.len(), 1);
    assert_eq!(attachment[0]["filename"], "OpenSans.ttf");
    assert_eq!(attachment[0]["mime_type"], "font/ttf");
}

fn assert_mkvmerge_attachment_inventory(path: &Path) {
    let output = Command::new("mkvmerge")
        .arg("--identify")
        .arg("--identification-format")
        .arg("json")
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "mkvmerge output inspection failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let identify: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let attachments = identify["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0]["file_name"], "OpenSans.ttf");
    assert_eq!(attachments[0]["content_type"], "font/ttf");
}

async fn assert_result_replans_from_authoritative_snapshot(
    cp: &ControlPlane,
    policy_version_id: voom_core::PolicyVersionId,
    result_file_version_id: FileVersionId,
    result_media_snapshot_id: MediaSnapshotId,
) {
    let result_input = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "movie-remux-result".to_owned(),
            file_version_id: result_file_version_id,
            media_snapshot_id: result_media_snapshot_id,
            container: "mkv".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap();
    let report = cp
        .generate_compliance_report(policy_version_id, result_input.input_set_id)
        .await
        .unwrap();

    assert_eq!(report.plan.nodes.len(), 1);
    assert_eq!(report.plan.nodes[0].status, voom_plan::NodeStatus::NoOp);
    assert_eq!(
        report.plan.nodes[0].observed_state.as_ref().unwrap()["container"],
        "mkv"
    );
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

/// Read a succeeded operation ticket's durable result JSON for a job. The flat
/// `tickets` field was removed from `ComplianceExecuteData`; the tickets a run
/// executed remain queryable in the `tickets` table (`state = 'succeeded'`
/// folds in the prior `ticket.state` assertion).
async fn ticket_result(url: &str, job_id: u64, operation: &str) -> serde_json::Value {
    let pool = voom_store::connect(url).await.unwrap();
    let kind = format!("synthetic.workflow.operation.{operation}");
    let result: String = sqlx::query_scalar(
        "SELECT result FROM tickets \
         WHERE job_id = ? AND kind = ? AND state = 'succeeded' AND result IS NOT NULL \
         ORDER BY id ASC LIMIT 1",
    )
    .bind(i64::try_from(job_id).unwrap())
    .bind(kind)
    .fetch_one(&pool)
    .await
    .unwrap();
    serde_json::from_str(&result).unwrap()
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

fn generate_remux_fixture(path: &Path) {
    let dir = path.parent().unwrap();
    let base = dir.join("Movie.base.mkv");
    let subtitle = dir.join("english.srt");
    let forced_subtitle = dir.join("forced.srt");
    let font = dir.join("OpenSans.ttf");
    let cover = dir.join("cover.bin");
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
    std::fs::write(&font, b"generated font attachment fixture").unwrap();
    std::fs::write(&cover, b"generated cover attachment fixture").unwrap();

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
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=880:sample_rate=48000",
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
            "3:a:0",
            "-map",
            "4:s:0",
            "-map",
            "5:s:0",
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
            "-metadata:s:a:2",
            "language=eng",
            "-disposition:a:2",
            "comment",
            "-metadata:s:s:0",
            "language=eng",
            "-metadata:s:s:1",
            "language=spa",
            "-disposition:s:1",
            "forced",
            base.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(
        status.success(),
        "ffmpeg remux fixture generation failed: {status}"
    );
    attach_remux_fixture(path, &base, &font, &cover);
    for generated_input in [&base, &subtitle, &forced_subtitle, &font, &cover] {
        std::fs::remove_file(generated_input).unwrap();
    }
}

fn attach_remux_fixture(path: &Path, base: &Path, font: &Path, cover: &Path) {
    let status = Command::new("mkvmerge")
        .args([
            "--output",
            path.to_str().unwrap(),
            "--attachment-name",
            "OpenSans.ttf",
            "--attachment-mime-type",
            "font/ttf",
            "--attach-file",
            font.to_str().unwrap(),
            "--attachment-name",
            "cover.bin",
            "--attachment-mime-type",
            "application/octet-stream",
            "--attach-file",
            cover.to_str().unwrap(),
            base.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(
        status.success(),
        "mkvmerge attachment fixture generation failed: {status}"
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
