//! Remote node and lease heartbeat refresh.

use sqlx::{Sqlite, Transaction};
use time::Duration;
use voom_core::VoomError;
use voom_store::repo::remote_idempotency::{IdempotencyOutcome, RemoteIdempotencyInput};

use crate::ControlPlane;
use crate::cases::execution::remote_execution::{
    RemoteLeaseHeartbeatInput, RemoteLeaseHeartbeatOutcome, RemoteNodeHeartbeatInput,
    RemoteNodeHeartbeatOutcome, ReplayRoute, decode_replay, is_remote_replayable_error,
};
use crate::cases::{begin_immediate_tx, commit_tx};

impl ControlPlane {
    /// Record a remote node heartbeat.
    ///
    /// # Errors
    /// Returns authentication, idempotency, retired-node, or heartbeat errors.
    pub async fn remote_node_heartbeat(
        &self,
        input: RemoteNodeHeartbeatInput,
    ) -> Result<RemoteNodeHeartbeatOutcome, VoomError> {
        let now = self.clock().now();
        let route_key = super::route_node_heartbeat(input.node_id);
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let auth = self
            .verify_remote_node_token_in_tx(&mut tx, input.node_id, &input.token)
            .await?;

        match self
            .remote_idempotency
            .reserve_or_replay_in_tx(
                &mut tx,
                RemoteIdempotencyInput {
                    node_id: input.node_id,
                    route_key: route_key.clone(),
                    worker_id: None,
                    idempotency_key: input.idempotency_key.clone(),
                    request_hash: input.request_hash.clone(),
                    created_at: now,
                },
            )
            .await?
        {
            IdempotencyOutcome::Reserved => {}
            IdempotencyOutcome::Replay(replay) => {
                return self
                    .finish_replay_in_tx(tx, input.replay_slot(), replay, |data| {
                        decode_replay::<RemoteNodeHeartbeatOutcome>(data, "remote node heartbeat")
                    })
                    .await;
            }
        }

        if let Err(err) =
            super::recover::validate_remote_node_live(&auth, input.node_id, now, false)
        {
            self.complete_remote_error_in_tx(
                &mut tx,
                input.node_id,
                &route_key,
                None,
                &input.idempotency_key,
                &err,
            )
            .await?;
            commit_tx(tx).await?;
            return Err(err);
        }

        let node = self
            .heartbeat_node_in_tx(&mut tx, input.node_id, now)
            .await?;
        let outcome = RemoteNodeHeartbeatOutcome {
            node_id: node.id,
            status: node.status.as_str().to_owned(),
        };
        self.complete_remote_ok_in_tx(
            &mut tx,
            input.node_id,
            &route_key,
            None,
            &input.idempotency_key,
            &outcome,
        )
        .await?;
        commit_tx(tx).await?;
        Ok(outcome)
    }

    /// Heartbeat a held remote lease without emitting audit events.
    ///
    /// # Errors
    /// Returns authentication, idempotency, ownership, or lease heartbeat errors.
    pub async fn remote_lease_heartbeat(
        &self,
        input: RemoteLeaseHeartbeatInput,
    ) -> Result<RemoteLeaseHeartbeatOutcome, VoomError> {
        let now = self.clock().now();
        let route_key = super::route_lease_heartbeat(input.lease_id);
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let auth = self
            .verify_remote_node_token_in_tx(&mut tx, input.node_id, &input.token)
            .await?;

        match self
            .remote_idempotency
            .reserve_or_replay_in_tx(
                &mut tx,
                RemoteIdempotencyInput {
                    node_id: input.node_id,
                    route_key: route_key.clone(),
                    worker_id: Some(input.worker_id),
                    idempotency_key: input.idempotency_key.clone(),
                    request_hash: input.request_hash.clone(),
                    created_at: now,
                },
            )
            .await?
        {
            IdempotencyOutcome::Reserved => {}
            IdempotencyOutcome::Replay(replay) => {
                return self
                    .finish_replay_in_tx(tx, input.replay_slot(), replay, |data| {
                        decode_replay::<RemoteLeaseHeartbeatOutcome>(data, "remote lease heartbeat")
                    })
                    .await;
            }
        }

        if let Err(err) =
            super::recover::validate_remote_node_live(&auth, input.node_id, now, false)
        {
            self.complete_remote_error_in_tx(
                &mut tx,
                input.node_id,
                &route_key,
                Some(input.worker_id),
                &input.idempotency_key,
                &err,
            )
            .await?;
            commit_tx(tx).await?;
            return Err(err);
        }

        let outcome = match self
            .remote_lease_heartbeat_preflight_in_tx(&mut tx, &input)
            .await
        {
            Ok(()) => {
                self.remote_lease_heartbeat_mutation_in_tx(&mut tx, &input, now)
                    .await?
            }
            Err(err) => {
                if !is_remote_replayable_error(&err) {
                    return Err(err);
                }
                self.complete_remote_error_in_tx(
                    &mut tx,
                    input.node_id,
                    &route_key,
                    Some(input.worker_id),
                    &input.idempotency_key,
                    &err,
                )
                .await?;
                commit_tx(tx).await?;
                return Err(err);
            }
        };
        self.complete_remote_ok_in_tx(
            &mut tx,
            input.node_id,
            &route_key,
            Some(input.worker_id),
            &input.idempotency_key,
            &outcome,
        )
        .await?;
        commit_tx(tx).await?;
        Ok(outcome)
    }

    async fn remote_lease_heartbeat_preflight_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteLeaseHeartbeatInput,
    ) -> Result<(), VoomError> {
        super::recover::require_positive_ttl(input.lease_ttl_seconds)?;
        let worker = self
            .workers
            .node_owned_worker_in_tx(tx, input.worker_id, input.node_id)
            .await?;
        super::recover::require_remote_worker(&worker)?;
        self.leases
            .get_held_for_worker_in_tx(tx, input.lease_id, input.worker_id)
            .await?;
        Ok(())
    }

    async fn remote_lease_heartbeat_mutation_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteLeaseHeartbeatInput,
        now: time::OffsetDateTime,
    ) -> Result<RemoteLeaseHeartbeatOutcome, VoomError> {
        let lease = self
            .heartbeat_lease_in_tx(
                tx,
                input.lease_id,
                Duration::seconds(input.lease_ttl_seconds),
                now,
            )
            .await?;
        Ok(RemoteLeaseHeartbeatOutcome {
            lease_id: lease.id,
            worker_id: lease.worker_id,
            ttl_seconds: lease.ttl_seconds,
        })
    }
}
