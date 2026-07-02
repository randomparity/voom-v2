use super::*;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use time::OffsetDateTime;
use voom_core::clock_test_support::ManualClock;
use voom_core::rng_test_support::FrozenRng;
use voom_core::{ErrorCode, FailureClass, WorkerId};
use voom_worker_protocol::{ProbeFileRequest, ProbeFileResult, ProbeFileStatus};

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

#[tokio::test]
async fn directory_scan_summarizes_successes_and_skips() {
    let dir = tempfile::tempdir().unwrap();
    let alpha = write_file(dir.path(), "alpha.mkv", b"alpha");
    let beta = write_file(dir.path(), "beta.mp4", b"beta");
    let note = write_file(dir.path(), "note.txt", b"not media");
    let (cp, _db) = cp_with_manual_clock(T0).await;
    let mut launcher = FakeLauncher::new(FakePlan::AllSuccess);

    let report = cp
        .scan_path_with_launcher(
            ScanPathInput {
                path: dir.path().to_path_buf(),
                extension_allowlist: Vec::new(),
            },
            &mut launcher,
        )
        .await
        .unwrap();

    assert_eq!(report.mode, discovery::ScanMode::Directory);
    assert_eq!(report.path, dir.path().canonicalize().unwrap());
    assert_eq!(report.summary.discovered, 3);
    assert_eq!(report.summary.ingested, 2);
    assert_eq!(report.summary.probed, 2);
    assert_eq!(report.summary.snapshots_recorded, 2);
    assert_eq!(report.summary.skipped, 1);
    assert_eq!(report.summary.failed, 0);
    assert_eq!(
        report
            .files
            .iter()
            .map(|file| (file.path.as_path(), file.status))
            .collect::<Vec<_>>(),
        vec![
            (alpha.as_path(), ScanReportFileStatus::Scanned),
            (beta.as_path(), ScanReportFileStatus::Scanned),
        ]
    );
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].path, note);
    assert_eq!(
        report.skipped[0].status,
        ScanReportFileStatus::SkippedUnsupportedExtension
    );
    assert_eq!(launcher.shutdowns(), vec![launcher.launched()[0]]);
}

#[tokio::test]
async fn directory_scan_persists_matching_srt_sidecar_as_bundle_member() {
    let dir = tempfile::tempdir().unwrap();
    let media = write_file(dir.path(), "Movie.Name.mkv", b"movie");
    let sidecar = write_file(dir.path(), "Movie.Name.eng.srt", b"subtitle");
    let (cp, _db) = cp_with_manual_clock(T0).await;
    let mut launcher = FakeLauncher::new(FakePlan::AllSuccess);

    let report = cp
        .scan_path_with_launcher(
            ScanPathInput {
                path: dir.path().to_path_buf(),
                extension_allowlist: Vec::new(),
            },
            &mut launcher,
        )
        .await
        .unwrap();

    assert_eq!(report.summary.ingested, 2);
    assert_eq!(report.summary.discovered, 2);
    assert_eq!(report.summary.snapshots_recorded, 1);
    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].path, media);
    assert!(report.files[0].bundle_id.is_some());
    assert_eq!(
        report.files[0].bundle_member_role.as_deref(),
        Some("primary_video")
    );
    assert_eq!(report.files[0].sidecars.len(), 1);
    assert_eq!(report.files[0].sidecars[0].path, sidecar);
    assert_eq!(
        report.files[0].sidecars[0].bundle_id,
        report.files[0].bundle_id.unwrap()
    );
    assert_eq!(
        report.files[0].sidecars[0].bundle_member_role,
        "external_subtitle"
    );
    assert!(
        report.files[0].sidecars[0]
            .content_hash
            .starts_with("sha256:")
    );
    assert_eq!(table_count(&cp, "file_assets").await, 2);
    assert_eq!(table_count(&cp, "media_snapshots").await, 1);
    assert_eq!(table_count(&cp, "media_works").await, 1);
    assert_eq!(table_count(&cp, "media_variants").await, 1);
    assert_eq!(table_count(&cp, "asset_bundles").await, 1);
    assert_eq!(table_count(&cp, "asset_bundle_members").await, 2);
}

