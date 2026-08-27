//! Remote lease acquire: scoring, capacity recheck, decision and plan building.

use std::collections::HashMap;

use crate::ControlPlane;
use crate::cases::commit_tx;
use crate::cases::execution::remote_execution::{
    ROUTE_ACQUIRE, RemoteAcquireInput, RemoteAcquireOutcome, RemoteArtifactAccessPlan,
    RemoteLeaseDispatch, ReplayRoute, decode_acquire_replay, is_remote_replayable_error,
};
use crate::workflow::plan::artifact_access_resolution::{
    AccessResolution, AccessResolutionError, resolve_artifact_access,
};
use crate::workflow::plan::ticket_payload::WorkflowTicketPayload;
use serde_json::{Value as JsonValue, json};
use sqlx::{Sqlite, Transaction};
use time::{Duration, OffsetDateTime};
use voom_core::{
    ArtifactAccessDeclaration, ArtifactAccessMode, LeaseId, NodeId, StorageRootId, TicketId,
    TicketOperation, VoomError, WorkerId,
    owner_access_evidence::{
        AccessReferenceReason, AccessRejectionEvidence, DecisionAccessEvidence,
        OwnerAccessEvidence, RootEpoch,
    },
};
use voom_scheduler::{
    NodeCandidate, SCORING_VERSION, SchedulerCandidate, SchedulerScorer, ScoreDecision,
    ScoreOutcome, ScoreReasonCode, TicketCandidate, WorkerCandidate,
};
use voom_store::repo::execution::leases::{
    LeaseAcquireOutcome, LeaseIneligibilityReason, NewLease,
};
use voom_store::repo::execution::remote_idempotency::{
    IdempotencyOutcome, RemoteIdempotencyInput, RemoteMutationReplay,
};
use voom_store::repo::execution::scheduler_decisions::{
    NewSchedulerDecision, SchedulerDecisionKind, SchedulerDecisionOutcome,
    SchedulerReasonCode as StoreSchedulerReasonCode, SchedulerRequestSource,
};
use voom_store::repo::execution::tickets::Ticket;
use voom_store::repo::execution::workers::WorkerOperationEligibility;
use voom_store::repo::media::artifact_access_plans::{ArtifactAccessPlan, NewArtifactAccessPlan};
use voom_store::tx::begin_read_then_write;

