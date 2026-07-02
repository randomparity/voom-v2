use time::OffsetDateTime;
use voom_core::{FileVersionId, JobId, TicketId};

use super::*;

struct Fixture {
    pool: sqlx::SqlitePool,
    repo: SqliteBackupRepo,
    file_version_id: FileVersionId,
    job_id: JobId,
    ticket_id: TicketId,
    _tmp: tempfile::NamedTempFile,
}

const NOW: &str = "1970-01-01T00:00:00Z";

async fn fixture() -> Fixture {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    let repo = SqliteBackupRepo::new(pool.clone());

    let file_asset_id = sqlx::query("INSERT INTO file_assets (created_at) VALUES (?)")
        .bind(NOW)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
    let file_version_id = FileVersionId(
        sqlx::query(
            "INSERT INTO file_versions \
             (file_asset_id, content_hash, size_bytes, produced_by, created_at) \
             VALUES (?, 'blake3:source', 3, 'external_observed', ?)",
        )
        .bind(file_asset_id)
        .bind(NOW)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid()
        .try_into()
        .unwrap(),
    );
    let job_id = JobId(
        sqlx::query(
            "INSERT INTO jobs (kind, state, priority, created_at, updated_at) \
             VALUES ('backup-test', 'open', 0, ?, ?)",
        )
        .bind(NOW)
        .bind(NOW)
        .execute(&pool)
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
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid()
        .try_into()
        .unwrap(),
    );

    Fixture {
        pool,
        repo,
        file_version_id,
        job_id,
        ticket_id,
        _tmp: tmp,
    }
}

impl Fixture {
    fn new_backup(&self) -> NewBackup {
        NewBackup {
            source_file_version_id: self.file_version_id,
            job_id: self.job_id,
            ticket_id: self.ticket_id,
            provider: "voom-backup-worker".to_owned(),
            destination_path: "/backups/1/movie.mkv".to_owned(),
        }
    }
}

fn at(secs: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(secs).unwrap()
}

#[tokio::test]
async fn insert_pending_writes_a_recoverable_row() {
    let f = fixture().await;
    let backup = f.repo.insert_pending(f.new_backup(), at(0)).await.unwrap();

    assert_eq!(backup.status, BackupStatus::Pending);
    assert_eq!(backup.size_bytes, None);
    assert_eq!(backup.checksum, None);
    assert_eq!(backup.finished_at, None);

    let fetched = f.repo.get(backup.id).await.unwrap().unwrap();
    assert_eq!(fetched, backup);
}

#[tokio::test]
async fn mark_verified_records_size_and_checksum() {
    let f = fixture().await;
    let backup = f.repo.insert_pending(f.new_backup(), at(0)).await.unwrap();
    f.repo
        .mark_verified(backup.id, 4096, "blake3:abcdef", at(1))
        .await
        .unwrap();

    let fetched = f.repo.get(backup.id).await.unwrap().unwrap();
    assert_eq!(fetched.status, BackupStatus::Verified);
    assert_eq!(fetched.size_bytes, Some(4096));
    assert_eq!(fetched.checksum.as_deref(), Some("blake3:abcdef"));
    assert!(fetched.finished_at.is_some());
}

#[tokio::test]
async fn mark_failed_records_failure_detail() {
    let f = fixture().await;
    let backup = f.repo.insert_pending(f.new_backup(), at(0)).await.unwrap();
    let detail = BackupFailureDetail {
        failure_class: "backup_failure".to_owned(),
        error_code: "BACKUP_FAILURE".to_owned(),
        message: "destination unwritable".to_owned(),
    };
    f.repo.mark_failed(backup.id, &detail, at(1)).await.unwrap();

    let fetched = f.repo.get(backup.id).await.unwrap().unwrap();
    assert_eq!(fetched.status, BackupStatus::Failed);
    assert_eq!(fetched.failure_class.as_deref(), Some("backup_failure"));
    assert_eq!(fetched.error_code.as_deref(), Some("BACKUP_FAILURE"));
    assert_eq!(fetched.message.as_deref(), Some("destination unwritable"));
    assert!(fetched.finished_at.is_some());
}

#[tokio::test]
async fn mark_verified_on_non_pending_is_conflict() {
    let f = fixture().await;
    let backup = f.repo.insert_pending(f.new_backup(), at(0)).await.unwrap();
    f.repo
        .mark_verified(backup.id, 1, "blake3:1", at(1))
        .await
        .unwrap();

    let err = f
        .repo
        .mark_verified(backup.id, 1, "blake3:1", at(2))
        .await
        .unwrap_err();
    assert!(matches!(err, voom_core::VoomError::Conflict(_)));
}

