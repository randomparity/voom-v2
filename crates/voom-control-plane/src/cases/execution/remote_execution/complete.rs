//! Remote lease completion: success path and complete-evidence validation.

use serde_json::Value as JsonValue;
use sqlx::{Sqlite, Transaction};
use voom_core::{FailureClass, NodeId, VoomError, WorkerId};
use voom_store::repo::artifact_access_plans::{ArtifactAccessPlan, ArtifactAccessPlanStatus};
use voom_store::repo::remote_idempotency::{
    IdempotencyOutcome, RemoteIdempotencyInput, RemoteMutationReplay,
};

use crate::ControlPlane;
use crate::cases::execution::remote_execution::{
    RemoteCompleteInput, RemoteCompleteOutcome, ReplayRoute, decode_replay,
    is_remote_replayable_error, remote_error_message, route_lease_complete,
};
use crate::cases::{begin_immediate_tx, commit_tx};

impl ControlPlane {
    /// Complete a held remote lease successfully.
    ///
    /// # Errors
    /// Returns authentication, idempotency, ownership, artifact validation,
    /// or lease-release errors.
    pub async fn remote_complete(
        &self,
        input: RemoteCompleteInput,
    ) -> Result<RemoteCompleteOutcome, VoomError> {
        let now = self.clock().now();
        let route_key = route_lease_complete(input.lease_id);
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
                        decode_replay::<RemoteCompleteOutcome>(data, "remote complete")
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

        let prepared = match self.remote_complete_preflight_in_tx(&mut tx, &input).await {
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
            .remote_complete_mutation_in_tx(&mut tx, &input, &route_key, prepared, now)
            .await?;
        commit_tx(tx).await?;
        Ok(outcome)
    }

    async fn remote_complete_preflight_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteCompleteInput,
    ) -> Result<RemoteCompletePrepared, VoomError> {
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
                    "remote complete rejected: lease {} has no artifact access plan",
                    input.lease_id
                ))
            })?;
        let evidence = validated_artifact_complete_evidence(&input.result, &plan)?;
        Ok(RemoteCompletePrepared {
            plan_id: plan.id,
            evidence,
        })
    }

    async fn remote_complete_mutation_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteCompleteInput,
        route_key: &str,
        prepared: RemoteCompletePrepared,
        now: time::OffsetDateTime,
    ) -> Result<RemoteCompleteOutcome, VoomError> {
        let consumed = self
            .artifact_access_plans
            .mark_status_in_tx(
                tx,
                prepared.plan_id,
                ArtifactAccessPlanStatus::Consumed,
                Some("worker validated artifact access".to_owned()),
                prepared.evidence,
                now,
            )
            .await?;
        let released = self
            .release_lease_in_tx(tx, input.lease_id, input.result.clone(), now)
            .await?;
        let outcome = RemoteCompleteOutcome {
            lease_id: released.id,
            ticket_id: released.ticket_id,
            worker_id: released.worker_id,
            artifact_access_plan: super::acquire::remote_plan(&consumed),
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

    pub(super) async fn complete_remote_ok_in_tx<T>(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        node_id: NodeId,
        route_key: &str,
        worker_id: Option<WorkerId>,
        idempotency_key: &str,
        outcome: &T,
    ) -> Result<(), VoomError>
    where
        T: serde::Serialize,
    {
        self.remote_idempotency
            .complete_in_tx(
                tx,
                node_id,
                route_key,
                worker_id,
                idempotency_key,
                RemoteMutationReplay::Ok {
                    data: serde_json::to_value(outcome).map_err(|e| {
                        VoomError::Internal(format!("remote replay serialization: {e}"))
                    })?,
                },
            )
            .await
    }

    pub(super) async fn complete_remote_error_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        node_id: NodeId,
        route_key: &str,
        worker_id: Option<WorkerId>,
        idempotency_key: &str,
        err: &VoomError,
    ) -> Result<(), VoomError> {
        self.remote_idempotency
            .complete_in_tx(
                tx,
                node_id,
                route_key,
                worker_id,
                idempotency_key,
                RemoteMutationReplay::Error {
                    code: err.code().to_owned(),
                    message: remote_error_message(err),
                },
            )
            .await
    }
}

struct RemoteCompletePrepared {
    plan_id: u64,
    evidence: JsonValue,
}

pub(super) fn artifact_failure_status(
    class: FailureClass,
    reason: &str,
) -> ArtifactAccessPlanStatus {
    if matches!(
        class,
        FailureClass::WorkerTimeout
            | FailureClass::WorkerCrash
            | FailureClass::ProgressTimeout
            | FailureClass::ExternalSystemUnavailable
            | FailureClass::ExternalSystemRateLimited
            | FailureClass::BackupFailure
            | FailureClass::CommitFailure
    ) {
        return ArtifactAccessPlanStatus::Failed;
    }

    let reason = reason.to_ascii_lowercase();
    if matches!(
        class,
        FailureClass::ArtifactUnavailable
            | FailureClass::ArtifactChecksumMismatch
            | FailureClass::MalformedWorkerResult
            | FailureClass::MissingCapability
    ) || reason.contains("artifact")
        || reason.contains("access mode")
        || reason.contains("selected mode")
    {
        ArtifactAccessPlanStatus::Rejected
    } else {
        ArtifactAccessPlanStatus::Failed
    }
}

fn validated_artifact_complete_evidence(
    result: &JsonValue,
    plan: &ArtifactAccessPlan,
) -> Result<JsonValue, VoomError> {
    let evidence = result.get("artifact_access").ok_or_else(|| {
        VoomError::Conflict(
            "remote complete rejected: artifact access validation missing".to_owned(),
        )
    })?;
    if evidence.get("validated") != Some(&JsonValue::Bool(true)) {
        return Err(VoomError::Conflict(
            "remote complete rejected: artifact access validation missing".to_owned(),
        ));
    }
    let mode = evidence
        .get("mode")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| {
            VoomError::Conflict("remote complete rejected: artifact access mode missing".to_owned())
        })?;
    if mode != plan.selected_access_mode.as_str() {
        return Err(VoomError::Conflict(format!(
            "remote complete rejected: artifact access mode {mode} does not match selected mode {}",
            plan.selected_access_mode.as_str()
        )));
    }
    let inputs = string_array_evidence(evidence, "inputs_consumed")?;
    if inputs != plan.input_handles {
        return Err(VoomError::Conflict(
            "remote complete rejected: artifact input handles do not match selected plan"
                .to_owned(),
        ));
    }
    let outputs = string_array_evidence(evidence, "outputs_declared")?;
    if outputs != plan.output_handles {
        return Err(VoomError::Conflict(
            "remote complete rejected: artifact output handles do not match selected plan"
                .to_owned(),
        ));
    }
    Ok(evidence.clone())
}

fn string_array_evidence(value: &JsonValue, field: &'static str) -> Result<Vec<String>, VoomError> {
    value
        .get(field)
        .and_then(JsonValue::as_array)
        .ok_or_else(|| VoomError::Conflict(format!("remote complete rejected: {field} missing")))?
        .iter()
        .map(|item| {
            item.as_str().map(str::to_owned).ok_or_else(|| {
                VoomError::Conflict(format!("remote complete rejected: {field} must be strings"))
            })
        })
        .collect()
}
