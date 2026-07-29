#![expect(
    clippy::panic,
    clippy::unwrap_used,
    reason = "integration test setup should fail loudly with direct assertions"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use voom_control_plane::ControlPlane;
use voom_control_plane::policy::{ComplianceExecutionOptions, PolicyInputFromScanInput};
use voom_control_plane::scan::{ScanPathInput, ScanReportFileStatus};
use voom_core::{FileAssetId, FileVersionId, JobId, MediaSnapshotId};
use voom_plan::PlanOperationKind;
use voom_policy::{MediaSnapshotInput, PolicyInputSetDraft, PolicyInputSourceKind, TargetRef};
use voom_store::repo::bundles::{BundleMemberRole, SqliteBundleRepo};
use voom_store::repo::identity::{IdentityRepo, SqliteIdentityRepo};
use voom_test_support::TempDatabase;
use voom_test_support::worker::{
    TestWorkerConfig, TestWorkerLaunch, cargo_build_package, hide_stale_fake_ffprobe_sibling,
    target_debug_binary,
};

const EXTRACT_COMMENTARY_POLICY: &str = r#"
policy "extract commentary audio" {
  phase normalize {
    extract audio where commentary
  }
}
"#;

const EXTRACT_ENGLISH_POLICY: &str = r#"
policy "extract english audio" {
  phase normalize {
    extract audio where language in ["eng"]
  }
}
"#;

static AUDIO_EXTRACT_FLOW_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn audio_extract_flow_verifies_commits_and_adds_sidecar_to_source_bundle() {
    let _guard = AUDIO_EXTRACT_FLOW_LOCK.lock().await;
    require_command("ffmpeg", &["-version"]);
    cargo_build_package("voom-ffprobe-worker").unwrap();
    cargo_build_package("voom-verify-artifact-worker").unwrap();
    cargo_build_package("voom-ffmpeg-worker").unwrap();
    let _ffprobe_guard = hide_stale_fake_ffprobe_sibling("audio-extract-flow").unwrap();

    let tmp = tempdir_in_repo();
    let source = tmp.path().join("Movie.mkv");
    generate_audio_extract_fixture(&source, CommentaryFixture::SingleMatch);

    let db = TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp =
        ControlPlane::open_with_pool(pool.clone(), std::sync::Arc::new(voom_core::SystemClock))
            .await
            .unwrap();

    let scanned = scan_source(&cp, &source).await;
    let scanned = enrich_audio_snapshot_for_extract(&cp, &url, scanned).await;
    assert_audio_snapshot_has_single_commentary_match(
        &url,
        scanned.file_version_id,
        scanned.snapshot_id,
    )
    .await;
    let policy = cp
        .create_policy_document("extract-commentary-audio", EXTRACT_COMMENTARY_POLICY)
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "movie-audio-extract-commentary".to_owned(),
            file_version_id: scanned.file_version_id,
            media_snapshot_id: scanned.snapshot_id,
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
    assert_eq!(
        plan.plan.nodes[0].operation_kind,
        PlanOperationKind::ExtractAudio
    );
    assert_eq!(plan.plan.nodes[0].status, voom_plan::NodeStatus::Planned);

    let mut worker = ExtractAudioWorkerLaunch::start(&cp).await.unwrap();
    let out_dir = tmp.path().join("out");
    let executed = cp
        .execute_compliance_policy_with_options(
            policy.version.id,
            input.input_set_id,
            ComplianceExecutionOptions {
                audio_staging_root: tmp.path().join("audio-stage"),
                audio_target_dir: out_dir.clone(),
                ..ComplianceExecutionOptions::default()
            },
        )
        .await
        .unwrap();
    worker.shutdown().unwrap();

    let outputs =
        assert_extract_execution_result(&url, &out_dir, scanned.file_version_id, &executed, 1)
            .await;
    assert_produced_audio_facts(&outputs, &["Commentary"]);
}

