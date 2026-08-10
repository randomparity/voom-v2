use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, Transaction};
use time::{Duration, OffsetDateTime};
use voom_core::{
    FileLocationId, NodeId, NodeIncarnationId, ScanSessionId, ScanSessionStatus,
    ScanTerminalReason, StorageRootId, VoomError,
};
use voom_events::payload::{
    ScanObservationBatchAcceptedPayload, ScanSessionLifecyclePayload, ScanSessionSucceededPayload,
};
use voom_events::{Event, SubjectType};
use voom_store::repo::execution::remote_idempotency::{
    IdempotencyOutcome, RemoteIdempotencyInput, RemoteMutationReplay,
};
use voom_store::repo::library::library_roots::EffectiveLibraryRoot;
use voom_store::repo::scan::sessions::{
    CompleteScanSessionInput, NewScanObservationBatch, NewScanSession, ScanBatchOutcome,
    ScanCompletionRecord, is_completion_commit_lock_conflict,
};
pub use voom_store::repo::scan::sessions::{
    ScanObservation, ScanReconciliationEvidence, ScanReconciliationPage, ScanReconciliationQuery,
    ScanSession, ScanSessionListQuery, ScanSessionPage,
};

use crate::ControlPlane;
use crate::cases::execution::remote_execution::{
    ReplaySlot, decode_replay, incarnation_replay_key, is_remote_replayable_error,
    remote_error_message,
};
use crate::cases::{append_event, begin_immediate_tx, begin_tx, commit_tx};

const ROUTE_SCAN_START: &str = "POST /v1/scan/session/start";
const ROUTE_SCAN_BATCH: &str = "POST /v1/scan/session/batch";
const ROUTE_SCAN_COMPLETE: &str = "POST /v1/scan/session/complete";
const ROUTE_SCAN_FAIL: &str = "POST /v1/scan/session/fail";

#[derive(Debug, Clone)]
pub struct RemoteScanStartInput {
    pub node_id: NodeId,
    pub scan_session_id: ScanSessionId,
    pub incarnation_id: NodeIncarnationId,
    pub token: SecretString,
    pub idempotency_key: String,
    pub request_hash: String,
}

#[derive(Debug, Clone)]
pub struct RemoteScanBatchInput {
    pub node_id: NodeId,
    pub scan_session_id: ScanSessionId,
    pub incarnation_id: NodeIncarnationId,
    pub token: SecretString,
    pub idempotency_key: String,
    pub request_hash: String,
    pub sequence: u64,
    pub observations: Vec<ScanObservation>,
}

#[derive(Debug, Clone)]
pub struct RemoteScanFailInput {
    pub node_id: NodeId,
    pub scan_session_id: ScanSessionId,
    pub incarnation_id: NodeIncarnationId,
    pub token: SecretString,
    pub idempotency_key: String,
    pub request_hash: String,
    pub reason: ScanTerminalReason,
}

#[derive(Debug, Clone)]
pub struct RemoteScanCompleteInput {
    pub node_id: NodeId,
    pub scan_session_id: ScanSessionId,
    pub incarnation_id: NodeIncarnationId,
    pub token: SecretString,
    pub idempotency_key: String,
    pub request_hash: String,
    pub last_sequence: Option<u64>,
    pub observation_count: u64,
}

#[derive(Debug, Clone)]
pub struct RemoteScanInspectInput {
    pub node_id: NodeId,
    pub scan_session_id: ScanSessionId,
    pub incarnation_id: NodeIncarnationId,
    pub token: SecretString,
}

