//! Remote recovery primitives and node-validation helpers.

use secrecy::{ExposeSecret, SecretString};
use sqlx::{Sqlite, Transaction};
use time::Duration;
use voom_core::{NodeId, NodeIncarnationId, VoomError, WorkerId};
use voom_store::repo::execution::nodes::{NodeAuthRecord, NodeKind, NodeStatus};
use voom_store::repo::execution::workers::{Worker, WorkerKind};

use crate::ControlPlane;
use crate::cases::commit_tx;
use crate::cases::execution::remote_execution::RemoteRecoverReport;
use crate::node_auth::verify_node_token;

use super::append_scan_stale_events_in_tx;
use voom_store::tx::begin_read_then_write;

const REMOTE_NODE_AUTH_FAILURE: &str = "remote node authentication failed";

impl ControlPlane {
    /// Run remote recovery primitives for stale nodes and expired leases.
    ///
    /// # Errors
    /// Propagates stale-node marking, scan-session transition, event-append,
    /// scan-transaction commit, or lease-expiry errors.
    pub async fn remote_recover(
        &self,
        now: time::OffsetDateTime,
    ) -> Result<RemoteRecoverReport, VoomError> {
        let stale_nodes = self.mark_stale_nodes(now).await?;
        let stale_scan_sessions = self.recover_expired_scan_sessions(now).await?;
        let expired = self.expire_due(now).await?;
        Ok(RemoteRecoverReport {
            stale_nodes: stale_nodes.iter().map(|node| node.id).collect(),
            stale_scan_sessions,
            expired_leases: expired.expired_leases,
            requeued_tickets: expired.requeued_tickets,
            failed_tickets: expired
                .failed_expiries
                .iter()
                .map(|failed| failed.ticket_id)
                .collect(),
        })
    }

    async fn recover_expired_scan_sessions(
        &self,
        now: time::OffsetDateTime,
    ) -> Result<Vec<voom_core::ScanSessionId>, VoomError> {
        let mut tx =
            begin_read_then_write(&self.pool, "recover: recover_expired_scan_sessions").await?;
        let stale = self.scan_sessions.stale_expired_in_tx(&mut tx, now).await?;
        append_scan_stale_events_in_tx(self, &mut tx, &stale, now).await?;
        let ids = stale.iter().map(|session| session.id).collect();
        commit_tx(tx).await?;
        Ok(ids)
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
            .ok_or_else(|| VoomError::Unauthorized(REMOTE_NODE_AUTH_FAILURE.to_owned()))?;
        if auth.kind != NodeKind::Remote {
            return Err(VoomError::Unauthorized(REMOTE_NODE_AUTH_FAILURE.to_owned()));
        }
        if !verify_node_token(token.expose_secret(), &auth.auth_token_hash) {
            return Err(VoomError::Unauthorized(REMOTE_NODE_AUTH_FAILURE.to_owned()));
        }
        Ok(auth)
    }

    pub(crate) async fn require_remote_incarnation_fence_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        node_id: NodeId,
        token: &SecretString,
        incarnation_id: NodeIncarnationId,
        worker_id: Option<WorkerId>,
    ) -> Result<NodeAuthRecord, VoomError> {
        let auth = self
            .verify_remote_node_token_in_tx(tx, node_id, token)
            .await?;
        self.nodes
            .require_active_incarnation_in_tx(tx, node_id, incarnation_id)
            .await?;
        if let Some(worker_id) = worker_id {
            self.workers
                .incarnation_owned_worker_in_tx(tx, worker_id, node_id, incarnation_id)
                .await?;
        }
        Ok(auth)
    }
}

pub(crate) fn validate_remote_node_live(
    auth: &NodeAuthRecord,
    node_id: NodeId,
    now: time::OffsetDateTime,
    require_fresh_for_acquire: bool,
) -> Result<(), VoomError> {
    validate_remote_node_freshness(
        auth.status,
        auth.last_seen_at,
        auth.heartbeat_ttl_seconds,
        node_id,
        now,
        require_fresh_for_acquire,
    )
}

pub(crate) fn validate_remote_node_freshness(
    status: NodeStatus,
    last_seen_at: time::OffsetDateTime,
    heartbeat_ttl_seconds: u32,
    node_id: NodeId,
    now: time::OffsetDateTime,
    require_fresh_for_acquire: bool,
) -> Result<(), VoomError> {
    if status == NodeStatus::Retired {
        return Err(VoomError::Conflict(format!(
            "remote node {node_id} is retired"
        )));
    }
    if require_fresh_for_acquire {
        if status == NodeStatus::Stale {
            return Err(VoomError::Conflict(format!(
                "remote node {node_id} is stale"
            )));
        }
        let expires_at = last_seen_at + Duration::seconds(i64::from(heartbeat_ttl_seconds));
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
