//! Fenced node-local commit intent cases (ADR 0074): authorize, receipts, and
//! completion for the storage-owner node. Each case follows the
//! `remote_execution` pattern (`cases/execution/remote_execution/*`):
//! incarnation-fenced authentication, reserve-or-replay idempotency, guarded
//! mutation inside one transaction, and a stored replay outcome. Fence values
//! travel only in the authorize outcome payload — never in events.

use constant_time_eq::constant_time_eq;
use secrecy::SecretString;
use serde_json::Value as JsonValue;
use sqlx::Sqlite;
use voom_artifact::commit_pipeline::{
    RecoveryRequiredCommit, mark_recovery_required_with_event_in_tx,
};
use voom_core::ids::{ArtifactCommitIntentId, ArtifactHandleId};
use voom_core::{ErrorCode, FailureClass, NodeId, NodeIncarnationId, VoomError};
use voom_events::Event;
use voom_events::payload::{
    ArtifactCommitIntentAuthorizedPayload, ArtifactCommitReceiptReportedPayload,
};
use voom_store::repo::execution::remote_idempotency::{
    IdempotencyOutcome, RemoteIdempotencyInput, RemoteMutationReplay,
};
use voom_store::repo::media::artifact_commit_intents::{
    AppliedReceipt, ApplyingReceipt, ArtifactCommitIntent, ArtifactCommitIntentState,
    CommitExpectedFacts, CommitObservedFacts, CommitReceipt, MismatchedReceipt,
    OutcomeUnknownReceipt,
};
use voom_store::repo::media::artifacts::{ArtifactCommitFailure, ArtifactCommitRecord};

use crate::ControlPlane;
use crate::artifact::commit::finalize;
use crate::artifact::commit::prepare::evaluate_commit_safety_gate;
use crate::artifact::fs::ArtifactFileFacts;
use crate::cases::execution::remote_execution::{
    ReplaySlot, decode_replay, incarnation_replay_key, is_remote_replayable_error,
    validate_remote_node_live,
};
use crate::cases::{append_event, commit_tx};
use voom_store::tx::{begin_read_then_write, begin_serialized_read};

/// Supplemental-receipt reason written by the current owner's re-observation
/// when the target is absent with no temp sibling: positive evidence that
/// promotion never happened (ADR 0074). Recovery treats this exact value as
/// resolved-not-applied.
pub const RESOLVED_NOT_APPLIED_REASON: &str = "target_absent_no_temp_sibling";

/// Placeholder rendered by the redacting [`std::fmt::Debug`] impls of every
/// struct carrying `fence_hex` so no log or telemetry surface can leak the
/// one-time fence value.
const FENCE_DEBUG_REDACTED: &str = "[REDACTED]";

pub(crate) fn route_intent_authorize(intent_id: ArtifactCommitIntentId) -> String {
    format!("POST /v1/artifact/commit/{}/authorize", intent_id.0)
}

pub(crate) fn route_intent_applying(intent_id: ArtifactCommitIntentId) -> String {
    format!("POST /v1/artifact/commit/{}/applying", intent_id.0)
}

pub(crate) fn route_intent_outcome(intent_id: ArtifactCommitIntentId) -> String {
    format!("POST /v1/artifact/commit/{}/outcome", intent_id.0)
}

pub(crate) fn route_intent_complete(intent_id: ArtifactCommitIntentId) -> String {
    format!("POST /v1/artifact/commit/{}/complete", intent_id.0)
}

#[derive(Debug, Clone)]
pub struct RemoteCommitAuthorizeInput {
    pub intent_id: ArtifactCommitIntentId,
    pub node_id: NodeId,
    pub token: SecretString,
    pub incarnation_id: NodeIncarnationId,
    pub idempotency_key: String,
    pub request_hash: String,
}

/// The fenced authorization payload returned to the node (and stored verbatim
/// as the replay outcome, fence included — spec §Threat model).
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizeCommitOutcome {
    pub intent_id: ArtifactCommitIntentId,
    pub commit_record_id: voom_core::ids::ArtifactCommitRecordId,
    pub staging_storage_root_id: voom_core::StorageRootId,
    pub staging_provider_relative_locator: String,
    pub target_storage_root_id: voom_core::StorageRootId,
    pub target_provider_relative_locator: String,
    pub source_storage_root_id: voom_core::StorageRootId,
    /// Where the staged bytes come from: the pinned source rooted address
    /// the node materializes staging from during `applying` (ADR 0075).
    pub source_provider_relative_locator: voom_core::ProviderRelativeLocator,
    pub expected_size_bytes: u64,
    pub expected_content_hash: String,
    /// Hex-encoded one-time 32-byte commit fence. Never serialized into events.
    pub fence_hex: String,
}

