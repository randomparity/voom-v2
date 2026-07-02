use std::ffi::OsString;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use voom_core::rng_test_support::FrozenRng;
use voom_worker_protocol::{BackUpFileResult, BackUpFileStatus};

use super::*;

const NOW: &str = "1970-01-01T00:00:00Z";

#[test]
fn bundled_command_prefers_configured_bin_env_override() {
    let command =
        bundled_backup_worker_command_from(Some(OsString::from("/custom/backup")), Err(no_exe()));
    assert_eq!(command.program, OsString::from("/custom/backup"));
}

#[test]
fn bundled_command_discovers_worker_beside_current_exe() {
    let dir = tempfile::tempdir().unwrap();
    let deps_dir = dir.path().join("deps");
    std::fs::create_dir(&deps_dir).unwrap();
    let current_exe = deps_dir.join("backup_control_plane_test");
    let worker = dir.path().join("voom-backup-worker");
    std::fs::write(&worker, b"").unwrap();

    let command = bundled_backup_worker_command_from(None, Ok(current_exe));

    assert_eq!(command.program, worker.as_os_str());
}

fn no_exe() -> std::io::Error {
    std::io::Error::other("no current exe")
}

enum FakeOutcome {
    Verified { size_bytes: u64, checksum: String },
    Failed,
}

struct FakeDispatcher {
    outcome: FakeOutcome,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl BackUpFileDispatcher for FakeDispatcher {
    async fn dispatch_back_up_file(
        &self,
        _request: BackUpFileRequest,
    ) -> Result<BackUpFileResult, VoomError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match &self.outcome {
            FakeOutcome::Verified {
                size_bytes,
                checksum,
            } => Ok(BackUpFileResult {
                status: BackUpFileStatus::BackedUp,
                provider: "fake-backup".to_owned(),
                provider_version: "0".to_owned(),
                destination_path: String::new(),
                size_bytes: *size_bytes,
                checksum: checksum.clone(),
            }),
            FakeOutcome::Failed => Err(VoomError::BackupFailure("fake backup failure".to_owned())),
        }
    }
}

struct Fixture {
    cp: crate::ControlPlane,
    job_id: JobId,
    ticket_id: TicketId,
    _db: tempfile::NamedTempFile,
}

async fn fixture() -> Fixture {
    let db = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        Arc::new(voom_core::SystemClock),
        Arc::new(std::sync::Mutex::new(FrozenRng::new(u32::MAX))),
    )
    .await
    .unwrap();
    let pool = cp.pool_for_test();
    let job_id = JobId(
        sqlx::query(
            "INSERT INTO jobs (kind, state, priority, created_at, updated_at) \
             VALUES ('backup-test', 'open', 0, ?, ?)",
        )
        .bind(NOW)
        .bind(NOW)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
        .try_into()
        .unwrap(),
    );
    let ticket_id = TicketId(
        sqlx::query(
            "INSERT INTO tickets \
             (job_id, kind, state, priority, payload, attempt, max_attempts, next_eligible_at, \
              created_at, state_changed_at) \
             VALUES (?, 'backup-test', 'leased', 0, '{}', 1, 3, ?, ?, ?)",
        )
        .bind(i64::try_from(job_id.0).unwrap())
        .bind(NOW)
        .bind(NOW)
        .bind(NOW)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
        .try_into()
        .unwrap(),
    );
    Fixture {
        cp,
        job_id,
        ticket_id,
        _db: db,
    }
}

async fn seed_file_version(cp: &crate::ControlPlane) -> FileVersionId {
    let pool = cp.pool_for_test();
    let file_asset_id = sqlx::query("INSERT INTO file_assets (created_at) VALUES (?)")
        .bind(NOW)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
    FileVersionId(
        sqlx::query(
            "INSERT INTO file_versions \
             (file_asset_id, content_hash, size_bytes, produced_by, created_at) \
             VALUES (?, 'blake3:source', 3, 'external_observed', ?)",
        )
        .bind(file_asset_id)
        .bind(NOW)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
        .try_into()
        .unwrap(),
    )
}

