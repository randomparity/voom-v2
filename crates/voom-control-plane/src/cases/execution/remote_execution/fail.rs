//! Remote lease failure: failure-status mapping and lease-failure transitions.

use sqlx::{Sqlite, Transaction};
use voom_core::VoomError;
use voom_store::repo::artifact_access_plans::ArtifactAccessPlanStatus;
use voom_store::repo::remote_idempotency::{IdempotencyOutcome, RemoteIdempotencyInput};

use crate::ControlPlane;
use crate::cases::execution::remote_execution::{
    RemoteFailInput, RemoteFailOutcome, ReplayRoute, decode_replay, is_remote_replayable_error,
};
use crate::cases::{begin_immediate_tx, commit_tx};

impl ControlPlane {
    /// Fail a held remote lease and update its selected artifact plan.
    ///
    /// # Errors
    /// Returns authentication, idempotency, ownership, artifact plan, or
    /// lease-failure errors.
    pub async fn remote_fail(
        &self,
        input: RemoteFailInput,
    ) -> Result<RemoteFailOutcome, VoomError> {
        let now = self.clock().now();
        let route_key = super::route_lease_fail(input.lease_id);
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
                        decode_replay::<RemoteFailOutcome>(data, "remote fail")
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

        let prepared = match self.remote_fail_preflight_in_tx(&mut tx, &input).await {
            Ok(prepared) => prepared,
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
        let outcome = self
            .remote_fail_mutation_in_tx(&mut tx, &input, &route_key, prepared, now)
            .await?;
        commit_tx(tx).await?;
        Ok(outcome)
    }

    async fn remote_fail_preflight_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteFailInput,
    ) -> Result<RemoteFailPrepared, VoomError> {
        let worker = self
            .workers
            .node_owned_worker_in_tx(tx, input.worker_id, input.node_id)
            .await?;
        super::recover::require_remote_worker(&worker)?;
        self.leases
            .get_held_for_worker_in_tx(tx, input.lease_id, input.worker_id)
            .await?;
        let plan = self
            .artifact_access_plans
            .get_by_lease_in_tx(tx, input.lease_id)
            .await?
            .ok_or_else(|| {
                VoomError::Conflict(format!(
                    "remote fail rejected: lease {} has no artifact access plan",
                    input.lease_id
                ))
            })?;
        Ok(RemoteFailPrepared {
            plan_id: plan.id,
            status: super::complete::artifact_failure_status(input.class, &input.reason),
        })
    }

    async fn remote_fail_mutation_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteFailInput,
        route_key: &str,
        prepared: RemoteFailPrepared,
        now: time::OffsetDateTime,
    ) -> Result<RemoteFailOutcome, VoomError> {
        let marked = self
            .artifact_access_plans
            .mark_status_in_tx(
                tx,
                prepared.plan_id,
                prepared.status,
                Some(input.reason.clone()),
                input.evidence.clone(),
                now,
            )
            .await?;
        let failed = self
            .fail_lease_in_tx(tx, input.lease_id, input.reason.clone(), input.class, now)
            .await?;
        let outcome = RemoteFailOutcome {
            lease_id: failed.id,
            ticket_id: failed.ticket_id,
            worker_id: failed.worker_id,
            artifact_access_plan: super::acquire::remote_plan(&marked),
        };
        self.complete_remote_ok_in_tx(
            tx,
            input.node_id,
            route_key,
            Some(input.worker_id),
            &input.idempotency_key,
            &outcome,
        )
        .await?;
        Ok(outcome)
    }
}

struct RemoteFailPrepared {
    plan_id: u64,
    status: ArtifactAccessPlanStatus,
}