impl std::fmt::Debug for AuthorizeCommitOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The fence is capability material: its Debug rendering must never
        // leak it into a log or telemetry surface.
        f.debug_struct("AuthorizeCommitOutcome")
            .field("intent_id", &self.intent_id)
            .field("commit_record_id", &self.commit_record_id)
            .field("staging_storage_root_id", &self.staging_storage_root_id)
            .field(
                "staging_provider_relative_locator",
                &self.staging_provider_relative_locator,
            )
            .field("source_storage_root_id", &self.source_storage_root_id)
            .field(
                "source_provider_relative_locator",
                &self.source_provider_relative_locator,
            )
            .field("target_storage_root_id", &self.target_storage_root_id)
            .field(
                "target_provider_relative_locator",
                &self.target_provider_relative_locator,
            )
            .field("expected_size_bytes", &self.expected_size_bytes)
            .field("expected_content_hash", &self.expected_content_hash)
            .field("fence_hex", &FENCE_DEBUG_REDACTED)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct RemoteCommitApplyingInput {
    pub intent_id: ArtifactCommitIntentId,
    pub node_id: NodeId,
    pub token: SecretString,
    pub incarnation_id: NodeIncarnationId,
    pub idempotency_key: String,
    pub request_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteCommitApplyingOutcome {
    pub intent_id: ArtifactCommitIntentId,
}

#[derive(Debug, Clone)]
pub struct RemoteCommitOutcomeInput {
    pub intent_id: ArtifactCommitIntentId,
    pub node_id: NodeId,
    pub token: SecretString,
    pub incarnation_id: NodeIncarnationId,
    pub idempotency_key: String,
    pub request_hash: String,
    pub evidence: CommitOutcomeEvidence,
}

/// Typed receipt evidence reported by the node. The tag discriminator rejects
/// unknown variant names; each variant carries a dedicated wire struct that
/// rejects unknown fields (ADR 0013 pattern, mirroring
/// `artifact_commit_intents::CommitReceipt`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommitOutcomeEvidence {
    Applied(AppliedEvidence),
    Mismatched(MismatchedEvidence),
    OutcomeUnknown(OutcomeUnknownEvidence),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppliedEvidence {
    pub observed: CommitObservedFacts,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MismatchedEvidence {
    pub reason: String,
    pub observed: Option<CommitObservedFacts>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutcomeUnknownEvidence {
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteCommitReceiptOutcome {
    pub intent_id: ArtifactCommitIntentId,
    pub kind: String,
}
#[derive(Clone)]
pub struct RemoteCommitCompleteInput {
    pub intent_id: ArtifactCommitIntentId,
    pub node_id: NodeId,
    pub token: SecretString,
    pub incarnation_id: NodeIncarnationId,
    pub idempotency_key: String,
    pub request_hash: String,
    /// Hex-encoded one-time 32-byte commit fence from the authorize outcome.
    pub fence_hex: String,
}

impl std::fmt::Debug for RemoteCommitCompleteInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteCommitCompleteInput")
            .field("intent_id", &self.intent_id)
            .field("node_id", &self.node_id)
            .field("token", &self.token)
            .field("incarnation_id", &self.incarnation_id)
            .field("idempotency_key", &self.idempotency_key)
            .field("request_hash", &self.request_hash)
            .field("fence_hex", &FENCE_DEBUG_REDACTED)
            .finish()
    }
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteCommitCompleteOutcome {
    pub intent_id: ArtifactCommitIntentId,
    pub commit_record_id: voom_core::ids::ArtifactCommitRecordId,
    pub result_file_version_id: Option<voom_core::FileVersionId>,
    pub result_file_location_id: Option<voom_core::FileLocationId>,
}

/// Input for the node pull listing of open commit intents.
#[derive(Debug, Clone)]
pub struct RemoteCommitIntentsOpenInput {
    pub node_id: NodeId,
    pub token: SecretString,
    pub incarnation_id: NodeIncarnationId,
}

/// One advertised open commit intent: everything a coordinator needs to
/// drive the intent, projected from the pinned scope. Never carries fence
/// material.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenCommitIntent {
    pub id: ArtifactCommitIntentId,
    pub state: String,
    pub artifact_handle_id: ArtifactHandleId,
    pub expected_facts: CommitExpectedFacts,
    pub staging_storage_root_id: voom_core::StorageRootId,
    pub staging_provider_relative_locator: String,
    pub staging_location_epoch: u64,
    pub source_storage_root_id: voom_core::StorageRootId,
    pub source_provider_relative_locator: voom_core::ProviderRelativeLocator,
    pub target_storage_root_id: voom_core::StorageRootId,
    pub target_provider_relative_locator: String,
    pub target_root_epoch: u64,
    pub intent_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteCommitIntentsOpenOutcome {
    pub intents: Vec<OpenCommitIntent>,
}

fn rfc3339(now: time::OffsetDateTime) -> Result<String, VoomError> {
    now.format(&time::format_description::well_known::Rfc3339)
        .map_err(|err| VoomError::Internal(format!("receipt timestamp format: {err}")))
}

/// Authentication + reservation preamble shared by every intent case:
/// verifies the node token and active incarnation, checks node liveness,
/// then reserves (or replays) the idempotency row for the route.
///
/// # Errors
/// Authentication, liveness, or idempotency storage errors.
async fn begin_intent_case<'a>(
    cp: &'a ControlPlane,
    node_id: NodeId,
    token: &SecretString,
    incarnation_id: NodeIncarnationId,
    identity: &CaseIdentity<'_>,
    now: time::OffsetDateTime,
) -> Result<CaseReservation<'a>, VoomError> {
    let mut tx = begin_read_then_write(&cp.pool, "intent: begin_intent_case").await?;
    let auth = cp
        .require_remote_incarnation_fence_in_tx(&mut tx, node_id, token, incarnation_id, None)
        .await?;
    validate_remote_node_live(&auth, node_id, now, false)?;
    let replay_key = incarnation_replay_key(incarnation_id, identity.idempotency_key);
    match cp
        .remote_idempotency
        .reserve_or_replay_in_tx(
            &mut tx,
            RemoteIdempotencyInput {
                node_id,
                route_key: identity.route_key.to_owned(),
                worker_id: None,
                idempotency_key: replay_key.clone(),
                request_hash: identity.request_hash.to_owned(),
                created_at: now,
            },
        )
        .await?
    {
        IdempotencyOutcome::Reserved => Ok(CaseReservation::Fresh { tx, replay_key }),
        IdempotencyOutcome::Replay(replay) => Ok(CaseReservation::Replay {
            tx,
            replay,
            replay_key,
        }),
    }
}

/// Route and idempotency identity shared by every case call.
struct CaseIdentity<'a> {
    route_key: &'a str,
    idempotency_key: &'a str,
    request_hash: &'a str,
}

