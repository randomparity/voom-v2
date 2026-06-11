//! Remote recovery primitives and node-validation helpers.

use secrecy::{ExposeSecret, SecretString};
use sqlx::{Sqlite, Transaction};
use time::Duration;
use voom_core::{NodeId, VoomError};
use voom_store::repo::nodes::{NodeAuthRecord, NodeKind, NodeStatus};
use voom_store::repo::workers::{Worker, WorkerKind};

use crate::ControlPlane;
use crate::cases::execution::remote_execution::RemoteRecoverReport;
use crate::node_auth::verify_node_token;

impl ControlPlane {
    /// Run remote recovery primitives for stale nodes and expired leases.
    ///
    /// # Errors
    /// Propagates stale-node marking or lease-expiry errors.
    pub async fn remote_recover(
        &self,
        now: time::OffsetDateTime,
    ) -> Result<RemoteRecoverReport, VoomError> {
        let stale_nodes = self.mark_stale_nodes(now).await?;
        let expired = self.expire_due(now).await?;
        Ok(RemoteRecoverReport {
            stale_nodes: stale_nodes.iter().map(|node| node.id).collect(),
            expired_leases: expired.expired_leases,
            requeued_tickets: expired.requeued_tickets,
            failed_tickets: expired
                .failed_expiries
                .iter()
                .map(|failed| failed.ticket_id)
                .collect(),
        })
    }

    pub(crate) async fn verify_remote_node_token_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        node_id: NodeId,
        token: &SecretString,
    ) -> Result<NodeAuthRecord, VoomError> {
        let auth = self
            .nodes
            .auth_record_in_tx(tx, node_id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("remote node {node_id} not found")))?;
        if auth.kind != NodeKind::Remote {
            return Err(VoomError::Conflict(format!(
                "remote node {node_id} is not a remote node"
            )));
        }
        if !verify_node_token(token.expose_secret(), &auth.auth_token_hash) {
            return Err(VoomError::Conflict(format!(
                "remote node {node_id} token mismatch"
            )));
        }
        Ok(auth)
    }
}

pub(super) fn validate_remote_node_live(
    auth: &NodeAuthRecord,
    node_id: NodeId,
    now: time::OffsetDateTime,
    require_fresh_for_acquire: bool,
) -> Result<(), VoomError> {
    if auth.status == NodeStatus::Retired {
        return Err(VoomError::Conflict(format!(
            "remote node {node_id} is retired"
        )));
    }
    if require_fresh_for_acquire {
        if auth.status == NodeStatus::Stale {
            return Err(VoomError::Conflict(format!(
                "remote node {node_id} is stale"
            )));
        }
        let expires_at =
            auth.last_seen_at + Duration::seconds(i64::from(auth.heartbeat_ttl_seconds));
        if expires_at <= now {
            return Err(VoomError::Conflict(format!(
                "remote node {node_id} heartbeat expired"
            )));
        }
    }
    Ok(())
}

pub(super) fn require_remote_worker(worker: &Worker) -> Result<(), VoomError> {
    if worker.kind != WorkerKind::Remote {
        return Err(VoomError::Conflict(format!(
            "remote execution rejected: worker {} is not a remote worker",
            worker.id
        )));
    }
    Ok(())
}

pub(super) fn require_positive_ttl(ttl_seconds: i64) -> Result<(), VoomError> {
    if ttl_seconds <= 0 {
        return Err(VoomError::Config(format!(
            "lease ttl must be positive, got {ttl_seconds}s"
        )));
    }
    Ok(())
}