#[tokio::test]
async fn success_writes_a_verified_backup_record() {
    let f = fixture().await;
    let fvid = seed_file_version(&f.cp).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let dispatcher = FakeDispatcher {
        outcome: FakeOutcome::Verified {
            size_bytes: 4096,
            checksum: "blake3:abcdef".to_owned(),
        },
        calls: calls.clone(),
    };

    back_up_source_before_mutation_with_dispatcher(
        &f.cp,
        std::path::Path::new("/backups"),
        std::path::Path::new("/library/movie.mkv"),
        fvid,
        f.job_id,
        f.ticket_id,
        &dispatcher,
    )
    .await
    .unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let rows = f.cp.backups.list(100).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].status,
        voom_store::repo::backups::BackupStatus::Verified
    );
    assert_eq!(rows[0].size_bytes, Some(4096));
    assert_eq!(rows[0].checksum.as_deref(), Some("blake3:abcdef"));
    assert_eq!(
        rows[0].destination_path,
        format!("/backups/v{}/movie.mkv", fvid.0)
    );
}

#[tokio::test]
async fn dispatcher_failure_aborts_with_backup_failure_and_failed_record() {
    let f = fixture().await;
    let fvid = seed_file_version(&f.cp).await;
    let dispatcher = FakeDispatcher {
        outcome: FakeOutcome::Failed,
        calls: Arc::new(AtomicUsize::new(0)),
    };

    let err = back_up_source_before_mutation_with_dispatcher(
        &f.cp,
        std::path::Path::new("/backups"),
        std::path::Path::new("/library/movie.mkv"),
        fvid,
        f.job_id,
        f.ticket_id,
        &dispatcher,
    )
    .await
    .unwrap_err();

    assert!(matches!(err, VoomError::BackupFailure(_)));
    let rows = f.cp.backups.list(100).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].status,
        voom_store::repo::backups::BackupStatus::Failed
    );
    assert_eq!(rows[0].error_code.as_deref(), Some("BACKUP_FAILURE"));
}

#[tokio::test]
async fn verified_backup_short_circuits_on_retry() {
    let f = fixture().await;
    let fvid = seed_file_version(&f.cp).await;
    let first_calls = Arc::new(AtomicUsize::new(0));
    back_up_source_before_mutation_with_dispatcher(
        &f.cp,
        std::path::Path::new("/backups"),
        std::path::Path::new("/library/movie.mkv"),
        fvid,
        f.job_id,
        f.ticket_id,
        &FakeDispatcher {
            outcome: FakeOutcome::Verified {
                size_bytes: 1,
                checksum: "blake3:1".to_owned(),
            },
            calls: first_calls,
        },
    )
    .await
    .unwrap();

    // A retry would re-enter the helper; a dispatcher that fails must never be
    // called because the verified backup short-circuits.
    let retry_calls = Arc::new(AtomicUsize::new(0));
    back_up_source_before_mutation_with_dispatcher(
        &f.cp,
        std::path::Path::new("/backups"),
        std::path::Path::new("/library/movie.mkv"),
        fvid,
        f.job_id,
        f.ticket_id,
        &FakeDispatcher {
            outcome: FakeOutcome::Failed,
            calls: retry_calls.clone(),
        },
    )
    .await
    .unwrap();

    assert_eq!(retry_calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.cp.backups.list(100).await.unwrap().len(), 1);
}

#[tokio::test]
async fn same_basename_sources_get_distinct_destinations() {
    let f = fixture().await;
    let first = seed_file_version(&f.cp).await;
    let second = seed_file_version(&f.cp).await;
    let dispatcher = FakeDispatcher {
        outcome: FakeOutcome::Verified {
            size_bytes: 1,
            checksum: "blake3:1".to_owned(),
        },
        calls: Arc::new(AtomicUsize::new(0)),
    };

    for (fvid, source) in [
        (first, "/library/a/movie.mkv"),
        (second, "/library/b/movie.mkv"),
    ] {
        back_up_source_before_mutation_with_dispatcher(
            &f.cp,
            std::path::Path::new("/backups"),
            std::path::Path::new(source),
            fvid,
            f.job_id,
            f.ticket_id,
            &dispatcher,
        )
        .await
        .unwrap();
    }

    let first_rows = f.cp.backups.list_by_file_version(first, 10).await.unwrap();
    let second_rows = f.cp.backups.list_by_file_version(second, 10).await.unwrap();
    assert_eq!(
        first_rows[0].destination_path,
        format!("/backups/v{}/movie.mkv", first.0)
    );
    assert_eq!(
        second_rows[0].destination_path,
        format!("/backups/v{}/movie.mkv", second.0)
    );
    assert_ne!(
        first_rows[0].destination_path,
        second_rows[0].destination_path
    );
}