impl ControlPlane {
    /// Acquire the next ready ticket for a node-owned remote worker.
    ///
    /// # Errors
    /// Returns authentication, idempotency, eligibility, lease, or artifact
    /// access plan errors.
    #[expect(
        clippy::too_many_lines,
        reason = "the atomic replay, validation, and lease branches are clearer in transaction order"
    )]
    pub async fn remote_acquire(
        &self,
        input: RemoteAcquireInput,
    ) -> Result<RemoteAcquireOutcome, VoomError> {
        let now = self.clock().now();
        let mut tx = begin_read_then_write(&self.pool, "acquire: remote_acquire").await?;
        let auth = self
            .require_remote_incarnation_fence_in_tx(
                &mut tx,
                input.node_id,
                &input.token,
                input.incarnation_id,
                Some(input.worker_id),
            )
            .await?;
        let replay_key =
            super::incarnation_replay_key(input.incarnation_id, &input.idempotency_key);

        match self
            .remote_idempotency
            .reserve_or_replay_in_tx(
                &mut tx,
                RemoteIdempotencyInput {
                    node_id: input.node_id,
                    route_key: ROUTE_ACQUIRE.to_owned(),
                    worker_id: Some(input.worker_id),
                    idempotency_key: replay_key.clone(),
                    request_hash: input.request_hash.clone(),
                    created_at: now,
                },
            )
            .await?
        {
            IdempotencyOutcome::Reserved => {}
            IdempotencyOutcome::Replay(replay) => {
                return self.finish_acquire_replay_in_tx(tx, &input, replay).await;
            }
        }

        if let Err(err) = super::recover::validate_remote_node_live(&auth, input.node_id, now, true)
        {
            self.complete_remote_error_in_tx(
                &mut tx,
                input.node_id,
                ROUTE_ACQUIRE,
                Some(input.worker_id),
                &replay_key,
                &err,
            )
            .await?;
            commit_tx(tx).await?;
            return Err(err);
        }

        let prepared = match self
            .remote_acquire_preflight_in_tx(&mut tx, &input, now)
            .await
        {
            Ok(prepared) => prepared,
            Err(err) => {
                if !is_remote_replayable_error(&err) {
                    return Err(err);
                }
                self.complete_remote_error_in_tx(
                    &mut tx,
                    input.node_id,
                    ROUTE_ACQUIRE,
                    Some(input.worker_id),
                    &replay_key,
                    &err,
                )
                .await?;
                commit_tx(tx).await?;
                return Err(err);
            }
        };

        let outcome = match prepared {
            RemoteAcquirePrepared::Idle(outcome) | RemoteAcquirePrepared::NoCandidate(outcome) => {
                self.complete_remote_ok_in_tx(
                    &mut tx,
                    input.node_id,
                    ROUTE_ACQUIRE,
                    Some(input.worker_id),
                    &replay_key,
                    &outcome,
                )
                .await?;
                commit_tx(tx).await?;
                return Ok(outcome);
            }
            RemoteAcquirePrepared::Leased {
                ticket,
                eligibility,
                locality,
                score,
            } => {
                self.remote_acquire_leased_in_tx(
                    &mut tx,
                    &input,
                    ticket,
                    eligibility,
                    locality,
                    &score,
                    now,
                )
                .await?
            }
        };
        commit_tx(tx).await?;
        Ok(outcome)
    }

    async fn remote_acquire_preflight_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteAcquireInput,
        now: time::OffsetDateTime,
    ) -> Result<RemoteAcquirePrepared, VoomError> {
        super::recover::require_positive_ttl(input.lease_ttl_seconds)?;
        let worker = self
            .workers
            .node_owned_worker_in_tx(tx, input.worker_id, input.node_id)
            .await?;
        super::recover::require_remote_worker(&worker)?;
        let operations = self
            .workers
            .candidate_operations_in_tx(tx, input.worker_id)
            .await?;
        let mut tickets = self
            .tickets
            .ready_for_operations_in_tx(tx, &operations, now)
            .await?;
        // Owner-local gate: reject non-owner and mixed-owner byte work before
        // scoring, and persist each rejection as a durable scheduler decision
        // (issue #477). Filtering here keeps the deterministic ready-ticket
        // ordering and lets a fully rejected snapshot fall through to the
        // idle path.
        let mut gated = Vec::with_capacity(tickets.len());
        let mut locality_by_ticket = HashMap::new();
        for ticket in tickets {
            match resolve_ticket_owner_locality_in_tx(tx, &ticket, input.node_id).await? {
                TicketLocality::OwnerLocal(declaration, resolution) => {
                    locality_by_ticket.insert(ticket.id, (declaration, resolution));
                    gated.push(ticket);
                }
                TicketLocality::Rejected {
                    evidence,
                    fingerprint,
                } => {
                    self.scheduler_decisions
                        .create_or_suppress_in_tx(
                            tx,
                            gate_rejection_decision(input, &ticket, &evidence, &fingerprint, now),
                        )
                        .await?;
                }
                TicketLocality::NoDeclaration => gated.push(ticket),
            }
        }
        tickets = gated;
        if tickets.is_empty() {
            #[expect(
                clippy::default_constructed_unit_structs,
                reason = "Task 3 intentionally wires the default scheduler scorer"
            )]
            let mut score = SchedulerScorer::default().score(&[])?;
            set_operation_set(&mut score.explanation, &operations);
            let decision = self
                .scheduler_decisions
                .create_or_suppress_in_tx(
                    tx,
                    decision_from_score(input, &score, None, Ok(None), now)?,
                )
                .await?;
            return Ok(RemoteAcquirePrepared::Idle(RemoteAcquireOutcome::Idle {
                worker_id: input.worker_id,
                scheduler_decision_id: decision.id,
            }));
        }

        let candidate_set = self
            .remote_acquire_candidates_in_tx(tx, input, tickets, locality_by_ticket)
            .await?;
        let score = score_remote_candidates(&candidate_set.candidates)?;
        match score.outcome {
            ScoreOutcome::Idle => Err(VoomError::Internal(
                "remote acquire scorer returned idle for non-empty candidates".to_owned(),
            )),
            ScoreOutcome::NoEligibleCandidate => {
                let decision = self
                    .scheduler_decisions
                    .create_or_suppress_in_tx(
                        tx,
                        decision_from_score(input, &score, None, Ok(None), now)?,
                    )
                    .await?;
                Ok(RemoteAcquirePrepared::NoCandidate(
                    RemoteAcquireOutcome::NoCandidate {
                        worker_id: input.worker_id,
                        scheduler_decision_id: decision.id,
                    },
                ))
            }
            ScoreOutcome::Selected => {
                self.remote_acquire_selected_in_tx(tx, input, &candidate_set, &score, now)
                    .await
            }
        }
    }

    async fn remote_acquire_selected_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteAcquireInput,
        candidate_set: &RemoteAcquireCandidateSet,
        score: &ScoreDecision,
        now: time::OffsetDateTime,
    ) -> Result<RemoteAcquirePrepared, VoomError> {
        let selected = score.selected.as_ref().ok_or_else(|| {
            VoomError::Internal("remote acquire selected score missing tuple".to_owned())
        })?;
        let selected_candidate = candidate_set
            .candidates
            .iter()
            .find(|candidate| {
                candidate.ticket.ticket_id == selected.ticket_id
                    && candidate.worker.worker_id == selected.worker_id
                    && candidate.node.node_id == selected.node_id
            })
            .ok_or_else(|| {
                VoomError::Internal(format!(
                    "remote acquire selected candidate vanished ticket={}",
                    selected.ticket_id
                ))
            })?;
        let ticket = candidate_set
            .tickets
            .iter()
            .find(|ticket| ticket.id == selected.ticket_id)
            .ok_or_else(|| {
                VoomError::Internal(format!(
                    "remote acquire selected ticket vanished id={}",
                    selected.ticket_id
                ))
            })?
            .clone();
        let eligibility = candidate_set
            .eligibility_by_operation
            .get(&ticket.kind)
            .ok_or_else(|| {
                VoomError::Internal(format!(
                    "remote acquire selected eligibility vanished operation={}",
                    ticket.kind
                ))
            })?
            .clone();
        // Advisory scoring used stale capacity facts; re-read them before
        // committing to this candidate.
        if let Some(outcome) = self
            .recheck_selected_remote_capacity_in_tx(tx, input, selected_candidate, &ticket, now)
            .await?
        {
            return Ok(RemoteAcquirePrepared::NoCandidate(outcome));
        }

        // Reuse the gate's resolution: one resolution, one point in time.
        // The selected decision is created only after the lease and plan
        // exist, so a changed post-selection gate never leaves a selected
        // decision row behind (ADR 0072).
        let locality = candidate_set.locality_by_ticket.get(&selected.ticket_id);
        Ok(RemoteAcquirePrepared::Leased {
            ticket,
            eligibility,
            locality: locality.cloned(),
            score: score.clone(),
        })
    }

    async fn remote_acquire_candidates_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteAcquireInput,
        tickets: Vec<Ticket>,
        locality_by_ticket: HashMap<TicketId, (ArtifactAccessDeclaration, AccessResolution)>,
    ) -> Result<RemoteAcquireCandidateSet, VoomError> {
        let mut eligibility_by_operation = HashMap::new();
        let mut capacity_by_operation = HashMap::new();
        let node_limit = self
            .scheduler_node_limits
            .node_limit_in_tx(tx, input.node_id)
            .await?;
        let node_active_leases = self
            .leases
            .active_count_for_node_in_tx(tx, input.node_id)
            .await?;
        let mut candidates = Vec::with_capacity(tickets.len());
        for ticket in &tickets {
            let eligibility =
                if let Some(eligibility) = eligibility_by_operation.get(&ticket.kind).cloned() {
                    eligibility
                } else {
                    let eligibility = self
                        .workers
                        .operation_eligibility_in_tx(tx, input.worker_id, &ticket.kind)
                        .await?;
                    eligibility_by_operation.insert(ticket.kind.clone(), eligibility.clone());
                    eligibility
                };

            let capacity = if let Some(capacity) = capacity_by_operation.get(&ticket.kind) {
                *capacity
            } else {
                let capacity = self
                    .workers
                    .operation_capacity_in_tx(tx, input.worker_id, &ticket.kind)
                    .await?;
                capacity_by_operation.insert(ticket.kind.clone(), capacity);
                capacity
            };
            candidates.push(candidate_from_ticket(
                input,
                ticket,
                &eligibility,
                capacity.active_leases,
                capacity.max_parallel,
                node_active_leases,
                node_limit,
            )?);
        }

        Ok(RemoteAcquireCandidateSet {
            tickets,
            candidates,
            eligibility_by_operation,
            locality_by_ticket,
        })
    }

    async fn recheck_selected_remote_capacity_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteAcquireInput,
        selected_candidate: &SchedulerCandidate,
        ticket: &Ticket,
        now: time::OffsetDateTime,
    ) -> Result<Option<RemoteAcquireOutcome>, VoomError> {
        // Candidate scoring uses advisory capacity facts; re-read the selected
        // worker and node before lease creation so capacity decisions use the
        // current transaction view.
        let capacity = self
            .workers
            .operation_capacity_in_tx(tx, input.worker_id, &ticket.kind)
            .await?;
        if !capacity.has_capacity() {
            return self
                .capacity_no_candidate_in_tx(
                    tx,
                    input,
                    SelectedCapacityFull {
                        reason_code: StoreSchedulerReasonCode::WorkerCapacityFull,
                        selected_candidate,
                        observed_active: capacity.active_leases,
                        observed_limit: capacity.max_parallel,
                    },
                    now,
                )
                .await
                .map(Some);
        }

        let node_active = self
            .leases
            .active_count_for_node_in_tx(tx, input.node_id)
            .await?;
        let node_limit = self
            .scheduler_node_limits
            .node_limit_in_tx(tx, input.node_id)
            .await?;
        if node_active >= node_limit {
            return self
                .capacity_no_candidate_in_tx(
                    tx,
                    input,
                    SelectedCapacityFull {
                        reason_code: StoreSchedulerReasonCode::NodeCapacityFull,
                        selected_candidate,
                        observed_active: node_active,
                        observed_limit: node_limit,
                    },
                    now,
                )
                .await
                .map(Some);
        }

        Ok(None)
    }

    async fn capacity_no_candidate_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteAcquireInput,
        capacity: SelectedCapacityFull<'_>,
        now: time::OffsetDateTime,
    ) -> Result<RemoteAcquireOutcome, VoomError> {
        let decision = self
            .scheduler_decisions
            .create_or_suppress_in_tx(
                tx,
                capacity_decision(
                    input,
                    capacity.reason_code,
                    capacity.selected_candidate,
                    1,
                    capacity.observed_active,
                    capacity.observed_limit,
                    now,
                ),
            )
            .await?;
        Ok(RemoteAcquireOutcome::NoCandidate {
            worker_id: input.worker_id,
            scheduler_decision_id: decision.id,
        })
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "remote acquire keeps the transaction input and selected facts explicit"
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "the acquired and changed-gate branches are clearer in transaction order"
    )]
    async fn remote_acquire_leased_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &RemoteAcquireInput,
        ticket: Ticket,
        eligibility: WorkerOperationEligibility,
        locality: Option<(ArtifactAccessDeclaration, AccessResolution)>,
        score: &ScoreDecision,
        now: time::OffsetDateTime,
    ) -> Result<RemoteAcquireOutcome, VoomError> {
        let outcome = self
            .try_acquire_lease_in_tx(
                tx,
                NewLease {
                    ticket_id: ticket.id,
                    worker_id: input.worker_id,
                    ttl: Duration::seconds(input.lease_ttl_seconds),
                    now,
                },
            )
            .await?;
        let lease = match &outcome {
            LeaseAcquireOutcome::Acquired(lease) => lease.clone(),
            LeaseAcquireOutcome::WorkerIneligible {
                worker_id,
                reason: LeaseIneligibilityReason::WorkerMissing,
                ..
            } => {
                // The same transaction read this worker at preflight; a
                // mid-transaction vanish is an invariant violation, not a
                // scheduling gate.
                return Err(VoomError::Internal(format!(
                    "remote acquire selected worker {worker_id} vanished before lease creation"
                )));
            }
            rejected => {
                // A post-selection gate changed under the selected candidate.
                // The savepoint rolled back, so zero leases and zero bound
                // access plans exist; the changed gate becomes one durable
                // decision carrying the documented stable reason (ADR 0072).
                let reason_code = outcome_reason_code(rejected);
                let decision = self
                    .scheduler_decisions
                    .create_or_suppress_in_tx(
                        tx,
                        changed_gate_decision(
                            input,
                            &ticket,
                            reason_code,
                            changed_gate_explanation(rejected, reason_code),
                            now,
                        ),
                    )
                    .await?;
                let outcome = RemoteAcquireOutcome::NoCandidate {
                    worker_id: input.worker_id,
                    scheduler_decision_id: decision.id,
                };
                // The request reached a terminal decision, so its idempotency
                // reservation must complete — an unfinished reservation would
                // poison every replay of this key with a conflict.
                self.complete_remote_ok_in_tx(
                    tx,
                    input.node_id,
                    ROUTE_ACQUIRE,
                    Some(input.worker_id),
                    &super::incarnation_replay_key(input.incarnation_id, &input.idempotency_key),
                    &outcome,
                )
                .await?;
                return Ok(outcome);
            }
        };

        let plan = self
            .artifact_access_plans
            .create_selected_in_tx(
                tx,
                artifact_plan_input(
                    input,
                    &ticket,
                    &eligibility,
                    locality.as_ref(),
                    lease.id,
                    now,
                )?,
            )
            .await?;
        let access_evidence = locality
            .as_ref()
            .map(|(declaration, resolution)| decision_owner_evidence(declaration, resolution))
            .transpose()?;
        let scheduler_decision = self
            .scheduler_decisions
            .create_in_tx(
                tx,
                decision_from_score(
                    input,
                    score,
                    Some((ticket.id, input.worker_id, input.node_id, lease.id)),
                    Ok(access_evidence),
                    now,
                )?,
            )
            .await?;
        let outcome = RemoteAcquireOutcome::Leased(RemoteLeaseDispatch {
            lease_id: lease.id,
            scheduler_decision_id: scheduler_decision.id,
            ticket_id: ticket.id,
            worker_id: input.worker_id,
            operation: ticket.kind.normalize().matching_token().into_string(),
            dispatch_payload: ticket.payload,
            lease_ttl_seconds: lease.ttl_seconds,
            heartbeat_after_seconds: heartbeat_after_seconds(lease.ttl_seconds),
            artifact_access_plan: remote_plan(&plan),
        });
        self.complete_remote_ok_in_tx(
            tx,
            input.node_id,
            ROUTE_ACQUIRE,
            Some(input.worker_id),
            &super::incarnation_replay_key(input.incarnation_id, &input.idempotency_key),
            &outcome,
        )
        .await?;
        Ok(outcome)
    }
}