#[derive(Debug, Clone)]
pub struct RemoteScanReconciliationInput {
    pub auth: RemoteScanInspectInput,
    pub after_id: Option<FileLocationId>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteScanStartOutcome {
    pub scan_session_id: ScanSessionId,
    pub status: ScanSessionStatus,
    pub owner_incarnation_id: NodeIncarnationId,
    pub location_high_watermark_id: Option<FileLocationId>,
    pub progress_deadline_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteScanBatchOutcome {
    pub scan_session_id: ScanSessionId,
    pub sequence: u64,
    pub accepted_observation_count: u64,
    pub cumulative_observation_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteScanTerminalOutcome {
    pub scan_session_id: ScanSessionId,
    pub status: ScanSessionStatus,
    pub terminal_at: OffsetDateTime,
    pub terminal_reason: ScanTerminalReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteScanCompleteOutcome {
    pub scan_session_id: ScanSessionId,
    pub status: ScanSessionStatus,
    pub observation_count: u64,
    pub retired_location_count: u64,
}

impl ControlPlane {
    /// Request a durable scan session without scheduling work.
    pub async fn request_scan_session(
        &self,
        storage_root_id: StorageRootId,
        idle_timeout_seconds: u32,
    ) -> Result<ScanSession, VoomError> {
        validate_idle_timeout(idle_timeout_seconds)?;
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let now = self.clock().now();
        let expired = self.scan_sessions.stale_expired_in_tx(&mut tx, now).await?;
        for session in &expired {
            append_lifecycle_event(self, &mut tx, session, now).await?;
        }
        let effective = self
            .libraries
            .effective_library_root_in_tx(&mut tx, storage_root_id)
            .await?;
        let Some(effective) = effective else {
            commit_tx(tx).await?;
            return Err(VoomError::NotFound(format!(
                "library root {storage_root_id} not found"
            )));
        };
        if let Err(error) = require_root_available_for_request(&effective) {
            commit_tx(tx).await?;
            return Err(error);
        }
        let owner_node_id = effective.root.owner_node_id.ok_or_else(|| {
            VoomError::database(format!(
                "available storage root {storage_root_id} has no owner"
            ))
        })?;
        let session = self
            .scan_sessions
            .insert_requested_in_tx(
                &mut tx,
                NewScanSession {
                    storage_root_id,
                    root_epoch: effective.root.root_epoch,
                    owner_node_id,
                    idle_timeout_seconds,
                    progress_deadline_at: progress_deadline(now, idle_timeout_seconds)?,
                    requested_at: now,
                },
            )
            .await;
        let session = match session {
            Ok(session) => session,
            Err(error) if is_remote_replayable_error(&error) => {
                commit_tx(tx).await?;
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        append_lifecycle_event(self, &mut tx, &session, now).await?;
        commit_tx(tx).await?;
        Ok(session)
    }

    /// Authenticate and bind a requested session to the current owner incarnation.
    pub async fn start_scan_session(
        &self,
        input: RemoteScanStartInput,
    ) -> Result<RemoteScanStartOutcome, VoomError> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let now = self.clock().now();
        self.require_scan_authority_in_tx(&mut tx, &input.scan_auth())
            .await?;
        let replay = self
            .reserve_scan_replay_in_tx(&mut tx, &input.replay(ROUTE_SCAN_START), now)
            .await?;
        if let Some(replay) = replay {
            return self
                .finish_scan_replay(tx, &input.replay(ROUTE_SCAN_START), replay, "scan start")
                .await;
        }
        let session = match self
            .owned_scan_session_in_tx(&mut tx, input.scan_session_id, input.node_id)
            .await
        {
            Ok(session) => session,
            Err(error) => {
                return self
                    .finish_scan_error(tx, &input.replay(ROUTE_SCAN_START), error)
                    .await;
            }
        };
        if session.status != ScanSessionStatus::Requested {
            let error = VoomError::Conflict(format!(
                "scan session {} cannot start from {}",
                session.id,
                session.status.as_str()
            ));
            return self
                .finish_scan_error(tx, &input.replay(ROUTE_SCAN_START), error)
                .await;
        }
        if let Some(error) = self
            .stale_fence_error_in_tx(&mut tx, &session, None, now)
            .await?
        {
            return self
                .stale_remote_scan(tx, &input.replay(ROUTE_SCAN_START), session, error, now)
                .await;
        }
        let started = self
            .scan_sessions
            .start_in_tx(
                &mut tx,
                session.id,
                input.incarnation_id,
                progress_deadline(now, session.idle_timeout_seconds)?,
                now,
            )
            .await?;
        append_lifecycle_event(self, &mut tx, &started, now).await?;
        let outcome = start_outcome(&started)?;
        self.complete_scan_ok_in_tx(&mut tx, &input.replay(ROUTE_SCAN_START), &outcome)
            .await?;
        commit_tx(tx).await?;
        Ok(outcome)
    }

    /// Accept one ordered observation batch or return its exact accepted replay.
    pub async fn accept_scan_observation_batch(
        &self,
        input: RemoteScanBatchInput,
    ) -> Result<RemoteScanBatchOutcome, VoomError> {
        validate_batch_input(&input)?;
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let now = self.clock().now();
        self.require_scan_authority_in_tx(&mut tx, &input.scan_auth())
            .await?;
        let replay_identity = input.replay(ROUTE_SCAN_BATCH);
        let replay = self
            .reserve_scan_replay_in_tx(&mut tx, &replay_identity, now)
            .await?;
        if let Some(replay) = replay {
            return self
                .finish_scan_replay(tx, &replay_identity, replay, "scan batch")
                .await;
        }
        let session = match self
            .owned_scan_session_in_tx(&mut tx, input.scan_session_id, input.node_id)
            .await
        {
            Ok(session) => session,
            Err(error) => return self.finish_scan_error(tx, &replay_identity, error).await,
        };
        if input.sequence < session.next_sequence {
            return self
                .finish_batch_ledger_replay(tx, &input, replay_identity, now)
                .await;
        }
        self.accept_new_scan_batch(tx, input, replay_identity, session, now)
            .await
    }

    /// Mark a running scan failed without reconciling locations.
    pub async fn fail_scan_session(
        &self,
        input: RemoteScanFailInput,
    ) -> Result<RemoteScanTerminalOutcome, VoomError> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let now = self.clock().now();
        self.require_scan_authority_in_tx(&mut tx, &input.scan_auth())
            .await?;
        let replay_identity = input.replay(ROUTE_SCAN_FAIL);
        let replay = self
            .reserve_scan_replay_in_tx(&mut tx, &replay_identity, now)
            .await?;
        if let Some(replay) = replay {
            return self
                .finish_scan_replay(tx, &replay_identity, replay, "scan failure")
                .await;
        }
        let session = match self
            .owned_scan_session_in_tx(&mut tx, input.scan_session_id, input.node_id)
            .await
        {
            Ok(session) => session,
            Err(error) => return self.finish_scan_error(tx, &replay_identity, error).await,
        };
        if session.status != ScanSessionStatus::Running {
            let error = running_required(&session, "fail");
            return self.finish_scan_error(tx, &replay_identity, error).await;
        }
        if let Some(error) = self
            .stale_fence_error_in_tx(&mut tx, &session, Some(input.incarnation_id), now)
            .await?
        {
            return self
                .stale_remote_scan(tx, &replay_identity, session, error, now)
                .await;
        }
        let failed = self
            .scan_sessions
            .terminalize_in_tx(
                &mut tx,
                session.id,
                ScanSessionStatus::Failed,
                input.reason,
                now,
            )
            .await?;
        append_lifecycle_event(self, &mut tx, &failed, now).await?;
        let outcome = terminal_outcome(&failed)?;
        self.complete_scan_ok_in_tx(&mut tx, &replay_identity, &outcome)
            .await?;
        commit_tx(tx).await?;
        Ok(outcome)
    }

    /// Atomically succeed a complete traversal and reconcile absent locations.
    pub async fn complete_scan_session(
        &self,
        input: RemoteScanCompleteInput,
    ) -> Result<RemoteScanCompleteOutcome, VoomError> {
        validate_completion_input(&input)?;
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let now = self.clock().now();
        self.require_scan_authority_in_tx(&mut tx, &input.scan_auth())
            .await?;
        let replay_identity = input.replay(ROUTE_SCAN_COMPLETE);
        let replay = self
            .reserve_scan_replay_in_tx(&mut tx, &replay_identity, now)
            .await?;
        if let Some(replay) = replay {
            return self
                .finish_scan_replay(tx, &replay_identity, replay, "scan completion")
                .await;
        }
        let session = match self
            .owned_scan_session_in_tx(&mut tx, input.scan_session_id, input.node_id)
            .await
        {
            Ok(session) => session,
            Err(error) => return self.finish_scan_error(tx, &replay_identity, error).await,
        };
        let fence_error = self
            .stale_fence_error_in_tx(&mut tx, &session, Some(input.incarnation_id), now)
            .await?;
        if let Err(error) = require_running_completion(&session) {
            return self.finish_scan_error(tx, &replay_identity, error).await;
        }
        if let Some(error) = fence_error {
            return self
                .stale_remote_scan(tx, &replay_identity, session, error, now)
                .await;
        }
        self.finish_new_scan_completion(tx, input, replay_identity, session, now)
            .await
    }

    /// Cancel an active session as a local operator action.
    pub async fn cancel_scan_session(
        &self,
        id: ScanSessionId,
        reason: ScanTerminalReason,
    ) -> Result<ScanSession, VoomError> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let now = self.clock().now();
        let session = self
            .scan_sessions
            .get_in_tx(&mut tx, id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("scan session {id} not found")))?;
        if !is_active_status(session.status) {
            return Err(VoomError::Conflict(format!(
                "scan session {id} cannot cancel from {}",
                session.status.as_str()
            )));
        }
        let fence_error = self
            .stale_fence_error_in_tx(&mut tx, &session, session.owner_incarnation_id, now)
            .await?;
        let fence_error = match fence_error {
            Some(error) => Some(error),
            None => {
                self.cancel_incarnation_fence_in_tx(&mut tx, &session)
                    .await?
            }
        };
        if let Some(error) = fence_error {
            let stale = self.stale_scan_in_tx(&mut tx, &session, now).await?;
            commit_tx(tx).await?;
            return Err(error_with_stale_context(error, stale.id));
        }
        let cancelled = self
            .scan_sessions
            .terminalize_in_tx(&mut tx, id, ScanSessionStatus::Cancelled, reason, now)
            .await?;
        append_lifecycle_event(self, &mut tx, &cancelled, now).await?;
        commit_tx(tx).await?;
        Ok(cancelled)
    }

    pub async fn scan_session(&self, id: ScanSessionId) -> Result<ScanSession, VoomError> {
        self.scan_sessions
            .get(id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("scan session {id} not found")))
    }

    pub async fn scan_sessions(
        &self,
        query: ScanSessionListQuery,
    ) -> Result<ScanSessionPage, VoomError> {
        self.scan_sessions.list(query).await
    }

    pub async fn scan_reconciliation(
        &self,
        query: ScanReconciliationQuery,
    ) -> Result<ScanReconciliationPage, VoomError> {
        self.scan_sessions.reconciliation_page(query).await
    }

    pub async fn inspect_remote_scan_session(
        &self,
        input: RemoteScanInspectInput,
    ) -> Result<ScanSession, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        self.require_scan_authority_in_tx(&mut tx, &input.scan_auth())
            .await?;
        let session = self
            .owned_scan_session_in_tx(&mut tx, input.scan_session_id, input.node_id)
            .await?;
        commit_tx(tx).await?;
        Ok(session)
    }

