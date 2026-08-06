use std::io;

use secrecy::ExposeSecret;
use serde::Serialize;
use serde_json::json;
use voom_control_plane::workers::RegisterNodeInput;
use voom_core::{ErrorCode, NodeId, NodeIncarnationId};
use voom_store::repo::execution::node_incarnations::NodeIncarnation;
use voom_store::repo::execution::nodes::Node;

use crate::cli::{NodeCommand, NodeIncarnationCommand};
use crate::commands::common::{emit_voom_error, open_control_plane};
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
struct NodeShowEnvelopeData {
    node: NodeShowData,
}

#[derive(Debug, Serialize)]
struct ListData {
    nodes: Vec<NodeData>,
}

#[derive(Debug, Serialize)]
struct IncarnationListData {
    incarnations: Vec<NodeIncarnationData>,
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

#[derive(Debug, Serialize)]
struct NodeShowData {
    #[serde(flatten)]
    node: NodeData,
    active_incarnation_id: Option<NodeIncarnationId>,
}

#[derive(Debug, Serialize)]
struct NodeIncarnationData {
    incarnation_id: NodeIncarnationId,
    status: &'static str,
    #[serde(with = "time::serde::rfc3339")]
    started_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    last_seen_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    ended_at: Option<time::OffsetDateTime>,
    end_reason: Option<&'static str>,
    worker_count: u32,
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
        NodeCommand::Incarnation(NodeIncarnationCommand::List { node_id, limit }) => {
            list_incarnations(database_url, local, node_id, limit).await
        }
        NodeCommand::Show { node_id } => show(database_url, local, node_id).await,
        NodeCommand::Retire {
            node_id,
            expected_epoch,
        } => retire(database_url, local, node_id, expected_epoch).await,
    }
}

async fn list_incarnations(
    database_url: &str,
    local: Local,
    node_id: u64,
    limit: u32,
) -> io::Result<i32> {
    let cp = match open_control_plane("node", database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp.list_node_incarnations(NodeId(node_id), limit).await {
        Ok(incarnations) => emit_ok(
            "node",
            IncarnationListData {
                incarnations: incarnations
                    .into_iter()
                    .map(NodeIncarnationData::from)
                    .collect(),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_voom_error("node", &err, local),
    }
}

async fn register(
    database_url: &str,
    local: Local,
    name: String,
    kind: crate::cli::NodeKindArg,
    heartbeat_ttl_seconds: Option<u32>,
) -> io::Result<i32> {
    let cp = match open_control_plane("node", database_url, &local).await? {
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
        Err(err) => emit_voom_error("node", &err, local),
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
    let cp = match open_control_plane("node", database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp.heartbeat_node(NodeId(node_id), &token).await {
        Ok(node) => emit_node(node, local),
        Err(err) => emit_voom_error("node", &err, local),
    }
}

async fn list(
    database_url: &str,
    local: Local,
    status: Option<crate::cli::NodeStatusArg>,
) -> io::Result<i32> {
    let cp = match open_control_plane("node", database_url, &local).await? {
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
        Err(err) => emit_voom_error("node", &err, local),
    }
}

async fn show(database_url: &str, local: Local, node_id: u64) -> io::Result<i32> {
    let cp = match open_control_plane("node", database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp.get_node(NodeId(node_id)).await {
        Ok(Some(node)) => emit_node_show(node, local),
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
        Err(err) => emit_voom_error("node", &err, local),
    }
}

fn emit_node_show(node: Node, local: Local) -> io::Result<i32> {
    emit_ok(
        "node",
        NodeShowEnvelopeData {
            node: NodeShowData {
                active_incarnation_id: node.active_incarnation_id,
                node: NodeData::from(node),
            },
        },
        Some(local),
        Vec::new(),
    )
    .map(|()| 0)
}

async fn retire(
    database_url: &str,
    local: Local,
    node_id: u64,
    expected_epoch: u64,
) -> io::Result<i32> {
    let cp = match open_control_plane("node", database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp
        .retire_node(NodeId(node_id), expected_epoch, cp.clock().now())
        .await
    {
        Ok(node) => emit_node(node, local),
        Err(err) => emit_voom_error("node", &err, local),
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

impl From<Node> for NodeData {
    fn from(node: Node) -> Self {
        Self {
            id: node.id.0,
            name: node.name,
            kind: node.kind.as_str(),
            status: node.status.as_str(),
            heartbeat_ttl_seconds: node.heartbeat_ttl_seconds,
            epoch: node.epoch,
        }
    }
}

impl From<NodeIncarnation> for NodeIncarnationData {
    fn from(incarnation: NodeIncarnation) -> Self {
        Self {
            incarnation_id: incarnation.id,
            status: incarnation.status.as_str(),
            started_at: incarnation.started_at,
            last_seen_at: incarnation.last_seen_at,
            ended_at: incarnation.ended_at,
            end_reason: incarnation
                .end_reason
                .map(voom_core::NodeIncarnationEndReason::as_str),
            worker_count: incarnation.worker_count,
        }
    }
}

#[cfg(test)]
#[path = "node_test.rs"]
mod tests;