#[tokio::test]
async fn repeated_directory_scan_links_sidecar_without_membership_conflict() {
    let dir = tempfile::tempdir().unwrap();
    write_file(dir.path(), "Movie.Name.mkv", b"movie");
    write_file(dir.path(), "Movie.Name.eng.srt", b"subtitle");
    let (cp, _db) = cp_with_manual_clock(T0).await;

    let first = cp
        .scan_path_with_launcher(
            ScanPathInput {
                path: dir.path().to_path_buf(),
                extension_allowlist: Vec::new(),
            },
            &mut FakeLauncher::new(FakePlan::AllSuccess),
        )
        .await
        .unwrap();
    let second = cp
        .scan_path_with_launcher(
            ScanPathInput {
                path: dir.path().to_path_buf(),
                extension_allowlist: Vec::new(),
            },
            &mut FakeLauncher::new(FakePlan::AllSuccess),
        )
        .await
        .unwrap();

    assert_eq!(first.files[0].sidecars.len(), 1);
    assert_eq!(second.files[0].sidecars.len(), 1);
    assert_eq!(
        first.files[0].sidecars[0].bundle_id,
        first.files[0].bundle_id.unwrap()
    );
    assert_eq!(
        second.files[0].sidecars[0].bundle_id,
        second.files[0].bundle_id.unwrap()
    );
    assert_eq!(table_count(&cp, "asset_bundles").await, 2);
    assert_eq!(table_count(&cp, "asset_bundle_members").await, 4);
}

#[tokio::test]
async fn scan_report_root_path_is_canonical() {
    let dir = tempfile::tempdir().unwrap();
    let _media = write_file(dir.path(), "movie.mkv", b"movie");
    let nested = dir.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    let noncanonical = nested.join("..");
    let (cp, _db) = cp_with_manual_clock(T0).await;
    let mut launcher = FakeLauncher::new(FakePlan::AllSuccess);

    let report = cp
        .scan_path_with_launcher(
            ScanPathInput {
                path: noncanonical,
                extension_allowlist: Vec::new(),
            },
            &mut launcher,
        )
        .await
        .unwrap();

    assert_eq!(report.path, dir.path().canonicalize().unwrap());
}

#[tokio::test]
async fn all_skipped_directory_does_not_launch_worker() {
    let dir = tempfile::tempdir().unwrap();
    let note = write_file(dir.path(), "note.txt", b"not media");
    let (cp, _db) = cp_with_manual_clock(T0).await;
    let mut launcher = FakeLauncher::new(FakePlan::AllSuccess);

    let report = cp
        .scan_path_with_launcher(
            ScanPathInput {
                path: dir.path().to_path_buf(),
                extension_allowlist: Vec::new(),
            },
            &mut launcher,
        )
        .await
        .unwrap();

    assert_eq!(report.summary.discovered, 1);
    assert_eq!(report.summary.skipped, 1);
    assert_eq!(report.summary.probed, 0);
    assert_eq!(report.summary.failed, 0);
    assert_eq!(report.skipped[0].path, note);
    assert!(launcher.launched().is_empty());
    assert!(launcher.dispatched().is_empty());
    assert!(launcher.shutdowns().is_empty());
}

#[tokio::test]
async fn single_filesystem_directory_scan_uses_one_worker() {
    let dir = tempfile::tempdir().unwrap();
    let alpha = write_file(dir.path(), "alpha.mkv", b"alpha");
    let beta = write_file(dir.path(), "beta.mkv", b"beta");
    let classifier = fake_classifier([
        (alpha.clone(), ScanFilesystemIdentity(7)),
        (beta.clone(), ScanFilesystemIdentity(7)),
    ]);
    let (cp, _db) = cp_with_manual_clock(T0).await;
    let mut launcher = FakeLauncher::new(FakePlan::AllSuccess);

    let report = cp
        .scan_path_with_launcher_and_classifier(
            ScanPathInput {
                path: dir.path().to_path_buf(),
                extension_allowlist: Vec::new(),
            },
            &mut launcher,
            &classifier,
        )
        .await
        .unwrap();

    assert_eq!(
        report
            .files
            .iter()
            .map(|file| file.path.as_path())
            .collect::<Vec<_>>(),
        vec![alpha.as_path(), beta.as_path()]
    );
    assert_eq!(launcher.launched().len(), 1);
    assert_eq!(
        launcher.dispatched(),
        vec![launcher.launched()[0], launcher.launched()[0]]
    );
    assert_eq!(launcher.shutdowns(), launcher.launched());
}

