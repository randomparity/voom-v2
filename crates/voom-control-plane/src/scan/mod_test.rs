use super::*;

use std::path::Path;
use std::sync::{Arc, Mutex};

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
    assert_eq!(
        launcher.shutdowns(),
        vec![launcher.launched_worker_id.unwrap()]
    );
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
        .scan_path_with_launcher(ScanPathInput { path: noncanonical }, &mut launcher)
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
    assert!(launcher.launched_worker_id.is_none());
    assert!(launcher.dispatched_worker_ids.is_empty());
    assert!(launcher.shutdowns().is_empty());
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
    assert_eq!(
        launcher.shutdowns(),
        vec![launcher.launched_worker_id.unwrap()]
    );
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
            },
            &mut launcher,
        )
        .await
        .unwrap();

    let launched_worker_id = launcher.launched_worker_id.unwrap();
    assert_eq!(
        launcher.builtin_name_seen_at_launch.as_deref(),
        Some("builtin.ffprobe")
    );
    assert_eq!(launcher.dispatched_worker_ids, vec![launched_worker_id]);
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
    assert!(launcher.dispatched_worker_ids.is_empty());
    assert_eq!(
        launcher.shutdowns(),
        vec![launcher.launched_worker_id.unwrap()]
    );
}

#[derive(Clone)]
enum FakePlan {
    AllSuccess,
    DriftOnPath(std::path::PathBuf),
}

struct FakeLauncher {
    plan: FakePlan,
    pool: Option<sqlx::SqlitePool>,
    launched_worker_id: Option<WorkerId>,
    builtin_name_seen_at_launch: Option<String>,
    dispatched_worker_ids: Vec<WorkerId>,
    shutdown_worker_ids: Arc<Mutex<Vec<WorkerId>>>,
}

impl FakeLauncher {
    fn new(plan: FakePlan) -> Self {
        Self {
            plan,
            pool: None,
            launched_worker_id: None,
            builtin_name_seen_at_launch: None,
            dispatched_worker_ids: Vec::new(),
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
}

#[async_trait::async_trait]
impl ScanWorkerLauncher for FakeLauncher {
    async fn launch_ffprobe(
        &mut self,
        worker_id: WorkerId,
    ) -> Result<Box<dyn ProbeWorkerSession + Send>, worker::ScanWorkerError> {
        self.launched_worker_id = Some(worker_id);
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
            shutdown_worker_ids: self.shutdown_worker_ids.clone(),
        }))
    }

    fn record_dispatch(&mut self, worker_id: WorkerId) {
        self.dispatched_worker_ids.push(worker_id);
    }
}

struct FakeSession {
    worker_id: WorkerId,
    plan: FakePlan,
    shutdown_worker_ids: Arc<Mutex<Vec<WorkerId>>>,
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
