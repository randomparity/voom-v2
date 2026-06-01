#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test setup should fail loudly with direct assertions"
)]

//! End-to-end named and inline video-profile flows: scan -> policy plan ->
//! execute -> transcode -> verify -> commit -> result snapshot, exercised
//! against the real `FFmpeg` worker.
//!
//! The test calls `preflight_from_process_env()` up front so a missing encoder
//! fails loudly instead of being silently skipped (spec §10).

use std::path::Path;
use std::process::Command;

use serde_json::json;
use tempfile::NamedTempFile;
use voom_control_plane::ControlPlane;
use voom_control_plane::policy::{
    ComplianceExecuteData, ComplianceExecutionOptions, PolicyInputFromScanInput,
};
use voom_control_plane::scan::{ScanPathInput, ScanReportFileStatus};
use voom_core::{FileVersionId, MediaSnapshotId, PolicyVersionId};
use voom_ffmpeg_worker::preflight_from_process_env;
use voom_plan::PlanOperationKind;
use voom_policy::{MediaSnapshotInput, PolicyInputSetDraft, PolicyInputSourceKind, TargetRef};
use voom_store::repo::identity::{IdentityRepo, SqliteIdentityRepo};
use voom_test_support::worker::{
    FfprobeSiblingGuard, TestWorkerConfig, TestWorkerLaunch, cargo_build_package,
    hide_stale_fake_ffprobe_sibling, target_debug_binary,
};

const HEVC_POLICY: &str = r#"policy "video transcode hevc default" {
  phase normalize {
    transcode video to hevc
  }
}
"#;

const HEVC_1080P_POLICY: &str = r#"policy "video transcode hevc 1080p" {
  phase normalize {
    transcode video to hevc using profile "hevc-1080p"
  }
}
"#;

const AV1_INLINE_POLICY: &str = r#"policy "video transcode av1 inline" {
  phase normalize {
    transcode video to av1 {
      encoder: libsvtav1
      crf: 32
      preset: 8
      output_container: mp4
    }
  }
}
"#;

struct Case {
    slug: &'static str,
    policy_source: &'static str,
    source_codec: &'static str,
    source_container: &'static str,
    source_width: u32,
    source_height: u32,
    expected_output_glob: &'static str,
    expected_container: &'static str,
    expected_codec: &'static str,
}

#[tokio::test]
async fn named_default_hevc_mkv_flow_commits_and_replans_as_no_op() {
    let case = Case {
        slug: "video-transcode-hevc-default",
        policy_source: HEVC_POLICY,
        source_codec: "h264",
        source_container: "mp4",
        source_width: 320,
        source_height: 240,
        expected_output_glob: "Movie.default-hevc.hevc.mkv",
        expected_container: "mkv",
        expected_codec: "hevc",
    };
    let outcome = run_case(&case).await;
    // A committed default-hevc result must re-plan to NoOp under the same policy.
    assert_replans_as_no_op(
        &outcome.cp,
        outcome.policy_version_id,
        outcome.result_file_version_id,
        outcome.result_media_snapshot_id,
    )
    .await;
}

#[tokio::test]
async fn named_hevc_1080p_downscales_oversized_hevc_source_to_mp4() {
    let case = Case {
        slug: "video-transcode-hevc-1080p",
        policy_source: HEVC_1080P_POLICY,
        source_codec: "hevc",
        source_container: "mp4",
        source_width: 2560,
        source_height: 1440,
        expected_output_glob: "Movie.hevc-1080p.hevc.mp4",
        expected_container: "mp4",
        expected_codec: "hevc",
    };
    let outcome = run_case(&case).await;
    assert!(
        outcome.output_width <= 1920,
        "downscaled output width must fit the 1920 cap, got {}",
        outcome.output_width
    );
    assert!(
        outcome.output_height <= 1080,
        "downscaled output height must fit the 1080 cap, got {}",
        outcome.output_height
    );
    assert!(
        outcome.output_width < 2560,
        "an oversized HEVC source must actually be downscaled, got width {}",
        outcome.output_width
    );
}