/// What the owner-local gate proved about one ready ticket.
///
/// `Rejected` carries ready-to-persist rejection evidence: resolution
/// short-circuits at the first domain failure, so the evidence records exactly
/// that failing reference — reasons for references it never reached are never
/// invented (ADR 0071).
enum TicketLocality {
    OwnerLocal(ArtifactAccessDeclaration, AccessResolution),
    Rejected {
        evidence: AccessRejectionEvidence,
        fingerprint: String,
    },
    NoDeclaration,
}

/// Prove a ready ticket's declared artifact access resolves to the acquiring
/// node as its single common owner, or produce stable locator-free rejection
/// evidence.
///
/// A ticket with no resolvable declaration is not byte work this gate owns:
/// payload decoding already enforces a declaration on every byte-touching
/// operation. A `DatabaseError` from resolution is *not* a rejection — it
/// propagates and fails the acquire, because corruption is never an
/// eligibility result.
async fn resolve_ticket_owner_locality_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    ticket: &Ticket,
    node_id: NodeId,
) -> Result<TicketLocality, VoomError> {
    let declaration =
        WorkflowTicketPayload::parse_ticket(ticket.kind.as_str(), ticket.payload.clone())
            .ok()
            .and_then(|payload| payload.declared_artifact_access);
    let Some(declaration) = declaration else {
        return Ok(TicketLocality::NoDeclaration);
    };
    let node = i64::try_from(node_id.0)
        .map_err(|_| VoomError::Internal("acquiring node id exceeds i64".to_owned()))?;

    match resolve_artifact_access(tx, &declaration).await {
        Ok(resolution) if resolution.owner_node_id == node => {
            Ok(TicketLocality::OwnerLocal(declaration, resolution))
        }
        Ok(_resolution) => Ok(TicketLocality::Rejected {
            evidence: owner_mismatch_evidence(&declaration)?,
            fingerprint: declaration_fingerprint(&declaration),
        }),
        Err(failure) => match failure.error {
            // Corruption is a database error, never an eligibility result.
            AccessResolutionError::DatabaseError(message) => Err(VoomError::database(format!(
                "artifact access resolution: {message}"
            ))),
            domain_error => Ok(TicketLocality::Rejected {
                evidence: failure_evidence(&failure.target, &domain_error)?,
                fingerprint: declaration_fingerprint(&declaration),
            }),
        },
    }
}

