#![expect(
    clippy::unwrap_used,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tempfile::{NamedTempFile, TempDir};
use voom_control_plane::scan::ScanPathInput;
use voom_control_plane::{
    ArtifactInspectionState, ArtifactListInput, CommitArtifactInput, ControlPlane, StageCopyInput,
    VerifyArtifactInput,
};
use voom_core::ErrorCode;
use voom_store::repo::artifacts::ArtifactCommitState;
use voom_test_support::worker::{
    FfprobeSiblingGuard, cargo_bin_or_build, install_fake_ffprobe_sibling, target_debug_binary,
    workspace_root,
};

const BASIC_FFPROBE_JSON: &str =
    include_str!("../../voom-ffprobe-worker/fixtures/ffprobe/basic-mp4.json");

#[tokio::test]
async fn scan_stage_verify_commit_flow_persists_committed_artifact() {
    let _ffprobe_guard = install_worker_siblings();
    let (cp, _db, dir) = fixture().await;
    let media = tiny_media_fixture();

    let scan = cp
        .scan_path(ScanPathInput {
            path: media.clone(),
            extension_allowlist: Vec::new(),
        })
        .await
        .unwrap();
    let scanned = scan.files.first().unwrap();
    let staging_path = dir.path().join("staged.mp4");
    let staged = cp
        .stage_copy(StageCopyInput {
            file_version_id: scanned.file_version_id.unwrap(),
            source_location_id: scanned.file_location_id,
            staging_path: staging_path.clone(),
        })
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
    let unverified = stage_fixture(&cp, dir.path(), "unverified").await;
    let verified = verified_fixture(&cp, dir.path(), "drift").await;
    std::fs::write(&verified.staging_path, b"changed bytes").unwrap();
    let recovery = verified_fixture(&cp, dir.path(), "recovery").await;

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
    assert_eq!(recoveries.len(), 1);
    assert_eq!(
        recoveries[0].artifact_handle_id,
        recovery.artifact_handle_id
    );
}

#[derive(Debug)]
struct Db {
    _tmp: NamedTempFile,
    url: String,
}

#[derive(Debug)]
struct StagedFixture {
    artifact_handle_id: voom_core::ArtifactHandleId,
    source_file_version_id: voom_core::FileVersionId,
    staging_path: PathBuf,
    verification_id: Option<voom_core::ids::ArtifactVerificationId>,
}

async fn fixture() -> (ControlPlane, Db, TempDir) {
    let tmp = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::init(&url).await.unwrap();
    let cp = ControlPlane::open(&url).await.unwrap();
    (cp, Db { _tmp: tmp, url }, artifact_tempdir())
}

fn artifact_tempdir() -> TempDir {
    TempDir::new_in(std::env::current_dir().unwrap()).unwrap()
}

async fn stage_fixture(cp: &ControlPlane, dir: &Path, name: &str) -> StagedFixture {
    let scan = cp
        .scan_path(ScanPathInput {
            path: tiny_media_fixture(),
            extension_allowlist: Vec::new(),
        })
        .await
        .unwrap();
    let scanned = scan.files.first().unwrap();
    let staging_path = dir.join(format!("{name}-staged.mp4"));
    let staged = cp
        .stage_copy(StageCopyInput {
            file_version_id: scanned.file_version_id.unwrap(),
            source_location_id: scanned.file_location_id,
            staging_path: staging_path.clone(),
        })
        .await
        .unwrap();
    StagedFixture {
        artifact_handle_id: staged.artifact_handle_id,
        source_file_version_id: staged.source_file_version_id,
        staging_path,
        verification_id: None,
    }
}

async fn verified_fixture(cp: &ControlPlane, dir: &Path, name: &str) -> StagedFixture {
    let mut staged = stage_fixture(cp, dir, name).await;
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
         VALUES (?, ?, ?, ?, NULL, NULL, 'recovery_required', 'database_unavailable', \
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
