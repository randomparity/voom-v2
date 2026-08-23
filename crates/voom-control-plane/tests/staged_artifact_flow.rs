#![expect(
    clippy::unwrap_used,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tempfile::TempDir;
use voom_control_plane::ControlPlane;
use voom_control_plane::artifact::{
    ArtifactInspectionState, ArtifactListInput, CommitArtifactInput, VerifyArtifactInput,
};
use voom_core::ErrorCode;
use voom_store::repo::media::artifacts::ArtifactCommitState;
use voom_test_support::TempDatabase;
use voom_test_support::scan_seed::{SeedFile, SeededSource, seed_scanned_files};
use voom_test_support::worker::{
    FfprobeSiblingGuard, cargo_bin_or_build, install_fake_ffprobe_sibling, target_debug_binary,
    workspace_root,
};

/// Canned normalized probe snapshot matching what the fake ffprobe sibling
/// (`basic-mp4.json`) reports once `voom-ffprobe-worker` normalizes it, so the
/// seeded source snapshot agrees with every later staged-artifact probe.
fn basic_mp4_probe_snapshot() -> serde_json::Value {
    serde_json::json!({
        "format": "sprint10-v1",
        "container": {
            "format_name": "mov,mp4,m4a,3gp,3g2,mj2",
            "format_long_name": "QuickTime / MOV",
        },
        "streams": [
            {
                "index": 0,
                "kind": "video",
                "codec_name": "h264",
                "width": 320,
                "height": 180,
            },
            {
                "index": 1,
                "kind": "audio",
                "codec_name": "aac",
                "channels": 2,
            },
        ],
    })
}

const BASIC_FFPROBE_JSON: &str =
    include_str!("../../voom-ffprobe-worker/fixtures/ffprobe/basic-mp4.json");

#[tokio::test]
async fn scan_stage_verify_commit_flow_persists_committed_artifact() {
    let _ffprobe_guard = install_worker_siblings();
    let (cp, db, dir) = fixture().await;
    let media = dir.path().join("committed-source.mp4");
    std::fs::copy(tiny_media_fixture(), &media).unwrap();

    let seeded = seed_scanned_files(
        &cp,
        &db.url,
        voom_store::test_support::TEST_STORAGE_ROOT_ID,
        &[SeedFile {
            locator: "committed-source.mp4",
            path: &media,
            probe_snapshot: basic_mp4_probe_snapshot(),
        }],
    )
    .await
    .unwrap();
    let seeded = &seeded[0];
    let staging_path = dir.path().join("staged.mp4");
    std::fs::copy(&media, &staging_path).unwrap();
    let pool = voom_store::connect(&db.url).await.unwrap();
    let staged = voom_test_support::staging_seed::seed_staged_artifact(
        &pool,
        seeded.file_version_id,
        &staging_path,
    )
    .await
    .unwrap();
    let verified = cp
        .verify_artifact(VerifyArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            staging_root: staging_path.parent().unwrap().to_path_buf(),
        })
        .await
        .unwrap();
    let target_path = dir.path().join("committed.mp4");
    let committed = cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            target_path: target_path.clone(),
        })
        .await
        .unwrap();
    let shown = cp.show_artifact(staged.artifact_handle_id).await.unwrap();

    assert_eq!(verified.status.as_str(), "succeeded");
    assert_eq!(committed.state, ArtifactCommitState::Committed);
    assert_eq!(shown.state, ArtifactInspectionState::Committed);
    assert_eq!(shown.latest_commit.unwrap().id, committed.commit_record_id);
    assert_eq!(
        std::fs::read(&target_path).unwrap(),
        std::fs::read(media).unwrap()
    );
}