/// A started intent case: either a stored replay to finish verbatim, or a
/// fresh reservation holding the open transaction and the replay key.
enum CaseReservation<'a> {
    Fresh {
        tx: sqlx::Transaction<'a, Sqlite>,
        replay_key: String,
    },
    Replay {
        tx: sqlx::Transaction<'a, Sqlite>,
        replay: RemoteMutationReplay,
        replay_key: String,
    },
}

/// Finish the replay branch of an intent case with `decode`.
async fn finish_intent_replay<T>(
    cp: &ControlPlane,
    tx: sqlx::Transaction<'_, Sqlite>,
    node_id: NodeId,
    route_key: String,
    replay_key: String,
    replay: RemoteMutationReplay,
    decode: impl FnOnce(JsonValue) -> Result<T, VoomError>,
) -> Result<T, VoomError> {
    cp.finish_replay_in_tx(
        tx,
        ReplaySlot {
            node_id,
            route_key,
            worker_id: None,
            idempotency_key: replay_key,
        },
        replay,
        decode,
    )
    .await
}

/// Terminal error path for a reserved case: store the error as the replay
/// outcome when it is remote-replayable and commit; otherwise return it
/// directly so dropping `tx` rolls the reservation back.
async fn store_case_error(
    cp: &ControlPlane,
    mut tx: sqlx::Transaction<'_, Sqlite>,
    node_id: NodeId,
    route_key: &str,
    replay_key: &str,
    err: VoomError,
) -> VoomError {
    if !is_remote_replayable_error(&err) {
        return err;
    }
    if let Err(store) = cp
        .complete_remote_error_in_tx(&mut tx, node_id, route_key, None, replay_key, &err)
        .await
    {
        return store;
    }
    if let Err(commit) = commit_tx(tx).await {
        return commit;
    }
    err
}

impl ControlPlane {
    /// Authorize a pending commit intent for the requesting storage owner
    /// (spec step 2): revalidate ownership, liveness, pinned epochs and the
    /// lineage safety gate, then transition `pending -> authorized`, minting
    /// the one-time fence. Drift aborts the still-pending intent fail-closed.
    ///
    /// # Errors
    /// Authentication/liveness errors, `Conflict` on any drift or non-pending
    /// state, `BlockedByUseLease` when the re-run gate finds a blocking lease.
    pub async fn remote_authorize_commit_intent(
        &self,
        input: RemoteCommitAuthorizeInput,
    ) -> Result<AuthorizeCommitOutcome, VoomError> {
        let route_key = route_intent_authorize(input.intent_id);
        let now = self.clock().now();
        match begin_intent_case(
            self,
            input.node_id,
            &input.token,
            input.incarnation_id,
            &CaseIdentity {
                route_key: &route_key,
                idempotency_key: &input.idempotency_key,
                request_hash: &input.request_hash,
            },
            now,
        )
        .await?
        {
            CaseReservation::Replay {
                tx,
                replay,
                replay_key,
            } => {
                finish_intent_replay(
                    self,
                    tx,
                    input.node_id,
                    route_key,
                    replay_key,
                    replay,
                    |data| decode_replay::<AuthorizeCommitOutcome>(data, "commit intent authorize"),
                )
                .await
            }
            CaseReservation::Fresh { mut tx, replay_key } => {
                let outcome = match authorize_pending_mutation(self, &mut tx, &input, now).await {
                    Ok(outcome) => outcome,
                    // Any authorization failure is drift evidence: the
                    // still-pending intent aborts fail-closed (authorized
                    // intents are never touched here — a duplicate authorize
                    // is not drift against a live fence, G2).
                    Err(err) => {
                        if is_remote_replayable_error(&err) {
                            abort_pending_on_drift(self, &mut tx, input.intent_id, now).await;
                            return Err(store_case_error(
                                self,
                                tx,
                                input.node_id,
                                &route_key,
                                &replay_key,
                                err,
                            )
                            .await);
                        }
                        // Transient drift (e.g. `BlockedByUseLease`) is not
                        // replay-stored: the reservation rolls back, but the
                        // fail-closed abort must persist.
                        drop(tx);
                        let mut abort_tx = begin_read_then_write(
                            &self.pool,
                            "intent: remote_authorize_commit_intent",
                        )
                        .await?;
                        abort_pending_on_drift(self, &mut abort_tx, input.intent_id, now).await;
                        commit_tx(abort_tx).await?;
                        return Err(err);
                    }
                };
                self.complete_remote_ok_in_tx(
                    &mut tx,
                    input.node_id,
                    &route_key,
                    None,
                    &replay_key,
                    &outcome,
                )
                .await?;
                commit_tx(tx).await?;
                Ok(outcome)
            }
        }
    }
    /// Journal the node's `applying` receipt before it touches bytes (spec
    /// step 3). Accepted only while the intent is `authorized` with no prior
    /// receipt.
    ///
    /// # Errors
    /// Authentication errors; `Conflict` on drift, wrong state, or an existing
    /// receipt.
    pub async fn remote_report_commit_applying(
        &self,
        input: RemoteCommitApplyingInput,
    ) -> Result<RemoteCommitApplyingOutcome, VoomError> {
        let route_key = route_intent_applying(input.intent_id);
        let now = self.clock().now();
        match begin_intent_case(
            self,
            input.node_id,
            &input.token,
            input.incarnation_id,
            &CaseIdentity {
                route_key: &route_key,
                idempotency_key: &input.idempotency_key,
                request_hash: &input.request_hash,
            },
            now,
        )
        .await?
        {
            CaseReservation::Replay {
                tx,
                replay,
                replay_key,
            } => {
                finish_intent_replay(
                    self,
                    tx,
                    input.node_id,
                    route_key,
                    replay_key,
                    replay,
                    |data| {
                        decode_replay::<RemoteCommitApplyingOutcome>(data, "commit intent applying")
                    },
                )
                .await
            }
            CaseReservation::Fresh { mut tx, replay_key } => {
                let outcome = match applying_mutation(self, &mut tx, &input, now).await {
                    Ok(outcome) => outcome,
                    Err(err) => {
                        return Err(store_case_error(
                            self,
                            tx,
                            input.node_id,
                            &route_key,
                            &replay_key,
                            err,
                        )
                        .await);
                    }
                };
                self.complete_remote_ok_in_tx(
                    &mut tx,
                    input.node_id,
                    &route_key,
                    None,
                    &replay_key,
                    &outcome,
                )
                .await?;
                commit_tx(tx).await?;
                Ok(outcome)
            }
        }
    }