#[tokio::test]
async fn audio_extract_multi_match_publishes_ordered_media_and_lineage() {
    let _guard = AUDIO_EXTRACT_FLOW_LOCK.lock().await;
    require_command("ffmpeg", &["-version"]);
    cargo_build_package("voom-ffprobe-worker").unwrap();
    cargo_build_package("voom-verify-artifact-worker").unwrap();
    cargo_build_package("voom-ffmpeg-worker").unwrap();
    let _ffprobe_guard = hide_stale_fake_ffprobe_sibling("audio-extract-flow").unwrap();

    let tmp = tempdir_in_repo();
    let source = tmp.path().join("Movie.mkv");
    generate_audio_extract_fixture(&source, CommentaryFixture::SingleMatch);

    let db = TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp =
        ControlPlane::open_with_pool(pool.clone(), std::sync::Arc::new(voom_core::SystemClock))
            .await
            .unwrap();

    let scanned = scan_source(&cp, &source).await;
    let scanned = enrich_audio_snapshot_for_extract(&cp, &url, scanned).await;
    let policy = cp
        .create_policy_document("extract-english-audio", EXTRACT_ENGLISH_POLICY)
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "movie-audio-extract-english".to_owned(),
            file_version_id: scanned.file_version_id,
            media_snapshot_id: scanned.snapshot_id,
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
    assert_eq!(
        plan.plan.nodes[0].operation_kind,
        PlanOperationKind::ExtractAudio
    );
    assert_eq!(plan.plan.nodes[0].status, voom_plan::NodeStatus::Planned);

    let mut worker = ExtractAudioWorkerLaunch::start(&cp).await.unwrap();
    let out_dir = tmp.path().join("out");
    let executed = cp
        .execute_compliance_policy_with_options(
            policy.version.id,
            input.input_set_id,
            ComplianceExecutionOptions {
                audio_staging_root: tmp.path().join("audio-stage"),
                audio_target_dir: out_dir.clone(),
                ..ComplianceExecutionOptions::default()
            },
        )
        .await
        .unwrap();
    worker.shutdown().unwrap();

    let outputs =
        assert_extract_execution_result(&url, &out_dir, scanned.file_version_id, &executed, 2)
            .await;
    assert_ne!(outputs[0].file_name(), outputs[1].file_name());
    assert_ne!(
        executed.audio_extract_outputs[0].output_id(),
        executed.audio_extract_outputs[1].output_id()
    );
    assert_eq!(
        executed.audio_extract_outputs[0].source_provider_stream_index(),
        Some(1)
    );
    assert_eq!(
        executed.audio_extract_outputs[1].source_provider_stream_index(),
        Some(2)
    );
    let post_run = cp
        .read_compliance_run_report(JobId(executed.summary.job_id))
        .await
        .unwrap();
    assert_eq!(
        post_run.audio_extract_outputs,
        executed.audio_extract_outputs
    );
    assert_produced_audio_facts(&outputs, &["Main", "Commentary"]);
    assert_extract_lineage(
        &pool,
        scanned.file_version_id,
        scanned.snapshot_id,
        &executed.audio_extract_outputs,
    )
    .await;
}

#[tokio::test]
async fn duplicate_basename_sidecars_keep_their_source_subtrees() {
    let _guard = AUDIO_EXTRACT_FLOW_LOCK.lock().await;
    require_command("ffmpeg", &["-version"]);
    cargo_build_package("voom-ffprobe-worker").unwrap();
    cargo_build_package("voom-verify-artifact-worker").unwrap();
    cargo_build_package("voom-ffmpeg-worker").unwrap();
    let _ffprobe_guard = hide_stale_fake_ffprobe_sibling("audio-extract-flow").unwrap();
    let tmp = tempdir_in_repo();
    let first_path = tmp.path().join("show-a").join("Movie.mkv");
    let second_path = tmp.path().join("show-b").join("Movie.mkv");
    std::fs::create_dir_all(first_path.parent().unwrap()).unwrap();
    std::fs::create_dir_all(second_path.parent().unwrap()).unwrap();
    generate_audio_extract_fixture(&first_path, CommentaryFixture::SingleMatch);
    generate_audio_extract_fixture(&second_path, CommentaryFixture::AlternateSingleMatch);
    let db = TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = ControlPlane::open_with_pool(pool, std::sync::Arc::new(voom_core::SystemClock))
        .await
        .unwrap();
    let first =
        enrich_audio_snapshot_for_extract(&cp, &url, scan_source(&cp, &first_path).await).await;
    let second =
        enrich_audio_snapshot_for_extract(&cp, &url, scan_source(&cp, &second_path).await).await;
    let policy = cp
        .create_policy_document("extract-commentary-audio", EXTRACT_COMMENTARY_POLICY)
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set(two_audio_file_input(&[first, second]))
        .await
        .unwrap();
    let mut worker = ExtractAudioWorkerLaunch::start(&cp).await.unwrap();
    let out_dir = tmp.path().join("out");

    cp.execute_compliance_policy_with_options(
        policy.version.id,
        input.id,
        ComplianceExecutionOptions {
            audio_staging_root: tmp.path().join("audio-stage"),
            audio_target_dir: out_dir.clone(),
            max_in_flight_files: 2,
            ..ComplianceExecutionOptions::default()
        },
    )
    .await
    .unwrap();
    worker.shutdown().unwrap();

    for show in ["show-a", "show-b"] {
        assert!(
            out_dir.join(show).join("Movie.stream-2.opus.ogg").is_file(),
            "{show} sidecar must retain its source-relative subtree"
        );
    }
}

