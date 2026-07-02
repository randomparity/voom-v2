//! Backup-before-mutation: a defensive copy of a mutating operation's source
//! taken before the worker dispatch, dispatched to the bundled
//! `voom-backup-worker` and recorded durably in the `backups` table.
//!
//! The gate is opt-in (`--backup-root`, threaded as
//! `Execute*Input::backup_root`) and **fail-closed**: a backup failure writes a
//! `failed` record and returns `VoomError::BackupFailure`, aborting the
//! mutating operation. It is idempotent under the phase-barrier coordinator's
//! retries — a verified backup for `(ticket, source version)` short-circuits.
//! See `docs/adr/0025-backup-worker-and-backup-before-mutation-gate.md`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use voom_core::{FailureClass, FileVersionId, JobId, LeaseId, TicketId, VoomError, WorkerId};
use voom_store::repo::backups::{BackupFailureDetail, NewBackup};
use voom_worker_protocol::{
    BackUpFileRequest, BackUpFileResult, ClientHandle, OperationKind, WorkerCredentials,
};

use crate::ControlPlane;
use crate::worker_process::{
    BundledWorkerProcess, NoopWorkerProgressHandler, WorkerCommand, WorkerOperationDispatch,
    WorkerStreamLabels, bundled_worker_command_from, dispatch_operation_with_client,
};

const PROVIDER: &str = "voom-backup-worker";
const BACKUP_WORKER_BIN_ENV: &str = "VOOM_BACKUP_WORKER_BIN";
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
const DISPATCH_IDLE_DEADLINE_MS: u32 = 30_000;
const HEARTBEAT_DEADLINE_MS: u32 = 30_000;

/// Dispatches a `back_up_file` operation to a worker. Injectable so the
/// execute-path gate can be driven with a fake in tests without a real
/// subprocess.
#[async_trait]
pub(crate) trait BackUpFileDispatcher: Send + Sync {
    async fn dispatch_back_up_file(
        &self,
        request: BackUpFileRequest,
    ) -> Result<BackUpFileResult, VoomError>;
}

/// Launches the bundled `voom-backup-worker` subprocess per dispatch, exactly
/// as the remux/transcode/audio dispatchers launch their bundled workers.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BundledBackUpFileDispatcher;

#[async_trait]
impl BackUpFileDispatcher for BundledBackUpFileDispatcher {
    async fn dispatch_back_up_file(
        &self,
        request: BackUpFileRequest,
    ) -> Result<BackUpFileResult, VoomError> {
        let command = bundled_backup_worker_command();
        let worker = BundledWorkerProcess::launch(WorkerId(0), command)
            .await
            .map_err(|err| VoomError::WorkerCrash(err.to_string()))?;
        let result =
            dispatch_back_up_file_with_client(&worker.client, &worker.credentials, request).await;
        let _status = worker.shutdown(SHUTDOWN_GRACE).await;
        result
    }
}

/// Convenience wrapper: back up the source when `backup_root` is `Some`,
/// otherwise a no-op. Keeps the guarded call at each `execute_*` site to one
/// statement.
///
/// # Errors
/// Propagates any error from [`back_up_source_before_mutation`].
pub(crate) async fn maybe_back_up_source(
    cp: &ControlPlane,
    backup_root: Option<&Path>,
    source_path: &Path,
    source_file_version_id: FileVersionId,
    job_id: JobId,
    ticket_id: TicketId,
) -> Result<(), VoomError> {
    if let Some(backup_root) = backup_root {
        back_up_source_before_mutation(
            cp,
            backup_root,
            source_path,
            source_file_version_id,
            job_id,
            ticket_id,
        )
        .await?;
    }
    Ok(())
}

/// Back up the source of a mutating operation before it is dispatched, using
/// the bundled worker. See [`back_up_source_before_mutation_with_dispatcher`].
///
/// # Errors
/// Propagates `VoomError::BackupFailure` (and any other worker/DB error) after
/// recording a `failed` backup row.
pub(crate) async fn back_up_source_before_mutation(
    cp: &ControlPlane,
    backup_root: &Path,
    source_path: &Path,
    source_file_version_id: FileVersionId,
    job_id: JobId,
    ticket_id: TicketId,
) -> Result<(), VoomError> {
    back_up_source_before_mutation_with_dispatcher(
        cp,
        backup_root,
        source_path,
        source_file_version_id,
        job_id,
        ticket_id,
        &BundledBackUpFileDispatcher,
    )
    .await
}

