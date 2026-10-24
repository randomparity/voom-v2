//! Receipt-based commit recovery (ADR 0074, spec step 7). The control plane
//! never probes local paths: classification reads only the fenced intent's
//! receipts against the pinned expected facts.
//!
//! Four states:
//! 1. Not started (pending, or authorized with no journal) — safe abort via
//!    CAS, then a fresh successor generation prepares.
//! 2. Promoted (`applied` receipt, original or supplemental, matching facts) —
//!    finalizes directly without further byte mutation.
//! 3. Resolved not-applied (supplemental re-observation: target absent, no
//!    temp sibling) — abort and re-drive a fresh generation.
//! 4. Mismatched / unresolved `outcome_unknown` / pinned-scope drift —
//!    operator-required `Conflict`; the record stays `recovery_required`.

use time::OffsetDateTime;
use voom_core::{ErrorCode, FailureClass, VoomError};
use voom_store::repo::media::artifact_commit_intents::{
    ArtifactCommitIntent, ArtifactCommitIntentState, CommitReceipt,
};
use voom_store::repo::media::artifacts::{
    ArtifactCommitFailure, ArtifactCommitRecord, ArtifactCommitState,
};
use voom_store::repo::media::identity::FileLocationRepo;

use crate::ControlPlane;
use crate::artifact::commit::intent::{RESOLVED_NOT_APPLIED_REASON, guard_intent_scope_in_tx};
use crate::artifact::commit::prepare::{
    PrepareCommitError, evaluate_commit_safety_gate, prepare_commit_in_tx,
};
use crate::artifact::commit::{CommitArtifactInput, CommitArtifactReport};
use crate::cases::commit_tx;
use voom_store::tx::{begin_read_only, begin_read_then_write, begin_write_first};

pub(super) async fn recover_commit(
    cp: &ControlPlane,
    artifact_handle_id: voom_core::ArtifactHandleId,
) -> Result<CommitArtifactReport, VoomError> {
    let records = cp.artifacts.list_commit_records(artifact_handle_id).await?;
    let record = records
        .iter()
        .find(|record| {
            matches!(
                record.state,
                ArtifactCommitState::Pending | ArtifactCommitState::RecoveryRequired
            )
        })
        .cloned()
        .ok_or_else(|| {
            VoomError::Conflict(format!(
                "artifact_handle {artifact_handle_id} has no non-terminal commit to recover"
            ))
        })?;
    let (intent, extraction_group) = {
        let mut tx = begin_read_only(&cp.pool, "recovery: recover_commit").await?;
        let intent = cp
            .artifact_commit_intents
            .get_by_commit_record_in_tx(&mut tx, record.id)
            .await?
            .ok_or_else(|| {
                VoomError::database(format!(
                    "artifact commit {} has no fenced intent row",
                    record.id
                ))
            })?;
        let extraction_group = voom_store::repo::media::audio_extract_operations::SqliteAudioExtractOperationRepo::operation_for_commit_in_tx(&mut tx, record.id).await?.is_some();
        commit_tx(tx).await?;
        (intent, extraction_group)
    };

    if extraction_group {
        return recover_extraction_group(cp, &record).await;
    }

    let receiptless = intent.receipt.is_none();
    match intent.state {
        // The `applying` journal is the sole mutation gate: no receipt means
        // the node never mutated, so aborting is safe even under an existing
        // fence (ADR 0074).
        ArtifactCommitIntentState::Pending | ArtifactCommitIntentState::Authorized
            if receiptless =>
        {
            abort_and_reprepare_report(cp, &record, &intent, intent.intent_epoch).await
        }
        ArtifactCommitIntentState::Authorized => {
            // A receipt-bearing authorized intent enters recovery_required
            // before classification (never silently on a timer).
            let mut tx = begin_write_first(&cp.pool, "recovery: recover_commit").await?;
            let now = cp.clock().now();
            cp.artifact_commit_intents
                .mark_recovery_required_in_tx(&mut tx, intent.id, now)
                .await?;
            commit_tx(tx).await?;
            classify_recovery_required(cp, record, intent).await
        }
        ArtifactCommitIntentState::RecoveryRequired => {
            classify_recovery_required(cp, record, intent).await
        }
        // A pending intent that somehow carries a receipt is a state-machine
        // impossibility; fail closed to the operator.
        ArtifactCommitIntentState::Pending => operator_required(&record, &intent),
        ArtifactCommitIntentState::Completed | ArtifactCommitIntentState::Aborted => {
            Err(VoomError::Conflict(format!(
                "artifact commit {} intent is already {}",
                record.id,
                intent.state.as_str()
            )))
        }
    }
}

