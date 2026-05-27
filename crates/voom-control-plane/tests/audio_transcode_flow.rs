#![expect(
    clippy::panic,
    clippy::unwrap_used,
    reason = "integration test setup should fail loudly with direct assertions"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
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

const AUDIO_TRANSCODE_POLICY: &str = r#"
policy "audio transcode opus" {
  phase normalize {
    transcode audio to opus where lang in [eng, jpn]
  }
}
"#;

static AUDIO_TRANSCODE_FLOW_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn audio_transcode_flow_verifies_commits_and_replans_result_as_no_op() {
    let _guard = AUDIO_TRANSCODE_FLOW_LOCK.lock().await;
    require_command("ffmpeg", &["-version"]);
    require_command("ffprobe", &["-version"]);
    cargo_build_package("voom-ffprobe-worker").unwrap();
    cargo_build_package("voom-verify-artifact-worker").unwrap();
    cargo_build_package("voom-ffmpeg-worker").unwrap();
    let _ffprobe_guard = hide_stale_fake_ffprobe_sibling();

    let tmp = tempdir_in_repo();
    let source = tmp.path().join("Movie.mkv");
    generate_audio_fixture(&source);

    let (cp, url, _db) = control_plane().await;
    let scanned = scan_fixture(&cp, &source).await;
    let source_snapshot_id =
        record_augmented_audio_snapshot(&cp, &url, scanned.file_version_id, scanned.snapshot_id)
            .await;
    assert_source_audio_facts(&url, scanned.file_version_id, source_snapshot_id).await;

    let policy = cp
        .create_policy_document("audio-transcode-opus", AUDIO_TRANSCODE_POLICY)
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "movie-audio-transcode-opus".to_owned(),
            file_version_id: scanned.file_version_id,
            media_snapshot_id: source_snapshot_id,
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
    assert_eq!(plan.plan.nodes[0].operation_kind, "transcode_audio");
    assert_eq!(plan.plan.nodes[0].status, voom_plan::NodeStatus::Planned);
    assert_eq!(
        plan.plan.nodes[0].operation_payload["source_media_snapshot_id"],
        source_snapshot_id.0
    );

    let mut worker = AudioTranscodeWorkerLaunch::start(&cp).await.unwrap();
    let out_dir = tmp.path().join("out");
    let executed = cp
        .execute_compliance_policy_with_options(
            policy.version.id,
            input.input_set_id,
            ComplianceExecutionOptions {
                audio_staging_root: tmp.path().join("stage"),
                audio_target_dir: out_dir.clone(),
                ..ComplianceExecutionOptions::default()
            },
        )
        .await
        .unwrap();
    worker.shutdown().unwrap();

    let (result_file_version_id, result_media_snapshot_id) =
        assert_audio_transcode_execution_result(&url, &out_dir, &executed).await;
    assert_result_replans_as_no_op(
        &cp,
        policy.version.id,
        result_file_version_id,
        result_media_snapshot_id,
    )
    .await;
}

#[tokio::test]
async fn audio_transcode_existing_target_path_fails_before_success_reporting() {
    let _guard = AUDIO_TRANSCODE_FLOW_LOCK.lock().await;
    require_command("ffmpeg", &["-version"]);
    require_command("ffprobe", &["-version"]);
    cargo_build_package("voom-ffprobe-worker").unwrap();
    cargo_build_package("voom-verify-artifact-worker").unwrap();
    cargo_build_package("voom-ffmpeg-worker").unwrap();
    let _ffprobe_guard = hide_stale_fake_ffprobe_sibling();

    let tmp = tempdir_in_repo();
    let source = tmp.path().join("Movie.mkv");
    generate_audio_fixture(&source);

    let (cp, url, _db) = control_plane().await;
    let scanned = scan_fixture(&cp, &source).await;
    let source_snapshot_id =
        record_augmented_audio_snapshot(&cp, &url, scanned.file_version_id, scanned.snapshot_id)
            .await;
    let policy = cp
        .create_policy_document("audio-transcode-opus", AUDIO_TRANSCODE_POLICY)
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "movie-audio-transcode-existing-target".to_owned(),
            file_version_id: scanned.file_version_id,
            media_snapshot_id: source_snapshot_id,
            container: "mkv".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap();

    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    std::fs::write(out_dir.join("Movie.audio-opus.mkv"), b"existing").unwrap();

    let mut worker = AudioTranscodeWorkerLaunch::start(&cp).await.unwrap();
    let err = cp
        .execute_compliance_policy_with_options(
            policy.version.id,
            input.input_set_id,
            ComplianceExecutionOptions {
                audio_staging_root: tmp.path().join("stage"),
                audio_target_dir: out_dir,
                ..ComplianceExecutionOptions::default()
            },
        )
        .await
        .unwrap_err();
    worker.shutdown().unwrap();

    assert!(
        err.source
            .to_string()
            .contains("audio target path already exists")
    );
    let partial = err.partial.unwrap();
    assert_eq!(partial.execution.failure_count, 1);
    assert!(
        !partial
            .tickets
            .iter()
            .any(|ticket| ticket.operation == "transcode_audio" && ticket.state == "succeeded")
    );
}

