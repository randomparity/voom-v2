#![expect(
    clippy::panic,
    clippy::unwrap_used,
    reason = "integration test setup should fail loudly with direct assertions"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::NamedTempFile;
use voom_control_plane::ControlPlane;
use voom_control_plane::cases::policy::compliance::ComplianceExecutionOptions;
use voom_control_plane::cases::policy::policy_inputs::PolicyInputFromScanInput;
use voom_control_plane::scan::{ScanPathInput, ScanReportFileStatus};
use voom_core::{BundleId, FileAssetId, FileVersionId, MediaSnapshotId};
use voom_plan::PlanOperationKind;
use voom_store::repo::bundles::{
    BundleMemberRole, BundleRepo, NewAssetBundle, NewBundleMember, SqliteBundleRepo,
};
use voom_store::repo::identity::{
    IdentityRepo, MediaWorkKind, NewMediaVariant, NewMediaWork, SqliteIdentityRepo,
};
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
    extract audio where lang in [eng]
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

    let db = NamedTempFile::new().unwrap();
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
    let source_bundle_id = create_primary_bundle(&pool, scanned.file_version_id).await;

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

    assert_extract_execution_result(&url, &out_dir, source_bundle_id, &executed).await;
}

#[tokio::test]
async fn audio_extract_multi_match_blocks_before_sidecar_commit() {
    let _guard = AUDIO_EXTRACT_FLOW_LOCK.lock().await;
    require_command("ffmpeg", &["-version"]);
    cargo_build_package("voom-ffprobe-worker").unwrap();
    let _ffprobe_guard = hide_stale_fake_ffprobe_sibling("audio-extract-flow").unwrap();

    let tmp = tempdir_in_repo();
    let source = tmp.path().join("Movie.mkv");
    generate_audio_extract_fixture(&source, CommentaryFixture::SingleMatch);

    let db = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp =
        ControlPlane::open_with_pool(pool.clone(), std::sync::Arc::new(voom_core::SystemClock))
            .await
            .unwrap();

    let scanned = scan_source(&cp, &source).await;
    let scanned = enrich_audio_snapshot_for_extract(&cp, &url, scanned).await;
    create_primary_bundle(&pool, scanned.file_version_id).await;
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
    assert_eq!(plan.plan.nodes[0].status, voom_plan::NodeStatus::Blocked);
    assert!(
        plan.plan.nodes[0]
            .status_reason
            .contains("multiple audio streams")
    );
    assert_table_count(&pool, "artifact_commit_records", 0).await;
    assert!(!tmp.path().join("out").exists());
}

fn tempdir_in_repo() -> tempfile::TempDir {
    tempfile::TempDir::new_in(std::env::current_dir().unwrap()).unwrap()
}

struct ScannedSource {
    file_version_id: FileVersionId,
    snapshot_id: MediaSnapshotId,
}

async fn scan_source(cp: &ControlPlane, source: &Path) -> ScannedSource {
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

async fn create_primary_bundle(
    pool: &sqlx::SqlitePool,
    file_version_id: FileVersionId,
) -> BundleId {
    let identity = SqliteIdentityRepo::new(pool.clone());
    let bundles = SqliteBundleRepo::new(pool.clone());
    let work = identity
        .create_media_work(NewMediaWork {
            kind: MediaWorkKind::Movie,
            display_title: "Movie".to_owned(),
            provisional: true,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let variant = identity
        .create_media_variant(NewMediaVariant {
            media_work_id: work.id,
            label: "source".to_owned(),
            provisional: true,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    let file_asset_id = file_asset_id_for(pool, file_version_id).await;
    let bundle = bundles
        .create(NewAssetBundle {
            media_variant_id: variant.id,
            display_name: "Movie".to_owned(),
            created_at: time::OffsetDateTime::UNIX_EPOCH,
        })
        .await
        .unwrap();
    bundles
        .add_member(NewBundleMember {
            bundle_id: bundle.id,
            file_asset_id,
            role: BundleMemberRole::PrimaryVideo,
        })
        .await
        .unwrap();
    bundle.id
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
    source_bundle_id: BundleId,
    executed: &voom_control_plane::cases::policy::compliance::ComplianceExecuteData,
) {
    let result = ticket_result(url, executed.summary.job_id, "extract_audio").await;
    let staged_artifact_handle_id = result["staged_artifact_handle_id"].as_u64().unwrap();
    let verification_id = result["verification_id"].as_u64().unwrap();
    let commit_record_id = result["commit_record_id"].as_u64().unwrap();
    let result_file_version_id = FileVersionId(result["result_file_version_id"].as_u64().unwrap());
    let result_file_location_id = result["result_file_location_id"].as_u64().unwrap();
    let target_path = PathBuf::from(result["target_path"].as_str().unwrap());

    assert!(staged_artifact_handle_id > 0);
    assert!(verification_id > 0);
    assert!(commit_record_id > 0);
    assert!(result_file_version_id.0 > 0);
    assert!(result_file_location_id > 0);
    assert!(target_path.is_file());
    assert!(target_path.starts_with(out_dir.canonicalize().unwrap()));
    assert!(
        target_path
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .is_some_and(|file_name| file_name.ends_with(".opus.ogg"))
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
        result_file_location_id,
    )
    .await;

    let result_asset_id = file_asset_id_for(&pool, result_file_version_id).await;
    let members = SqliteBundleRepo::new(pool)
        .list_members(source_bundle_id)
        .await
        .unwrap();
    assert!(members.iter().any(|member| {
        member.role == BundleMemberRole::PrimaryVideo && member.file_asset_id != result_asset_id
    }));
    assert!(members.iter().any(|member| {
        member.role == BundleMemberRole::CommentaryAudio && member.file_asset_id == result_asset_id
    }));
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

async fn assert_table_count(pool: &sqlx::SqlitePool, table: &str, expected: i64) {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    let count: i64 = sqlx::query_scalar(&sql).fetch_one(pool).await.unwrap();
    assert_eq!(count, expected);
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
}

fn generate_audio_extract_fixture(path: &Path, fixture: CommentaryFixture) {
    let commentary_disposition = match fixture {
        CommentaryFixture::SingleMatch => "comment",
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