/// Stable rejection evidence for the failing reference resolution reported.
fn failure_evidence(
    target: &voom_core::ArtifactAccessTarget,
    error: &AccessResolutionError,
) -> Result<AccessRejectionEvidence, VoomError> {
    let reason = match error {
        AccessResolutionError::StorageRootNotFound { .. } => {
            AccessReferenceReason::StorageRootNotFound
        }
        AccessResolutionError::FileLocationNotFound { .. } => {
            AccessReferenceReason::FileLocationNotFound
        }
        AccessResolutionError::LocationRootInvalid { .. } => {
            AccessReferenceReason::LocationRootInvalid
        }
        AccessResolutionError::InvalidRootState { .. } => AccessReferenceReason::InvalidRootState,
        AccessResolutionError::InvalidRootEpoch { .. } => AccessReferenceReason::InvalidRootEpoch,
        AccessResolutionError::InvalidLocationState { .. } => {
            AccessReferenceReason::InvalidLocationState
        }
        AccessResolutionError::MixedOwner { .. } => AccessReferenceReason::MixedOwner,
        AccessResolutionError::NoActiveIncarnation { .. } => {
            AccessReferenceReason::NoActiveIncarnation
        }
        // The gate never routes database errors here.
        AccessResolutionError::DatabaseError(message) => {
            return Err(VoomError::database(format!(
                "artifact access resolution: {message}"
            )));
        }
    };
    AccessRejectionEvidence::new(vec![
        voom_core::owner_access_evidence::AccessReferenceRejection {
            target: target.clone(),
            reason,
        },
    ])
    .map_err(|err| VoomError::Internal(format!("rejection evidence rejected: {err}")))
}

/// Rejection evidence when resolution succeeded but its common owner is not
/// the acquiring node.
fn owner_mismatch_evidence(
    declaration: &voom_core::ArtifactAccessDeclaration,
) -> Result<AccessRejectionEvidence, VoomError> {
    AccessRejectionEvidence::new(vec![
        voom_core::owner_access_evidence::AccessReferenceRejection {
            target: declaration.entries()[0].target.clone(),
            reason: AccessReferenceReason::OwnerMismatch,
        },
    ])
    .map_err(|err| VoomError::Internal(format!("rejection evidence rejected: {err}")))
}

/// Canonical locality fingerprint for a suppression key: the compact JSON of
/// the declaration alone. A failed resolution produced no trustworthy epochs,
/// so only the locality claim is hashed.
fn declaration_fingerprint(declaration: &voom_core::ArtifactAccessDeclaration) -> String {
    serde_json::to_string(declaration).unwrap_or_else(|_| "opaque".to_owned())
}

#[expect(
    clippy::large_enum_variant,
    reason = "Task 3 carries the selected scheduler decision through prepared state for lease linking"
)]
enum RemoteAcquirePrepared {
    Idle(RemoteAcquireOutcome),
    NoCandidate(RemoteAcquireOutcome),
    Leased {
        ticket: Ticket,
        eligibility: WorkerOperationEligibility,
        locality: Option<(ArtifactAccessDeclaration, AccessResolution)>,
        score: ScoreDecision,
    },
}

#[derive(Debug)]
struct RemoteAcquireCandidateSet {
    tickets: Vec<Ticket>,
    candidates: Vec<SchedulerCandidate>,
    eligibility_by_operation: HashMap<TicketOperation, WorkerOperationEligibility>,
    /// The owner-local proof the gate captured for each byte-work ticket,
    /// reused at selection time so no second resolution runs.
    locality_by_ticket: HashMap<TicketId, (ArtifactAccessDeclaration, AccessResolution)>,
}

#[derive(Debug, Clone, Copy)]
struct SelectedCapacityFull<'a> {
    reason_code: StoreSchedulerReasonCode,
    selected_candidate: &'a SchedulerCandidate,
    observed_active: u32,
    observed_limit: u32,
}

fn candidate_from_ticket(
    input: &RemoteAcquireInput,
    ticket: &Ticket,
    eligibility: &WorkerOperationEligibility,
    worker_active: u32,
    worker_limit: u32,
    node_active: u32,
    node_limit: u32,
) -> Result<SchedulerCandidate, VoomError> {
    if worker_limit == 0 || node_limit == 0 {
        return Err(VoomError::Config(
            "scheduler candidate limits must be positive".to_owned(),
        ));
    }
    Ok(SchedulerCandidate {
        ticket: TicketCandidate {
            ticket_id: ticket.id,
            operation: ticket.kind.clone(),
            priority: ticket.priority,
            next_eligible_at_epoch_seconds: ticket.next_eligible_at.unix_timestamp(),
        },
        worker: WorkerCandidate {
            worker_id: input.worker_id,
            node_id: input.node_id,
            executable: true,
            has_capability: eligibility.has_capability,
            has_grant: eligibility.has_grant,
            denied: eligibility.is_denied,
            active_leases: worker_active,
            max_parallel: worker_limit,
            artifact_access: eligibility
                .artifact_access
                .iter()
                .filter_map(|mode| ArtifactAccessMode::from_wire(mode))
                .collect(),
        },
        node: NodeCandidate {
            node_id: input.node_id,
            executable: true,
            heartbeat_fresh: true,
            active_leases: node_active,
            max_parallel_leases: node_limit,
        },
    })
}

pub(super) fn score_remote_candidates(
    candidates: &[SchedulerCandidate],
) -> Result<ScoreDecision, VoomError> {
    if candidates.is_empty() {
        #[expect(
            clippy::default_constructed_unit_structs,
            reason = "Task 4 keeps scorer ownership of idle explanations"
        )]
        return SchedulerScorer::default().score(candidates);
    }

    // Remote acquire is still scoped to one worker's ready-ticket snapshot, so
    // candidate breadth stays bounded. Keep the scorer API simple with cloned
    // homogeneous operation slices unless this path grows beyond that scope.
    let mut operation_order = Vec::new();
    let mut by_operation: HashMap<TicketOperation, Vec<SchedulerCandidate>> = HashMap::new();
    for candidate in candidates {
        if !by_operation.contains_key(&candidate.ticket.operation) {
            operation_order.push(candidate.ticket.operation.clone());
        }
        by_operation
            .entry(candidate.ticket.operation.clone())
            .or_default()
            .push(candidate.clone());
    }

    #[expect(
        clippy::default_constructed_unit_structs,
        reason = "Task 4 intentionally uses the default scheduler scorer"
    )]
    let scorer = SchedulerScorer::default();
    let mut best_selected: Option<(ScoreDecision, SchedulerCandidate)> = None;
    let mut first_no_candidate = None;
    let mut group_scores = Vec::new();

    for operation in operation_order {
        let operation_candidates = by_operation.remove(&operation).ok_or_else(|| {
            VoomError::Internal(format!(
                "remote acquire candidate group vanished operation={operation}"
            ))
        })?;
        let score = scorer.score(&operation_candidates)?;
        match score.outcome {
            ScoreOutcome::Selected => {
                let selected_candidate =
                    selected_candidate_for_score(&score, &operation_candidates)?;
                match &best_selected {
                    Some((best_score, best_candidate))
                        if !selected_score_is_better(
                            &score,
                            &selected_candidate,
                            best_score,
                            best_candidate,
                        ) => {}
                    _ => best_selected = Some((score.clone(), selected_candidate)),
                }
            }
            ScoreOutcome::NoEligibleCandidate => {
                first_no_candidate.get_or_insert_with(|| score.clone());
            }
            ScoreOutcome::Idle => {}
        }
        group_scores.push(score);
    }

    if let Some((score, _)) = best_selected {
        return Ok(aggregate_score_decision(
            score,
            &group_scores,
            candidates.len(),
        ));
    }
    first_no_candidate
        .map(|score| aggregate_score_decision(score, &group_scores, candidates.len()))
        .ok_or_else(|| VoomError::Internal("remote acquire scorer returned no decision".to_owned()))
}