    /// Record a typed node outcome receipt (spec steps 4/7): `applied`
    /// journals the promotion evidence; `mismatched`/`outcome_unknown` land
    /// both the intent and its commit record in `recovery_required` in one
    /// transaction. On a `recovery_required` intent the same route files a
    /// supplemental re-observation into the supplemental slot so the original
    /// evidence survives (filed by the current root owner, fenced like every
    /// other receipt).
    ///
    /// # Errors
    /// Authentication errors; `Conflict` on drift, ordering violations, or
    /// wrong state.
    pub async fn remote_report_commit_outcome(
        &self,
        input: RemoteCommitOutcomeInput,
    ) -> Result<RemoteCommitReceiptOutcome, VoomError> {
        let route_key = route_intent_outcome(input.intent_id);
        let now = self.clock().now();
        match begin_intent_case(
            self,
            input.node_id,
            &input.token,
            input.incarnation_id,
            &CaseIdentity {
                route_key: &route_key,
                idempotency_key: &input.idempotency_key,
                request_hash: &input.request_hash,
            },
            now,
        )
        .await?
        {
            CaseReservation::Replay {
                tx,
                replay,
                replay_key,
            } => {
                finish_intent_replay(
                    self,
                    tx,
                    input.node_id,
                    route_key,
                    replay_key,
                    replay,
                    |data| {
                        decode_replay::<RemoteCommitReceiptOutcome>(data, "commit intent outcome")
                    },
                )
                .await
            }
            CaseReservation::Fresh { mut tx, replay_key } => {
                let outcome = match outcome_mutation(self, &mut tx, &input, now).await {
                    Ok(outcome) => outcome,
                    Err(err) => {
                        return Err(store_case_error(
                            self,
                            tx,
                            input.node_id,
                            &route_key,
                            &replay_key,
                            err,
                        )
                        .await);
                    }
                };
                self.complete_remote_ok_in_tx(
                    &mut tx,
                    input.node_id,
                    &route_key,
                    None,
                    &replay_key,
                    &outcome,
                )
                .await?;
                commit_tx(tx).await?;
                Ok(outcome)
            }
        }
    }

    /// Complete an authorized intent (spec step 6): validate the exact,
    /// unconsumed fence and the matching applied evidence, then run the
    /// finalize transaction (result version/location, retire staging, mark
    /// committed) and mark the intent completed in the SAME transaction.
    ///
    /// # Errors
    /// Authentication errors; `Conflict` on fence mismatch, missing/mismatched
    /// applied evidence, or any pinned-scope drift.
    pub async fn remote_complete_commit_intent(
        &self,
        input: RemoteCommitCompleteInput,
    ) -> Result<RemoteCommitCompleteOutcome, VoomError> {
        let route_key = route_intent_complete(input.intent_id);
        let now = self.clock().now();
        match begin_intent_case(
            self,
            input.node_id,
            &input.token,
            input.incarnation_id,
            &CaseIdentity {
                route_key: &route_key,
                idempotency_key: &input.idempotency_key,
                request_hash: &input.request_hash,
            },
            now,
        )
        .await?
        {
            CaseReservation::Replay {
                tx,
                replay,
                replay_key,
            } => {
                finish_intent_replay(
                    self,
                    tx,
                    input.node_id,
                    route_key,
                    replay_key,
                    replay,
                    |data| {
                        decode_replay::<RemoteCommitCompleteOutcome>(data, "commit intent complete")
                    },
                )
                .await
            }
            CaseReservation::Fresh { mut tx, replay_key } => {
                let outcome = match complete_mutation(self, &mut tx, &input, now).await {
                    Ok(outcome) => outcome,
                    Err(err) => {
                        return Err(store_case_error(
                            self,
                            tx,
                            input.node_id,
                            &route_key,
                            &replay_key,
                            err,
                        )
                        .await);
                    }
                };
                self.complete_remote_ok_in_tx(
                    &mut tx,
                    input.node_id,
                    &route_key,
                    None,
                    &replay_key,
                    &outcome,
                )
                .await?;
                commit_tx(tx).await?;
                Ok(outcome)
            }
        }
    }