    pub async fn inspect_remote_scan_reconciliation(
        &self,
        input: RemoteScanReconciliationInput,
    ) -> Result<ScanReconciliationPage, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        self.require_scan_authority_in_tx(&mut tx, &input.auth.scan_auth())
            .await?;
        self.owned_scan_session_in_tx(&mut tx, input.auth.scan_session_id, input.auth.node_id)
            .await?;
        let page = self
            .scan_sessions
            .reconciliation_page_in_tx(
                &mut tx,
                ScanReconciliationQuery {
                    scan_session_id: input.auth.scan_session_id,
                    after_id: input.after_id,
                    limit: input.limit,
                },
            )
            .await?;
        commit_tx(tx).await?;
        Ok(page)
    }
}

impl ControlPlane {
    async fn finish_new_scan_completion(
        &self,
        mut tx: Transaction<'_, Sqlite>,
        input: RemoteScanCompleteInput,
        replay: ScanReplayIdentity,
        session: ScanSession,
        now: OffsetDateTime,
    ) -> Result<RemoteScanCompleteOutcome, VoomError> {
        let completion = self
            .scan_sessions
            .complete_in_tx(
                &mut tx,
                CompleteScanSessionInput {
                    scan_session_id: session.id,
                    expected_storage_root_id: session.storage_root_id,
                    expected_root_epoch: session.root_epoch,
                    expected_owner_node_id: session.owner_node_id,
                    expected_owner_incarnation_id: input.incarnation_id,
                    last_sequence: input.last_sequence,
                    observation_count: input.observation_count,
                    completed_at: now,
                },
            )
            .await;
        let completion = match completion {
            Ok(completion) => completion,
            Err(error) if is_completion_commit_lock_conflict(&error) => {
                return rollback_scan_error(tx, error).await;
            }
            Err(error) => return self.finish_scan_error(tx, &replay, error).await,
        };
        let outcome = completion_outcome(&completion)?;
        append_completion_event(self, &mut tx, &outcome, session.storage_root_id, now).await?;
        self.complete_scan_ok_in_tx(&mut tx, &replay, &outcome)
            .await?;
        commit_tx(tx).await?;
        Ok(outcome)
    }

