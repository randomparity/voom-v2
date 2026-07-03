#![expect(
    clippy::unwrap_used,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

//! #297 — the commit safety gate is re-evaluated on the `recover_commit`
//! re-drive path. A blocking use lease acquired while a commit sat in
//! `recovery_required` fails the re-drive before the target file is written; a
//! clean re-drive completes and records the leases the gate considered in the
//! completed event.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tempfile::{NamedTempFile, TempDir};
use time::{Duration, OffsetDateTime};

use voom_control_plane::scan::ScanPathInput;
use voom_control_plane::{ControlPlane, StageCopyInput, VerifyArtifactInput};
use voom_core::ErrorCode;
use voom_core::ids::ArtifactVerificationId;
use voom_store::repo::artifacts::ArtifactCommitState;
use voom_store::repo::{BlockingMode, IssuerKind, LeaseScope, NewUseLease, UseLeaseKind};
use voom_test_support::worker::{
    FfprobeSiblingGuard, cargo_bin_or_build, install_fake_ffprobe_sibling, target_debug_binary,
    workspace_root,
};

const BASIC_FFPROBE_JSON: &str =
    include_str!("../../voom-ffprobe-worker/fixtures/ffprobe/basic-mp4.json");

#[tokio::test]
async fn blocking_lease_acquired_during_recovery_blocks_redrive() {
    let _ffprobe_guard = install_worker_siblings();
    let (cp, db, dir) = fixture().await;
    let verified = verified_fixture(&cp, dir.path(), "blocked-recovery").await;
    let target_path = dir.path().join("blocked-recovery-target.mp4");
    inject_recovery_required(&db.url, &verified, &target_path).await;

    // A blocking lease appears after the commit is already stuck in
    // recovery_required — the re-drive must fail closed rather than promote.
    let lease = cp
        .use_leases()
        .acquire(blocking_lease(verified.source_file_version_id))
        .await
        .unwrap();

    let err = cp
        .recover_commit(verified.artifact_handle_id)
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::BlockedByUseLease);
    assert!(
        err.to_string().contains(&lease.id.0.to_string()),
        "error {err} must name the blocking lease {}",
        lease.id.0
    );
    assert!(
        !target_path.exists(),
        "re-drive must not install the target when blocked"
    );
    let commit = cp
        .show_artifact(verified.artifact_handle_id)
        .await
        .unwrap()
        .latest_commit
        .unwrap();
    assert_eq!(commit.state, ArtifactCommitState::RecoveryRequired);
}

#[tokio::test]
async fn clean_recovery_redrive_completes_and_records_evaluated_leases() {
    let _ffprobe_guard = install_worker_siblings();
    let (cp, db, dir) = fixture().await;
    let verified = verified_fixture(&cp, dir.path(), "clean-recovery").await;
    let target_path = dir.path().join("clean-recovery-target.mp4");
    inject_recovery_required(&db.url, &verified, &target_path).await;

    // An advisory lease overlaps the commit scope but does not block; the
    // re-drive completes and records the lease the gate considered.
    let lease = cp
        .use_leases()
        .acquire(advisory_lease(verified.source_file_version_id))
        .await
        .unwrap();

    let recovered = cp
        .recover_commit(verified.artifact_handle_id)
        .await
        .unwrap();

    assert_eq!(recovered.state, ArtifactCommitState::Committed);
    assert!(
        target_path.exists(),
        "clean re-drive must install the target"
    );
    let evaluated =
        commit_completed_evaluated_lease_ids(&db.url, verified.artifact_handle_id.0).await;
    assert!(
        evaluated.contains(&lease.id.0),
        "advisory lease {} must appear in gate_evaluated_lease_ids {evaluated:?}",
        lease.id.0
    );
}

fn blocking_lease(version_id: voom_core::FileVersionId) -> NewUseLease {
    NewUseLease {
        kind: UseLeaseKind::Playback,
        scope: LeaseScope::Version(version_id),
        issuer_kind: IssuerKind::User,
        issuer_ref: "watcher".to_owned(),
        blocking_mode: BlockingMode::Blocking,
        ttl: Some(Duration::seconds(3600)),
        acquired_at: OffsetDateTime::now_utc(),
    }
}