fn tempdir_in_repo() -> tempfile::TempDir {
    tempfile::TempDir::new_in(std::env::current_dir().unwrap()).unwrap()
}

struct ScannedFixture {
    file_version_id: FileVersionId,
    snapshot_id: MediaSnapshotId,
}

async fn control_plane() -> (ControlPlane, String, NamedTempFile) {
    let db = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = ControlPlane::open_with_pool(pool, std::sync::Arc::new(voom_core::SystemClock))
        .await
        .unwrap();
    (cp, url, db)
}

async fn scan_fixture(cp: &ControlPlane, source: &Path) -> ScannedFixture {
    let scan = cp
        .scan_path(ScanPathInput {
            path: source.to_path_buf(),
        })
        .await
        .unwrap();
    assert_eq!(scan.summary.scanned_count(), 1);
    let scanned = scan
        .files
        .iter()
        .find(|file| file.status == ScanReportFileStatus::Scanned)
        .unwrap();
    ScannedFixture {
        file_version_id: scanned.file_version_id.unwrap(),
        snapshot_id: scanned.media_snapshot_id.unwrap(),
    }
}

async fn record_augmented_audio_snapshot(
    cp: &ControlPlane,
    url: &str,
    file_version_id: FileVersionId,
    scanned_snapshot_id: MediaSnapshotId,
) -> MediaSnapshotId {
    let snapshots = SqliteIdentityRepo::new(voom_store::connect(url).await.unwrap())
        .list_media_snapshots_by_version(file_version_id)
        .await
        .unwrap();
    let scanned = snapshots
        .iter()
        .find(|snapshot| snapshot.id == scanned_snapshot_id)
        .unwrap();
    let payload = audio_snapshot_with_preservation_facts(scanned.payload.clone());
    cp.record_media_snapshot(
        file_version_id,
        scanned.probed_by,
        payload,
        cp.clock().now(),
    )
    .await
    .unwrap()
    .id
}

fn audio_snapshot_with_preservation_facts(mut payload: Value) -> Value {
    let streams = payload["streams"].as_array_mut().unwrap();
    let mut audio_index = 0;
    for stream in streams {
        if stream["kind"].as_str() != Some("audio") {
            continue;
        }
        match audio_index {
            0 => {
                stream["language"] = Value::String("eng".to_owned());
                stream["title"] = Value::String("Main".to_owned());
                stream["disposition"]["default"] = Value::Bool(true);
                stream["disposition"]["commentary"] = Value::Bool(false);
            }
            1 => {
                stream["language"] = Value::String("jpn".to_owned());
                stream["title"] = Value::String("Secondary".to_owned());
                stream["disposition"]["default"] = Value::Bool(false);
                stream["disposition"]["commentary"] = Value::Bool(false);
            }
            other => panic!("unexpected audio stream count in fixture: {other}"),
        }
        audio_index += 1;
    }
    assert_eq!(audio_index, 2);
    payload
}

async fn assert_source_audio_facts(
    url: &str,
    file_version_id: FileVersionId,
    media_snapshot_id: MediaSnapshotId,
) {
    let snapshot = media_snapshot(url, file_version_id, media_snapshot_id).await;
    let streams = snapshot["streams"].as_array().unwrap();
    assert!(streams.iter().any(|stream| {
        stream["kind"].as_str() == Some("audio")
            && stream["codec_name"].as_str() == Some("aac")
            && stream["language"].as_str() == Some("eng")
            && stream["title"].as_str() == Some("Main")
            && stream["disposition"]["default"] == true
            && stream["disposition"]["commentary"] == false
    }));
    assert!(streams.iter().any(|stream| {
        stream["kind"].as_str() == Some("audio")
            && stream["codec_name"].as_str() == Some("aac")
            && stream["language"].as_str() == Some("jpn")
            && stream["title"].as_str() == Some("Secondary")
            && stream["disposition"]["default"] == false
            && stream["disposition"]["commentary"] == false
    }));
}