fn aggregate_score_decision(
    mut base: ScoreDecision,
    group_scores: &[ScoreDecision],
    candidate_count: usize,
) -> ScoreDecision {
    let mut candidate_rows = Vec::new();
    let mut operations = Vec::new();
    for score in group_scores {
        if let Some(operation) = score
            .explanation
            .get("operation")
            .and_then(JsonValue::as_str)
            && operations.iter().all(|existing| existing != operation)
        {
            operations.push(operation.to_owned());
        }
        if let Some(rows) = score
            .explanation
            .get("candidates")
            .and_then(JsonValue::as_array)
        {
            candidate_rows.extend(rows.iter().cloned());
        }
    }
    if let Some(object) = base.explanation.as_object_mut() {
        object.insert("candidates".to_owned(), JsonValue::Array(candidate_rows));
        object.insert("operation_set".to_owned(), json!(operations));
        if operations.len() != 1 {
            object.insert("operation".to_owned(), JsonValue::Null);
        }
    }
    base.candidate_count = candidate_count;
    if base.outcome == ScoreOutcome::NoEligibleCandidate {
        base.reason_code = first_rejection_reason(&base.explanation);
    }
    base
}

fn first_rejection_reason(explanation: &JsonValue) -> ScoreReasonCode {
    explanation
        .get("candidates")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| row.get("reasons").and_then(JsonValue::as_array))
        .flatten()
        .filter_map(JsonValue::as_str)
        .filter_map(ScoreReasonCode::parse)
        .min_by_key(|reason| reason.priority())
        .unwrap_or(ScoreReasonCode::NoEligibleCandidate)
}

fn selected_candidate_for_score(
    score: &voom_scheduler::ScoreDecision,
    candidates: &[SchedulerCandidate],
) -> Result<SchedulerCandidate, VoomError> {
    let selected = score
        .selected
        .as_ref()
        .ok_or_else(|| VoomError::Internal("selected score missing tuple".to_owned()))?;
    candidates
        .iter()
        .find(|candidate| {
            candidate.ticket.ticket_id == selected.ticket_id
                && candidate.worker.worker_id == selected.worker_id
                && candidate.node.node_id == selected.node_id
        })
        .cloned()
        .ok_or_else(|| {
            VoomError::Internal(format!(
                "selected score references missing candidate ticket={}",
                selected.ticket_id
            ))
        })
}

fn selected_score_is_better(
    challenger: &ScoreDecision,
    challenger_candidate: &SchedulerCandidate,
    incumbent: &ScoreDecision,
    incumbent_candidate: &SchedulerCandidate,
) -> bool {
    let challenger_score = challenger
        .selected
        .as_ref()
        .map_or(i64::MIN, |selected| selected.score);
    let incumbent_score = incumbent
        .selected
        .as_ref()
        .map_or(i64::MIN, |selected| selected.score);
    challenger_score > incumbent_score
        || (challenger_score == incumbent_score
            && selected_candidate_key(challenger_candidate)
                < selected_candidate_key(incumbent_candidate))
}

fn selected_candidate_key(
    candidate: &SchedulerCandidate,
) -> (std::cmp::Reverse<i64>, i64, u64, u64, u64) {
    (
        std::cmp::Reverse(candidate.ticket.priority),
        candidate.ticket.next_eligible_at_epoch_seconds,
        candidate.node.node_id.0,
        candidate.worker.worker_id.0,
        candidate.ticket.ticket_id.0,
    )
}

fn decision_from_score(
    input: &RemoteAcquireInput,
    score: &voom_scheduler::ScoreDecision,
    selected: Option<(TicketId, WorkerId, NodeId, LeaseId)>,
    access_evidence: Result<Option<DecisionAccessEvidence>, VoomError>,
    now: OffsetDateTime,
) -> Result<NewSchedulerDecision, VoomError> {
    let (ticket_id, selected_worker_id, selected_node_id, selected_lease_id) = selected.map_or(
        (None, None, None, None),
        |(ticket_id, worker_id, node_id, lease_id)| {
            (
                Some(ticket_id),
                Some(worker_id),
                Some(node_id),
                Some(lease_id),
            )
        },
    );
    let (decision_kind, outcome) = match score.outcome {
        ScoreOutcome::Selected => (
            SchedulerDecisionKind::LeaseAcquire,
            SchedulerDecisionOutcome::Selected,
        ),
        ScoreOutcome::Idle => (SchedulerDecisionKind::Idle, SchedulerDecisionOutcome::Idle),
        ScoreOutcome::NoEligibleCandidate => (
            SchedulerDecisionKind::NoCandidate,
            SchedulerDecisionOutcome::NoEligibleCandidate,
        ),
    };
    Ok(NewSchedulerDecision {
        decision_kind,
        request_source: SchedulerRequestSource::RemoteAcquire,
        idempotency_key: Some(input.idempotency_key.clone()),
        request_node_id: Some(input.node_id),
        request_worker_id: Some(input.worker_id),
        ticket_id,
        selected_worker_id,
        selected_node_id,
        selected_lease_id,
        outcome,
        reason_code: scheduler_reason(score.reason_code),
        summary: scheduler_summary(score),
        candidate_count: u32::try_from(score.candidate_count).unwrap_or(u32::MAX),
        selected_score: match score.outcome {
            ScoreOutcome::Selected => score.selected.as_ref().map(|selected| selected.score),
            ScoreOutcome::Idle | ScoreOutcome::NoEligibleCandidate => None,
        },
        access_evidence: access_evidence?,
        suppression_key: suppression_key(input, score),
        explanation: score.explanation.clone(),
        now,
    })
}

fn capacity_decision(
    input: &RemoteAcquireInput,
    reason_code: StoreSchedulerReasonCode,
    selected_candidate: &SchedulerCandidate,
    candidate_count: usize,
    observed_active: u32,
    observed_limit: u32,
    now: OffsetDateTime,
) -> NewSchedulerDecision {
    let reason = reason_code.as_str();
    NewSchedulerDecision {
        decision_kind: SchedulerDecisionKind::NoCandidate,
        request_source: SchedulerRequestSource::RemoteAcquire,
        idempotency_key: Some(input.idempotency_key.clone()),
        request_node_id: Some(input.node_id),
        request_worker_id: Some(input.worker_id),
        // The key names this ticket, so the row must agree with it.
        ticket_id: Some(selected_candidate.ticket.ticket_id),
        selected_worker_id: None,
        selected_node_id: None,
        selected_lease_id: None,
        outcome: SchedulerDecisionOutcome::NoEligibleCandidate,
        reason_code,
        summary: format!("no eligible candidate: {reason}"),
        candidate_count: u32::try_from(candidate_count).unwrap_or(u32::MAX),
        selected_score: None,
        access_evidence: None,
        suppression_key: Some(capacity_suppression_key(
            input,
            reason,
            &selected_candidate.ticket.operation,
            selected_candidate.ticket.ticket_id,
        )),
        explanation: json!({
            "scoring_version": SCORING_VERSION,
            "outcome": "no_eligible_candidate",
            "reason": reason,
            "operation": selected_candidate.ticket.operation.as_str(),
            "selected_ticket_id": selected_candidate.ticket.ticket_id.0,
            "observed": {
                "active_leases": observed_active,
                "limit": observed_limit
            }
        }),
        now,
    }
}