fn advisory_lease(version_id: voom_core::FileVersionId) -> NewUseLease {
    NewUseLease {
        kind: UseLeaseKind::Scan,
        scope: LeaseScope::Version(version_id),
        issuer_kind: IssuerKind::Worker,
        issuer_ref: "scanner".to_owned(),
        blocking_mode: BlockingMode::Advisory,
        ttl: Some(Duration::seconds(3600)),
        acquired_at: OffsetDateTime::now_utc(),
    }
}

// --- recovery injection -----------------------------------------------------

/// Inject a `recovery_required` commit record whose target does not yet exist,
/// so `recover_commit` re-drives a fresh install. The live staging location and
/// successful verification from `verified_fixture` remain valid inputs.
async fn inject_recovery_required(url: &str, staged: &StagedFixture, target_path: &Path) {
    let pool = voom_store::connect(url).await.unwrap();
    let temp_path = target_path.with_extension("mp4.voom.tmp");
    sqlx::query(
        "INSERT INTO artifact_commit_records \
         (artifact_handle_id, source_file_version_id, verification_id, target_path, \
          result_file_version_id, result_file_location_id, state, failure_class, error_code, \
          message, recovery_reason, temp_path, report, started_at, promotion_started_at, finished_at) \
         VALUES (?, ?, ?, ?, NULL, NULL, 'recovery_required', 'database_unavailable', \
          'DB_UNREACHABLE', 'injected recovery for gate re-drive', 'finalize_failed', ?, \
          '{\"test\":true}', '2026-05-25T00:00:00Z', '2026-05-25T00:00:01Z', '2026-05-25T00:00:02Z')",
    )
    .bind(i64::try_from(staged.artifact_handle_id.0).unwrap())
    .bind(i64::try_from(staged.source_file_version_id.0).unwrap())
    .bind(i64::try_from(staged.verification_id.0).unwrap())
    .bind(target_path.display().to_string())
    .bind(temp_path.display().to_string())
    .execute(&pool)
    .await
    .unwrap();
}

async fn commit_completed_evaluated_lease_ids(url: &str, artifact_handle_id: u64) -> Vec<u64> {
    let pool = voom_store::connect(url).await.unwrap();
    let payload: String = sqlx::query_scalar(
        "SELECT payload FROM events \
         WHERE kind = 'artifact.commit_completed' AND subject_id = ? \
         ORDER BY event_id DESC LIMIT 1",
    )
    .bind(i64::try_from(artifact_handle_id).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
    payload["gate_evaluated_lease_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect()
}

// --- harness (mirrors staged_artifact_flow.rs) ------------------------------

#[derive(Debug)]
struct Db {
    _tmp: NamedTempFile,
    url: String,
}

#[derive(Debug)]
#[expect(
    clippy::struct_field_names,
    reason = "fields mirror the persisted commit-record columns the re-drive reads"
)]
struct StagedFixture {
    artifact_handle_id: voom_core::ArtifactHandleId,
    source_file_version_id: voom_core::FileVersionId,
    verification_id: ArtifactVerificationId,
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

async fn verified_fixture(cp: &ControlPlane, dir: &Path, name: &str) -> StagedFixture {
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
    let verified = cp
        .verify_artifact(VerifyArtifactInput {
            artifact_handle_id: staged.artifact_handle_id,
            staging_root: staging_path.parent().unwrap().to_path_buf(),
        })
        .await
        .unwrap();
    assert_eq!(verified.status.as_str(), "succeeded");
    StagedFixture {
        artifact_handle_id: staged.artifact_handle_id,
        source_file_version_id: staged.source_file_version_id,
        verification_id: verified.verification_id,
    }
}

fn install_worker_siblings() -> FfprobeSiblingGuard {
    copy_worker_to_profile_dir("voom-ffprobe-worker");
    copy_worker_to_profile_dir("voom-verify-artifact-worker");
    install_fake_ffprobe_sibling(success_ffprobe_binary(), "recover-commit-gate").unwrap()
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