async fn assert_audio_transcode_execution_result(
    url: &str,
    out_dir: &Path,
    executed: &voom_control_plane::cases::compliance::ComplianceExecuteData,
) -> (FileVersionId, MediaSnapshotId) {
    let ticket = executed
        .tickets
        .iter()
        .find(|ticket| ticket.operation == "transcode_audio")
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
    assert!(out_dir.join("Movie.audio-opus.mkv").is_file());
    assert_eq!(
        result["target_path"].as_str(),
        Some(out_dir.join("Movie.audio-opus.mkv").to_str().unwrap())
    );

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
    assert_audio_transcode_succeeded_event(
        &pool,
        staged_artifact_handle_id,
        &json!(["stream-1", "stream-2"]),
        &json!(["opus", "opus"]),
    )
    .await;

    let result_snapshot =
        media_snapshot(url, result_file_version_id, result_media_snapshot_id).await;
    assert_eq!(result_snapshot["container"]["format_name"], "matroska,webm");
    let streams = result_snapshot["streams"].as_array().unwrap();
    assert!(streams.iter().any(|stream| {
        stream["kind"].as_str() == Some("audio")
            && stream["id"].as_str() == Some("stream-1")
            && stream["codec_name"].as_str() == Some("opus")
            && stream["language"].as_str() == Some("eng")
            && stream["disposition"]["default"] == true
    }));
    assert!(streams.iter().any(|stream| {
        stream["kind"].as_str() == Some("audio")
            && stream["id"].as_str() == Some("stream-2")
            && stream["codec_name"].as_str() == Some("opus")
            && stream["language"].as_str() == Some("jpn")
    }));

    (result_file_version_id, result_media_snapshot_id)
}

async fn assert_audio_transcode_succeeded_event(
    pool: &sqlx::SqlitePool,
    artifact_handle_id: u64,
    expected_stream_ids: &Value,
    expected_codecs: &Value,
) {
    let payload: String = sqlx::query_scalar(
        "SELECT payload FROM events \
         WHERE kind = ? AND subject_id = ? \
         ORDER BY event_id DESC LIMIT 1",
    )
    .bind("artifact.audio_transcode_succeeded")
    .bind(i64::try_from(artifact_handle_id).unwrap())
    .fetch_one(pool)
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["output_container"], "mkv");
    assert_eq!(
        &payload["selected_snapshot_stream_ids"],
        expected_stream_ids
    );
    assert_eq!(&payload["output_audio_codecs"], expected_codecs);
    assert_eq!(
        payload["selected_streams"][0]["snapshot_stream_id"],
        "stream-1"
    );
    assert_eq!(
        payload["selected_streams"][1]["snapshot_stream_id"],
        "stream-2"
    );
}

async fn assert_result_replans_as_no_op(
    cp: &ControlPlane,
    policy_version_id: voom_core::PolicyVersionId,
    result_file_version_id: FileVersionId,
    result_media_snapshot_id: MediaSnapshotId,
) {
    let result_input = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "movie-audio-transcode-result".to_owned(),
            file_version_id: result_file_version_id,
            media_snapshot_id: result_media_snapshot_id,
            container: "mkv".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap();
    let result_plan = cp
        .generate_compliance_report(policy_version_id, result_input.input_set_id)
        .await
        .unwrap();
    assert_eq!(result_plan.plan.nodes.len(), 1);
    assert_eq!(
        result_plan.plan.nodes[0].status,
        voom_plan::NodeStatus::NoOp
    );
}

async fn media_snapshot(
    url: &str,
    file_version_id: FileVersionId,
    media_snapshot_id: MediaSnapshotId,
) -> Value {
    let snapshots = SqliteIdentityRepo::new(voom_store::connect(url).await.unwrap())
        .list_media_snapshots_by_version(file_version_id)
        .await
        .unwrap();
    snapshots
        .iter()
        .find(|snapshot| snapshot.id == media_snapshot_id)
        .unwrap()
        .payload
        .clone()
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

trait ScanSummaryExt {
    fn scanned_count(&self) -> u64;
}

impl ScanSummaryExt for voom_control_plane::scan::ScanSummary {
    fn scanned_count(&self) -> u64 {
        self.ingested
    }
}

fn require_command(program: &str, args: &[&str]) {
    let output = Command::new(program).args(args).output().unwrap_or_else(|err| {
        panic!("required media tool `{program}` is unavailable for audio transcode integration tests: {err}")
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
    let hidden = path.with_file_name(format!(
        "ffprobe.audio-transcode-flow-hidden-{}",
        std::process::id()
    ));
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

fn generate_audio_fixture(path: &Path) {
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
            "-t",
            "1",
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-map",
            "2:a:0",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-metadata:s:a:0",
            "language=eng",
            "-metadata:s:a:0",
            "title=Main",
            "-metadata:s:a:1",
            "language=jpn",
            "-metadata:s:a:1",
            "title=Secondary",
            "-disposition:a:0",
            "default",
            "-disposition:a:1",
            "0",
            path.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(
        status.success(),
        "ffmpeg audio fixture generation failed: {status}"
    );
}

struct AudioTranscodeWorkerLaunch {
    inner: TestWorkerLaunch,
}

impl AudioTranscodeWorkerLaunch {
    async fn start(cp: &ControlPlane) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            inner: TestWorkerLaunch::start(
                cp,
                TestWorkerConfig::synthetic(
                    target_debug_binary("voom-ffmpeg-worker"),
                    "e2e-ffmpeg-audio-transcode",
                    "control-plane-audio-transcode-e2e-secret",
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