pub(super) fn scheduler_reason(reason: ScoreReasonCode) -> StoreSchedulerReasonCode {
    match reason {
        ScoreReasonCode::Selected => StoreSchedulerReasonCode::Selected,
        ScoreReasonCode::NoReadyTicket => StoreSchedulerReasonCode::NoReadyTicket,
        ScoreReasonCode::MissingCapability => StoreSchedulerReasonCode::MissingCapability,
        ScoreReasonCode::MissingGrant => StoreSchedulerReasonCode::MissingGrant,
        ScoreReasonCode::OperationDenied => StoreSchedulerReasonCode::OperationDenied,
        ScoreReasonCode::WorkerNotExecutable => StoreSchedulerReasonCode::WorkerNotExecutable,
        ScoreReasonCode::NodeNotExecutable => StoreSchedulerReasonCode::NodeNotExecutable,
        ScoreReasonCode::HeartbeatExpired => StoreSchedulerReasonCode::HeartbeatExpired,
        ScoreReasonCode::UnsupportedArtifactAccess => {
            StoreSchedulerReasonCode::UnsupportedArtifactAccess
        }
        ScoreReasonCode::WorkerCapacityFull => StoreSchedulerReasonCode::WorkerCapacityFull,
        ScoreReasonCode::NodeCapacityFull => StoreSchedulerReasonCode::NodeCapacityFull,
        ScoreReasonCode::NoEligibleCandidate => StoreSchedulerReasonCode::NoEligibleCandidate,
    }
}

fn scheduler_summary(score: &voom_scheduler::ScoreDecision) -> String {
    match score.outcome {
        ScoreOutcome::Selected => {
            if let Some(selected) = &score.selected {
                format!(
                    "selected worker {} on node {} for ticket {}",
                    selected.worker_id, selected.node_id, selected.ticket_id
                )
            } else {
                "selected scheduler candidate".to_owned()
            }
        }
        ScoreOutcome::Idle => "no ready tickets".to_owned(),
        ScoreOutcome::NoEligibleCandidate => {
            format!("no eligible candidate: {}", score.reason_code.as_str())
        }
    }
}

pub(super) fn suppression_key(
    input: &RemoteAcquireInput,
    score: &voom_scheduler::ScoreDecision,
) -> Option<String> {
    if score.outcome == ScoreOutcome::Selected {
        return None;
    }
    Some(remote_acquire_suppression_key(
        input,
        score.reason_code.as_str(),
        &operation_fingerprint(&score.explanation),
        None,
    ))
}

pub(super) fn capacity_suppression_key(
    input: &RemoteAcquireInput,
    reason: &str,
    operation: &TicketOperation,
    ticket_id: TicketId,
) -> String {
    remote_acquire_suppression_key(input, reason, operation.as_str(), Some(ticket_id))
}

fn remote_acquire_suppression_key(
    input: &RemoteAcquireInput,
    reason: &str,
    operation_fingerprint: &str,
    ticket_id: Option<TicketId>,
) -> String {
    let bucket = input.lease_ttl_seconds.max(1) / 30;
    let ticket_segment = ticket_id
        .map(|ticket| format!(":ticket:{}", ticket.0))
        .unwrap_or_default();
    format!(
        "remote_acquire:node:{}:worker:{}{ticket_segment}:reason:{}:ops:{}:bucket:{}",
        input.node_id, input.worker_id, reason, operation_fingerprint, bucket
    )
}

fn set_operation_set(explanation: &mut JsonValue, operations: &[TicketOperation]) {
    if let Some(object) = explanation.as_object_mut() {
        object.insert(
            "operation_set".to_owned(),
            json!(
                operations
                    .iter()
                    .map(TicketOperation::as_str)
                    .collect::<Vec<_>>()
            ),
        );
    }
}

fn operation_fingerprint(explanation: &JsonValue) -> String {
    let mut operations = explanation
        .get("operation_set")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(JsonValue::as_str)
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    if operations.is_empty()
        && let Some(operation) = explanation.get("operation").and_then(JsonValue::as_str)
    {
        operations.push(operation.to_owned());
    }

    if operations.is_empty() {
        operations = explanation
            .get("candidates")
            .and_then(JsonValue::as_array)
            .into_iter()
            .flatten()
            .filter_map(|candidate| candidate.get("operation").and_then(JsonValue::as_str))
            .map(ToOwned::to_owned)
            .collect();
    }

    operations.sort();
    operations.dedup();
    if operations.is_empty() {
        "none".to_owned()
    } else {
        operations.join("+")
    }
}

/// Build the selected decision's owner-local evidence from the resolution the
/// gate already proved — one resolution, one point in time.
fn decision_owner_evidence(
    declaration: &ArtifactAccessDeclaration,
    resolution: &AccessResolution,
) -> Result<DecisionAccessEvidence, VoomError> {
    Ok(DecisionAccessEvidence::Owner(owner_access_evidence(
        declaration,
        resolution,
    )?))
}

/// Fold every resolved reference's root epoch into one canonical epoch set.
///
/// Roots reached through `file_location` and `existing_artifact` entries carry
/// their epoch on the resolved location, so one resolution yields the complete
/// set; a disagreement between references to the same root is corruption.
fn owner_access_evidence(
    declaration: &ArtifactAccessDeclaration,
    resolution: &AccessResolution,
) -> Result<OwnerAccessEvidence, VoomError> {
    let mut epoch_by_root: std::collections::BTreeMap<u64, u64> = std::collections::BTreeMap::new();
    let mut record = |root_id: u64, epoch: i64| -> Result<(), VoomError> {
        let epoch = u64::try_from(epoch).map_err(|_| {
            VoomError::database("artifact access resolution returned a negative root epoch")
        })?;
        match epoch_by_root.get(&root_id) {
            Some(existing) if *existing != epoch => Err(VoomError::database(format!(
                "artifact access resolution disagreed on root {root_id} epoch"
            ))),
            _ => {
                epoch_by_root.insert(root_id, epoch);
                Ok(())
            }
        }
    };
    for root in &resolution.resolved_roots {
        record(root.storage_root_id.0, root.root_epoch)?;
    }
    for location in &resolution.resolved_locations {
        record(location.storage_root_id.0, location.root_epoch)?;
    }
    let root_epochs = epoch_by_root
        .into_iter()
        .map(|(storage_root_id, root_epoch)| RootEpoch {
            storage_root_id: StorageRootId(storage_root_id),
            root_epoch,
        })
        .collect();
    OwnerAccessEvidence::new(declaration.clone(), root_epochs)
        .map_err(|err| VoomError::Internal(format!("owner access evidence rejected: {err}")))
}

/// The durable plan record for a selected lease: full owner-local proof when
/// the ticket declared byte work, the explicit absent pair otherwise.
fn artifact_plan_input(
    input: &RemoteAcquireInput,
    ticket: &Ticket,
    eligibility: &WorkerOperationEligibility,
    locality: Option<&(ArtifactAccessDeclaration, AccessResolution)>,
    lease_id: LeaseId,
    now: time::OffsetDateTime,
) -> Result<NewArtifactAccessPlan, VoomError> {
    let (owner_node_id, access_evidence) = match locality {
        Some((declaration, resolution)) => (
            Some(NodeId(u64::try_from(resolution.owner_node_id).map_err(
                |_| VoomError::database("artifact access resolution owner node id overflow"),
            )?)),
            Some(owner_access_evidence(declaration, resolution)?),
        ),
        None => (None, None),
    };
    Ok(NewArtifactAccessPlan {
        lease_id,
        ticket_id: ticket.id,
        worker_id: input.worker_id,
        node_id: input.node_id,
        owner_node_id,
        access_evidence,
        evidence: json!({
            "selected_by": "remote_acquire",
            "route": ROUTE_ACQUIRE,
            "advertised_artifact_access": eligibility.artifact_access,
        }),
        now,
    })
}