fn tempdir_in_repo() -> tempfile::TempDir {
    tempfile::TempDir::new_in(std::env::current_dir().unwrap()).unwrap()
}

struct ScannedSource {
    file_version_id: FileVersionId,
    snapshot_id: MediaSnapshotId,
}

fn two_audio_file_input(files: &[ScannedSource]) -> PolicyInputSetDraft {
    let media_snapshots = files
        .iter()
        .enumerate()
        .map(|(index, file)| MediaSnapshotInput {
            ordinal: u32::try_from(index + 1).unwrap(),
            target: TargetRef::FileVersion {
                id: file.file_version_id,
            },
            container: Some("mkv".to_owned()),
            stream_summary: serde_json::json!({}),
            video_codec: Some("h264".to_owned()),
            width: Some(32),
            height: Some(32),
            hdr: None,
            bitrate: None,
            duration_millis: Some(1000),
            audio_languages: vec!["eng".to_owned()],
            subtitle_languages: Vec::new(),
            health_flags: Vec::new(),
            existing_media_snapshot_id: Some(file.snapshot_id),
        })
        .collect();
    PolicyInputSetDraft {
        slug: "duplicate-basename-audio".to_owned(),
        display_name: "duplicate-basename-audio".to_owned(),
        schema_version: 1,
        source_kind: PolicyInputSourceKind::Test,
        created_at: time::OffsetDateTime::UNIX_EPOCH,
        description: None,
        fixture_labels: vec!["duplicate-basename-audio".to_owned()],
        synthetic_targets: Vec::new(),
        media_snapshots,
        identity_evidence: Vec::new(),
        bundle_targets: Vec::new(),
        quality_profiles: Vec::new(),
        issues: Vec::new(),
    }
}

async fn scan_source(cp: &ControlPlane, source: &Path) -> ScannedSource {
    let scan = cp
        .scan_path(ScanPathInput {
            path: source.to_path_buf(),
            extension_allowlist: Vec::new(),
        })
        .await
        .unwrap();
    assert_eq!(scan.summary.scanned_count(), 1);
    let scanned = scan
        .files
        .iter()
        .find(|file| file.status == ScanReportFileStatus::Scanned)
        .unwrap();
    ScannedSource {
        file_version_id: scanned.file_version_id.unwrap(),
        snapshot_id: scanned.media_snapshot_id.unwrap(),
    }
}

trait ScanSummaryExt {
    fn scanned_count(&self) -> u64;
}

impl ScanSummaryExt for voom_control_plane::scan::ScanSummary {
    fn scanned_count(&self) -> u64 {
        self.ingested
    }
}

async fn assert_audio_snapshot_has_single_commentary_match(
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
            >= 2
    );
    assert_eq!(
        streams
            .iter()
            .filter(|stream| {
                stream["kind"].as_str() == Some("audio")
                    && stream["disposition"]["commentary"] == true
            })
            .count(),
        1
    );
}

async fn enrich_audio_snapshot_for_extract(
    cp: &ControlPlane,
    url: &str,
    scanned: ScannedSource,
) -> ScannedSource {
    let snapshots = SqliteIdentityRepo::new(voom_store::connect(url).await.unwrap())
        .list_media_snapshots_by_version(scanned.file_version_id)
        .await
        .unwrap();
    let snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.id == scanned.snapshot_id)
        .unwrap();
    let mut payload = snapshot.payload.clone();
    let streams = payload["streams"].as_array_mut().unwrap();
    let mut audio_index = 0;
    for stream in streams {
        if stream["kind"].as_str() != Some("audio") {
            continue;
        }
        let object = stream.as_object_mut().unwrap();
        object.insert(
            "title".to_owned(),
            serde_json::Value::String(if audio_index == 0 {
                "Main".to_owned()
            } else {
                "Commentary".to_owned()
            }),
        );
        let disposition = object
            .entry("disposition".to_owned())
            .or_insert_with(|| serde_json::json!({}));
        disposition.as_object_mut().unwrap().insert(
            "commentary".to_owned(),
            serde_json::Value::Bool(audio_index == 1),
        );
        audio_index += 1;
    }
    assert_eq!(audio_index, 2);
    let enriched = cp
        .record_media_snapshot(scanned.file_version_id, None, payload, cp.clock().now())
        .await
        .unwrap();
    ScannedSource {
        file_version_id: scanned.file_version_id,
        snapshot_id: enriched.id,
    }
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