#[tokio::test]
async fn multi_filesystem_directory_scan_uses_one_worker_per_identity() {
    let dir = tempfile::tempdir().unwrap();
    let alpha = write_file(dir.path(), "alpha.mkv", b"alpha");
    let beta = write_file(dir.path(), "beta.mkv", b"beta");
    let classifier = fake_classifier([
        (alpha.clone(), ScanFilesystemIdentity(10)),
        (beta.clone(), ScanFilesystemIdentity(20)),
    ]);
    let (cp, _db) = cp_with_manual_clock(T0).await;
    let mut launcher = FakeLauncher::new(FakePlan::AllSuccess);

    let report = cp
        .scan_path_with_launcher_and_classifier(
            ScanPathInput {
                path: dir.path().to_path_buf(),
                extension_allowlist: Vec::new(),
            },
            &mut launcher,
            &classifier,
        )
        .await
        .unwrap();

    assert_eq!(launcher.launched().len(), 2);
    assert_eq!(launcher.dispatched().len(), 2);
    assert_eq!(launcher.shutdowns().len(), 2);
    assert_eq!(
        report
            .files
            .iter()
            .map(|file| file.path.as_path())
            .collect::<Vec<_>>(),
        vec![alpha.as_path(), beta.as_path()]
    );
}

#[tokio::test]
async fn multi_filesystem_directory_scan_dispatches_groups_concurrently() {
    let dir = tempfile::tempdir().unwrap();
    let alpha = write_file(dir.path(), "alpha.mkv", b"alpha");
    let beta = write_file(dir.path(), "beta.mkv", b"beta");
    let classifier = fake_classifier([
        (alpha.clone(), ScanFilesystemIdentity(10)),
        (beta.clone(), ScanFilesystemIdentity(20)),
    ]);
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let (cp, _db) = cp_with_manual_clock(T0).await;
    let mut launcher = FakeLauncher::new(FakePlan::WaitForDispatchBarrier(barrier));

    let report = tokio::time::timeout(
        Duration::from_secs(2),
        cp.scan_path_with_launcher_and_classifier(
            ScanPathInput {
                path: dir.path().to_path_buf(),
                extension_allowlist: Vec::new(),
            },
            &mut launcher,
            &classifier,
        ),
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(launcher.launched().len(), 2);
    assert_eq!(launcher.dispatched().len(), 2);
    assert_eq!(
        report
            .files
            .iter()
            .map(|file| file.path.as_path())
            .collect::<Vec<_>>(),
        vec![alpha.as_path(), beta.as_path()]
    );
}

#[tokio::test]
async fn multi_filesystem_fatal_probe_error_preserves_ordered_report() {
    let dir = tempfile::tempdir().unwrap();
    let alpha = write_file(dir.path(), "alpha.mkv", b"alpha");
    let beta = write_file(dir.path(), "beta.mkv", b"beta");
    let classifier = fake_classifier([
        (alpha.clone(), ScanFilesystemIdentity(10)),
        (beta.clone(), ScanFilesystemIdentity(20)),
    ]);
    let (cp, _db) = cp_with_manual_clock(T0).await;
    let mut launcher = FakeLauncher::new(FakePlan::WorkerErrorOnPath {
        path: beta.clone(),
        error: ffprobe_spawn_error(),
    });

    let err = cp
        .scan_path_with_launcher_and_classifier(
            ScanPathInput {
                path: dir.path().to_path_buf(),
                extension_allowlist: Vec::new(),
            },
            &mut launcher,
            &classifier,
        )
        .await
        .unwrap_err();

    assert_eq!(err.code(), ErrorCode::ExternalSystemUnavailable);
    assert_eq!(err.report().summary.ingested, 1);
    assert_eq!(err.report().summary.failed, 1);
    assert_eq!(table_count(&cp, "media_snapshots").await, 1);
    assert_eq!(err.report().files.len(), 2);
    assert_eq!(err.report().files[0].path, alpha);
    assert_eq!(err.report().files[0].status, ScanReportFileStatus::Scanned);
    assert_eq!(err.report().files[1].path, beta);
    assert_eq!(err.report().files[1].status, ScanReportFileStatus::Failed);
    assert_eq!(launcher.launched().len(), 2);
    assert_eq!(launcher.shutdowns().len(), 2);
}

#[tokio::test]
async fn launch_failure_after_prior_group_shuts_down_started_worker() {
    let dir = tempfile::tempdir().unwrap();
    let alpha = write_file(dir.path(), "alpha.mkv", b"alpha");
    let beta = write_file(dir.path(), "beta.mkv", b"beta");
    let classifier = fake_classifier([
        (alpha, ScanFilesystemIdentity(10)),
        (beta, ScanFilesystemIdentity(20)),
    ]);
    let (cp, _db) = cp_with_manual_clock(T0).await;
    let mut launcher = FakeLauncher::new(FakePlan::LaunchErrorOnAttempt(2));

    let err = cp
        .scan_path_with_launcher_and_classifier(
            ScanPathInput {
                path: dir.path().to_path_buf(),
                extension_allowlist: Vec::new(),
            },
            &mut launcher,
            &classifier,
        )
        .await
        .unwrap_err();

    assert_eq!(err.code(), ErrorCode::ExternalSystemUnavailable);
    assert_eq!(launcher.launched().len(), 1);
    assert_eq!(launcher.shutdowns(), launcher.launched());
}

#[tokio::test]
async fn failure_after_prior_commit_returns_success_and_failing_file() {
    let dir = tempfile::tempdir().unwrap();
    let alpha = write_file(dir.path(), "alpha.mkv", b"alpha");
    let beta = write_file(dir.path(), "beta.mkv", b"beta");
    let (cp, _db) = cp_with_manual_clock(T0).await;
    let mut launcher = FakeLauncher::new(FakePlan::DriftOnPath(beta.clone()));

    let err = cp
        .scan_path_with_launcher(
            ScanPathInput {
                path: dir.path().to_path_buf(),
                extension_allowlist: Vec::new(),
            },
            &mut launcher,
        )
        .await
        .unwrap_err();

    assert_eq!(err.code(), ErrorCode::ArtifactChecksumMismatch);
    let report = err.report();
    assert_eq!(report.summary.ingested, 1);
    assert_eq!(report.summary.probed, 2);
    assert_eq!(report.summary.snapshots_recorded, 1);
    assert_eq!(report.summary.failed, 1);
    assert_eq!(table_count(&cp, "file_assets").await, 1);
    assert_eq!(table_count(&cp, "media_snapshots").await, 1);
    assert_eq!(report.files.len(), 2);
    assert_eq!(report.files[0].path, alpha);
    assert_eq!(report.files[0].status, ScanReportFileStatus::Scanned);
    assert!(report.files[0].file_asset_id.is_some());
    assert_eq!(report.files[1].path, beta);
    assert_eq!(
        report.files[1].status,
        ScanReportFileStatus::FailedContentDrift
    );
    let file_error = report.files[1].error.as_ref().unwrap();
    assert_eq!(file_error.code, ErrorCode::ArtifactChecksumMismatch);
    assert_eq!(
        file_error.failure_class,
        FailureClass::ArtifactChecksumMismatch
    );
    assert_eq!(launcher.shutdowns(), vec![launcher.launched()[0]]);
}

#[tokio::test]
async fn directory_scan_continues_after_unprobeable_media_file() {
    let dir = tempfile::tempdir().unwrap();
    let alpha = write_file(dir.path(), "alpha.mkv", b"alpha");
    let beta = write_file(dir.path(), "beta.mkv", b"beta");
    let (cp, _db) = cp_with_manual_clock(T0).await;
    let mut launcher = FakeLauncher::new(FakePlan::WorkerErrorOnPath {
        path: beta.clone(),
        error: ffprobe_exit_error(),
    });

    let report = cp
        .scan_path_with_launcher(
            ScanPathInput {
                path: dir.path().to_path_buf(),
                extension_allowlist: Vec::new(),
            },
            &mut launcher,
        )
        .await
        .unwrap();

    assert_eq!(report.summary.ingested, 1);
    assert_eq!(report.summary.probed, 1);
    assert_eq!(report.summary.snapshots_recorded, 1);
    assert_eq!(report.summary.failed, 1);
    assert_eq!(report.files.len(), 2);
    assert_eq!(report.files[0].path, alpha);
    assert_eq!(report.files[0].status, ScanReportFileStatus::Scanned);
    assert_eq!(report.files[1].path, beta);
    assert_eq!(report.files[1].status, ScanReportFileStatus::Failed);
    assert_eq!(
        report.files[1].error.as_ref().unwrap().code,
        ErrorCode::ExternalSystemUnavailable
    );
    assert_eq!(table_count(&cp, "media_snapshots").await, 1);
    assert_eq!(launcher.shutdowns(), vec![launcher.launched()[0]]);
}

#[tokio::test]
async fn explicit_file_scan_keeps_unprobeable_media_failure_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let media = write_file(dir.path(), "movie.mkv", b"movie");
    let (cp, _db) = cp_with_manual_clock(T0).await;
    let mut launcher = FakeLauncher::new(FakePlan::WorkerErrorOnPath {
        path: media.clone(),
        error: ffprobe_exit_error(),
    });

    let err = cp
        .scan_path_with_launcher(
            ScanPathInput {
                path: media.clone(),
                extension_allowlist: Vec::new(),
            },
            &mut launcher,
        )
        .await
        .unwrap_err();

    assert_eq!(err.code(), ErrorCode::ExternalSystemUnavailable);
    assert_eq!(err.report().summary.failed, 1);
    assert_eq!(err.report().files[0].path, media);
    assert_eq!(err.report().files[0].status, ScanReportFileStatus::Failed);
}