/// Durable rejection decision for one gated ticket: stable reason on the
/// failing reference, suppressed by ticket + locality identity (ADR 0071).
fn gate_rejection_decision(
    input: &RemoteAcquireInput,
    ticket: &Ticket,
    evidence: &AccessRejectionEvidence,
    fingerprint: &str,
    now: OffsetDateTime,
) -> NewSchedulerDecision {
    let bucket = input.lease_ttl_seconds.max(1) / 30;
    NewSchedulerDecision {
        decision_kind: SchedulerDecisionKind::NoCandidate,
        request_source: SchedulerRequestSource::RemoteAcquire,
        idempotency_key: Some(input.idempotency_key.clone()),
        request_node_id: Some(input.node_id),
        request_worker_id: Some(input.worker_id),
        ticket_id: Some(ticket.id),
        selected_worker_id: None,
        selected_node_id: None,
        selected_lease_id: None,
        outcome: SchedulerDecisionOutcome::NoEligibleCandidate,
        reason_code: StoreSchedulerReasonCode::UnsupportedArtifactAccess,
        summary: "no eligible candidate: unsupported_artifact_access".to_owned(),
        candidate_count: 1,
        selected_score: None,
        access_evidence: Some(DecisionAccessEvidence::Rejected(evidence.clone())),
        suppression_key: Some(format!(
            "remote_acquire:node:{}:worker:{}:ticket:{}:reason:unsupported_artifact_access:\
             refs:{}:bucket:{}",
            input.node_id, input.worker_id, ticket.id.0, fingerprint, bucket
        )),
        explanation: json!({
            "scoring_version": SCORING_VERSION,
            "outcome": "no_eligible_candidate",
            "reason": "unsupported_artifact_access",
            "rejected_references": evidence
                .references
                .iter()
                .map(|reference| json!({ "reason": reference.reason.as_str() }))
                .collect::<Vec<_>>(),
        }),
        now,
    }
}

/// Map a structured changed-gate outcome onto its documented stable reason.
pub(super) fn outcome_reason_code(outcome: &LeaseAcquireOutcome) -> StoreSchedulerReasonCode {
    match outcome {
        LeaseAcquireOutcome::TicketNotReady { .. } => StoreSchedulerReasonCode::NoReadyTicket,
        LeaseAcquireOutcome::WorkerIneligible { reason, .. } => match reason {
            LeaseIneligibilityReason::WorkerNotReady
            | LeaseIneligibilityReason::WorkerStale
            | LeaseIneligibilityReason::WorkerRetired => {
                StoreSchedulerReasonCode::WorkerNotExecutable
            }
            LeaseIneligibilityReason::OperationDenied => StoreSchedulerReasonCode::OperationDenied,
            LeaseIneligibilityReason::MissingCapability => {
                StoreSchedulerReasonCode::MissingCapability
            }
            LeaseIneligibilityReason::MissingGrant => StoreSchedulerReasonCode::MissingGrant,
            LeaseIneligibilityReason::WorkerMissing => {
                StoreSchedulerReasonCode::NoEligibleCandidate
            }
        },
        LeaseAcquireOutcome::CapacityFull(_) => StoreSchedulerReasonCode::WorkerCapacityFull,
        LeaseAcquireOutcome::Acquired(_) => StoreSchedulerReasonCode::Selected,
    }
}

/// Locator-free observed facts for one changed post-selection gate.
pub(super) fn changed_gate_explanation(
    outcome: &LeaseAcquireOutcome,
    reason_code: StoreSchedulerReasonCode,
) -> JsonValue {
    let mut explanation = json!({
        "scoring_version": SCORING_VERSION,
        "outcome": "no_eligible_candidate",
        "reason": reason_code.as_str(),
    });
    let Some(object) = explanation.as_object_mut() else {
        return explanation;
    };
    match outcome {
        LeaseAcquireOutcome::TicketNotReady { ticket_id } => {
            object.insert("selected_ticket_id".to_owned(), json!(ticket_id.0));
        }
        LeaseAcquireOutcome::WorkerIneligible { operation, .. } => {
            object.insert("operation".to_owned(), json!(operation.as_str()));
        }
        LeaseAcquireOutcome::CapacityFull(saturation) => {
            object.insert("operation".to_owned(), json!(saturation.operation.as_str()));
            object.insert(
                "observed".to_owned(),
                json!({
                    "active_leases": saturation.active_leases,
                    "limit": saturation.max_parallel
                }),
            );
        }
        LeaseAcquireOutcome::Acquired(_) => {}
    }
    explanation
}

/// Durable no-candidate record for one changed post-selection gate: it names
/// the selected ticket, and its suppression key carries the matching ticket
/// segment so key and row agree (ADR 0072).
fn changed_gate_decision(
    input: &RemoteAcquireInput,
    ticket: &Ticket,
    reason_code: StoreSchedulerReasonCode,
    explanation: JsonValue,
    now: OffsetDateTime,
) -> NewSchedulerDecision {
    NewSchedulerDecision {
        decision_kind: SchedulerDecisionKind::NoCandidate,
        request_source: SchedulerRequestSource::RemoteAcquire,
        idempotency_key: Some(input.idempotency_key.clone()),
        request_node_id: Some(input.node_id),
        request_worker_id: Some(input.worker_id),
        ticket_id: Some(ticket.id),
        selected_worker_id: None,
        selected_node_id: None,
        selected_lease_id: None,
        outcome: SchedulerDecisionOutcome::NoEligibleCandidate,
        reason_code,
        summary: format!("no eligible candidate: {}", reason_code.as_str()),
        candidate_count: 1,
        selected_score: None,
        access_evidence: None,
        suppression_key: Some(remote_acquire_suppression_key(
            input,
            reason_code.as_str(),
            ticket.kind.as_str(),
            Some(ticket.id),
        )),
        explanation,
        now,
    }
}

fn heartbeat_after_seconds(ttl_seconds: i64) -> i64 {
    (ttl_seconds / 2).max(1)
}

pub(super) fn remote_plan(plan: &ArtifactAccessPlan) -> RemoteArtifactAccessPlan {
    RemoteArtifactAccessPlan {
        id: plan.id,
        owner_node_id: plan.owner_node_id.map(|id| id.0),
        access_evidence: plan.access_evidence.clone(),
    }
}

impl ControlPlane {
    /// Finish an acquire replay: decode the stored response, prove its evidence
    /// against durable rows, and return the original outcome.
    ///
    /// Decode failures keep the existing poison-repoint contract
    /// (`finish_replay_in_tx`). Semantic corruption — a decodable response whose
    /// identities disagree with the lease, plan, decision, or ticket rows — is a
    /// database error and never repoints the row: the stored response is the only
    /// surviving copy of the original outcome (ADR 0073).
    pub(super) async fn finish_acquire_replay_in_tx(
        &self,
        mut tx: Transaction<'_, Sqlite>,
        input: &RemoteAcquireInput,
        replay: RemoteMutationReplay,
    ) -> Result<RemoteAcquireOutcome, VoomError> {
        let slot = input.replay_slot();
        let data = match &replay {
            RemoteMutationReplay::Error { .. } => {
                return self
                    .finish_replay_in_tx(tx, slot, replay, decode_acquire_replay)
                    .await;
            }
            RemoteMutationReplay::Ok { data } => data.clone(),
        };
        let outcome = match decode_acquire_replay(data) {
            Ok(outcome) => outcome,
            Err(decode_error) => {
                // Unreadable stored result: keep the poison-repoint behavior.
                return self
                    .finish_replay_in_tx(tx, slot, replay, |_| Err(decode_error))
                    .await;
            }
        };
        Self::validate_acquire_replay_evidence_in_tx(&mut tx, self, input, &outcome).await?;
        commit_tx(tx).await?;
        Ok(outcome)
    }