/// Explicit operator recovery classifies the entire extraction. It never
/// supplies normal completion capabilities or lets one applied sibling publish
/// while another member is unresolved.
async fn recover_extraction_group(
    cp: &ControlPlane,
    requested: &ArtifactCommitRecord,
) -> Result<CommitArtifactReport, VoomError> {
    use voom_store::repo::media::audio_extract_operations::{
        AudioExtractOperationState, SqliteAudioExtractOperationRepo,
    };
    let mut tx = begin_read_then_write(&cp.pool, "recovery: extraction operation").await?;
    let now = cp.clock().now();
    let (operation, claim) =
        SqliteAudioExtractOperationRepo::operation_for_commit_in_tx(&mut tx, requested.id)
            .await?
            .ok_or_else(|| {
                VoomError::database("extraction recovery association disappeared".to_owned())
            })?;
    let members = read_extraction_members_in_tx(cp, &mut tx, &operation).await?;
    if operation.outputs.is_empty()
        || operation.outputs.len() > voom_worker_protocol::MAX_COMMIT_GROUP_OUTPUTS
        || !matches!(
            operation.operation.state,
            AudioExtractOperationState::Prepared | AudioExtractOperationState::RecoveryRequired
        )
        || claim.expires_at <= now
    {
        return Err(VoomError::Conflict(
            "extraction recovery requires the entire live prepared operation".to_owned(),
        ));
    }
    let unresolved =
        classify_extraction_members_in_tx(cp, &mut tx, &operation, &members, now).await?;
    if unresolved {
        let error = VoomError::Conflict(
            "extraction recovery has an unresolved member; no outputs published".to_owned(),
        );
        mark_extraction_recovery_in_tx(
            cp,
            &mut tx,
            operation.operation.id,
            &claim,
            &members,
            &error,
            now,
        )
        .await?;
        commit_tx(tx).await?;
        return Err(error);
    }
    let outcome = crate::artifact::publication::finish_extraction_group_in_tx(
        cp, &mut tx, &operation, &claim, &members, now,
    )
    .await?;
    let result = outcome
        .results
        .iter()
        .find(|result| result.commit_record_id == requested.id)
        .ok_or_else(|| {
            VoomError::database("extraction recovery result omitted requested member".to_owned())
        })?;
    let report = CommitArtifactReport {
        commit_record_id: requested.id,
        artifact_handle_id: requested.artifact_handle_id,
        verification_id: requested.verification_id,
        target_path: std::path::PathBuf::from(&requested.target_path),
        temp_path: requested.temp_path.as_ref().map(std::path::PathBuf::from),
        state: ArtifactCommitState::Committed,
        result_file_version_id: Some(result.result_file_version_id),
        result_file_location_id: Some(result.result_file_location_id),
        recovery_required: None,
    };
    commit_tx(tx).await?;
    Ok(report)
}

async fn mark_extraction_recovery_in_tx(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    operation_id: u64,
    claim: &voom_store::repo::media::audio_extract_operations::NewAudioExtractClaim,
    members: &[(ArtifactCommitIntent, ArtifactCommitRecord)],
    error: &VoomError,
    now: OffsetDateTime,
) -> Result<(), VoomError> {
    for (intent, record) in members {
        if intent.state == ArtifactCommitIntentState::Authorized {
            cp.artifact_commit_intents
                .mark_recovery_required_in_tx(tx, intent.id, now)
                .await?;
        }
        cp.artifacts
            .mark_commit_recovery_required_in_tx(
                tx,
                record.id,
                ArtifactCommitFailure {
                    failure_class: FailureClass::CommitFailure,
                    error_code: error.error_code(),
                    message: error.to_string(),
                    finished_at: now,
                },
                error.to_string(),
            )
            .await?;
    }
    voom_store::repo::media::audio_extract_operations::SqliteAudioExtractOperationRepo::mark_recovery_required_in_tx(tx, operation_id, claim,
            &voom_store::repo::media::audio_extract_operations::AudioExtractRecoveryFailure { error_code: error.error_code().as_str().to_owned(), message: error.to_string() }, now).await?;
    Ok(())
}