    async fn accept_new_scan_batch(
        &self,
        mut tx: Transaction<'_, Sqlite>,
        input: RemoteScanBatchInput,
        replay: ScanReplayIdentity,
        session: ScanSession,
        now: OffsetDateTime,
    ) -> Result<RemoteScanBatchOutcome, VoomError> {
        if session.status != ScanSessionStatus::Running {
            return self
                .finish_scan_error(tx, &replay, running_required(&session, "accept batch"))
                .await;
        }
        if let Some(error) = self
            .stale_fence_error_in_tx(&mut tx, &session, Some(input.incarnation_id), now)
            .await?
        {
            return self
                .stale_remote_scan(tx, &replay, session, error, now)
                .await;
        }
        let outcome = self
            .scan_sessions
            .accepted_batch_in_tx(
                &mut tx,
                NewScanObservationBatch {
                    scan_session_id: input.scan_session_id,
                    sequence: input.sequence,
                    request_hash: input.request_hash,
                    observations: input.observations,
                    accepted_at: now,
                    next_progress_deadline_at: progress_deadline(
                        now,
                        session.idle_timeout_seconds,
                    )?,
                },
            )
            .await;
        let outcome = match outcome {
            Ok(outcome) => RemoteScanBatchOutcome::from(outcome),
            Err(error) => return self.finish_scan_error(tx, &replay, error).await,
        };
        append_batch_event(self, &mut tx, &outcome, now).await?;
        self.complete_scan_ok_in_tx(&mut tx, &replay, &outcome)
            .await?;
        commit_tx(tx).await?;
        Ok(outcome)
    }

