use std::io;

use serde::Serialize;
use voom_core::{BackupId, ErrorCode, format_iso8601};
use voom_store::repo::backups::Backup;

use crate::cli::BackupCommand;
use crate::commands::common::{emit_voom_error, next_cursor, open_control_plane};
use crate::envelope::{Local, emit_err, emit_ok, emit_ok_page};

const COMMAND: &str = "backup";

#[derive(Debug, Serialize)]
struct ListData {
    backups: Vec<BackupData>,
}

#[derive(Debug, Serialize)]
struct ShowData {
    backup: BackupData,
}

#[derive(Debug, Serialize)]
struct BackupData {
    id: u64,
    source_file_version_id: u64,
    job_id: u64,
    ticket_id: u64,
    provider: String,
    destination_path: String,
    size_bytes: Option<u64>,
    checksum: Option<String>,
    status: &'static str,
    failure_class: Option<String>,
    error_code: Option<String>,
    message: Option<String>,
    started_at: String,
    finished_at: Option<String>,
    created_at: String,
}

impl From<Backup> for BackupData {
    fn from(backup: Backup) -> Self {
        Self {
            id: backup.id.0,
            source_file_version_id: backup.source_file_version_id.0,
            job_id: backup.job_id.0,
            ticket_id: backup.ticket_id.0,
            provider: backup.provider,
            destination_path: backup.destination_path,
            size_bytes: backup.size_bytes,
            checksum: backup.checksum,
            status: backup.status.as_str(),
            failure_class: backup.failure_class,
            error_code: backup.error_code,
            message: backup.message,
            started_at: format_iso8601(backup.started_at),
            finished_at: backup.finished_at.map(format_iso8601),
            created_at: format_iso8601(backup.created_at),
        }
    }
}

pub async fn run(database_url: &str, local: Local, command: BackupCommand) -> io::Result<i32> {
    match command {
        BackupCommand::List {
            limit,
            status,
            after_id,
        } => list(database_url, local, limit, status, after_id).await,
        BackupCommand::Show { backup_id } => show(database_url, local, backup_id).await,
    }
}

async fn list(
    database_url: &str,
    local: Local,
    limit: u32,
    status: Option<crate::cli::BackupStatusArg>,
    after_id: Option<u64>,
) -> io::Result<i32> {
    let cp = match open_control_plane(COMMAND, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp
        .list_backups(
            status.map(crate::cli::BackupStatusArg::to_store),
            after_id,
            limit,
        )
        .await
    {
        Ok(rows) => {
            let cursor = next_cursor(&rows, limit, |backup| backup.id.0);
            emit_ok_page(
                COMMAND,
                ListData {
                    backups: rows.into_iter().map(BackupData::from).collect(),
                },
                cursor,
                Some(local),
                Vec::new(),
            )
            .map(|()| 0)
        }
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

async fn show(database_url: &str, local: Local, backup_id: u64) -> io::Result<i32> {
    let cp = match open_control_plane(COMMAND, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp.get_backup(BackupId(backup_id)).await {
        Ok(Some(backup)) => emit_ok(
            COMMAND,
            ShowData {
                backup: BackupData::from(backup),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Ok(None) => {
            emit_err(
                COMMAND,
                ErrorCode::NotFound.as_str(),
                format!("backup show: id={backup_id} not found"),
                None,
                Some(local),
            )?;
            Ok(2)
        }
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}
