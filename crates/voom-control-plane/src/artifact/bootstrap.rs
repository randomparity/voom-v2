use voom_core::{OperationKind, TicketOperation, VoomError};
use voom_store::repo::workers::{
    NewCapability, NewGrant, Worker, WorkerKind, WorkerRepo, WorkerStatus,
};

use crate::ControlPlane;

const BUILTIN_VERIFY_ARTIFACT_WORKER_NAME: &str = "builtin.verify_artifact";
pub async fn ensure_builtin_verify_artifact_worker_in_tx(
    control_plane: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<Worker, VoomError> {
    insert_builtin_worker_if_missing(control_plane, tx).await?;
    let worker = control_plane
        .workers
        .get_by_name_in_tx(tx, BUILTIN_VERIFY_ARTIFACT_WORKER_NAME)
        .await?
        .ok_or_else(|| {
            VoomError::Internal(format!(
                "built-in worker {BUILTIN_VERIFY_ARTIFACT_WORKER_NAME} missing after insert"
            ))
        })?;

    validate_builtin_worker(&worker)?;

    let operation = TicketOperation::from(OperationKind::VerifyArtifact);
    let eligibility = control_plane
        .workers
        .operation_eligibility_in_tx(tx, worker.id, &operation)
        .await?;

    if eligibility.is_denied {
        return Err(VoomError::Conflict(format!(
            "built-in worker {} is denied {}",
            worker.name,
            operation.as_str()
        )));
    }

    if !eligibility.has_capability {
        control_plane
            .workers
            .record_capability_in_tx(
                tx,
                NewCapability {
                    worker_id: worker.id,
                    operation: operation.clone(),
                    codecs: Vec::new(),
                    hardware: Vec::new(),
                    artifact_access: vec!["local_path".to_owned()],
                    extra: serde_json::json!({"dispatch": "bundled_direct"}),
                },
            )
            .await?;
    }

    if !eligibility.has_grant {
        control_plane
            .workers
            .record_grant_in_tx(
                tx,
                NewGrant {
                    worker_id: worker.id,
                    can_execute: vec![operation],
                    can_access_read: vec!["local_path".to_owned()],
                    can_access_write: Vec::new(),
                    denies: Vec::new(),
                    max_parallel: serde_json::json!({"bundled_direct": 1}),
                },
            )
            .await?;
    }

    Ok(worker)
}

async fn insert_builtin_worker_if_missing(
    control_plane: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), VoomError> {
    let ts = control_plane
        .clock()
        .now()
        .format(&time::format_description::well_known::Iso8601::DEFAULT)
        .map_err(|err| VoomError::Internal(format!("format built-in worker timestamp: {err}")))?;
    sqlx::query(
        "INSERT OR IGNORE INTO workers \
         (name, kind, status, registered_at, last_seen_at, node_id) \
         VALUES (?, 'local', 'registered', ?, ?, NULL)",
    )
    .bind(BUILTIN_VERIFY_ARTIFACT_WORKER_NAME)
    .bind(&ts)
    .bind(&ts)
    .execute(&mut **tx)
    .await
    .map_err(|err| {
        VoomError::Database(format!("workers insert built-in verify_artifact: {err}"))
    })?;
    Ok(())
}

fn validate_builtin_worker(worker: &Worker) -> Result<(), VoomError> {
    if worker.kind != WorkerKind::Local {
        return Err(VoomError::Conflict(format!(
            "built-in worker {} has kind {}",
            worker.name,
            worker.kind.as_str()
        )));
    }
    if worker.node_id.is_some() {
        return Err(VoomError::Conflict(format!(
            "built-in worker {} must not be linked to a node",
            worker.name
        )));
    }
    if !matches!(
        worker.status,
        WorkerStatus::Registered | WorkerStatus::Active
    ) {
        return Err(VoomError::Conflict(format!(
            "built-in worker {} has non-live status {}",
            worker.name,
            worker.status.as_str()
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "bootstrap_test.rs"]
mod tests;
