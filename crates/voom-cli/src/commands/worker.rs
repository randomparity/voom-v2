use std::io;

use secrecy::SecretString;
use serde::Serialize;
use serde_json::json;
use voom_control_plane::ControlPlane;
use voom_control_plane::cases::workers::{NewWorkerCapabilityDraft, RegisterWorkerForNodeInput};
use voom_core::{ErrorCode, NodeId, WorkerId};
use voom_store::repo::workers::{WorkerInspection, WorkerNodeContext};

use crate::cli::WorkerCommand;
use crate::commands::common::{emit_voom_error, open_control_plane};
use crate::commands::token_source::{TokenSourceArgs, read_token};
use crate::envelope::{Local, emit_err, emit_ok};

const LIST_LIMIT: u32 = 1_000;

#[derive(Debug, Serialize)]
struct WorkerEnvelopeData {
    worker: WorkerData,
}

#[derive(Debug, Serialize)]
struct ListData {
    workers: Vec<WorkerData>,
}

#[derive(Debug, Serialize)]
struct WorkerData {
    id: u64,
    node_id: Option<u64>,
    name: String,
    kind: &'static str,
    status: &'static str,
    registered_at: String,
    last_seen_at: String,
    retired_at: Option<String>,
    epoch: u64,
    node: Option<NodeContextData>,
}

#[derive(Debug, Serialize)]
struct NodeContextData {
    id: u64,
    name: String,
    kind: &'static str,
    status: &'static str,
    last_seen_at: String,
}

pub async fn run(database_url: &str, local: Local, command: WorkerCommand) -> io::Result<i32> {
    match command {
        WorkerCommand::Register {
            node_id,
            name,
            kind,
            capability,
            token_file,
            token_env,
            token_stdin,
        } => {
            register(
                database_url,
                local,
                node_id,
                name,
                kind,
                capability,
                TokenSourceArgs {
                    token_file,
                    token_env,
                    token_stdin,
                },
            )
            .await
        }
        WorkerCommand::List { status } => list(database_url, local, status).await,
        WorkerCommand::Show { worker_id } => show(database_url, local, worker_id).await,
    }
}

async fn register(
    database_url: &str,
    local: Local,
    node_id: u64,
    name: String,
    kind: crate::cli::WorkerKindArg,
    capabilities: Vec<String>,
    token_source: TokenSourceArgs,
) -> io::Result<i32> {
    if capabilities.is_empty() {
        emit_err(
            "worker",
            ErrorCode::BadArgs.as_str(),
            "workers register requires at least one capability".to_owned(),
            None,
            Some(local),
        )?;
        return Ok(1);
    }
    let token = match read_token(&token_source).await {
        Ok(token) => token,
        Err(err) => {
            emit_err(
                "worker",
                err.code().as_str(),
                err.to_string(),
                Some("Pass exactly one token source".to_owned()),
                Some(local),
            )?;
            return Ok(1);
        }
    };
    let cp = match open_control_plane("worker", database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    let input = RegisterWorkerForNodeInput {
        node_id: NodeId(node_id),
        token: SecretString::from(token),
        name,
        kind: kind.to_store(),
        capabilities: capabilities.into_iter().map(capability_draft).collect(),
        grants: Vec::new(),
    };
    match cp.register_worker_for_node(input).await {
        Ok(worker) => emit_inspection(&cp, local, worker.id).await,
        Err(err) => emit_voom_error("worker", &err, local),
    }
}

async fn list(
    database_url: &str,
    local: Local,
    status: Option<crate::cli::WorkerStatusArg>,
) -> io::Result<i32> {
    let cp = match open_control_plane("worker", database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp
        .list_worker_inspections(
            status.map(crate::cli::WorkerStatusArg::to_store),
            LIST_LIMIT,
        )
        .await
    {
        Ok(workers) => emit_ok(
            "worker",
            ListData {
                workers: workers.into_iter().map(WorkerData::from).collect(),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_voom_error("worker", &err, local),
    }
}

async fn show(database_url: &str, local: Local, worker_id: u64) -> io::Result<i32> {
    let cp = match open_control_plane("worker", database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp.get_worker_inspection(WorkerId(worker_id)).await {
        Ok(Some(worker)) => emit_worker(worker, local),
        Ok(None) => {
            emit_err(
                "worker",
                ErrorCode::NotFound.as_str(),
                format!("workers show: id={worker_id} not found"),
                None,
                Some(local),
            )?;
            Ok(2)
        }
        Err(err) => emit_voom_error("worker", &err, local),
    }
}

async fn emit_inspection(cp: &ControlPlane, local: Local, worker_id: WorkerId) -> io::Result<i32> {
    match cp.get_worker_inspection(worker_id).await {
        Ok(Some(worker)) => emit_worker(worker, local),
        Ok(None) => {
            emit_err(
                "worker",
                ErrorCode::Internal.as_str(),
                format!("workers register: id={worker_id} missing after registration"),
                None,
                Some(local),
            )?;
            Ok(2)
        }
        Err(err) => emit_voom_error("worker", &err, local),
    }
}

fn emit_worker(worker: WorkerInspection, local: Local) -> io::Result<i32> {
    emit_ok(
        "worker",
        WorkerEnvelopeData {
            worker: WorkerData::from(worker),
        },
        Some(local),
        Vec::new(),
    )
    .map(|()| 0)
}

fn capability_draft(operation: String) -> NewWorkerCapabilityDraft {
    NewWorkerCapabilityDraft {
        operation,
        codecs: Vec::new(),
        hardware: Vec::new(),
        artifact_access: Vec::new(),
        extra: json!({}),
    }
}

impl From<WorkerInspection> for WorkerData {
    fn from(inspection: WorkerInspection) -> Self {
        Self {
            id: inspection.worker.id.0,
            node_id: inspection.worker.node_id.map(|id| id.0),
            name: inspection.worker.name,
            kind: inspection.worker.kind.as_str(),
            status: inspection.worker.status.as_str(),
            registered_at: inspection.worker.registered_at.to_string(),
            last_seen_at: inspection.worker.last_seen_at.to_string(),
            retired_at: inspection.worker.retired_at.map(|at| at.to_string()),
            epoch: inspection.worker.epoch,
            node: inspection.node.map(NodeContextData::from),
        }
    }
}

impl From<WorkerNodeContext> for NodeContextData {
    fn from(node: WorkerNodeContext) -> Self {
        Self {
            id: node.id.0,
            name: node.name,
            kind: node.kind.as_str(),
            status: node.status.as_str(),
            last_seen_at: node.last_seen_at.to_string(),
        }
    }
}

#[cfg(test)]
#[path = "worker_test.rs"]
mod tests;