    /// List the requesting node's actionable commit intents (the node pull
    /// listing): every non-terminal (`pending`/`authorized`/
    /// `recovery_required`) intent whose pinned target root is currently
    /// owned by the caller at the pinned epoch. Authentication-only — no
    /// idempotency reservation, a pure read projection. An intent whose
    /// pinned staging location no longer resolves to a live rooted location
    /// at the pinned epoch is not advertised: it is not actionable through
    /// these routes (every mutating case revalidates the same pin and fails
    /// closed), mirroring how the store listing stops advertising stale
    /// roots. Never includes fence material.
    ///
    /// # Errors
    /// Authentication or storage errors.
    pub async fn remote_open_commit_intents(
        &self,
        input: RemoteCommitIntentsOpenInput,
    ) -> Result<RemoteCommitIntentsOpenOutcome, VoomError> {
        use voom_store::repo::media::identity::{FileLocationAddress, FileLocationRepo};

        let now = self.clock().now();
        let mut tx =
            begin_serialized_read(&self.pool, "intent: remote_open_commit_intents").await?;
        let auth = self
            .require_remote_incarnation_fence_in_tx(
                &mut tx,
                input.node_id,
                &input.token,
                input.incarnation_id,
                None,
            )
            .await?;
        validate_remote_node_live(&auth, input.node_id, now, false)?;
        let intents = self
            .artifact_commit_intents
            .list_open_for_roots_in_tx(&mut tx, input.node_id)
            .await?;
        let mut listed = Vec::with_capacity(intents.len());
        for intent in intents {
            let Some(location) = self
                .identity
                .get_file_location_in_tx(&mut tx, intent.staging_location_id)
                .await?
            else {
                continue;
            };
            if location.retired_at.is_some() || location.epoch != intent.staging_location_epoch {
                continue;
            }
            let FileLocationAddress::Rooted {
                storage_root_id,
                provider_relative_locator,
            } = location.address
            else {
                continue;
            };
            listed.push(OpenCommitIntent {
                id: intent.id,
                source_storage_root_id: intent.source_storage_root_id,
                source_provider_relative_locator: intent.source_provider_relative_locator.clone(),
                state: intent.state.as_str().to_owned(),
                artifact_handle_id: intent.artifact_handle_id,
                expected_facts: intent.expected_facts,
                staging_storage_root_id: storage_root_id,
                staging_provider_relative_locator: provider_relative_locator.as_str().to_owned(),
                staging_location_epoch: intent.staging_location_epoch,
                target_storage_root_id: intent.target_storage_root_id,
                target_provider_relative_locator: intent.target_provider_relative_locator,
                target_root_epoch: intent.target_root_epoch,
                intent_epoch: intent.intent_epoch,
            });
        }
        commit_tx(tx).await?;
        Ok(RemoteCommitIntentsOpenOutcome { intents: listed })
    }
}

/// Journal the `applying` receipt on the authorized intent (spec step 3).
async fn applying_mutation(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    input: &RemoteCommitApplyingInput,
    now: time::OffsetDateTime,
) -> Result<RemoteCommitApplyingOutcome, VoomError> {
    let intent = require_authorized_intent_in_tx(cp, tx, input.intent_id, input.node_id).await?;
    let receipt = CommitReceipt::Applying(ApplyingReceipt {
        reported_at: rfc3339(now)?,
    });
    record_receipt_in_tx(cp, tx, &intent, receipt, now).await?;
    Ok(RemoteCommitApplyingOutcome {
        intent_id: intent.id,
    })
}

/// Record the typed outcome receipt (spec steps 4/7); on a
/// `recovery_required` intent the same route files a supplemental
/// re-observation into the supplemental slot so the original evidence
/// survives.
async fn outcome_mutation(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    input: &RemoteCommitOutcomeInput,
    now: time::OffsetDateTime,
) -> Result<RemoteCommitReceiptOutcome, VoomError> {
    let reported_at = rfc3339(now)?;
    let receipt = match &input.evidence {
        CommitOutcomeEvidence::Applied(evidence) => CommitReceipt::Applied(AppliedReceipt {
            observed: evidence.observed.clone(),
            reported_at: reported_at.clone(),
        }),
        CommitOutcomeEvidence::Mismatched(evidence) => {
            CommitReceipt::Mismatched(MismatchedReceipt {
                reason: evidence.reason.clone(),
                observed: evidence.observed.clone(),
                reported_at: reported_at.clone(),
            })
        }
        CommitOutcomeEvidence::OutcomeUnknown(evidence) => {
            CommitReceipt::OutcomeUnknown(OutcomeUnknownReceipt {
                reason: evidence.reason.clone(),
                reported_at: reported_at.clone(),
            })
        }
    };
    let intent = cp
        .artifact_commit_intents
        .require_intent_in_tx(tx, input.intent_id)
        .await?;
    if intent.state == ArtifactCommitIntentState::RecoveryRequired {
        // Supplemental re-observation by the current root owner; the
        // original evidence survives alongside it.
        guard_intent_scope_in_tx(cp, tx, &intent, input.node_id).await?;
        cp.artifact_commit_intents
            .append_supplemental_receipt_in_tx(tx, input.intent_id, receipt)
            .await?;
    } else {
        if intent.state != ArtifactCommitIntentState::Authorized {
            return Err(VoomError::Conflict(format!(
                "commit intent {} is {} not authorized",
                intent.id,
                intent.state.as_str()
            )));
        }
        guard_intent_scope_in_tx(cp, tx, &intent, input.node_id).await?;
        record_receipt_in_tx(cp, tx, &intent, receipt, now).await?;
    }
    Ok(RemoteCommitReceiptOutcome {
        intent_id: intent.id,
        kind: receipt_kind(&input.evidence).to_owned(),
    })
}