#[tokio::test]
async fn spawn_style_worker_failure_still_aborts_directory_scan() {
    let dir = tempfile::tempdir().unwrap();
    let alpha = write_file(dir.path(), "alpha.mkv", b"alpha");
    let beta = write_file(dir.path(), "beta.mkv", b"beta");
    let (cp, _db) = cp_with_manual_clock(T0).await;
    let mut launcher = FakeLauncher::new(FakePlan::WorkerErrorOnPath {
        path: beta.clone(),
        error: ffprobe_spawn_error(),
    });

    let err = cp
        .scan_path_with_launcher(
            ScanPathInput {
                path: dir.path().to_path_buf(),
                extension_allowlist: Vec::new(),
            },
            &mut launcher,
        )
        .await
        .unwrap_err();

    assert_eq!(err.code(), ErrorCode::ExternalSystemUnavailable);
    assert_eq!(err.report().summary.ingested, 1);
    assert_eq!(err.report().summary.failed, 1);
    assert_eq!(err.report().files[0].path, alpha);
    assert_eq!(err.report().files[1].path, beta);
}

#[tokio::test]
async fn bootstrap_worker_id_is_used_for_launch_dispatch_and_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let media = write_file(dir.path(), "movie.mkv", b"movie");
    let (cp, _db) = cp_with_manual_clock(T0).await;
    let mut launcher =
        FakeLauncher::new(FakePlan::AllSuccess).with_pool(cp.pool_for_test().clone());

    let report = cp
        .scan_path_with_launcher(
            ScanPathInput {
                path: media.clone(),
                extension_allowlist: Vec::new(),
            },
            &mut launcher,
        )
        .await
        .unwrap();

    let launched_worker_id = launcher.launched()[0];
    assert_eq!(
        launcher.builtin_name_seen_at_launch.as_deref(),
        Some("builtin.ffprobe")
    );
    assert_eq!(launcher.dispatched(), vec![launched_worker_id]);
    assert_eq!(report.files[0].probe_worker_id, Some(launched_worker_id));
    assert_eq!(
        media_snapshot_worker_ids(&cp).await,
        vec![launched_worker_id.0]
    );
    assert_eq!(launcher.shutdowns(), vec![launched_worker_id]);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn non_utf8_candidate_path_fails_before_worker_dispatch() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .join(OsString::from_vec(b"bad-\xff.mkv".to_vec()));
    std::fs::write(&path, b"movie").unwrap();
    let canonical = std::fs::canonicalize(path).unwrap();
    let (cp, _db) = cp_with_manual_clock(T0).await;
    let mut launcher = FakeLauncher::new(FakePlan::AllSuccess);

    let err = cp
        .scan_path_with_launcher(
            ScanPathInput {
                path: dir.path().to_path_buf(),
                extension_allowlist: Vec::new(),
            },
            &mut launcher,
        )
        .await
        .unwrap_err();

    let report = err.report();
    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
    assert_eq!(report.summary.failed, 1);
    assert_eq!(report.summary.probed, 0);
    assert_eq!(report.files[0].path, canonical);
    assert_eq!(report.files[0].status, ScanReportFileStatus::Failed);
    assert_eq!(
        report.files[0].error.as_ref().unwrap().code,
        ErrorCode::ConfigInvalid
    );
    assert!(launcher.dispatched().is_empty());
    assert_eq!(launcher.shutdowns(), vec![launcher.launched()[0]]);
}