#[tokio::test]
async fn commit_rejections_and_recovery_visibility_are_inspectable() {
    let _ffprobe_guard = install_worker_siblings();
    let (cp, db, dir) = fixture().await;
    // One scan session for all three sources: a completed session retires the
    // root locations it did not observe, so seeding sequentially would retire
    // earlier sources before their commits run.
    let names = ["unverified", "drift", "recovery"];
    let paths = names
        .iter()
        .map(|name| {
            let path = dir.path().join(format!("{name}-source.mp4"));
            std::fs::copy(tiny_media_fixture(), &path).unwrap();
            path
        })
        .collect::<Vec<_>>();
    let locators = names
        .iter()
        .map(|name| format!("{name}-source.mp4"))
        .collect::<Vec<_>>();
    let seed_files = names
        .iter()
        .zip(&paths)
        .zip(&locators)
        .map(|((_, path), locator)| SeedFile {
            locator,
            path,
            probe_snapshot: basic_mp4_probe_snapshot(),
        })
        .collect::<Vec<_>>();
    let seeded = seed_scanned_files(
        &cp,
        &db.url,
        voom_store::test_support::TEST_STORAGE_ROOT_ID,
        &seed_files,
    )
    .await
    .unwrap();
    let unverified = staged_fixture(&db, dir.path(), "unverified", &seeded[0]).await;
    let verified = verified_fixture(&cp, &db, dir.path(), "drift", &seeded[1]).await;
    std::fs::write(&verified.staging_path, b"changed bytes").unwrap();
    let recovery = verified_fixture(&cp, &db, dir.path(), "recovery", &seeded[2]).await;

    let unverified_err = cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id: unverified.artifact_handle_id,
            target_path: dir.path().join("unverified-target.mp4"),
        })
        .await
        .unwrap_err();
    assert_eq!(unverified_err.code(), ErrorCode::ConfigInvalid);

    let drift_err = cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id: verified.artifact_handle_id,
            target_path: dir.path().join("drift-target.mp4"),
        })
        .await
        .unwrap_err();
    assert_eq!(drift_err.code(), ErrorCode::ArtifactChecksumMismatch);

    inject_recovery_required(&db.url, &recovery, dir.path()).await;
    let shown = cp.show_artifact(recovery.artifact_handle_id).await.unwrap();
    assert_eq!(shown.state, ArtifactInspectionState::RecoveryRequired);
    let commit = shown.latest_commit.as_ref().unwrap();
    assert_eq!(commit.state, ArtifactCommitState::RecoveryRequired);
    let recovery_summary = commit.recovery.as_ref().unwrap();
    assert!(recovery_summary.target.exists);
    assert!(recovery_summary.temp.as_ref().unwrap().exists);
    assert!(recovery_summary.staging.as_ref().unwrap().exists);

    let recoveries = cp
        .list_artifacts(ArtifactListInput {
            state: Some(ArtifactInspectionState::RecoveryRequired),
            after_id: None,
            limit: 10,
        })
        .await
        .unwrap()
        .artifacts;
    // Both the injected recovery and the drift-rejected commit park in
    // recovery_required: under ADR 0074 the node's mismatched receipt makes
    // the drift commit operator-visible instead of failing silently.
    let recovery_ids: Vec<voom_core::ArtifactHandleId> =
        recoveries.iter().map(|a| a.artifact_handle_id).collect();
    assert!(recovery_ids.contains(&verified.artifact_handle_id));
    assert!(recovery_ids.contains(&recovery.artifact_handle_id));
}

fn artifact_tempdir() -> TempDir {
    TempDir::new_in(std::env::current_dir().unwrap()).unwrap()
}

async fn fixture() -> (ControlPlane, Db, TempDir) {
    let dir = artifact_tempdir();
    let tmp = TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    voom_store::test_support::seed_test_storage_root(&pool)
        .await
        .unwrap();
    voom_store::test_support::set_test_storage_root_path(&pool, dir.path())
        .await
        .unwrap();
    // Background stand-in for the storage-owner agent (ADR 0074): drives the
    // fenced commit intent so non-blocked commits converge.
    voom_test_support::commit_node::install_and_spawn_driver(&pool);
    let cp = ControlPlane::open_with_pool(pool, std::sync::Arc::new(voom_core::SystemClock))
        .await
        .unwrap()
        .with_local_node_id(Some(voom_core::NodeId(9_000_001)));
    (cp, Db { _tmp: tmp, url }, dir)
}

#[derive(Debug)]
struct Db {
    _tmp: TempDatabase,
    url: String,
}

#[derive(Debug)]
struct StagedFixture {
    artifact_handle_id: voom_core::ArtifactHandleId,
    source_file_version_id: voom_core::FileVersionId,
    staging_path: PathBuf,
    verification_id: Option<voom_core::ids::ArtifactVerificationId>,
}

