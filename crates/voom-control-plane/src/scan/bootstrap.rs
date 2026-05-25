use voom_core::VoomError;
use voom_store::repo::workers::{
    NewCapability, NewGrant, NewWorker, Worker, WorkerKind, WorkerRepo, WorkerStatus,
};

use crate::ControlPlane;

const BUILTIN_FFPROBE_WORKER_NAME: &str = "builtin.ffprobe";
const PROBE_FILE_OPERATION: &str = "probe_file";

pub async fn ensure_builtin_ffprobe_worker_in_tx(
    control_plane: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<Worker, VoomError> {
    let worker = match control_plane
        .workers
        .get_by_name_in_tx(tx, BUILTIN_FFPROBE_WORKER_NAME)
        .await?
    {
        Some(worker) => worker,
        None => {
            control_plane
                .workers
                .register_in_tx(
                    tx,
                    NewWorker {
                        name: BUILTIN_FFPROBE_WORKER_NAME.to_owned(),
                        kind: WorkerKind::Local,
                        registered_at: control_plane.clock().now(),
                        node_id: None,
                    },
                )
                .await?
        }
    };

    validate_builtin_worker(&worker)?;

    let eligibility = control_plane
        .workers
        .operation_eligibility_in_tx(tx, worker.id, PROBE_FILE_OPERATION)
        .await?;

    if eligibility.is_denied {
        return Err(VoomError::Conflict(format!(
            "built-in worker {} is denied {}",
            worker.name, PROBE_FILE_OPERATION
        )));
    }

    if !eligibility.has_capability {
        control_plane
            .workers
            .record_capability_in_tx(
                tx,
                NewCapability {
                    worker_id: worker.id,
                    operation: PROBE_FILE_OPERATION.to_owned(),
                    codecs: Vec::new(),
                    hardware: Vec::new(),
                    artifact_access: Vec::new(),
                    extra: serde_json::json!({}),
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
                    can_execute: vec![PROBE_FILE_OPERATION.to_owned()],
                    can_access_read: Vec::new(),
                    can_access_write: Vec::new(),
                    denies: Vec::new(),
                    max_parallel: serde_json::json!({}),
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