#[tokio::test]
async fn list_is_ordered_by_created_then_id() {
    let f = fixture().await;
    let first = f.repo.insert_pending(f.new_backup(), at(0)).await.unwrap();
    let second = f.repo.insert_pending(f.new_backup(), at(0)).await.unwrap();

    let rows = f.repo.list(100).await.unwrap();
    assert_eq!(
        rows.iter().map(|b| b.id).collect::<Vec<_>>(),
        vec![first.id, second.id]
    );
}

#[tokio::test]
async fn list_by_file_version_scopes_to_the_source() {
    let f = fixture().await;
    let backup = f.repo.insert_pending(f.new_backup(), at(0)).await.unwrap();

    let rows = f
        .repo
        .list_by_file_version(f.file_version_id, 100)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, backup.id);

    let other = f
        .repo
        .list_by_file_version(FileVersionId(f.file_version_id.0 + 999), 100)
        .await
        .unwrap();
    assert!(other.is_empty());
}

#[tokio::test]
async fn list_pending_returns_only_pending_rows() {
    let f = fixture().await;
    let pending = f.repo.insert_pending(f.new_backup(), at(0)).await.unwrap();
    let verified = f.repo.insert_pending(f.new_backup(), at(0)).await.unwrap();
    f.repo
        .mark_verified(verified.id, 1, "blake3:1", at(1))
        .await
        .unwrap();

    let rows = f.repo.list_pending(100).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, pending.id);
}

#[tokio::test]
async fn latest_by_file_version_returns_none_then_the_most_recent_row() {
    let f = fixture().await;
    assert!(
        f.repo
            .latest_by_file_version(f.file_version_id)
            .await
            .unwrap()
            .is_none()
    );

    // An earlier failed backup superseded by a later verified one: the safety
    // gate's self-clearing semantics depend on the latest row winning.
    let failed = f.repo.insert_pending(f.new_backup(), at(0)).await.unwrap();
    let detail = BackupFailureDetail {
        failure_class: "io".to_owned(),
        error_code: "BACKUP_FAILURE".to_owned(),
        message: "disk full".to_owned(),
    };
    f.repo.mark_failed(failed.id, &detail, at(1)).await.unwrap();
    let verified = f.repo.insert_pending(f.new_backup(), at(2)).await.unwrap();
    f.repo
        .mark_verified(verified.id, 1, "blake3:1", at(3))
        .await
        .unwrap();

    let latest = f
        .repo
        .latest_by_file_version(f.file_version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.id, verified.id);
    assert_eq!(latest.status, BackupStatus::Verified);
}

#[tokio::test]
async fn verified_lookup_finds_the_verified_backup() {
    let f = fixture().await;
    assert!(
        f.repo
            .verified_for_ticket_and_version(f.ticket_id, f.file_version_id)
            .await
            .unwrap()
            .is_none()
    );

    let backup = f.repo.insert_pending(f.new_backup(), at(0)).await.unwrap();
    f.repo
        .mark_verified(backup.id, 1, "blake3:1", at(1))
        .await
        .unwrap();

    let found = f
        .repo
        .verified_for_ticket_and_version(f.ticket_id, f.file_version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, backup.id);
}

#[tokio::test]
async fn verified_key_rejects_a_duplicate_verified_backup() {
    let f = fixture().await;
    let first = f.repo.insert_pending(f.new_backup(), at(0)).await.unwrap();
    let second = f.repo.insert_pending(f.new_backup(), at(0)).await.unwrap();
    f.repo
        .mark_verified(first.id, 1, "blake3:1", at(1))
        .await
        .unwrap();

    let err = f
        .repo
        .mark_verified(second.id, 1, "blake3:1", at(2))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("mark_verified"));
}

#[tokio::test]
async fn status_check_rejects_a_verified_row_without_checksum() {
    let f = fixture().await;
    let err = sqlx::query(
        "INSERT INTO backups \
         (source_file_version_id, job_id, ticket_id, provider, destination_path, size_bytes, \
          status, started_at, finished_at, created_at) \
         VALUES (?, ?, ?, 'p', '/d', 1, 'verified', ?, ?, ?)",
    )
    .bind(i64::try_from(f.file_version_id.0).unwrap())
    .bind(i64::try_from(f.job_id.0).unwrap())
    .bind(i64::try_from(f.ticket_id.0).unwrap())
    .bind(NOW)
    .bind(NOW)
    .bind(NOW)
    .execute(&f.pool)
    .await
    .unwrap_err();
    assert!(err.to_string().contains("CHECK"));
}