async fn read_extraction_members_in_tx(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    operation: &voom_store::repo::media::audio_extract_operations::AudioExtractOperationRecord,
) -> Result<Vec<(ArtifactCommitIntent, ArtifactCommitRecord)>, VoomError> {
    let mut members = Vec::with_capacity(operation.outputs.len());
    for output in &operation.outputs {
        let record_id = output.commit_record_id.ok_or_else(|| {
            VoomError::database("prepared extraction output has no commit".to_owned())
        })?;
        let record = cp
            .artifacts
            .get_commit_record_in_tx(tx, record_id)
            .await?
            .ok_or_else(|| VoomError::database("extraction recovery commit missing".to_owned()))?;
        let intent = cp
            .artifact_commit_intents
            .get_by_commit_record_in_tx(tx, record_id)
            .await?
            .ok_or_else(|| VoomError::database("extraction recovery intent missing".to_owned()))?;
        crate::artifact::publication::validate_verification_evidence_in_tx(
            cp,
            tx,
            intent.verification_id,
            record.id,
        )
        .await?;
        members.push((intent, record));
    }
    Ok(members)
}

/// Classify a `recovery_required` intent from its receipts.
async fn classify_recovery_required(
    cp: &ControlPlane,
    record: ArtifactCommitRecord,
    intent: ArtifactCommitIntent,
) -> Result<CommitArtifactReport, VoomError> {
    let applied = [
        intent.receipt.as_ref(),
        intent.supplemental_receipt.as_ref(),
    ]
    .into_iter()
    .flatten()
    .find_map(|receipt| match receipt {
        CommitReceipt::Applied(applied) => Some(applied.clone()),
        _ => None,
    });
    if let Some(applied) = applied {
        if applied.observed.size_bytes != intent.expected_facts.size_bytes
            || applied.observed.content_hash != intent.expected_facts.content_hash
        {
            return operator_required(&record, &intent);
        }
        return finalize_recovered(cp, &record, &intent).await;
    }
    if let Some(CommitReceipt::OutcomeUnknown(supplemental)) = &intent.supplemental_receipt {
        // Positive not-applied evidence from the current owner's read-only
        // re-observation resolves the ambiguity: re-drive a fresh generation.
        if supplemental.reason == RESOLVED_NOT_APPLIED_REASON {
            return abort_and_reprepare_report(cp, &record, &intent, intent.intent_epoch).await;
        }
    }
    operator_required(&record, &intent)
}

/// Recovery-driven finalization: re-run the lineage safety gate, revalidate
/// the pinned scope for its authorized owner (moved ownership is drift), then
/// converge the intent (result version/location, retire staging, mark
/// committed + completed) in one transaction — no further byte mutation.
async fn finalize_recovered(
    cp: &ControlPlane,
    record: &ArtifactCommitRecord,
    intent: &ArtifactCommitIntent,
) -> Result<CommitArtifactReport, VoomError> {
    let mut tx = begin_read_then_write(&cp.pool, "recovery: finalize_recovered").await?;
    let now = cp.clock().now();
    guard_intent_scope_in_tx(cp, &mut tx, intent, intent.owner_node_id).await?;
    let source_asset_id =
        crate::artifact::commit::intent::intent_source_asset_id(cp, &mut tx, intent).await?;
    let gate_evaluated_lease_ids = evaluate_commit_safety_gate(
        cp,
        &mut tx,
        source_asset_id,
        intent.source_file_version_id,
        now,
    )
    .await?;
    let outcome = crate::artifact::commit::intent::converge_intent_in_tx(
        cp,
        &mut tx,
        intent,
        record,
        now,
        gate_evaluated_lease_ids,
    )
    .await?;
    commit_tx(tx).await?;
    Ok(CommitArtifactReport {
        commit_record_id: outcome.commit_record_id,
        artifact_handle_id: record.artifact_handle_id,
        verification_id: record.verification_id,
        target_path: std::path::PathBuf::from(&record.target_path),
        temp_path: record.temp_path.as_ref().map(std::path::PathBuf::from),
        state: ArtifactCommitState::Committed,
        result_file_version_id: outcome.result_file_version_id,
        result_file_location_id: outcome.result_file_location_id,
        recovery_required: None,
    })
}