/// Validate the exact, unconsumed fence and the matching applied evidence,
/// then converge the intent through the finalize transaction.
async fn complete_mutation(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    input: &RemoteCommitCompleteInput,
    now: time::OffsetDateTime,
) -> Result<RemoteCommitCompleteOutcome, VoomError> {
    let intent = require_authorized_intent_in_tx(cp, tx, input.intent_id, input.node_id).await?;
    validate_fence_and_evidence(&intent, &input.fence_hex)?;
    let record = require_commit_record(cp, &intent).await?;
    // The authorize transaction re-ran the gate and audited its evaluated
    // leases on the authorized event; nothing further belongs here.
    converge_intent_in_tx(cp, tx, &intent, &record, now, Vec::new()).await
}

/// The guarded authorize mutation: scope revalidation, gate re-run, fence
/// minting, and the authorized audit event.
async fn authorize_pending_mutation(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    input: &RemoteCommitAuthorizeInput,
    now: time::OffsetDateTime,
) -> Result<AuthorizeCommitOutcome, VoomError> {
    let intent = require_pending_intent_in_tx(cp, tx, input.intent_id).await?;
    let staging_address = guard_intent_scope_in_tx(cp, tx, &intent, input.node_id).await?;
    let source_asset_id = intent_source_asset_id(cp, tx, &intent).await?;
    let gate_evaluated_lease_ids =
        evaluate_commit_safety_gate(cp, tx, source_asset_id, intent.source_file_version_id, now)
            .await?;
    let authorized = cp
        .artifact_commit_intents
        .authorize_in_tx(tx, intent.id, input.incarnation_id, now)
        .await?;
    append_event(
        &cp.events,
        tx,
        voom_events::SubjectType::ArtifactHandle,
        Some(intent.artifact_handle_id.0),
        now,
        Event::ArtifactCommitIntentAuthorized(ArtifactCommitIntentAuthorizedPayload {
            commit_record_id: intent.commit_record_id,
            artifact_handle_id: intent.artifact_handle_id,
            owner_node_id: intent.owner_node_id,
            incarnation_id: input.incarnation_id.to_string(),
            authorized_at: now,
            gate_evaluated_lease_ids,
        }),
    )
    .await?;
    Ok(authorize_outcome(&authorized, &staging_address))
}

/// Abort a still-pending intent fail-closed after an authorization drift.
/// Authorized intents are never touched here — a duplicate authorize is not
/// drift evidence against a live fence (G2).
pub(super) async fn abort_pending_on_drift(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    intent_id: ArtifactCommitIntentId,
    now: time::OffsetDateTime,
) {
    let Ok(intent) = cp
        .artifact_commit_intents
        .require_intent_in_tx(tx, intent_id)
        .await
    else {
        return;
    };
    if intent.state != ArtifactCommitIntentState::Pending {
        return;
    }
    let _ = cp
        .artifact_commit_intents
        .mark_aborted_in_tx(tx, intent_id, now)
        .await;
}

async fn require_pending_intent_in_tx(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    intent_id: ArtifactCommitIntentId,
) -> Result<ArtifactCommitIntent, VoomError> {
    let intent = cp
        .artifact_commit_intents
        .require_intent_in_tx(tx, intent_id)
        .await?;
    if intent.state != ArtifactCommitIntentState::Pending {
        return Err(VoomError::Conflict(format!(
            "commit intent {} is {} not pending",
            intent.id,
            intent.state.as_str()
        )));
    }
    Ok(intent)
}

/// Load an authorized intent and revalidate the full pinned scope for its
/// current owner: root active at the pinned epoch, staging location row live
/// at the pinned epoch, requester owns the target root.
async fn require_authorized_intent_in_tx(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    intent_id: ArtifactCommitIntentId,
    node_id: NodeId,
) -> Result<ArtifactCommitIntent, VoomError> {
    let intent = cp
        .artifact_commit_intents
        .require_intent_in_tx(tx, intent_id)
        .await?;
    if intent.state != ArtifactCommitIntentState::Authorized {
        return Err(VoomError::Conflict(format!(
            "commit intent {} is {} not authorized",
            intent.id,
            intent.state.as_str()
        )));
    }
    guard_intent_scope_in_tx(cp, tx, &intent, node_id).await?;
    Ok(intent)
}

/// The rooted staging address resolved from the pinned `file_locations` row.
#[derive(Debug, Clone)]
pub(super) struct StagingAddress {
    storage_root_id: voom_core::StorageRootId,
    provider_relative_locator: String,
}

/// Revalidate every pinned scope element of an intent for `node_id` and
/// return the pinned staging rooted address for the fenced payload.
pub(super) async fn guard_intent_scope_in_tx(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    intent: &ArtifactCommitIntent,
    node_id: NodeId,
) -> Result<StagingAddress, VoomError> {
    use voom_store::repo::media::identity::{FileLocationAddress, FileLocationRepo};

    let root = cp
        .libraries
        .get_library_root_in_tx(tx, intent.target_storage_root_id)
        .await?
        .ok_or_else(|| {
            VoomError::NotFound(format!("library_roots {}", intent.target_storage_root_id))
        })?;
    if root.owner_node_id != Some(node_id) {
        return Err(VoomError::Conflict(format!(
            "commit intent {} rejected: node {node_id} does not own target root {}",
            intent.id, intent.target_storage_root_id
        )));
    }
    if root.state != voom_core::StorageRootState::Active {
        return Err(VoomError::Conflict(format!(
            "commit intent {} rejected: target root {} is {} not active",
            intent.id,
            intent.target_storage_root_id,
            root.state.as_str()
        )));
    }
    if root.root_epoch != intent.target_root_epoch {
        return Err(scope_drift(intent, "target root epoch"));
    }
    let location = cp
        .identity
        .get_file_location_in_tx(tx, intent.staging_location_id)
        .await?
        .ok_or_else(|| {
            VoomError::NotFound(format!("file_locations {}", intent.staging_location_id))
        })?;
    if location.retired_at.is_some() || location.epoch != intent.staging_location_epoch {
        return Err(scope_drift(intent, "staging location epoch"));
    }
    let FileLocationAddress::Rooted {
        storage_root_id,
        provider_relative_locator,
    } = location.address
    else {
        return Err(scope_drift(intent, "staging location address"));
    };
    Ok(StagingAddress {
        storage_root_id,
        provider_relative_locator: provider_relative_locator.as_str().to_owned(),
    })
}

