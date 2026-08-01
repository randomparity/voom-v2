use voom_core::{OperationKind, TicketOperation, VoomError};
use voom_store::repo::execution::workers::{
    NewCapability, NewGrant, NewWorker, Worker, WorkerKind, WorkerStatus,
};

use crate::ControlPlane;

const BUILTIN_VERIFY_ARTIFACT_WORKER_NAME: &str = "builtin.verify_artifact";
pub async fn ensure_builtin_verify_artifact_worker_in_tx(
    control_plane: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<Worker, VoomError> {
    let worker = control_plane
        .workers
        .register_builtin_if_missing_in_tx(
            tx,
            NewWorker {
                name: BUILTIN_VERIFY_ARTIFACT_WORKER_NAME.to_owned(),
                kind: WorkerKind::Local,
                registered_at: control_plane.clock().now(),
                node_id: None,
            },
        )
        .await?;

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