/// Abort a classified intent and prepare a fresh successor generation in one
/// transaction, so the abort never commits without its successor. The
/// abort only lands when the intent is unchanged since the classification
/// snapshot: a receipt or supplemental receipt journaled in between bumps
/// `intent_epoch`, and the abort fails closed for a fresh `recover_commit`
/// classification instead of overriding a live node's mutation gate (the
/// epoch compare-and-set alone cannot see that, because it matches on the
/// re-read row's already-bumped epoch).
pub(super) async fn abort_and_reprepare_report(
    cp: &ControlPlane,
    record: &ArtifactCommitRecord,
    intent: &ArtifactCommitIntent,
    classified_intent_epoch: u64,
) -> Result<CommitArtifactReport, VoomError> {
    let now: OffsetDateTime = cp.clock().now();
    let mut tx = begin_read_then_write(&cp.pool, "recovery: abort_and_reprepare_report").await?;
    let current = cp
        .artifact_commit_intents
        .require_intent_in_tx(&mut tx, intent.id)
        .await?;
    if current.intent_epoch != classified_intent_epoch {
        return Err(VoomError::Conflict(format!(
            "commit intent {} changed under recovery classification (now {} at epoch {}, \
             classified at epoch {}); re-run recovery",
            intent.id,
            current.state.as_str(),
            current.intent_epoch,
            classified_intent_epoch
        )));
    }
    cp.artifact_commit_intents
        .mark_aborted_in_tx(&mut tx, intent.id, now)
        .await?;
    cp.artifacts
        .mark_commit_failed_in_tx(
            &mut tx,
            record.id,
            ArtifactCommitFailure {
                failure_class: FailureClass::CommitFailure,
                error_code: ErrorCode::CommitFailure,
                message: format!(
                    "commit intent {} classified not-started; aborted for a fresh generation",
                    intent.id
                ),
                finished_at: now,
            },
        )
        .await?;
    // Abort releases the staged bytes' rooted address (spec step 1: retired at
    // finalize or abort).
    let location = cp
        .identity
        .get_file_location_in_tx(&mut tx, intent.staging_location_id)
        .await?
        .filter(|location| location.retired_at.is_none());
    if let Some(location) = location {
        cp.identity
            .retire_file_location_in_tx(&mut tx, intent.staging_location_id, now, location.epoch)
            .await?;
    }
    // The successor prepares on the same transaction: if it fails, dropping
    // the transaction rolls the abort back and the classified attempt stays
    // recoverable (#665).
    let prepared = prepare_commit_in_tx(
        cp,
        &mut tx,
        CommitArtifactInput {
            artifact_handle_id: record.artifact_handle_id,
            target_path: std::path::PathBuf::from(&record.target_path),
            placement_intent: record.placement_intent,
        },
        now,
    )
    .await
    .map_err(|error| match error {
        PrepareCommitError::PreMutation(report) => VoomError::CommitFailure(report.message),
        PrepareCommitError::AfterPending(error) => VoomError::CommitFailure(error.to_string()),
    })?;
    commit_tx(tx).await?;
    Ok(CommitArtifactReport {
        commit_record_id: prepared.record.id,
        artifact_handle_id: prepared.record.artifact_handle_id,
        verification_id: prepared.record.verification_id,
        target_path: prepared.finalize.target_path.clone(),
        temp_path: None,
        state: prepared.record.state,
        result_file_version_id: None,
        result_file_location_id: None,
        recovery_required: None,
    })
}