    async fn finish_batch_ledger_replay(
        &self,
        mut tx: Transaction<'_, Sqlite>,
        input: &RemoteScanBatchInput,
        replay: ScanReplayIdentity,
        now: OffsetDateTime,
    ) -> Result<RemoteScanBatchOutcome, VoomError> {
        let outcome = self
            .scan_sessions
            .accepted_batch_in_tx(
                &mut tx,
                NewScanObservationBatch {
                    scan_session_id: input.scan_session_id,
                    sequence: input.sequence,
                    request_hash: input.request_hash.clone(),
                    observations: input.observations.clone(),
                    accepted_at: now,
                    next_progress_deadline_at: now,
                },
            )
            .await;
        let outcome = match outcome {
            Ok(outcome) => RemoteScanBatchOutcome::from(outcome),
            Err(error) => return self.finish_scan_error(tx, &replay, error).await,
        };
        self.complete_scan_ok_in_tx(&mut tx, &replay, &outcome)
            .await?;
        commit_tx(tx).await?;
        Ok(outcome)
    }

    async fn require_scan_authority_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        auth: &ScanAuth,
    ) -> Result<(), VoomError> {
        self.require_remote_incarnation_fence_in_tx(
            tx,
            auth.node_id,
            &auth.token,
            auth.incarnation_id,
            None,
        )
        .await?;
        Ok(())
    }

    async fn owned_scan_session_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: ScanSessionId,
        node_id: NodeId,
    ) -> Result<ScanSession, VoomError> {
        let session = self
            .scan_sessions
            .get_in_tx(tx, id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("scan session {id} not found")))?;
        if session.owner_node_id != node_id {
            return Err(VoomError::Conflict(format!(
                "scan session {id} is not owned by node {node_id}"
            )));
        }
        Ok(session)
    }

    async fn stale_fence_error_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        session: &ScanSession,
        incarnation_id: Option<NodeIncarnationId>,
        now: OffsetDateTime,
    ) -> Result<Option<VoomError>, VoomError> {
        let effective = self
            .libraries
            .effective_library_root_in_tx(tx, session.storage_root_id)
            .await?
            .ok_or_else(|| {
                VoomError::database(format!(
                    "scan session {} references missing storage root {}",
                    session.id, session.storage_root_id
                ))
            })?;
        Ok(scan_fence_error(session, &effective, incarnation_id, now))
    }

    async fn cancel_incarnation_fence_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        session: &ScanSession,
    ) -> Result<Option<VoomError>, VoomError> {
        let Some(incarnation_id) = session.owner_incarnation_id else {
            return Ok(None);
        };
        let result = self
            .nodes
            .require_active_incarnation_in_tx(tx, session.owner_node_id, incarnation_id)
            .await;
        match result {
            Ok(_) => Ok(None),
            Err(VoomError::Conflict(_)) => Ok(Some(VoomError::Conflict(format!(
                "scan session {} is stale because owner incarnation changed",
                session.id
            )))),
            Err(error) => Err(error),
        }
    }

    async fn stale_scan_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        session: &ScanSession,
        now: OffsetDateTime,
    ) -> Result<ScanSession, VoomError> {
        let stale = self
            .scan_sessions
            .terminalize_in_tx(
                tx,
                session.id,
                ScanSessionStatus::Stale,
                stale_reason(session, now)?,
                now,
            )
            .await?;
        append_lifecycle_event(self, tx, &stale, now).await?;
        Ok(stale)
    }

    async fn stale_remote_scan<T>(
        &self,
        mut tx: Transaction<'_, Sqlite>,
        replay: &ScanReplayIdentity,
        session: ScanSession,
        error: VoomError,
        now: OffsetDateTime,
    ) -> Result<T, VoomError> {
        let stale = self.stale_scan_in_tx(&mut tx, &session, now).await?;
        self.finish_scan_error(tx, replay, error_with_stale_context(error, stale.id))
            .await
    }

    async fn reserve_scan_replay_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        replay: &ScanReplayIdentity,
        now: OffsetDateTime,
    ) -> Result<Option<RemoteMutationReplay>, VoomError> {
        let outcome = self
            .remote_idempotency
            .reserve_or_replay_in_tx(
                tx,
                RemoteIdempotencyInput {
                    node_id: replay.node_id,
                    route_key: replay.route_key.to_owned(),
                    worker_id: None,
                    idempotency_key: replay.key.clone(),
                    request_hash: replay.request_hash.clone(),
                    created_at: now,
                },
            )
            .await?;
        match outcome {
            IdempotencyOutcome::Reserved => Ok(None),
            IdempotencyOutcome::Replay(replay) => Ok(Some(replay)),
        }
    }

    async fn complete_scan_ok_in_tx<T>(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        replay: &ScanReplayIdentity,
        outcome: &T,
    ) -> Result<(), VoomError>
    where
        T: Serialize,
    {
        let data = serde_json::to_value(outcome)
            .map_err(|error| VoomError::database_context("serialize scan replay outcome", error))?;
        self.remote_idempotency
            .complete_in_tx(
                tx,
                replay.node_id,
                replay.route_key,
                None,
                &replay.key,
                RemoteMutationReplay::Ok { data },
            )
            .await
    }

    async fn finish_scan_replay<T>(
        &self,
        tx: Transaction<'_, Sqlite>,
        replay: &ScanReplayIdentity,
        stored: RemoteMutationReplay,
        label: &str,
    ) -> Result<T, VoomError>
    where
        T: serde::de::DeserializeOwned,
    {
        self.finish_replay_in_tx(tx, replay.slot(), stored, |data| decode_replay(data, label))
            .await
    }

    async fn finish_scan_error<T>(
        &self,
        mut tx: Transaction<'_, Sqlite>,
        replay: &ScanReplayIdentity,
        error: VoomError,
    ) -> Result<T, VoomError> {
        if is_remote_replayable_error(&error) {
            self.remote_idempotency
                .complete_in_tx(
                    &mut tx,
                    replay.node_id,
                    replay.route_key,
                    None,
                    &replay.key,
                    RemoteMutationReplay::Error {
                        code: error.code().to_owned(),
                        message: remote_error_message(&error),
                    },
                )
                .await?;
            commit_tx(tx).await?;
        }
        Err(error)
    }
}

