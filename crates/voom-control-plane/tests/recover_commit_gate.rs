#![expect(
    clippy::unwrap_used,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

//! #297 — the commit safety gate holds on the recovery path under fenced
//! node-local commit intents (ADR 0074). While a non-terminal intent pins a
//! scope, conflicting blocking use leases are refused at acquisition (the
//! fence stays blocking through recovery), so they can never enter an
//! authorized scope; a clean redrive finalizes from the node's applied
//! receipt and records the leases the recovery gate re-run considered in the
//! completed event.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use tempfile::TempDir;
use time::{Duration, OffsetDateTime};
use voom_test_support::TempDatabase;

use voom_control_plane::ControlPlane;
use voom_control_plane::artifact::{CommitArtifactInput, StageCopyInput, VerifyArtifactInput};
use voom_control_plane::artifact_commit::{
    AppliedEvidence, CommitOutcomeEvidence, MismatchedEvidence,
};
use voom_control_plane::scan::RootScanOutcome;
use voom_core::ErrorCode;
use voom_core::ids::ArtifactCommitIntentId;
use voom_store::repo::media::artifacts::ArtifactCommitState;
use voom_store::repo::media::use_leases::{
    BlockingMode, IssuerKind, LeaseScope, NewUseLease, UseLeaseKind,
};
use voom_test_support::commit_node::{self, SimulatedOwnerNode};
use voom_test_support::worker::{
    FfprobeSiblingGuard, cargo_bin_or_build, install_fake_ffprobe_sibling, target_debug_binary,
    workspace_root,
};

const BASIC_FFPROBE_JSON: &str =
    include_str!("../../voom-ffprobe-worker/fixtures/ffprobe/basic-mp4.json");