fn operator_required(
    record: &ArtifactCommitRecord,
    intent: &ArtifactCommitIntent,
) -> Result<CommitArtifactReport, VoomError> {
    Err(VoomError::Conflict(format!(
        "commit {} requires an operator: intent {} carries receipt {:?} and supplemental \
         receipt {:?} against expected facts {:?}; the record stays recovery_required",
        record.id,
        intent.id,
        intent.receipt.as_ref().map(CommitReceipt::kind_str),
        intent
            .supplemental_receipt
            .as_ref()
            .map(CommitReceipt::kind_str),
        intent.expected_facts,
    )))
}
/// A test/ops hook failed after the durable prepare: nothing has mutated and
/// the intent is still pending, so both rows terminate as a clean failure
/// (`aborted` intent, `failed` record) and the staged-bytes address is
/// released.
pub(super) async fn abort_prepared_after_hook_failure(
    cp: &ControlPlane,
    prepared: &crate::artifact::commit::PreparedCommit,
    err: VoomError,
) -> Result<CommitArtifactReport, crate::artifact::commit::CommitArtifactCommandError> {
    let now = cp.clock().now();
    let mut tx = begin_write_first(&cp.pool, "recovery: abort_prepared_after_hook_failure").await?;
    let _ = cp
        .artifact_commit_intents
        .mark_aborted_in_tx(&mut tx, prepared.intent_id, now)
        .await;
    cp.artifacts
        .mark_commit_failed_in_tx(
            &mut tx,
            prepared.record.id,
            ArtifactCommitFailure {
                failure_class: FailureClass::CommitFailure,
                error_code: err.error_code(),
                message: err.to_string(),
                finished_at: now,
            },
        )
        .await?;
    commit_tx(tx).await?;
    Ok(durable_report(cp, prepared.record.id).await?)
}

/// Rebuild a report from the durable record only — the driver is byte-blind.
pub(super) async fn durable_report(
    cp: &ControlPlane,
    record_id: voom_core::ids::ArtifactCommitRecordId,
) -> Result<CommitArtifactReport, VoomError> {
    let record = cp
        .artifacts
        .get_commit_record(record_id)
        .await?
        .ok_or_else(|| VoomError::NotFound(format!("artifact_commit_records {record_id}")))?;
    let target_path = std::path::PathBuf::from(&record.target_path);
    let recovery = (record.state == ArtifactCommitState::RecoveryRequired)
        .then(|| durable_recovery_report(&record));
    Ok(super::finalize::report_from_record(
        &record,
        &target_path,
        recovery,
    ))
}

/// Durable recovery evidence for the report payload. Existence flags describe
/// only what the record claims; live observation stays with inspection.
pub(super) fn durable_recovery_report(
    record: &ArtifactCommitRecord,
) -> super::CommitRecoveryReport {
    super::CommitRecoveryReport {
        recovery_reason: record.recovery_reason.clone().unwrap_or_default(),
        target_path: std::path::PathBuf::from(&record.target_path),
        target_exists: false,
        temp_path: record.temp_path.as_ref().map(std::path::PathBuf::from),
        temp_exists: false,
        staging_path: std::path::PathBuf::new(),
        staging_exists: false,
        result_file_version_id: record.result_file_version_id,
        result_file_location_id: record.result_file_location_id,
    }
}

async fn classify_extraction_members_in_tx(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    operation: &voom_store::repo::media::audio_extract_operations::AudioExtractOperationRecord,
    members: &[(ArtifactCommitIntent, ArtifactCommitRecord)],
    now: OffsetDateTime,
) -> Result<bool, VoomError> {
    let mut unresolved = false;
    for ((intent, record), output) in members.iter().zip(&operation.outputs) {
        if !matches!(
            intent.state,
            ArtifactCommitIntentState::Authorized | ArtifactCommitIntentState::RecoveryRequired
        ) || output.verification_id != Some(intent.verification_id)
            || output.artifact_handle_id != Some(intent.artifact_handle_id)
            || record.source_file_version_id != operation.operation.source_file_version_id
        {
            return Err(VoomError::Conflict(
                "extraction recovery member is not fully applied; operator classification required"
                    .to_owned(),
            ));
        }
        let matching_applied = [
            intent.receipt.as_ref(),
            intent.supplemental_receipt.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|receipt| {
            matches!(receipt, CommitReceipt::Applied(applied)
                if applied.observed.size_bytes == intent.expected_facts.size_bytes
                    && applied.observed.content_hash == intent.expected_facts.content_hash)
        });
        if !matching_applied {
            unresolved = true;
        }
        guard_intent_scope_in_tx(cp, tx, intent, intent.owner_node_id).await?;
        let source_asset =
            crate::artifact::commit::intent::intent_source_asset_id(cp, tx, intent).await?;
        evaluate_commit_safety_gate(cp, tx, source_asset, intent.source_file_version_id, now)
            .await?;
    }
    Ok(unresolved)
}