fn scope_drift(intent: &ArtifactCommitIntent, what: &str) -> VoomError {
    VoomError::Conflict(format!(
        "commit intent {} rejected: {what} drifted from the pinned scope",
        intent.id
    ))
}

fn receipt_kind(evidence: &CommitOutcomeEvidence) -> &'static str {
    match evidence {
        CommitOutcomeEvidence::Applied(_) => "applied",
        CommitOutcomeEvidence::Mismatched(_) => "mismatched",
        CommitOutcomeEvidence::OutcomeUnknown(_) => "outcome_unknown",
    }
}

fn validate_fence_and_evidence(
    intent: &ArtifactCommitIntent,
    fence_hex: &str,
) -> Result<(), VoomError> {
    let matches = hex::decode(fence_hex)
        .ok()
        .filter(|bytes| bytes.len() == 32)
        .zip(intent.commit_fence.as_deref())
        .is_some_and(|(bytes, stored)| constant_time_eq(&bytes, stored));
    if !matches {
        return Err(VoomError::Conflict(format!(
            "commit intent {} completion rejected: commit fence mismatch",
            intent.id
        )));
    }
    let Some(CommitReceipt::Applied(applied)) = &intent.receipt else {
        return Err(VoomError::Conflict(format!(
            "commit intent {} completion rejected: no applied receipt",
            intent.id
        )));
    };
    if applied.observed.size_bytes != intent.expected_facts.size_bytes
        || applied.observed.content_hash != intent.expected_facts.content_hash
    {
        return Err(VoomError::Conflict(format!(
            "commit intent {} completion rejected: observed facts do not match the pinned \
             expected facts",
            intent.id
        )));
    }
    Ok(())
}

/// Journal a receipt on an authorized intent, emit the receipt event, and —
/// for drift evidence (`mismatched`/`outcome_unknown`) — move both the intent
/// and its commit record to `recovery_required` in the same transaction.
async fn record_receipt_in_tx(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    intent: &ArtifactCommitIntent,
    receipt: CommitReceipt,
    now: time::OffsetDateTime,
) -> Result<(), VoomError> {
    let kind = receipt.kind_str();
    cp.artifact_commit_intents
        .record_receipt_in_tx(tx, intent.id, receipt.clone())
        .await?;
    let (reason, observed) = match &receipt {
        CommitReceipt::Applying(_) => (None, None),
        CommitReceipt::Applied(applied) => (
            None,
            Some((
                applied.observed.size_bytes,
                applied.observed.content_hash.clone(),
            )),
        ),
        CommitReceipt::Mismatched(mismatched) => (
            Some(mismatched.reason.clone()),
            mismatched
                .observed
                .as_ref()
                .map(|observed| (observed.size_bytes, observed.content_hash.clone())),
        ),
        CommitReceipt::OutcomeUnknown(unknown) => (Some(unknown.reason.clone()), None),
    };
    append_event(
        &cp.events,
        tx,
        voom_events::SubjectType::ArtifactHandle,
        Some(intent.artifact_handle_id.0),
        now,
        Event::ArtifactCommitReceiptReported(ArtifactCommitReceiptReportedPayload {
            commit_record_id: intent.commit_record_id,
            artifact_handle_id: intent.artifact_handle_id,
            kind: kind.to_owned(),
            reason,
            observed_size_bytes: observed.as_ref().map(|(size, _)| *size),
            observed_checksum: observed.map(|(_, hash)| hash),
            reported_at: now,
        }),
    )
    .await?;
    if matches!(
        receipt,
        CommitReceipt::Mismatched(_) | CommitReceipt::OutcomeUnknown(_)
    ) {
        cp.artifact_commit_intents
            .mark_recovery_required_in_tx(tx, intent.id, now)
            .await?;
        let (failure_class, error_code) = if matches!(receipt, CommitReceipt::Mismatched(_)) {
            (
                FailureClass::ArtifactChecksumMismatch,
                ErrorCode::ArtifactChecksumMismatch,
            )
        } else {
            (FailureClass::CommitFailure, ErrorCode::CommitFailure)
        };
        let record = require_commit_record(cp, intent).await?;
        let recovery_reason = format!("intent_receipt_{kind}");
        let message = format!("commit intent {} reported {kind}", intent.id);
        mark_recovery_required_with_event_in_tx(
            &cp.artifacts,
            &cp.events,
            tx,
            RecoveryRequiredCommit {
                commit_record_id: record.id,
                artifact_handle_id: record.artifact_handle_id,
                failure: ArtifactCommitFailure {
                    failure_class,
                    error_code,
                    message: message.clone(),
                    finished_at: now,
                },
                recovery_reason: recovery_reason.clone(),
                event: Event::ArtifactCommitRecoveryRequired(
                    voom_events::payload::ArtifactCommitRecoveryRequiredPayload {
                        commit_record_id: record.id,
                        artifact_handle_id: record.artifact_handle_id,
                        target_path: record.target_path.clone(),
                        temp_path: record.temp_path.clone().unwrap_or_default(),
                        recovery_reason,
                        error_code: error_code.as_str().to_owned(),
                        message,
                    },
                ),
                occurred_at: now,
            },
        )
        .await?;
    }
    Ok(())
}