async fn rollback_scan_error<T>(
    tx: Transaction<'_, Sqlite>,
    error: VoomError,
) -> Result<T, VoomError> {
    match tx.rollback().await {
        Ok(()) => Err(error),
        Err(rollback_error) => Err(VoomError::database(format!(
            "scan completion failed: {error}; rollback also failed: {rollback_error}"
        ))),
    }
}

#[derive(Debug, Clone)]
struct ScanAuth {
    node_id: NodeId,
    incarnation_id: NodeIncarnationId,
    token: SecretString,
}

struct ScanReplayIdentity {
    node_id: NodeId,
    route_key: &'static str,
    key: String,
    request_hash: String,
}

impl ScanReplayIdentity {
    fn slot(&self) -> ReplaySlot {
        ReplaySlot {
            node_id: self.node_id,
            route_key: self.route_key.to_owned(),
            worker_id: None,
            idempotency_key: self.key.clone(),
        }
    }
}

macro_rules! remote_scan_input {
    ($type:ty) => {
        impl $type {
            fn scan_auth(&self) -> ScanAuth {
                ScanAuth {
                    node_id: self.node_id,
                    incarnation_id: self.incarnation_id,
                    token: self.token.clone(),
                }
            }

            fn replay(&self, route_key: &'static str) -> ScanReplayIdentity {
                ScanReplayIdentity {
                    node_id: self.node_id,
                    route_key,
                    key: incarnation_replay_key(self.incarnation_id, &self.idempotency_key),
                    request_hash: self.request_hash.clone(),
                }
            }
        }
    };
}

remote_scan_input!(RemoteScanStartInput);
remote_scan_input!(RemoteScanBatchInput);
remote_scan_input!(RemoteScanCompleteInput);
remote_scan_input!(RemoteScanFailInput);

impl RemoteScanInspectInput {
    fn scan_auth(&self) -> ScanAuth {
        ScanAuth {
            node_id: self.node_id,
            incarnation_id: self.incarnation_id,
            token: self.token.clone(),
        }
    }
}

impl From<ScanBatchOutcome> for RemoteScanBatchOutcome {
    fn from(outcome: ScanBatchOutcome) -> Self {
        Self {
            scan_session_id: outcome.scan_session_id,
            sequence: outcome.sequence,
            accepted_observation_count: outcome.accepted_observation_count,
            cumulative_observation_count: outcome.cumulative_observation_count,
        }
    }
}

