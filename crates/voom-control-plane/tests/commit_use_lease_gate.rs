#![expect(
    clippy::unwrap_used,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

//! #270 — the commit safety gate is wired into the production commit path.
//! A blocking use lease live at commit time fails `commit_artifact` before the
//! target file is written; a terminal or TTL-expired lease does not; an
//! advisory lease is recorded in the completed event for audit.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tempfile::{NamedTempFile, TempDir};
use time::{Duration, OffsetDateTime};

use voom_control_plane::scan::ScanPathInput;
use voom_control_plane::{CommitArtifactInput, ControlPlane, StageCopyInput, VerifyArtifactInput};
use voom_core::ErrorCode;
use voom_store::repo::{
    BlockingMode, IssuerKind, LeaseScope, NewUseLease, UseLeaseKind, UseLeaseReleaseReason,
};
use voom_test_support::worker::{
    FfprobeSiblingGuard, cargo_bin_or_build, install_fake_ffprobe_sibling, target_debug_binary,
    workspace_root,
};

const BASIC_FFPROBE_JSON: &str =
    include_str!("../../voom-ffprobe-worker/fixtures/ffprobe/basic-mp4.json");

#[tokio::test]
async fn blocking_use_lease_fails_commit_before_target_is_written() {
    let _ffprobe_guard = install_worker_siblings();
    let (cp, db, dir) = fixture().await;
    let verified = verified_fixture(&cp, dir.path(), "blocked").await;

    let lease = cp
        .use_leases()
        .acquire(blocking_lease(verified.source_file_version_id))
        .await
        .unwrap();

    let target_path = dir.path().join("blocked-target.mp4");
    let err = cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id: verified.artifact_handle_id,
            target_path: target_path.clone(),
        })
        .await
        .unwrap_err();

    assert_eq!(err.code(), ErrorCode::BlockedByUseLease);
    assert!(
        !target_path.exists(),
        "commit must not install the target when blocked"
    );
    // The blocking lease id is named in the pre-mutation failure event message.
    let (_kind, message) = latest_commit_failed_pre_mutation(&db.url).await;
    assert!(
        message.contains(&lease.id.0.to_string()),
        "failure message {message:?} must name the blocking lease {}",
        lease.id.0
    );
}

#[tokio::test]
async fn released_lease_does_not_block_commit() {
    let _ffprobe_guard = install_worker_siblings();
    let (cp, _db, dir) = fixture().await;
    let verified = verified_fixture(&cp, dir.path(), "released").await;

    let lease = cp
        .use_leases()
        .acquire(blocking_lease(verified.source_file_version_id))
        .await
        .unwrap();
    cp.use_leases()
        .release(
            lease.id,
            UseLeaseReleaseReason::Released,
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();

    let committed = cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id: verified.artifact_handle_id,
            target_path: dir.path().join("released-target.mp4"),
        })
        .await
        .unwrap();
    assert_eq!(
        committed.state,
        voom_store::repo::artifacts::ArtifactCommitState::Committed
    );
}

#[tokio::test]
async fn ttl_expired_lease_does_not_block_commit() {
    let _ffprobe_guard = install_worker_siblings();
    let (cp, _db, dir) = fixture().await;
    let verified = verified_fixture(&cp, dir.path(), "expired").await;

    // Acquired an hour ago with a 1s TTL — expired against the control-plane
    // clock, but never swept (release_reason still NULL).
    cp.use_leases()
        .acquire(NewUseLease {
            kind: UseLeaseKind::Playback,
            scope: LeaseScope::Version(verified.source_file_version_id),
            issuer_kind: IssuerKind::User,
            issuer_ref: "watcher".to_owned(),
            blocking_mode: BlockingMode::Blocking,
            ttl: Some(Duration::seconds(1)),
            acquired_at: OffsetDateTime::now_utc() - Duration::hours(1),
        })
        .await
        .unwrap();

    let committed = cp
        .commit_artifact(CommitArtifactInput {
            artifact_handle_id: verified.artifact_handle_id,
            target_path: dir.path().join("expired-target.mp4"),
        })
        .await
        .unwrap();
    assert_eq!(
        committed.state,
        voom_store::repo::artifacts::ArtifactCommitState::Committed
    );
}

#[tokio::test]
async fn advisory_lease_is_recorded_in_commit_event() {
    let _ffprobe_guard = install_worker_siblings();
    let (cp, db, dir) = fixture().await;
    let verified = verified_fixture(&cp, dir.path(), "advisory").await;

    let lease = cp
        .use_leases()
        .acquire(NewUseLease {
            kind: UseLeaseKind::Scan,
            scope: LeaseScope::Version(verified.source_file_version_id),
            issuer_kind: IssuerKind::Worker,
            issuer_ref: "scanner".to_owned(),
            blocking_mode: BlockingMode::Advisory,
            ttl: Some(Duration::seconds(3600)),
            acquired_at: OffsetDateTime::now_utc(),
        })
        .await
        .unwrap();

    cp.commit_artifact(CommitArtifactInput {
        artifact_handle_id: verified.artifact_handle_id,
        target_path: dir.path().join("advisory-target.mp4"),
    })
    .await
    .unwrap();

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

// --- event inspection -------------------------------------------------------

async fn latest_commit_failed_pre_mutation(url: &str) -> (String, String) {
    let pool = voom_store::connect(url).await.unwrap();
    let row: (String, String) = sqlx::query_as(
        "SELECT kind, payload FROM events \
         WHERE kind = 'artifact.commit_failed_pre_mutation' \
         ORDER BY event_id DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let payload: serde_json::Value = serde_json::from_str(&row.1).unwrap();
    let message = payload["message"].as_str().unwrap().to_owned();
    (row.0, message)
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
struct StagedFixture {
    artifact_handle_id: voom_core::ArtifactHandleId,
    source_file_version_id: voom_core::FileVersionId,
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
    }
}

fn install_worker_siblings() -> FfprobeSiblingGuard {
    copy_worker_to_profile_dir("voom-ffprobe-worker");
    copy_worker_to_profile_dir("voom-verify-artifact-worker");
    install_fake_ffprobe_sibling(success_ffprobe_binary(), "commit-use-lease-gate").unwrap()
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