/// Load the intent's commit record (pool-level read; the row is only mutated
/// by the finalize that follows inside the caller's transaction).
async fn require_commit_record(
    cp: &ControlPlane,
    intent: &ArtifactCommitIntent,
) -> Result<ArtifactCommitRecord, VoomError> {
    let commit_record_id = intent.commit_record_id;
    cp.artifacts
        .get_commit_record(commit_record_id)
        .await?
        .ok_or_else(|| VoomError::NotFound(format!("artifact_commit_records {commit_record_id}")))
}

/// Converge one authorized/recovery-required intent: validate applied
/// evidence against the pinned facts, run the finalize transaction (result
/// version/location, retire staging rows, mark committed), and mark the
/// intent completed — all in the caller's transaction. Shared by node
/// completion (which passes an empty audit list: the authorize transaction
/// already recorded the gate's evaluated leases on the authorized event)
/// and recovery-driven finalization (which passes the leases its own
/// fail-closed gate re-run evaluated).
pub(crate) async fn converge_intent_in_tx(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    intent: &ArtifactCommitIntent,
    record: &ArtifactCommitRecord,
    now: time::OffsetDateTime,
    gate_evaluated_lease_ids: Vec<voom_core::UseLeaseId>,
) -> Result<RemoteCommitCompleteOutcome, VoomError> {
    let Some(CommitReceipt::Applied(applied)) = &intent.receipt else {
        return Err(VoomError::Conflict(format!(
            "commit intent {} completion rejected: no applied receipt",
            intent.id
        )));
    };
    if applied.observed.size_bytes != intent.expected_facts.size_bytes
        || applied.observed.content_hash != intent.expected_facts.content_hash
    {
        return Err(VoomError::Conflict(format!(
            "commit intent {} completion rejected: observed facts do not match the pinned \
             expected facts",
            intent.id
        )));
    }
    let staging_artifact_location_id = live_staging_artifact_location(cp, tx, record).await?;
    let facts = ArtifactFileFacts {
        path: std::path::PathBuf::from(&record.target_path),
        size_bytes: intent.expected_facts.size_bytes,
        content_hash: intent.expected_facts.content_hash.clone(),
        modified_at: None,
        local_file_key: None,
    };
    let finalize_input = crate::artifact::commit::CommitFinalizeInput {
        record_id: record.id,
        artifact_handle_id: record.artifact_handle_id,
        source_file_asset_id: intent_source_asset_id(cp, tx, intent).await?,
        source_file_version_id: intent.source_file_version_id,
        staging_artifact_location_id,
        staging_file_location: Some((intent.staging_location_id, intent.staging_location_epoch)),
        target_storage_root_id: intent.target_storage_root_id,
        target_relative_locator: voom_core::ProviderRelativeLocator::parse_database(
            "artifact_commit_intents.target_provider_relative_locator",
            &intent.target_provider_relative_locator,
        )?,
        target_path: std::path::PathBuf::from(&record.target_path),
        // Promotion happened on the node; the durable promotion window runs
        // from authorization to this completion.
        promotion_started_at: intent.authorized_at.unwrap_or(record.started_at),
        gate_evaluated_lease_ids,
    };
    let report = finalize::finalize_commit_in_tx(cp, tx, &finalize_input, &facts).await?;
    cp.artifact_commit_intents
        .mark_completed_in_tx(tx, intent.id, now)
        .await?;
    Ok(RemoteCommitCompleteOutcome {
        intent_id: intent.id,
        commit_record_id: report.commit_record_id,
        result_file_version_id: report.result_file_version_id,
        result_file_location_id: report.result_file_location_id,
    })
}

pub(crate) async fn intent_source_asset_id(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    intent: &ArtifactCommitIntent,
) -> Result<voom_core::FileAssetId, VoomError> {
    use voom_store::repo::media::identity::FileVersionRepo;
    cp.identity
        .get_file_version_in_tx(tx, intent.source_file_version_id)
        .await?
        .map(|version| version.file_asset_id)
        .ok_or_else(|| {
            VoomError::NotFound(format!("file_versions {}", intent.source_file_version_id))
        })
}

async fn live_staging_artifact_location(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    record: &ArtifactCommitRecord,
) -> Result<voom_core::ArtifactLocationId, VoomError> {
    cp.artifacts
        .live_location_of_kind_in_tx(
            tx,
            record.artifact_handle_id,
            voom_store::repo::media::artifacts::ArtifactLocationKind::Staging,
        )
        .await?
        .map(|location| location.id)
        .ok_or_else(|| {
            VoomError::Conflict(format!(
                "artifact_handle {} has no live staging location to retire",
                record.artifact_handle_id
            ))
        })
}

fn authorize_outcome(
    authorized: &ArtifactCommitIntent,
    staging: &StagingAddress,
) -> AuthorizeCommitOutcome {
    AuthorizeCommitOutcome {
        intent_id: authorized.id,
        commit_record_id: authorized.commit_record_id,
        staging_storage_root_id: staging.storage_root_id,
        staging_provider_relative_locator: staging.provider_relative_locator.clone(),
        source_storage_root_id: authorized.source_storage_root_id,
        source_provider_relative_locator: authorized.source_provider_relative_locator.clone(),
        target_storage_root_id: authorized.target_storage_root_id,
        target_provider_relative_locator: authorized.target_provider_relative_locator.clone(),
        expected_size_bytes: authorized.expected_facts.size_bytes,
        expected_content_hash: authorized.expected_facts.content_hash.clone(),
        fence_hex: authorized
            .commit_fence
            .as_deref()
            .map(hex::encode)
            .unwrap_or_default(),
    }
}