fn scan_fence_error(
    session: &ScanSession,
    effective: &EffectiveLibraryRoot,
    incarnation_id: Option<NodeIncarnationId>,
    now: OffsetDateTime,
) -> Option<VoomError> {
    let detail = if now >= session.progress_deadline_at {
        Some("progress deadline expired".to_owned())
    } else if effective.root.root_epoch != session.root_epoch {
        Some("storage root epoch changed".to_owned())
    } else if effective.root.owner_node_id != Some(session.owner_node_id) {
        Some("storage root owner changed".to_owned())
    } else if !effective.available {
        Some(format!(
            "storage root unavailable: {}",
            effective.reason.as_str()
        ))
    } else if incarnation_id.is_some() && session.owner_incarnation_id != incarnation_id {
        Some("owner incarnation changed".to_owned())
    } else {
        None
    };
    detail.map(|detail| {
        VoomError::Conflict(format!(
            "scan session {} is stale because {detail}",
            session.id
        ))
    })
}

fn stale_reason(
    session: &ScanSession,
    now: OffsetDateTime,
) -> Result<ScanTerminalReason, VoomError> {
    let message = if now >= session.progress_deadline_at {
        "scan session progress deadline expired"
    } else {
        "scan session authority or storage root fence changed"
    };
    ScanTerminalReason::new(message)
}

fn error_with_stale_context(error: VoomError, id: ScanSessionId) -> VoomError {
    match error {
        VoomError::Conflict(message) => {
            VoomError::Conflict(format!("{message}; scan session {id} was marked stale"))
        }
        other => other,
    }
}

fn start_outcome(session: &ScanSession) -> Result<RemoteScanStartOutcome, VoomError> {
    let owner_incarnation_id = session.owner_incarnation_id.ok_or_else(|| {
        VoomError::database(format!(
            "started scan session {} has no incarnation",
            session.id
        ))
    })?;
    Ok(RemoteScanStartOutcome {
        scan_session_id: session.id,
        status: session.status,
        owner_incarnation_id,
        location_high_watermark_id: session.location_high_watermark_id,
        progress_deadline_at: session.progress_deadline_at,
    })
}

fn terminal_outcome(session: &ScanSession) -> Result<RemoteScanTerminalOutcome, VoomError> {
    let terminal_at = session.terminal_at.ok_or_else(|| {
        VoomError::database(format!(
            "terminal scan session {} has no timestamp",
            session.id
        ))
    })?;
    let terminal_reason = session.terminal_reason.clone().ok_or_else(|| {
        VoomError::database(format!(
            "terminal scan session {} has no reason",
            session.id
        ))
    })?;
    Ok(RemoteScanTerminalOutcome {
        scan_session_id: session.id,
        status: session.status,
        terminal_at,
        terminal_reason,
    })
}

fn completion_outcome(
    completion: &ScanCompletionRecord,
) -> Result<RemoteScanCompleteOutcome, VoomError> {
    let session = &completion.session;
    if session.status != ScanSessionStatus::Succeeded {
        return Err(VoomError::database(format!(
            "scan completion returned session {} in {}",
            session.id,
            session.status.as_str()
        )));
    }
    let retired_location_count = u64::try_from(completion.retired_location_ids.len())
        .map_err(|error| VoomError::database_context("scan completion retired count", error))?;
    if retired_location_count != session.retired_location_count {
        return Err(VoomError::database(format!(
            "scan completion session {} count does not match returned locations",
            session.id
        )));
    }
    Ok(RemoteScanCompleteOutcome {
        scan_session_id: session.id,
        status: session.status,
        observation_count: session.observation_count,
        retired_location_count,
    })
}

async fn append_lifecycle_event(
    control_plane: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    session: &ScanSession,
    now: OffsetDateTime,
) -> Result<(), VoomError> {
    let payload = ScanSessionLifecyclePayload {
        scan_session_id: session.id,
        storage_root_id: session.storage_root_id,
        status: session.status,
    };
    let event = match session.status {
        ScanSessionStatus::Requested => Event::ScanSessionRequested(payload),
        ScanSessionStatus::Running => Event::ScanSessionStarted(payload),
        ScanSessionStatus::Failed => Event::ScanSessionFailed(payload),
        ScanSessionStatus::Cancelled => Event::ScanSessionCancelled(payload),
        ScanSessionStatus::Stale => Event::ScanSessionStale(payload),
        ScanSessionStatus::Succeeded => {
            return Err(VoomError::Internal(
                "scan success uses its summary event".to_owned(),
            ));
        }
    };
    append_event(
        &control_plane.events,
        tx,
        SubjectType::ScanSession,
        Some(session.id.0),
        now,
        event,
    )
    .await
}