#[derive(Clone)]
enum FakePlan {
    AllSuccess,
    DriftOnPath(std::path::PathBuf),
    LaunchErrorOnAttempt(usize),
    WaitForDispatchBarrier(Arc<tokio::sync::Barrier>),
    WorkerErrorOnPath {
        path: std::path::PathBuf,
        error: worker::ScanWorkerError,
    },
}

struct FakeLauncher {
    plan: FakePlan,
    pool: Option<sqlx::SqlitePool>,
    launch_attempts: usize,
    launched_worker_ids: Vec<WorkerId>,
    builtin_name_seen_at_launch: Option<String>,
    dispatched_worker_ids: Arc<Mutex<Vec<WorkerId>>>,
    shutdown_worker_ids: Arc<Mutex<Vec<WorkerId>>>,
}

impl FakeLauncher {
    fn new(plan: FakePlan) -> Self {
        Self {
            plan,
            pool: None,
            launch_attempts: 0,
            launched_worker_ids: Vec::new(),
            builtin_name_seen_at_launch: None,
            dispatched_worker_ids: Arc::new(Mutex::new(Vec::new())),
            shutdown_worker_ids: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn with_pool(mut self, pool: sqlx::SqlitePool) -> Self {
        self.pool = Some(pool);
        self
    }

    fn shutdowns(&self) -> Vec<WorkerId> {
        self.shutdown_worker_ids.lock().unwrap().clone()
    }

    fn launched(&self) -> Vec<WorkerId> {
        self.launched_worker_ids.clone()
    }

    fn dispatched(&self) -> Vec<WorkerId> {
        self.dispatched_worker_ids.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl ScanWorkerLauncher for FakeLauncher {
    async fn launch_ffprobe(
        &mut self,
        worker_id: WorkerId,
    ) -> Result<Box<dyn ProbeWorkerSession + Send>, worker::ScanWorkerError> {
        self.launch_attempts += 1;
        if matches!(self.plan, FakePlan::LaunchErrorOnAttempt(attempt) if attempt == self.launch_attempts)
        {
            return Err(ffprobe_spawn_error());
        }
        self.launched_worker_ids.push(worker_id);
        if let Some(pool) = &self.pool {
            self.builtin_name_seen_at_launch =
                sqlx::query_scalar("SELECT name FROM workers WHERE id = ?")
                    .bind(i64::try_from(worker_id.0).unwrap())
                    .fetch_optional(pool)
                    .await
                    .unwrap();
        }
        Ok(Box::new(FakeSession {
            worker_id,
            plan: self.plan.clone(),
            dispatched_worker_ids: self.dispatched_worker_ids.clone(),
            shutdown_worker_ids: self.shutdown_worker_ids.clone(),
        }))
    }
}

struct FakeSession {
    worker_id: WorkerId,
    plan: FakePlan,
    dispatched_worker_ids: Arc<Mutex<Vec<WorkerId>>>,
    shutdown_worker_ids: Arc<Mutex<Vec<WorkerId>>>,
}

#[derive(Clone)]
struct FakeFilesystemClassifier {
    identities: Arc<BTreeMap<std::path::PathBuf, ScanFilesystemIdentity>>,
}

#[async_trait::async_trait]
impl ScanFilesystemClassifier for FakeFilesystemClassifier {
    async fn identify(&self, path: &Path) -> ScanFilesystemIdentity {
        self.identities
            .get(path)
            .copied()
            .unwrap_or(ScanFilesystemIdentity(0))
    }
}

fn fake_classifier<const N: usize>(
    identities: [(std::path::PathBuf, ScanFilesystemIdentity); N],
) -> FakeFilesystemClassifier {
    FakeFilesystemClassifier {
        identities: Arc::new(BTreeMap::from(identities)),
    }
}

#[async_trait::async_trait]
impl ProbeWorkerSession for FakeSession {
    fn worker_id(&self) -> WorkerId {
        self.worker_id
    }

    async fn dispatch_probe_file(
        &mut self,
        request: ProbeFileRequest,
    ) -> Result<ProbeFileResult, worker::ScanWorkerError> {
        self.dispatched_worker_ids
            .lock()
            .unwrap()
            .push(self.worker_id);
        if let FakePlan::WorkerErrorOnPath { path, error } = &self.plan
            && path.to_str() == Some(&request.path)
        {
            return Err(error.clone());
        }
        if let FakePlan::WaitForDispatchBarrier(barrier) = &self.plan {
            barrier.wait().await;
        }
        let mut result = matching_probe_result(&request);
        if matches!(&self.plan, FakePlan::DriftOnPath(path) if path.to_str() == Some(&request.path))
        {
            result.post_probe.content_hash = "blake3:changed".to_owned();
        }
        Ok(result)
    }

    async fn shutdown(self: Box<Self>) {
        self.shutdown_worker_ids
            .lock()
            .unwrap()
            .push(self.worker_id);
    }
}

fn ffprobe_exit_error() -> worker::ScanWorkerError {
    ffprobe_terminal_error(
        "exit",
        "external system unavailable: ffprobe exited with status 1",
    )
}

fn ffprobe_spawn_error() -> worker::ScanWorkerError {
    ffprobe_terminal_error(
        "spawn",
        "external system unavailable: No such file or directory",
    )
}

fn ffprobe_terminal_error(stage: &str, message: &str) -> worker::ScanWorkerError {
    worker::ScanWorkerError::terminal_error_for_test(
        FailureClass::ExternalSystemUnavailable,
        ErrorCode::ExternalSystemUnavailable,
        message,
        Some(json!({ "stage": stage })),
    )
}

fn matching_probe_result(request: &ProbeFileRequest) -> ProbeFileResult {
    let observed = voom_worker_protocol::ObservedFileFacts {
        size_bytes: request.expected.size_bytes,
        content_hash: request.expected.content_hash.clone(),
        modified_at: request.expected.modified_at.clone(),
        local_file_key: None,
    };
    ProbeFileResult {
        status: ProbeFileStatus::Probed,
        provider: "fake-ffprobe".to_owned(),
        provider_version: "test".to_owned(),
        pre_probe: observed.clone(),
        post_probe: observed,
        snapshot: json!({"ok": true}),
    }
}

fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    std::fs::canonicalize(path).unwrap()
}

async fn cp_with_manual_clock(
    now: OffsetDateTime,
) -> (crate::ControlPlane, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        Arc::new(ManualClock::new(now)),
        Arc::new(Mutex::new(FrozenRng::new(u32::MAX))),
    )
    .await
    .unwrap();
    (cp, tmp)
}

async fn table_count(cp: &crate::ControlPlane, table: &str) -> i64 {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    sqlx::query_scalar(&sql)
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
}

async fn media_snapshot_worker_ids(cp: &crate::ControlPlane) -> Vec<u64> {
    sqlx::query_scalar("SELECT probed_by FROM media_snapshots ORDER BY id ASC")
        .fetch_all(cp.pool_for_test())
        .await
        .unwrap()
}