async fn staged_fixture(
    db: &Db,
    dir: &Path,
    name: &str,
    seeded: &SeededSource,
) -> StagedFixture {
    let staging_path = dir.join(format!("{name}-staged.mp4"));
    std::fs::copy(dir.join(format!("{name}-source.mp4")), &staging_path).unwrap();
    let pool = voom_store::connect(&db.url).await.unwrap();
    let staged = voom_test_support::staging_seed::seed_staged_artifact(
        &pool,
        seeded.file_version_id,
        &staging_path,
    )
    .await
    .unwrap();
    StagedFixture {
        artifact_handle_id: staged.artifact_handle_id,
        source_file_version_id: staged.source_file_version_id,
        staging_path,
        verification_id: None,
    }
}

async fn verified_fixture(
    cp: &ControlPlane,
    db: &Db,
    dir: &Path,
    name: &str,
    seeded: &SeededSource,
) -> StagedFixture {
    let mut staged = staged_fixture(db, dir, name, seeded).await;
    let verified = cp
        .verify_artifact(VerifyArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            staging_root: staged.staging_path.parent().unwrap().to_path_buf(),
        })
        .await
        .unwrap();
    assert_eq!(verified.status.as_str(), "succeeded");
    staged.verification_id = Some(verified.verification_id);
    staged
}

async fn inject_recovery_required(url: &str, staged: &StagedFixture, dir: &Path) {
    let pool = voom_store::connect(url).await.unwrap();
    let target_path = dir.join("recovery-target.mp4");
    let temp_path = dir.join("recovery-target.mp4.voom.tmp");
    std::fs::write(&target_path, b"promoted bytes").unwrap();
    std::fs::write(&temp_path, b"temp bytes").unwrap();
    sqlx::query(
        "INSERT INTO artifact_commit_records \
         (artifact_handle_id, source_file_version_id, verification_id, target_path, \
          result_file_version_id, result_file_location_id, state, failure_class, error_code, \
          message, recovery_reason, temp_path, report, started_at, promotion_started_at, finished_at) \
         VALUES (?, ?, ?, ?, NULL, NULL, 'recovery_required', 'commit_failure', \
          'DB_UNREACHABLE', 'injected recovery for integration inspection', 'promotion_started', ?, \
          '{\"test\":true}', '2026-05-25T00:00:00Z', '2026-05-25T00:00:01Z', '2026-05-25T00:00:02Z')",
    )
    .bind(i64::try_from(staged.artifact_handle_id.0).unwrap())
    .bind(i64::try_from(staged.source_file_version_id.0).unwrap())
    .bind(i64::try_from(staged.verification_id.unwrap().0).unwrap())
    .bind(target_path.display().to_string())
    .bind(temp_path.display().to_string())
    .execute(&pool)
    .await
    .unwrap();
}

fn install_worker_siblings() -> FfprobeSiblingGuard {
    copy_worker_to_profile_dir("voom-ffprobe-worker");
    copy_worker_to_profile_dir("voom-verify-artifact-worker");
    install_fake_ffprobe_sibling(success_ffprobe_binary(), "staged-artifact-flow").unwrap()
}

fn copy_worker_to_profile_dir(package: &'static str) {
    let worker = cargo_bin_or_build(package, package).unwrap();
    let sibling = target_debug_binary(package);
    if sibling != worker {
        std::fs::copy(worker, &sibling).unwrap();
        make_executable(&sibling);
    }
}

fn tiny_media_fixture() -> PathBuf {
    workspace_root()
        .join("crates/voom-ffprobe-worker/fixtures/media/tiny.mp4")
        .canonicalize()
        .unwrap()
}

fn success_ffprobe_binary() -> &'static PathBuf {
    static BIN: OnceLock<(TempDir, PathBuf)> = OnceLock::new();
    &BIN.get_or_init(|| {
        let dir = TempDir::new().unwrap();
        let script = format!(
            "#!/usr/bin/env sh\n\
             set -eu\n\
             if [ \"${{1:-}}\" = '-version' ]; then printf 'ffprobe version test-helper Copyright\\n'; exit 0; fi\n\
             cat <<'JSON'\n\
             {BASIC_FFPROBE_JSON}\n\
             JSON\n"
        );
        let path = dir.path().join("ffprobe");
        std::fs::write(&path, script).unwrap();
        make_executable(&path);
        (dir, path)
    })
    .1
}

fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}