#[tokio::test]
async fn inline_av1_mp4_flow_commits_with_inline_discriminated_target() {
    let case = Case {
        slug: "video-transcode-av1-inline",
        policy_source: AV1_INLINE_POLICY,
        source_codec: "h264",
        source_container: "mp4",
        source_width: 320,
        source_height: 240,
        // Inline profiles use an `inline-<hash>` discriminator in the target.
        expected_output_glob: "Movie.inline-",
        expected_container: "mp4",
        expected_codec: "av1",
    };
    run_case(&case).await;
}

struct CaseOutcome {
    cp: ControlPlane,
    policy_version_id: PolicyVersionId,
    result_file_version_id: FileVersionId,
    result_media_snapshot_id: MediaSnapshotId,
    output_width: u64,
    output_height: u64,
    _tmp: tempfile::TempDir,
    _db: NamedTempFile,
    _ffprobe_guard: FfprobeSiblingGuard,
}

async fn run_case(case: &Case) -> CaseOutcome {
    require_encoders();
    // The post-commit result probe (spec §7 step 13) must run REAL ffprobe
    // against the committed transcode output. Other tests in this crate install
    // a canned test-helper `ffprobe` stub next to the worker binary in the shared
    // profile dir; hide it for the duration of this real-ffmpeg flow so the probe
    // observes the actual transcoded streams.
    cargo_build_package("voom-ffprobe-worker").unwrap();
    cargo_build_package("voom-verify-artifact-worker").unwrap();
    cargo_build_package("voom-ffmpeg-worker").unwrap();
    let ffprobe_guard = hide_stale_fake_ffprobe_sibling("video-profile-flow").unwrap();

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let source = root.join("Movie.mp4");
    generate_fixture(
        &source,
        case.source_codec,
        case.source_width,
        case.source_height,
    );

    let db = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = ControlPlane::open_with_pool(pool, std::sync::Arc::new(voom_core::SystemClock))
        .await
        .unwrap();

    let source_file_version_id = scan_source(&cp, &source).await;
    let policy = cp
        .create_policy_document(case.slug, case.policy_source)
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set(input_for(SnapshotFacts {
            slug: case.slug,
            file_version_id: source_file_version_id,
            media_snapshot_id: None,
            container: case.source_container,
            video_codec: case.source_codec,
            width: case.source_width,
            height: case.source_height,
        }))
        .await
        .unwrap();

    let plan = cp
        .generate_compliance_report(policy.version.id, input.id)
        .await
        .unwrap();
    assert_eq!(
        plan.plan.nodes[0].operation_kind,
        PlanOperationKind::TranscodeVideo
    );
    assert_eq!(plan.plan.nodes[0].status, voom_plan::NodeStatus::Planned);

    let executed = execute_with_worker(&cp, policy.version.id, input.id, &root).await;
    let committed = assert_committed_result(&url, &root, &executed, case).await;

    CaseOutcome {
        cp,
        policy_version_id: policy.version.id,
        result_file_version_id: committed.file_version_id,
        result_media_snapshot_id: committed.media_snapshot_id,
        output_width: committed.output_width,
        output_height: committed.output_height,
        _tmp: tmp,
        _db: db,
        _ffprobe_guard: ffprobe_guard,
    }
}

struct CommittedResult {
    file_version_id: FileVersionId,
    media_snapshot_id: MediaSnapshotId,
    output_width: u64,
    output_height: u64,
}

async fn execute_with_worker(
    cp: &ControlPlane,
    policy_version_id: PolicyVersionId,
    input_set_id: voom_core::PolicyInputSetId,
    tmp: &Path,
) -> ComplianceExecuteData {
    let mut worker = start_worker(cp).await;
    let executed = cp
        .execute_compliance_policy_with_options(
            policy_version_id,
            input_set_id,
            ComplianceExecutionOptions {
                transcode_staging_root: tmp.join("stage"),
                transcode_target_dir: tmp.join("out"),
                ..ComplianceExecutionOptions::default()
            },
        )
        .await
        .unwrap();
    worker.shutdown().unwrap();
    executed
}

/// Read a succeeded operation ticket's durable result JSON for a job. The flat
/// `tickets` field was removed from `ComplianceExecuteData`; the tickets a run
/// executed remain queryable in the `tickets` table.
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