#[tokio::test]
async fn blocking_lease_cannot_enter_a_pinned_recovery_scope() {
    let _ffprobe_guard = install_worker_siblings();
    let (cp, db, dir) = fixture().await;
    let node = simulated_node(&db.url).await;
    let verified = verified_fixture(&cp, dir.path(), "blocked-recovery").await;
    let target_path = dir.path().join("blocked-recovery-target.mp4");

    // The staged bytes drift after prepare; the node observes the pinned
    // facts no longer hold, reports mismatched without promoting, and the
    // record enters recovery_required.
    let task_cp = cp.clone();
    let task_target = target_path.clone();
    let task = tokio::spawn(async move {
        task_cp
            .commit_artifact(CommitArtifactInput {
                artifact_handle_id: verified.artifact_handle_id,
                target_path: task_target,
            })
            .await
    });
    let intent_id = wait_pending_intent_id(&db.url, verified.artifact_handle_id).await;
    std::fs::write(&verified.staging_path, b"mutated staging bytes").unwrap();
    node.authorize(&cp, intent_id).await.unwrap();
    node.report_applying(&cp, intent_id).await.unwrap();
    let drifted = std::fs::read(&verified.staging_path).unwrap();
    node.report_outcome(
        &cp,
        intent_id,
        CommitOutcomeEvidence::Mismatched(MismatchedEvidence {
            reason: "staged bytes do not match the pinned expected facts".to_owned(),
            observed: Some(commit_node::observed_facts(&drifted)),
        }),
    )
    .await
    .unwrap();
    let commit_err = task.await.unwrap().unwrap_err();
    assert_eq!(commit_err.code(), ErrorCode::ArtifactChecksumMismatch);

    // The fence stays blocking through recovery: a blocking lease on the
    // pinned source version is refused outright rather than entering the
    // authorized scope.
    let lease_err = cp
        .use_leases()
        .acquire(blocking_lease(verified.source_file_version_id))
        .await
        .unwrap_err();
    assert_eq!(lease_err.error_code(), ErrorCode::Conflict);
    assert!(
        lease_err.to_string().contains("artifact_commit_intent"),
        "refusal {lease_err} must name the pinning fenced intent"
    );

    // With no applied receipt the classification is operator-required: the
    // redrive fails closed and never installs the target.
    let recover_err = cp
        .recover_commit(verified.artifact_handle_id)
        .await
        .unwrap_err();
    assert_eq!(recover_err.error_code(), ErrorCode::Conflict);
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
    let node = simulated_node(&db.url).await;
    let verified = verified_fixture(&cp, dir.path(), "clean-recovery").await;
    let target_path = dir.path().join("clean-recovery-target.mp4");

    // An advisory lease overlaps the commit scope but does not block. It is
    // acquired before prepare: once the intent exists the fence refuses even
    // advisory acquisitions on the pinned scope.
    let lease = cp
        .use_leases()
        .acquire(advisory_lease(verified.source_file_version_id))
        .await
        .unwrap();

    // The node promotes matching bytes but crashes before completing: the
    // applied receipt survives and recovery finalizes from it.
    let task_cp = cp.clone();
    let task_target = target_path.clone();
    let task = tokio::spawn(async move {
        task_cp
            .commit_artifact(CommitArtifactInput {
                artifact_handle_id: verified.artifact_handle_id,
                target_path: task_target,
            })
            .await
    });
    let intent_id = wait_pending_intent_id(&db.url, verified.artifact_handle_id).await;
    node.authorize(&cp, intent_id).await.unwrap();
    node.report_applying(&cp, intent_id).await.unwrap();
    std::fs::copy(&verified.staging_path, &target_path).unwrap();
    let staged_bytes = std::fs::read(&verified.staging_path).unwrap();
    node.report_outcome(
        &cp,
        intent_id,
        CommitOutcomeEvidence::Applied(AppliedEvidence {
            observed: commit_node::observed_facts(&staged_bytes),
        }),
    )
    .await
    .unwrap();
    task.abort();

    let recovered = cp
        .recover_commit(verified.artifact_handle_id)
        .await
        .unwrap();

    assert_eq!(recovered.state, ArtifactCommitState::Committed);
    assert!(
        target_path.exists(),
        "clean re-drive must install the target"
    );
    // The recovery transaction re-runs the gate fail-closed; the leases it
    // evaluated are audited on the completed event.
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

// --- event inspection -------------------------------------------------------

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

// --- fenced-intent drivers --------------------------------------------------

/// Flip the seeded test storage-root owner into the simulated remote node.
async fn simulated_node(url: &str) -> SimulatedOwnerNode {
    let node = SimulatedOwnerNode::new().unwrap();
    let pool = voom_store::connect(url).await.unwrap();
    node.install(&pool).await.unwrap();
    node
}

/// Wait for the newest pending intent of an artifact handle (the spawned
/// `commit_artifact` prepares it asynchronously).
async fn wait_pending_intent_id(
    url: &str,
    artifact_handle_id: voom_core::ArtifactHandleId,
) -> ArtifactCommitIntentId {
    let pool = voom_store::connect(url).await.unwrap();
    let mut pending_id: Option<i64> = None;
    for _ in 0..200 {
        let pending: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM artifact_commit_intents \
             WHERE artifact_handle_id = ? AND state = 'pending' ORDER BY id DESC LIMIT 1",
        )
        .bind(i64::try_from(artifact_handle_id.0).unwrap())
        .fetch_optional(&pool)
        .await
        .unwrap();
        if pending.is_some() {
            pending_id = pending;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let id = pending_id.unwrap();
    ArtifactCommitIntentId(u64::try_from(id).unwrap())
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
    let cp = ControlPlane::open_with_pool(pool, std::sync::Arc::new(voom_core::SystemClock))
        .await
        .unwrap()
        .with_local_node_id(Some(voom_core::NodeId(9_000_001)));
    (cp, Db { _tmp: tmp, url }, dir)
}

fn artifact_tempdir() -> TempDir {
    TempDir::new_in(std::env::current_dir().unwrap()).unwrap()
}

async fn verified_fixture(cp: &ControlPlane, dir: &Path, name: &str) -> StagedFixture {
    let source_path = dir.join(format!("{name}-source.mp4"));
    std::fs::copy(tiny_media_fixture(), &source_path).unwrap();
    let outcome = cp
        .scan_library_root(voom_store::test_support::TEST_STORAGE_ROOT_ID)
        .await
        .unwrap();
    let RootScanOutcome::Scanned(scan) = outcome else {
        unreachable!("active local test root must scan")
    };
    let scanned = scan
        .files
        .iter()
        .find(|file| file.path == source_path)
        .unwrap();
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
        staging_path,
    }
}

fn install_worker_siblings() -> FfprobeSiblingGuard {
    copy_worker_to_profile_dir("voom-ffprobe-worker");
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
    static BIN: LazyLock<(TempDir, PathBuf)> = LazyLock::new(|| {
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
    });
    &BIN.1
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