async fn assert_extract_execution_result(
    url: &str,
    out_dir: &Path,
    source_file_version_id: FileVersionId,
    executed: &voom_control_plane::policy::ComplianceExecuteData,
    expected_count: usize,
) -> Vec<PathBuf> {
    let result = ticket_result(url, executed.summary.job_id, "extract_audio").await;
    let outputs = result["outputs"].as_array().unwrap();
    assert_eq!(outputs.len(), expected_count);
    assert_eq!(
        serde_json::to_value(&executed.audio_extract_outputs).unwrap(),
        serde_json::Value::Array(outputs.clone())
    );
    let pool = voom_store::connect(url).await.unwrap();
    let mut promoted = Vec::with_capacity(outputs.len());
    let mut result_asset_ids = Vec::with_capacity(outputs.len());
    for output in outputs {
        let (path, asset_id) = assert_published_output(&pool, out_dir, output).await;
        promoted.push(path);
        result_asset_ids.push(asset_id);
    }
    let source_bundle_id: i64 = sqlx::query_scalar(
        "SELECT abm.bundle_id FROM file_versions fv \
         JOIN asset_bundle_members abm ON abm.file_asset_id = fv.file_asset_id \
         WHERE fv.id = ? AND abm.role = 'primary_video'",
    )
    .bind(i64::try_from(source_file_version_id.0).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_primary_bundle_creation(&pool, source_file_version_id).await;
    let members = SqliteBundleRepo::new(pool.clone())
        .list_members(voom_core::BundleId(
            u64::try_from(source_bundle_id).unwrap(),
        ))
        .await
        .unwrap();
    assert!(members.iter().any(|member| {
        member.role == BundleMemberRole::PrimaryVideo
            && !result_asset_ids.contains(&member.file_asset_id)
    }));
    for result_asset_id in result_asset_ids {
        assert!(
            members
                .iter()
                .any(|member| member.file_asset_id == result_asset_id)
        );
    }
    promoted
}

async fn assert_primary_bundle_creation(
    pool: &sqlx::SqlitePool,
    source_file_version_id: FileVersionId,
) {
    let primary_members: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM file_versions fv \
         JOIN asset_bundle_members abm ON abm.file_asset_id = fv.file_asset_id \
         WHERE fv.id = ? AND abm.role = 'primary_video'",
    )
    .bind(i64::try_from(source_file_version_id.0).unwrap())
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(primary_members, 1);
    let kinds: Vec<String> = sqlx::query_scalar(
        "SELECT kind FROM events \
         WHERE kind IN (\
           'media_work.created', \
           'media_variant.created', \
           'asset_bundle.created', \
           'asset_bundle.member_added'\
         ) \
         ORDER BY event_id",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(
        kinds,
        [
            "media_work.created",
            "media_variant.created",
            "asset_bundle.created",
            "asset_bundle.member_added",
        ]
    );
}

async fn assert_published_output(
    pool: &sqlx::SqlitePool,
    out_dir: &Path,
    output: &serde_json::Value,
) -> (PathBuf, FileAssetId) {
    for (table, field) in [
        ("artifact_handles", "staged_artifact_handle_id"),
        ("artifact_verifications", "verification_id"),
        ("artifact_commit_records", "commit_record_id"),
        ("file_locations", "result_file_location_id"),
    ] {
        assert_row_exists(
            pool,
            &format!("SELECT COUNT(*) FROM {table} WHERE id = ?"),
            output[field].as_u64().unwrap(),
        )
        .await;
    }
    let committed_target = PathBuf::from(output["target_path"].as_str().unwrap());
    assert!(
        committed_target
            .to_str()
            .is_some_and(|target| target.contains("/.committed/audio/"))
    );
    let file_name = committed_target.file_name().unwrap();
    assert!(
        file_name
            .to_str()
            .is_some_and(|name| name.ends_with(".opus.ogg"))
    );
    let promoted = out_dir.canonicalize().unwrap().join(file_name);
    assert!(promoted.is_file(), "{} must exist", promoted.display());
    let version_id = FileVersionId(output["result_file_version_id"].as_u64().unwrap());
    (promoted, file_asset_id_for(pool, version_id).await)
}

fn assert_produced_audio_facts(paths: &[PathBuf], expected_titles: &[&str]) {
    assert_eq!(paths.len(), expected_titles.len());
    for (path, expected_title) in paths.iter().zip(expected_titles) {
        let output = Command::new("ffprobe")
            .args(["-v", "error", "-show_streams", "-of", "json"])
            .arg(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "ffprobe failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        let probe: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let streams = probe["streams"].as_array().unwrap();
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0]["codec_type"], "audio");
        assert_eq!(streams[0]["codec_name"], "opus");
        assert_eq!(streams[0]["tags"]["language"], "eng");
        assert_eq!(streams[0]["tags"]["title"], *expected_title);
    }
}