async fn assert_committed_result(
    url: &str,
    tmp: &Path,
    executed: &ComplianceExecuteData,
    case: &Case,
) -> CommittedResult {
    let result = ticket_result(url, executed.summary.job_id, "transcode_video").await;
    let file_version_id = FileVersionId(result["result_file_version_id"].as_u64().unwrap());
    let media_snapshot_id = MediaSnapshotId(result["result_media_snapshot_id"].as_u64().unwrap());
    assert!(result["staged_artifact_handle_id"].as_u64().unwrap() > 0);
    assert!(result["verification_id"].as_u64().unwrap() > 0);
    assert!(result["commit_record_id"].as_u64().unwrap() > 0);
    assert_eq!(result["target_codec"], case.expected_codec);
    assert_eq!(result["output_container"], case.expected_container);
    let output_width = result["output_width"].as_u64().unwrap();
    let output_height = result["output_height"].as_u64().unwrap();

    let committed = find_output(
        &tmp.join("out"),
        case.expected_output_glob,
        case.expected_container,
    );
    assert!(
        committed.is_file(),
        "expected committed output matching {}",
        case.expected_output_glob
    );

    let snapshots = SqliteIdentityRepo::new(voom_store::connect(url).await.unwrap())
        .list_media_snapshots_by_version(file_version_id)
        .await
        .unwrap();
    let result_snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.id == media_snapshot_id)
        .unwrap();
    assert_probed_result_snapshot(result_snapshot, case, output_width, output_height);
    CommittedResult {
        file_version_id,
        media_snapshot_id,
        output_width,
        output_height,
    }
}

/// The committed-result snapshot must be a REAL ffprobe observation of the
/// committed bytes (spec §7 step 13), not a synthesized stub. We prove the
/// probe actually ran by asserting the normalized `sprint10-v1` payload carries
/// a non-empty `streams` array whose video stream reports the expected codec,
/// pixel format, and the committed output dimensions.
fn assert_probed_result_snapshot(
    snapshot: &voom_store::repo::identity::MediaSnapshot,
    case: &Case,
    output_width: u64,
    output_height: u64,
) {
    assert_eq!(snapshot.payload["format"], "sprint10-v1");
    assert_eq!(snapshot.payload["probe"]["provider"], "ffprobe");
    let streams = snapshot.payload["streams"]
        .as_array()
        .expect("probed result snapshot must carry a streams array");
    assert!(
        !streams.is_empty(),
        "probed result snapshot streams array must not be empty"
    );
    let video = streams
        .iter()
        .find(|stream| stream["kind"] == "video")
        .expect("probed result snapshot must include a video stream");
    assert_eq!(video["codec_name"], case.expected_codec);
    assert_eq!(video["pixel_format"], "yuv420p");
    assert_eq!(video["width"].as_u64().unwrap(), output_width);
    assert_eq!(video["height"].as_u64().unwrap(), output_height);
}

/// Re-plan the committed result by projecting its DURABLE result `MediaSnapshot`
/// back through the normal `stream_summary_from_snapshot_payload` projection
/// (`create_policy_input_set_from_scan`), rather than a hand-built synthetic
/// draft. This proves the probed result snapshot round-trips: the projection
/// reads `payload["streams"]`, which only exists because the post-commit probe
/// recorded a real observation. A synthesized stub (no `streams`) would project
/// to `video_stream_count: 0` and the MP4/codec gates would not see the result
/// as already-compliant.
async fn assert_replans_as_no_op(
    cp: &ControlPlane,
    policy_version_id: PolicyVersionId,
    result_file_version_id: FileVersionId,
    result_media_snapshot_id: MediaSnapshotId,
) {
    let projected = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "replan-result".to_owned(),
            file_version_id: result_file_version_id,
            media_snapshot_id: result_media_snapshot_id,
            container: "mkv".to_owned(),
            video_codec: "hevc".to_owned(),
        })
        .await
        .unwrap();

    let set = cp
        .get_policy_input_set(projected.input_set_id)
        .await
        .unwrap()
        .unwrap();
    let projected_streams = &set.media_snapshots[0].stream_summary["streams"];
    let projected_streams = projected_streams
        .as_array()
        .expect("projected stream_summary must carry a streams array");
    assert!(
        projected_streams
            .iter()
            .any(|stream| stream["kind"] == "video"),
        "the durable result snapshot must project a video stream; a synthesized \
         stub without a streams array would not round-trip through the projection"
    );

    let result_plan = cp
        .generate_compliance_report(policy_version_id, projected.input_set_id)
        .await
        .unwrap();
    assert_eq!(
        result_plan.plan.nodes[0].status,
        voom_plan::NodeStatus::NoOp
    );
}

