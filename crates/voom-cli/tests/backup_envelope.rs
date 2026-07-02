#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::process::Command;

use serde_json::Value;
use tempfile::NamedTempFile;
use time::OffsetDateTime;
use voom_core::{FileVersionId, JobId, TicketId};
use voom_store::repo::backups::{BackupFailureDetail, NewBackup, SqliteBackupRepo};

mod backup_envelope {
    use super::*;

    const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;
    const NOW: &str = "1970-01-01T00:00:00Z";

    struct Fixture {
        _tmp: NamedTempFile,
        url: String,
    }

    async fn fixture() -> Fixture {
        let tmp = NamedTempFile::new().unwrap();
        let url = voom_store::test_support::sqlite_url_for(tmp.path());
        voom_store::init(&url).await.unwrap();
        let pool = voom_store::connect(&url).await.unwrap();

        let file_asset_id = sqlx::query("INSERT INTO file_assets (created_at) VALUES (?)")
            .bind(NOW)
            .execute(&pool)
            .await
            .unwrap()
            .last_insert_rowid();
        let file_version_id = FileVersionId(
            u64::try_from(
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
                .last_insert_rowid(),
            )
            .unwrap(),
        );
        let job_id = JobId(
            u64::try_from(
                sqlx::query(
                    "INSERT INTO jobs (kind, state, priority, created_at, updated_at) \
                     VALUES ('backup-test', 'open', 0, ?, ?)",
                )
                .bind(NOW)
                .bind(NOW)
                .execute(&pool)
                .await
                .unwrap()
                .last_insert_rowid(),
            )
            .unwrap(),
        );
        let ticket_ids = seed_tickets(&pool, job_id, 2).await;

        let repo = SqliteBackupRepo::new(pool.clone());
        let verified = repo
            .insert_pending(new_backup(file_version_id, job_id, ticket_ids[0]), T0)
            .await
            .unwrap();
        repo.mark_verified(verified.id, 3, "blake3:source", T0)
            .await
            .unwrap();
        let failed = repo
            .insert_pending(new_backup(file_version_id, job_id, ticket_ids[1]), T0)
            .await
            .unwrap();
        repo.mark_failed(
            failed.id,
            &BackupFailureDetail {
                failure_class: "BackupFailure".to_owned(),
                error_code: "BACKUP_FAILURE".to_owned(),
                message: "destination unwritable".to_owned(),
            },
            T0,
        )
        .await
        .unwrap();

        Fixture { _tmp: tmp, url }
    }

    async fn seed_tickets(pool: &sqlx::SqlitePool, job_id: JobId, count: usize) -> Vec<TicketId> {
        let mut ids = Vec::with_capacity(count);
        for _ in 0..count {
            let id = sqlx::query(
                "INSERT INTO tickets \
                 (job_id, kind, state, priority, payload, attempt, max_attempts, \
                  next_eligible_at, created_at, state_changed_at) \
                 VALUES (?, 'backup-test', 'leased', 0, '{}', 1, 3, ?, ?, ?)",
            )
            .bind(i64::try_from(job_id.0).unwrap())
            .bind(NOW)
            .bind(NOW)
            .bind(NOW)
            .execute(pool)
            .await
            .unwrap()
            .last_insert_rowid();
            ids.push(TicketId(u64::try_from(id).unwrap()));
        }
        ids
    }

    fn new_backup(file_version_id: FileVersionId, job_id: JobId, ticket_id: TicketId) -> NewBackup {
        NewBackup {
            source_file_version_id: file_version_id,
            job_id,
            ticket_id,
            provider: "voom-backup-worker".to_owned(),
            destination_path: format!("/backups/v{}/movie.mkv", file_version_id.0),
        }
    }

    fn backup_command(url: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
        command.args(["--database-url", url, "backup"]);
        command
    }

    fn envelope(stdout: Vec<u8>) -> Value {
        let stdout = String::from_utf8(stdout).unwrap();
        serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("stdout must be one JSON envelope; got {stdout:?}: {e}"))
    }

    fn redact_local(json: &mut Value) {
        json["local"]["db_url"] = Value::String("[db-url]".to_owned());
        json["local"]["config_path"] = Value::String("[config-path]".to_owned());
    }

    #[tokio::test]
    async fn backup_list_outputs_records() {
        let fixture = fixture().await;

        let output = backup_command(&fixture.url).arg("list").output().unwrap();

        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["command"], "backup");
        assert_eq!(json["status"], "ok");
        redact_local(&mut json);
        insta::assert_json_snapshot!("backup_list_outputs_records", json);
    }

    #[tokio::test]
    async fn backup_list_filters_by_status() {
        let fixture = fixture().await;

        let output = backup_command(&fixture.url)
            .args(["list", "--status", "failed"])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["status"], "ok");
        redact_local(&mut json);
        insta::assert_json_snapshot!("backup_list_filters_by_status", json);
    }

    #[tokio::test]
    async fn backup_show_outputs_record() {
        let fixture = fixture().await;

        let output = backup_command(&fixture.url)
            .args(["show", "--backup-id", "1"])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["command"], "backup");
        assert_eq!(json["status"], "ok");
        redact_local(&mut json);
        insta::assert_json_snapshot!("backup_show_outputs_record", json);
    }

    #[tokio::test]
    async fn backup_show_unknown_id_is_not_found() {
        let fixture = fixture().await;

        let output = backup_command(&fixture.url)
            .args(["show", "--backup-id", "999"])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(2));
        let mut json = envelope(output.stdout);
        assert_eq!(json["command"], "backup");
        assert_eq!(json["status"], "error");
        assert_eq!(json["error"]["code"], "NOT_FOUND");
        redact_local(&mut json);
        insta::assert_json_snapshot!("backup_show_unknown_id_is_not_found", json);
    }
}
