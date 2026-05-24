//! Node-lifecycle use cases.

use secrecy::SecretString;
use serde_json::Value as JsonValue;
use sqlx::{Sqlite, Transaction};
use time::OffsetDateTime;
use voom_core::{NodeId, VoomError};
use voom_events::payload::{
    NodeHeartbeatRecordedPayload, NodeMarkedStalePayload, NodeRegisteredPayload, NodeRetiredPayload,
};
use voom_events::{Event, SubjectType};
use voom_store::repo::nodes::{NewNode, Node, NodeKind, NodeRepo, NodeStatus};

use crate::ControlPlane;
use crate::node_auth::verify_node_token;

use super::{append_event, begin_tx, commit_tx};

/// Inputs required to register a durable node.
#[derive(Debug)]
pub struct RegisterNodeInput {
    pub name: String,
    pub kind: NodeKind,
    pub heartbeat_ttl_seconds: u32,
    pub metadata: JsonValue,
}

/// Newly registered node plus the one-time plaintext token.
#[derive(Debug)]
pub struct RegisteredNode {
    pub node: Node,
    pub token: SecretString,
}

impl ControlPlane {
    /// Register a node and emit `node.registered` in the same transaction.
    ///
    /// # Errors
    /// Propagates token generation, repository, and event-append errors.
    pub async fn register_node(
        &self,
        input: RegisterNodeInput,
    ) -> Result<RegisteredNode, VoomError> {
        if input.heartbeat_ttl_seconds == 0 {
            return Err(VoomError::Config(
                "nodes register requires heartbeat_ttl_seconds > 0".to_owned(),
            ));
        }
        let generated = self.generate_node_token()?;
        let now = self.clock().now();
        let mut tx = begin_tx(&self.pool).await?;
        let node = self
            .nodes
            .register_in_tx(
                &mut tx,
                NewNode {
                    name: input.name,
                    kind: input.kind,
                    registered_at: now,
                    heartbeat_ttl_seconds: input.heartbeat_ttl_seconds,
                    auth_token_hash: generated.hash,
                    auth_token_hint: generated.hint,
                    metadata: input.metadata,
                },
            )
            .await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::Node,
            Some(node.id.0),
            now,
            Event::NodeRegistered(NodeRegisteredPayload {
                node_id: node.id.0,
                name: node.name.clone(),
                kind: node.kind.as_str().to_owned(),
                status: node.status.as_str().to_owned(),
                heartbeat_ttl_seconds: node.heartbeat_ttl_seconds,
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(RegisteredNode {
            node,
            token: generated.plaintext,
        })
    }

    /// Record a node heartbeat after verifying its bearer token.
    ///
    /// # Errors
    /// Returns `CONFLICT` for token mismatch or retired nodes; otherwise
    /// propagates repository and event-append errors.
    pub async fn heartbeat_node(&self, node_id: NodeId, token: &str) -> Result<Node, VoomError> {
        let now = self.clock().now();
        let mut tx = begin_tx(&self.pool).await?;
        let auth = self
            .nodes
            .auth_record_in_tx(&mut tx, node_id)
            .await?
            .ok_or_else(|| {
                VoomError::NotFound(format!("nodes heartbeat: id={node_id} not found"))
            })?;
        if !verify_node_token(token, &auth.auth_token_hash) {
            return Err(VoomError::Conflict(format!(
                "nodes heartbeat rejected: id={node_id} token mismatch"
            )));
        }
        if auth.status == NodeStatus::Retired {
            return Err(VoomError::Conflict(format!(
                "nodes heartbeat rejected: id={node_id} is retired"
            )));
        }
        let node = self.heartbeat_node_in_tx(&mut tx, auth.id, now).await?;
        commit_tx(tx).await?;
        Ok(node)
    }

    /// Record a node heartbeat and emit `node.heartbeat_recorded`.
    ///
    /// The caller owns the transaction boundary and any authentication.
    ///
    /// # Errors
    /// Propagates repository and event-append errors.
    pub(crate) async fn heartbeat_node_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        node_id: NodeId,
        now: OffsetDateTime,
    ) -> Result<Node, VoomError> {
        let node = self.nodes.heartbeat_in_tx(tx, node_id, now).await?;
        append_event(
            &self.events,
            tx,
            SubjectType::Node,
            Some(node.id.0),
            now,
            Event::NodeHeartbeatRecorded(NodeHeartbeatRecordedPayload {
                node_id: node.id.0,
                status: node.status.as_str().to_owned(),
                last_seen_at: node.last_seen_at,
                epoch: node.epoch,
            }),
        )
        .await?;
        Ok(node)
    }

    /// Mark expired non-retired nodes stale and emit one event per changed row.
    ///
    /// # Errors
    /// Propagates repository and event-append errors.
    pub async fn mark_stale_nodes(&self, now: OffsetDateTime) -> Result<Vec<Node>, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let nodes = self.nodes.mark_stale_in_tx(&mut tx, now).await?;
        for node in &nodes {
            append_event(
                &self.events,
                &mut tx,
                SubjectType::Node,
                Some(node.id.0),
                now,
                Event::NodeMarkedStale(NodeMarkedStalePayload {
                    node_id: node.id.0,
                    marked_stale_at: now,
                    epoch: node.epoch,
                }),
            )
            .await?;
        }
        commit_tx(tx).await?;
        Ok(nodes)
    }

    /// Retire a node using optimistic epoch checking and emit `node.retired`.
    ///
    /// # Errors
    /// Propagates repository and event-append errors.
    pub async fn retire_node(
        &self,
        node_id: NodeId,
        expected_epoch: u64,
        now: OffsetDateTime,
    ) -> Result<Node, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let node = self
            .nodes
            .retire_in_tx(&mut tx, node_id, expected_epoch, now)
            .await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::Node,
            Some(node.id.0),
            now,
            Event::NodeRetired(NodeRetiredPayload {
                node_id: node.id.0,
                retired_at: now,
                epoch: node.epoch,
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(node)
    }

    /// Fetch a node by id.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn get_node(&self, node_id: NodeId) -> Result<Option<Node>, VoomError> {
        self.nodes.get(node_id).await
    }

    /// List nodes with optional status filtering.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn list_nodes(
        &self,
        status: Option<NodeStatus>,
        limit: u32,
    ) -> Result<Vec<Node>, VoomError> {
        self.nodes.list(status, limit).await
    }
}

#[cfg(test)]
#[path = "nodes_test.rs"]
mod tests;