fn find_output(out_dir: &Path, prefix: &str, container: &str) -> std::path::PathBuf {
    for entry in std::fs::read_dir(out_dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.starts_with(prefix) && name.ends_with(container) {
            return path;
        }
    }
    panic!(
        "no output file matching {prefix}*.{container} in {}",
        out_dir.display()
    );
}

async fn scan_source(cp: &ControlPlane, source: &Path) -> FileVersionId {
    let scan = cp
        .scan_path(ScanPathInput {
            path: source.to_path_buf(),
        })
        .await
        .unwrap();
    scan.files
        .iter()
        .find(|file| file.status == ScanReportFileStatus::Scanned)
        .unwrap()
        .file_version_id
        .unwrap()
}

fn require_encoders() {
    let preflight = preflight_from_process_env().expect("ffmpeg preflight must succeed");
    // These flows exercise libx265 (hevc cases) and libsvtav1 (inline AV1 case)
    // only; libaom-av1 is optional and not required here.
    for encoder in ["libx265", "libsvtav1"] {
        assert!(
            preflight.has_encoder(encoder),
            "required encoder {encoder} missing; this is a setup failure, not a skip"
        );
    }
    assert!(preflight.has_muxer("mp4"), "mp4 muxer required");
    assert!(preflight.has_muxer("matroska"), "matroska muxer required");
}

#[derive(Clone, Copy)]
struct SnapshotFacts<'a> {
    slug: &'a str,
    file_version_id: FileVersionId,
    media_snapshot_id: Option<MediaSnapshotId>,
    container: &'a str,
    video_codec: &'a str,
    width: u32,
    height: u32,
}

fn input_for(facts: SnapshotFacts<'_>) -> PolicyInputSetDraft {
    PolicyInputSetDraft {
        slug: facts.slug.to_owned(),
        display_name: facts.slug.to_owned(),
        schema_version: 1,
        source_kind: PolicyInputSourceKind::Test,
        created_at: time::OffsetDateTime::UNIX_EPOCH,
        description: None,
        fixture_labels: vec![format!("video-profile-flow-{}", facts.slug)],
        synthetic_targets: Vec::new(),
        media_snapshots: vec![MediaSnapshotInput {
            ordinal: 1,
            target: TargetRef::FileVersion {
                id: facts.file_version_id,
            },
            container: Some(facts.container.to_owned()),
            // A fully-enumerated single video stream: the MP4 muxability gate
            // requires kind+codec_name for every stream, and the source
            // fixtures carry no audio/subtitle streams.
            stream_summary: json!({
                "video_stream_count": 1,
                "streams": [{
                    "kind": "video",
                    "codec_name": facts.video_codec,
                    "provider_stream_index": 0,
                }],
            }),
            video_codec: Some(facts.video_codec.to_owned()),
            width: Some(facts.width),
            height: Some(facts.height),
            hdr: None,
            bitrate: None,
            duration_millis: Some(1000),
            audio_languages: Vec::new(),
            subtitle_languages: Vec::new(),
            health_flags: Vec::new(),
            existing_media_snapshot_id: facts.media_snapshot_id,
        }],
        identity_evidence: Vec::new(),
        bundle_targets: Vec::new(),
        quality_profiles: Vec::new(),
        issues: Vec::new(),
    }
}

fn generate_fixture(path: &Path, codec: &str, width: u32, height: u32) {
    let encoder = match codec {
        "h264" => "libx264",
        "hevc" => "libx265",
        other => panic!("unsupported fixture codec {other}"),
    };
    let size = format!("testsrc=size={width}x{height}:rate=1");
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            &size,
            "-t",
            "1",
            "-c:v",
            encoder,
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

async fn start_worker(cp: &ControlPlane) -> TestWorkerLaunch {
    TestWorkerLaunch::start(
        cp,
        TestWorkerConfig::synthetic(
            target_debug_binary("voom-ffmpeg-worker"),
            "e2e-ffmpeg-profile",
            "control-plane-profile-e2e-secret",
            "transcode_video",
        ),
    )
    .await
    .unwrap()
}