async fn assert_extract_lineage(
    pool: &sqlx::SqlitePool,
    source_file_version_id: FileVersionId,
    source_snapshot_id: MediaSnapshotId,
    outputs: &[voom_control_plane::policy::ComplianceAudioExtractOutput],
) {
    let rows: Vec<(i64, i64, i64, String, i64, i64)> = sqlx::query_as(
        "SELECT output.ordinal, lineage.source_file_version_id, \
                lineage.source_media_snapshot_id, lineage.source_snapshot_stream_id, \
                lineage.source_provider_stream_index, lineage.result_file_version_id \
         FROM audio_extract_output_lineage lineage \
         JOIN audio_extract_operation_outputs output \
           ON output.id = lineage.operation_output_id \
         ORDER BY output.ordinal ASC",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), outputs.len());
    for (ordinal, row) in rows.iter().enumerate() {
        let output = &outputs[ordinal];
        assert_eq!(row.0, i64::try_from(ordinal).unwrap());
        assert_eq!(row.1, i64::try_from(source_file_version_id.0).unwrap());
        assert_eq!(row.2, i64::try_from(source_snapshot_id.0).unwrap());
        assert_eq!(row.3, output.source_snapshot_stream_id().unwrap());
        assert_eq!(
            row.4,
            i64::from(output.source_provider_stream_index().unwrap())
        );
        assert_eq!(
            row.5,
            i64::try_from(output.result_file_version_id()).unwrap()
        );
    }
}

async fn file_asset_id_for(pool: &sqlx::SqlitePool, file_version_id: FileVersionId) -> FileAssetId {
    let id: i64 = sqlx::query_scalar("SELECT file_asset_id FROM file_versions WHERE id = ?")
        .bind(i64::try_from(file_version_id.0).unwrap())
        .fetch_one(pool)
        .await
        .unwrap();
    FileAssetId(u64::try_from(id).unwrap())
}

async fn assert_row_exists(pool: &sqlx::SqlitePool, sql: &str, id: u64) {
    let count: i64 = sqlx::query_scalar(sql)
        .bind(i64::try_from(id).unwrap())
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

fn require_command(program: &str, args: &[&str]) {
    let output = Command::new(program).args(args).output().unwrap_or_else(|err| {
        panic!(
            "required media tool `{program}` is unavailable; install it for Sprint 14 audio extraction integration tests: {err}"
        )
    });
    assert!(
        output.status.success(),
        "required media tool `{program}` failed setup check with {}: stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[derive(Debug, Clone, Copy)]
enum CommentaryFixture {
    SingleMatch,
    AlternateSingleMatch,
}

fn generate_audio_extract_fixture(path: &Path, fixture: CommentaryFixture) {
    let commentary_disposition = match fixture {
        CommentaryFixture::SingleMatch | CommentaryFixture::AlternateSingleMatch => "comment",
    };
    let commentary_tone = match fixture {
        CommentaryFixture::SingleMatch => "sine=frequency=660:sample_rate=48000",
        CommentaryFixture::AlternateSingleMatch => "sine=frequency=880:sample_rate=48000",
    };
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
            commentary_tone,
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
            "language=eng",
            "-metadata:s:a:1",
            "title=Commentary",
            "-disposition:a:0",
            "default",
            "-disposition:a:1",
            commentary_disposition,
            path.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(
        status.success(),
        "ffmpeg audio extract fixture generation failed: {status}"
    );
}

struct ExtractAudioWorkerLaunch {
    inner: TestWorkerLaunch,
}

impl ExtractAudioWorkerLaunch {
    async fn start(cp: &ControlPlane) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            inner: TestWorkerLaunch::start(
                cp,
                TestWorkerConfig::synthetic(
                    target_debug_binary("voom-ffmpeg-worker"),
                    "e2e-ffmpeg-extract-audio",
                    "control-plane-audio-extract-e2e-secret",
                    "extract_audio",
                ),
            )
            .await?,
        })
    }

    fn shutdown(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.inner.shutdown()
    }
}