/// Idempotent, fail-closed backup:
/// 1. short-circuit if a verified backup for `(ticket, source version)` exists;
/// 2. insert a `pending` record;
/// 3. dispatch the copy to a collision-free destination
///    (`<backup_root>/v<version>/<basename>`);
/// 4. mark `verified` on success, or `failed` and return the error.
///
/// # Errors
/// Returns the dispatch error (typically `VoomError::BackupFailure`) after
/// recording the failure; propagates DB errors.
pub(crate) async fn back_up_source_before_mutation_with_dispatcher(
    cp: &ControlPlane,
    backup_root: &Path,
    source_path: &Path,
    source_file_version_id: FileVersionId,
    job_id: JobId,
    ticket_id: TicketId,
    dispatcher: &dyn BackUpFileDispatcher,
) -> Result<(), VoomError> {
    // Retry short-circuit: reuse an existing verified backup. If its file is
    // still on disk we are done; if it was removed out-of-band, restore it in
    // place (the verified record stays — a second verified row for the same
    // (ticket, source version) would violate `backups_verified_key`) rather
    // than letting the mutation proceed with no real backup on disk.
    if let Some(existing) = cp
        .backups
        .verified_for_ticket_and_version(ticket_id, source_file_version_id)
        .await?
    {
        if tokio::fs::try_exists(&existing.destination_path)
            .await
            .unwrap_or(false)
        {
            return Ok(());
        }
        dispatcher
            .dispatch_back_up_file(BackUpFileRequest {
                source_path: source_path.to_string_lossy().into_owned(),
                destination_path: existing.destination_path,
            })
            .await?;
        return Ok(());
    }

    let destination = backup_destination(backup_root, source_file_version_id, source_path)?;
    let destination_path = destination.to_string_lossy().into_owned();
    let record = cp
        .backups
        .insert_pending(
            NewBackup {
                source_file_version_id,
                job_id,
                ticket_id,
                provider: PROVIDER.to_owned(),
                destination_path: destination_path.clone(),
            },
            cp.clock().now(),
        )
        .await?;

    let request = BackUpFileRequest {
        source_path: source_path.to_string_lossy().into_owned(),
        destination_path,
    };
    match dispatcher.dispatch_back_up_file(request).await {
        Ok(result) => {
            cp.backups
                .mark_verified(
                    record.id,
                    result.size_bytes,
                    &result.checksum,
                    cp.clock().now(),
                )
                .await?;
            Ok(())
        }
        Err(err) => {
            cp.backups
                .mark_failed(record.id, &failure_detail(&err), cp.clock().now())
                .await?;
            Err(err)
        }
    }
}

/// `<backup_root>/v<source_file_version_id>/<source basename>`. Namespacing by
/// the source file-version id keeps distinct sources that share a basename
/// (common in a library) from colliding on the same destination.
fn backup_destination(
    backup_root: &Path,
    source_file_version_id: FileVersionId,
    source_path: &Path,
) -> Result<PathBuf, VoomError> {
    let name = source_path.file_name().ok_or_else(|| {
        VoomError::Config(format!(
            "backup source path {} has no file name",
            source_path.display()
        ))
    })?;
    Ok(backup_root
        .join(format!("v{}", source_file_version_id.0))
        .join(name))
}

fn failure_detail(err: &VoomError) -> BackupFailureDetail {
    let code = err.error_code();
    BackupFailureDetail {
        failure_class: FailureClass::from_error_code(code)
            .map_or_else(|| code.as_str().to_owned(), |class| format!("{class:?}")),
        error_code: code.as_str().to_owned(),
        message: err.to_string(),
    }
}

async fn dispatch_back_up_file_with_client<C>(
    client: &C,
    credentials: &WorkerCredentials,
    request: BackUpFileRequest,
) -> Result<BackUpFileResult, VoomError>
where
    C: ClientHandle + ?Sized,
{
    let mut progress = NoopWorkerProgressHandler;
    dispatch_operation_with_client(
        client,
        credentials,
        WorkerOperationDispatch {
            idempotency_key: "backup-control-plane",
            operation: OperationKind::BackUpFile,
            lease_id: LeaseId(0),
            payload: request,
            heartbeat_deadline_ms: HEARTBEAT_DEADLINE_MS,
            progress_idle_deadline_ms: DISPATCH_IDLE_DEADLINE_MS,
            labels: backup_stream_labels(),
        },
        &mut progress,
    )
    .await
}

fn bundled_backup_worker_command() -> WorkerCommand {
    bundled_backup_worker_command_from(
        std::env::var_os(BACKUP_WORKER_BIN_ENV),
        std::env::current_exe(),
    )
}

fn bundled_backup_worker_command_from(
    configured_bin: Option<OsString>,
    current_exe: std::io::Result<PathBuf>,
) -> WorkerCommand {
    bundled_worker_command_from(
        configured_bin,
        current_exe,
        "voom-backup-worker",
        |command, _worker_dir| command,
    )
}

const fn backup_stream_labels() -> WorkerStreamLabels {
    WorkerStreamLabels {
        payload_encode: "back_up_file payload encode",
        dispatch_failed: "back_up_file dispatch failed",
        progress_idle_timeout: "back_up_file worker progress idle timeout",
        stream_protocol: "back_up_file stream",
        terminal_frame_as_progress: "back_up_file worker sent terminal frame as non-terminal progress",
        progress_terminal: "progress frame cannot terminate back_up_file stream",
        stream_ended: "back_up_file worker stream ended before terminal frame",
        result_decode: "back_up_file result decode",
    }
}

#[cfg(test)]
#[path = "backup_test.rs"]
mod tests;
