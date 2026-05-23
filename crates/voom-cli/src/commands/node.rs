use std::io;

use secrecy::ExposeSecret;
use serde::Serialize;
use serde_json::json;
use voom_control_plane::ControlPlane;
use voom_control_plane::cases::nodes::RegisterNodeInput;
use voom_core::{ErrorCode, NodeId};
use voom_store::repo::nodes::{Node, NodeStatus};

use crate::cli::NodeCommand;
use crate::commands::token_source::{TokenSourceArgs, read_token};
use crate::envelope::{Local, emit_err, emit_ok};

const DEFAULT_HEARTBEAT_TTL_SECONDS: u32 = 60;
const LIST_LIMIT: u32 = 1_000;

#[derive(Debug, Serialize)]
struct RegisterData {
    node: NodeData,
    token: String,
    token_hint: String,
}

#[derive(Debug, Serialize)]
struct NodeEnvelopeData {
    node: NodeData,
}

#[derive(Debug, Serialize)]
struct ListData {
    nodes: Vec<NodeData>,
}

#[derive(Debug, Serialize)]
struct NodeData {
    id: u64,
    name: String,
    kind: &'static str,
    status: &'static str,
    heartbeat_ttl_seconds: u32,
    epoch: u64,
}

pub async fn run(database_url: &str, local: Local, command: NodeCommand) -> io::Result<i32> {
    match command {
        NodeCommand::Register {
            name,
            kind,
            heartbeat_ttl_seconds,
        } => register(database_url, local, name, kind, heartbeat_ttl_seconds).await,
        NodeCommand::Heartbeat {
            node_id,
            token_file,
            token_env,
            token_stdin,
        } => {
            heartbeat(
                database_url,
                local,
                node_id,
                TokenSourceArgs {
                    token_file,
                    token_env,
                    token_stdin,
                },
            )
            .await
        }
        NodeCommand::List { status } => list(database_url, local, status).await,
        NodeCommand::Show { node_id } => show(database_url, local, node_id).await,
        NodeCommand::Retire {
            node_id,
            expected_epoch,
        } => retire(database_url, local, node_id, expected_epoch).await,
    }
}

async fn register(
    database_url: &str,
    local: Local,
    name: String,
    kind: crate::cli::NodeKindArg,
    heartbeat_ttl_seconds: Option<u32>,
) -> io::Result<i32> {
    let cp = match open(database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    let input = RegisterNodeInput {
        name,
        kind: kind.to_store(),
        heartbeat_ttl_seconds: heartbeat_ttl_seconds.unwrap_or(DEFAULT_HEARTBEAT_TTL_SECONDS),
        metadata: json!({}),
    };
    match cp.register_node(input).await {
        Ok(registered) => {
            let token = registered.token.expose_secret().to_owned();
            let token_hint = registered.node.auth_token_hint.clone();
            emit_ok(
                "node",
                RegisterData {
                    node: NodeData::from(registered.node),
                    token,
                    token_hint,
                },
                Some(local),
                Vec::new(),
            )
            .map(|()| 0)
        }
        Err(err) => emit_voom_error(&err, local),
    }
}

async fn heartbeat(
    database_url: &str,
    local: Local,
    node_id: u64,
    token_source: TokenSourceArgs,
) -> io::Result<i32> {
    let token = match read_token(&token_source).await {
        Ok(token) => token,
        Err(err) => {
            emit_err(
                "node",
                err.code().as_str(),
                err.to_string(),
                None,
                Some(local),
            )?;
            return Ok(1);
        }
    };
    let cp = match open(database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp.heartbeat_node(NodeId(node_id), &token).await {
        Ok(node) => emit_node(node, local),
        Err(err) => emit_voom_error(&err, local),
    }
}

async fn list(
    database_url: &str,
    local: Local,
    status: Option<crate::cli::NodeStatusArg>,
) -> io::Result<i32> {
    let cp = match open(database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp
        .list_nodes(status.map(crate::cli::NodeStatusArg::to_store), LIST_LIMIT)
        .await
    {
        Ok(nodes) => emit_ok(
            "node",
            ListData {
                nodes: nodes.into_iter().map(NodeData::from).collect(),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_voom_error(&err, local),
    }
}

async fn show(database_url: &str, local: Local, node_id: u64) -> io::Result<i32> {
    let cp = match open(database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp.get_node(NodeId(node_id)).await {
        Ok(Some(node)) => emit_node(node, local),
        Ok(None) => {
            emit_err(
                "node",
                ErrorCode::NotFound.as_str(),
                format!("nodes show: id={node_id} not found"),
                None,
                Some(local),
            )?;
            Ok(2)
        }
        Err(err) => emit_voom_error(&err, local),
    }
}

async fn retire(
    database_url: &str,
    local: Local,
    node_id: u64,
    expected_epoch: u64,
) -> io::Result<i32> {
    let cp = match open(database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp
        .retire_node(NodeId(node_id), expected_epoch, cp.clock().now())
        .await
    {
        Ok(node) => emit_node(node, local),
        Err(err) => emit_voom_error(&err, local),
    }
}

async fn open(database_url: &str, local: &Local) -> io::Result<Result<ControlPlane, i32>> {
    match ControlPlane::open(database_url).await {
        Ok(cp) => Ok(Ok(cp)),
        Err(err) => {
            emit_err(
                "node",
                err.code(),
                err.to_string(),
                None,
                Some(local.clone()),
            )?;
            Ok(Err(2))
        }
    }
}

fn emit_node(node: Node, local: Local) -> io::Result<i32> {
    emit_ok(
        "node",
        NodeEnvelopeData {
            node: NodeData::from(node),
        },
        Some(local),
        Vec::new(),
    )
    .map(|()| 0)
}

fn emit_voom_error(err: &voom_core::VoomError, local: Local) -> io::Result<i32> {
    emit_err("node", err.code(), err.to_string(), None, Some(local))?;
    Ok(2)
}

impl From<Node> for NodeData {
    fn from(node: Node) -> Self {
        Self {
            id: node.id.0,
            name: node.name,
            kind: node.kind.as_str(),
            status: node_status_str(node.status),
            heartbeat_ttl_seconds: node.heartbeat_ttl_seconds,
            epoch: node.epoch,
        }
    }
}

const fn node_status_str(status: NodeStatus) -> &'static str {
    match status {
        NodeStatus::Registered => "registered",
        NodeStatus::Active => "active",
        NodeStatus::Stale => "stale",
        NodeStatus::Retired => "retired",
    }
}

#[cfg(test)]
#[path = "node_test.rs"]
mod tests;