async fn append_batch_event(
    control_plane: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    outcome: &RemoteScanBatchOutcome,
    now: OffsetDateTime,
) -> Result<(), VoomError> {
    append_event(
        &control_plane.events,
        tx,
        SubjectType::ScanSession,
        Some(outcome.scan_session_id.0),
        now,
        Event::ScanObservationBatchAccepted(ScanObservationBatchAcceptedPayload {
            scan_session_id: outcome.scan_session_id,
            sequence: outcome.sequence,
            batch_observation_count: outcome.accepted_observation_count,
            cumulative_observation_count: outcome.cumulative_observation_count,
        }),
    )
    .await
}

async fn append_completion_event(
    control_plane: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    outcome: &RemoteScanCompleteOutcome,
    storage_root_id: StorageRootId,
    now: OffsetDateTime,
) -> Result<(), VoomError> {
    append_event(
        &control_plane.events,
        tx,
        SubjectType::ScanSession,
        Some(outcome.scan_session_id.0),
        now,
        Event::ScanSessionSucceeded(ScanSessionSucceededPayload {
            scan_session_id: outcome.scan_session_id,
            storage_root_id,
            observation_count: outcome.observation_count,
            retired_location_count: outcome.retired_location_count,
        }),
    )
    .await
}

fn require_root_available_for_request(root: &EffectiveLibraryRoot) -> Result<(), VoomError> {
    if root.available {
        Ok(())
    } else {
        Err(VoomError::Config(format!(
            "storage root {} unavailable: {}",
            root.root.id,
            root.reason.as_str()
        )))
    }
}

fn validate_idle_timeout(seconds: u32) -> Result<(), VoomError> {
    if (1..=86_400).contains(&seconds) {
        Ok(())
    } else {
        Err(VoomError::Config(format!(
            "scan session idle timeout {seconds} outside 1..=86400"
        )))
    }
}

fn validate_batch_input(input: &RemoteScanBatchInput) -> Result<(), VoomError> {
    if !(1..=1_000).contains(&input.observations.len()) {
        return Err(VoomError::Config(format!(
            "scan session batch observation count {} outside 1..=1000",
            input.observations.len()
        )));
    }
    require_storage_u64(input.sequence, "scan batch sequence")?;
    if !is_lowercase_sha256(&input.request_hash) {
        return Err(VoomError::Config(
            "scan session batch request hash must be lowercase SHA-256".to_owned(),
        ));
    }
    for observation in &input.observations {
        validate_observation(observation)?;
    }
    Ok(())
}

fn validate_completion_input(input: &RemoteScanCompleteInput) -> Result<(), VoomError> {
    if let Some(last_sequence) = input.last_sequence {
        require_storage_u64(last_sequence, "scan completion last sequence")?;
    }
    require_storage_u64(input.observation_count, "scan completion observation count")
}

fn validate_observation(observation: &ScanObservation) -> Result<(), VoomError> {
    let identity = &observation.provider_object_identity;
    if identity.is_empty() || identity.len() > 4_096 || identity.as_bytes().contains(&0) {
        return Err(VoomError::Config(
            "scan observation object identity must be 1..=4096 bytes without NUL".to_owned(),
        ));
    }
    require_storage_u64(observation.size_bytes, "scan observation size")?;
    if observation.stability_confirmed_at < observation.stability_started_at {
        return Err(VoomError::Config(
            "scan observation stability confirmation precedes start".to_owned(),
        ));
    }
    Ok(())
}

fn require_storage_u64(value: u64, field: &str) -> Result<(), VoomError> {
    i64::try_from(value)
        .map(|_| ())
        .map_err(|error| VoomError::Config(format!("{field} {value} exceeds storage: {error}")))
}

fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn progress_deadline(now: OffsetDateTime, seconds: u32) -> Result<OffsetDateTime, VoomError> {
    now.checked_add(Duration::seconds(i64::from(seconds)))
        .ok_or_else(|| {
            VoomError::Config("scan session progress deadline overflows time".to_owned())
        })
}

fn running_required(session: &ScanSession, operation: &str) -> VoomError {
    VoomError::Conflict(format!(
        "scan session {} cannot {operation} from {}",
        session.id,
        session.status.as_str()
    ))
}

fn require_running_completion(session: &ScanSession) -> Result<(), VoomError> {
    if session.status == ScanSessionStatus::Running {
        Ok(())
    } else {
        Err(running_required(session, "complete"))
    }
}

fn is_active_status(status: ScanSessionStatus) -> bool {
    match status {
        ScanSessionStatus::Requested | ScanSessionStatus::Running => true,
        ScanSessionStatus::Succeeded
        | ScanSessionStatus::Failed
        | ScanSessionStatus::Cancelled
        | ScanSessionStatus::Stale => false,
    }
}

#[cfg(test)]
#[path = "sessions_test.rs"]
mod tests;
