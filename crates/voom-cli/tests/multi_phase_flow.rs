//! Multi-phase `compliance execute` run + `compliance report --job-id` read-back
//! through the `voom` CLI, with media execution routed through owner-node
//! envelopes (issue #166 contract on the #423 owner-node dispatch cutover).
//!
//! A two-`transcode video` phase policy is the proven two-commit shape: phase 0
//! transcodes the scanned h264 to default hevc and commits; phase 1 re-plans
//! against the committed artifact — its envelope renders from the produced
//! version's committed location and reprobe snapshot — and applies the
//! independently necessary 10-bit `hevc-archive` profile. Both phases land a
//! `Committed` per-`(file, phase)` row, and `compliance report --job-id` reads
//! the durable two-phase chain back.
//!
//! Media tickets are settled by the owner-node emulator
//! (`support/owner_node.rs`) standing in for the storage owner's agent, the
//! same stand-in pattern the durable workflow drivers use.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests fail loudly and preserve paths for diagnosis"
)]

use std::path::Path;

use serde_json::{Value, json};
use voom_control_plane::ControlPlane;
use voom_core::FileVersionId;
use voom_policy::{MediaSnapshotInput, PolicyInputSetDraft, PolicyInputSourceKind, TargetRef};
use voom_store::test_support::sqlite_url_for;
use voom_test_support::TempDatabase;
use voom_test_support::scan_seed::{SeedFile, seed_scanned_files};
use voom_test_support::worker::{
    TestWorkerConfig, TestWorkerLaunch, cargo_bin_or_build, target_debug_binary,
};

#[path = "support/owner_node.rs"]
mod owner_node;

/// `compliance execute` drives a two-phase transcode policy to completion through
/// the CLI, and `compliance report --job-id` reads the durable two-phase chain back: two `completed` phases, two `committed` per-file rows, phase 1 rooted at
/// phase 0's produced version, and the post-run read returns the same chain with
/// `latest_phase_index` pointing at phase 1.
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

#[tokio::test]
async fn multi_phase_execute_then_report_by_job_id() {
    let _verify_worker =
        cargo_bin_or_build("voom-verify-artifact-worker", "voom-verify-artifact-worker").unwrap();
    cargo_build("voom-ffmpeg-worker");

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let source = root.join("Movie.mp4");
    std::fs::write(&source, b"multi-phase e2e source bytes").unwrap();

    let db = TempDatabase::new().unwrap();
    let url = sqlite_url_for(db.path());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    voom_store::test_support::seed_test_storage_root(&pool)
        .await
        .unwrap();
    voom_store::test_support::set_test_storage_root_path(&pool, &root)
        .await
        .unwrap();
    // Envelope destinations resolve through the library root's staging/backup
    // defaults; point both at the seeded test root.
    sqlx::query(
        "UPDATE library_roots SET default_staging_root_id = id, \
         default_backup_root_id = id WHERE id = ?",
    )
    .bind(i64::try_from(voom_store::test_support::TEST_STORAGE_ROOT_ID.0).unwrap())
    .execute(&pool)
    .await
    .unwrap();

    // Storage-owner stand-ins: fenced commit-intent driver + media settlement.
    let _emulator = owner_node::OwnerNodeEmulator::spawn(&url);

    let cp = ControlPlane::open_with_pool(pool, std::sync::Arc::new(voom_core::SystemClock))
        .await
        .unwrap();

    let file = scan_one(&cp, &url, &root, &source).await;
    let policy = cp
        .create_policy_document(
            "video-transcode-hevc-archive",
            "policy \"video transcode hevc archive\" {\n  \
               phase normalize { transcode video to hevc }\n  \
               phase archive { depends_on: [normalize] \
               transcode video to hevc using profile \"hevc-archive\" }\n}",
        )
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set(single_file_input(file))
        .await
        .unwrap();
    let version_id = policy.version.id.0;
    let input_id = input.id.0;

    let out_dir = root.join("out");
    let staging_root = root.join("stage");
    let mut worker = TranscodeWorkerLaunch::start(&cp).await.unwrap();
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
    assert_eq!(phases[1]["phase_name"], "archive");
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
    assert_eq!(report_phases[1]["phase_name"], "archive");
    assert_eq!(
        report_json["data"]["latest_phase_index"], 1,
        "latest index points at the highest-ordinal phase"
    );
    assert!(
        report_phases.iter().all(|p| p["report_id"].is_string()),
        "each phase carries its folded report id"
    );
    for (index, (run_phase, report_phase)) in run_phases.iter().zip(report_phases).enumerate() {
        assert_eq!(
            run_phase["report_id"], report_phase["report_id"],
            "report_id mismatch at index {index} across execute and report",
        );
        assert_eq!(
            run_phase["report"], report_phase["report"],
            "report body mismatch at index {index} across execute and report",
        );
    }
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
    std::process::Command::new(env!("CARGO_BIN_EXE_voom"))
        .env(
            "VOOM_LOCAL_NODE_ID",
            voom_store::test_support::TEST_STORAGE_ROOT_ID.0.to_string(),
        )
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

async fn scan_one(cp: &ControlPlane, url: &str, root: &Path, source: &Path) -> ScannedFile {
    let locator = source
        .strip_prefix(root)
        .unwrap()
        .to_str()
        .unwrap()
        .replace(std::path::MAIN_SEPARATOR, "/");
    let seeded = seed_scanned_files(
        cp,
        url,
        voom_store::test_support::TEST_STORAGE_ROOT_ID,
        &[SeedFile {
            locator: &locator,
            path: source,
            probe_snapshot: json!({
                "format": "sprint10-v1",
                "container": { "format_name": "mov,mp4,m4a,3gp,3g2,mj2" },
                "streams": [{
                    "index": 0,
                    "kind": "video",
                    "codec_name": "h264",
                    "width": 32,
                    "height": 32,
                    "disposition": { "default": true, "forced": false, "commentary": false },
                }],
            }),
            sidecars: Vec::new(),
        }],
    )
    .await
    .unwrap();
    let seeded = &seeded[0];
    ScannedFile {
        file_version_id: seeded.file_version_id,
        media_snapshot_id: Some(seeded.media_snapshot_id),
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

fn cargo_build(package: &str) {
    voom_test_support::worker::cargo_build_package(package).unwrap();
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