    fn require_replay_id(id: u64, label: &str) -> Result<(), VoomError> {
        if id == 0 {
            Err(VoomError::database(format!(
                "acquire replay evidence: zero {label}"
            )))
        } else {
            Ok(())
        }
    }

    fn replay_mismatch(detail: &str) -> VoomError {
        VoomError::database(format!(
            "acquire replay evidence disagrees with durable rows: {detail}"
        ))
    }

    /// Canonical JSON text of typed evidence; both sides serialize through the
    /// same validating types, so text equality is content equality.
    fn evidence_fingerprint(evidence: Option<&OwnerAccessEvidence>) -> Result<String, VoomError> {
        evidence
            .map(serde_json::to_string)
            .transpose()
            .map(Option::unwrap_or_default)
            .map_err(|e| VoomError::database(format!("acquire replay evidence: {e}")))
    }

    async fn validate_acquire_replay_evidence_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        plane: &ControlPlane,
        input: &RemoteAcquireInput,
        outcome: &RemoteAcquireOutcome,
    ) -> Result<(), VoomError> {
        match outcome {
            RemoteAcquireOutcome::Idle {
                worker_id,
                scheduler_decision_id,
            } => {
                Self::validate_non_selected_decision_replay_in_tx(
                    tx,
                    plane,
                    *worker_id,
                    *scheduler_decision_id,
                    SchedulerDecisionKind::Idle,
                    SchedulerDecisionOutcome::Idle,
                )
                .await
            }
            RemoteAcquireOutcome::NoCandidate {
                worker_id,
                scheduler_decision_id,
            } => {
                Self::validate_non_selected_decision_replay_in_tx(
                    tx,
                    plane,
                    *worker_id,
                    *scheduler_decision_id,
                    SchedulerDecisionKind::NoCandidate,
                    SchedulerDecisionOutcome::NoEligibleCandidate,
                )
                .await
            }
            RemoteAcquireOutcome::Leased(dispatch) => {
                Self::validate_leased_replay_evidence_in_tx(tx, plane, input, dispatch).await
            }
        }
    }

    /// An idle or no-candidate replay must name the decision that produced it.
    async fn validate_non_selected_decision_replay_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        plane: &ControlPlane,
        worker_id: WorkerId,
        scheduler_decision_id: u64,
        kind: SchedulerDecisionKind,
        expected_outcome: SchedulerDecisionOutcome,
    ) -> Result<(), VoomError> {
        Self::require_replay_id(scheduler_decision_id, "scheduler decision id")?;
        let decision = plane
            .scheduler_decisions
            .get_in_tx(tx, scheduler_decision_id)
            .await?
            .ok_or_else(|| {
                Self::replay_mismatch(&format!(
                    "scheduler decision {scheduler_decision_id} is missing"
                ))
            })?;
        if decision.decision_kind != kind
            || decision.outcome != expected_outcome
            || decision.request_worker_id != Some(worker_id)
        {
            return Err(Self::replay_mismatch(&format!(
                "scheduler decision {scheduler_decision_id} does not describe this outcome"
            )));
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "each identity binding of the stored acquisition is one explicit check"
    )]
    async fn validate_leased_replay_evidence_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        plane: &ControlPlane,
        input: &RemoteAcquireInput,
        dispatch: &RemoteLeaseDispatch,
    ) -> Result<(), VoomError> {
        Self::require_replay_id(dispatch.lease_id.0, "lease id")?;
        Self::require_replay_id(dispatch.scheduler_decision_id, "scheduler decision id")?;
        Self::require_replay_id(dispatch.ticket_id.0, "ticket id")?;
        Self::require_replay_id(dispatch.worker_id.0, "worker id")?;
        Self::require_replay_id(dispatch.artifact_access_plan.id, "access plan id")?;
        if dispatch.artifact_access_plan.owner_node_id == Some(0) {
            return Err(Self::replay_mismatch("zero owner node id"));
        }

        // Identity only: lease state and plan status legitimately change after
        // terminal processing, so replay must not depend on them (ADR 0073).
        let lease = plane
            .leases
            .get_in_tx(tx, dispatch.lease_id)
            .await?
            .ok_or_else(|| {
                Self::replay_mismatch(&format!("lease {} is missing", dispatch.lease_id.0))
            })?;
        if lease.ticket_id != dispatch.ticket_id || lease.worker_id != dispatch.worker_id {
            return Err(Self::replay_mismatch(&format!(
                "lease {} binds ticket {:?}/worker {:?}, not ticket {:?}/worker {:?}",
                dispatch.lease_id.0,
                lease.ticket_id,
                lease.worker_id,
                dispatch.ticket_id,
                dispatch.worker_id
            )));
        }

        let plan = plane
            .artifact_access_plans
            .get_by_lease_in_tx(tx, dispatch.lease_id)
            .await?
            .ok_or_else(|| {
                Self::replay_mismatch(&format!(
                    "access plan for lease {} is missing",
                    dispatch.lease_id.0
                ))
            })?;
        if plan.id != dispatch.artifact_access_plan.id {
            return Err(Self::replay_mismatch(&format!(
                "lease {} is bound to plan {}, not plan {}",
                dispatch.lease_id.0, plan.id, dispatch.artifact_access_plan.id
            )));
        }
        if plan.ticket_id != dispatch.ticket_id
            || plan.worker_id != dispatch.worker_id
            || plan.node_id != input.node_id
        {
            return Err(Self::replay_mismatch(
                "access plan bindings disagree with the dispatch",
            ));
        }
        if plan.owner_node_id.map(|id| id.0) != dispatch.artifact_access_plan.owner_node_id {
            return Err(Self::replay_mismatch(
                "access plan owner disagrees with the dispatch",
            ));
        }
        if Self::evidence_fingerprint(plan.access_evidence.as_ref())?
            != Self::evidence_fingerprint(dispatch.artifact_access_plan.access_evidence.as_ref())?
        {
            return Err(Self::replay_mismatch(
                "access plan evidence disagrees with the dispatch",
            ));
        }

        let decision = plane
            .scheduler_decisions
            .get_in_tx(tx, dispatch.scheduler_decision_id)
            .await?
            .ok_or_else(|| {
                Self::replay_mismatch(&format!(
                    "scheduler decision {} is missing",
                    dispatch.scheduler_decision_id
                ))
            })?;
        if decision.decision_kind != SchedulerDecisionKind::LeaseAcquire
            || decision.outcome != SchedulerDecisionOutcome::Selected
            || decision.selected_lease_id != Some(dispatch.lease_id)
            || decision.request_source != SchedulerRequestSource::RemoteAcquire
            || decision.request_worker_id != Some(dispatch.worker_id)
            || decision.request_node_id != Some(input.node_id)
        {
            return Err(Self::replay_mismatch(&format!(
                "scheduler decision {} does not select lease {}",
                dispatch.scheduler_decision_id, dispatch.lease_id.0
            )));
        }

        let ticket = plane
            .tickets
            .get_in_tx(tx, dispatch.ticket_id)
            .await?
            .ok_or_else(|| {
                Self::replay_mismatch(&format!("ticket {} is missing", dispatch.ticket_id.0))
            })?;
        if ticket.kind.normalize().matching_token().into_string() != dispatch.operation {
            return Err(Self::replay_mismatch(
                "dispatch operation disagrees with the ticket kind",
            ));
        }
        if ticket.payload != dispatch.dispatch_payload {
            return Err(Self::replay_mismatch(
                "dispatch payload disagrees with the ticket payload",
            ));
        }
        Ok(())
    }
}
