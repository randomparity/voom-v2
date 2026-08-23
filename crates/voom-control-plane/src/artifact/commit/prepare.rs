//! Prepare leg of the fenced commit driver (ADR 0074): read source facts,
//! resolve the rooted target, evaluate the lineage safety gate, create the
//! pending commit record, the rooted staging `file_locations` row addressing
//! the staged bytes, and the pending `artifact_commit_intents` row — all in
//! one transaction. Expected facts come from the pinned
//! [`ArtifactVerification`]; the control plane never observes staged bytes.

use std::path::{Path, PathBuf};

use serde_json::json;
use voom_core::ids::ArtifactVerificationId;
use voom_core::{ArtifactHandleId, FileAssetId, FileVersionId, NodeId, StorageRootId, VoomError};
use voom_events::Event;
use voom_events::payload::{ArtifactCommitFailedPreMutationPayload, ArtifactCommitStartedPayload};
use voom_store::repo::library::library_roots::LibraryRoot;
use voom_store::repo::media::artifact_commit_intents::NewArtifactCommitIntent;
use voom_store::repo::media::artifacts::{
    ArtifactExpectedFacts, ArtifactLocationKind, ArtifactVerification, LiveArtifactLocation,
    NewArtifactCommitRecord,
};
use voom_store::repo::media::commit_safety_gate::check_lineage_commit_leases_in_tx;
use voom_store::repo::media::identity::{FileLocationRepo, FileVersionRepo, NewFileLocation};

use voom_artifact::commit_pipeline::{
    PendingCommitRecordError, append_commit_event_in_tx,
    create_pending_commit_with_started_event_in_tx,
};

use crate::ControlPlane;
use crate::artifact::commit::{
    CommitArtifactCommandError, CommitArtifactInput, CommitArtifactPreMutationReport,
    PreparedCommit,
};
use crate::cases::{begin_immediate_tx, commit_tx};

pub(super) async fn prepare_commit(
    cp: &ControlPlane,
    input: CommitArtifactInput,
) -> Result<PreparedCommit, CommitArtifactCommandError> {
    let mut tx = begin_immediate_tx(&cp.pool).await?;
    let now = cp.clock().now();
    let prepared_result = prepare_commit_in_tx(cp, &mut tx, input, now).await;
    match prepared_result {
        Ok(prepared) => {
            commit_tx(tx).await?;
            Ok(prepared)
        }
        Err(PrepareCommitError::PreMutation(failure)) => {
            append_failed_pre_mutation(cp, &mut tx, &failure, now).await?;
            commit_tx(tx).await?;
            Err(CommitArtifactCommandError::pre_mutation(failure))
        }
        Err(PrepareCommitError::AfterPending(err)) => Err(err.into()),
    }
}

#[derive(Debug)]
pub(super) enum PrepareCommitError {
    PreMutation(CommitArtifactPreMutationReport),
    AfterPending(VoomError),
}

async fn prepare_commit_in_tx(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: CommitArtifactInput,
    now: time::OffsetDateTime,
) -> Result<PreparedCommit, PrepareCommitError> {
    let inputs = read_prepare_inputs(cp, tx, &input, now).await?;

    // The staged bytes must be addressable the way the storage-owner node will
    // read them: a rooted address under an active local root owned by the same
    // node that owns the target root (spec step 1).
    let staged_path = PathBuf::from(&inputs.verified_staging.staging.value);
    let scope = agree_commit_scope(
        cp,
        tx,
        &staged_path,
        &inputs.context,
        inputs.target_storage_root_id,
        inputs.target_relative_locator.clone(),
        inputs.resolved_target_path.clone(),
    )
    .await?;

    // Pin where the staged bytes come from: the source file version's single
    // live rooted location (ADR 0075). Byte-free on purpose — identity rows
    // only, no stat/canonicalize; the node resolves this handle against its
    // own bound roots when it materializes staging during `applying`.
    let source_location =
        crate::operation_source::select_location(cp, inputs.source.source_file_version_id, None)
            .await
            .map_err(|err| pre_mutation_error(&inputs.context, &err))?;
    let (source_storage_root_id, source_locator) = source_location
        .rooted_address()
        .map_err(|err| pre_mutation_error(&inputs.context, &err))?;

    let draft = PendingIntentDraft {
        artifact_handle_id: input.artifact_handle_id,
        source_file_version_id: inputs.source.source_file_version_id,
        verification_id: inputs.verified_staging.verification.id,
        expected_facts: inputs.expected_facts,
        source_storage_root_id,
        source_provider_relative_locator: source_locator.clone(),
        context: inputs.verified_staging.context.clone(),
    };
    let record = create_prepared_record(cp, tx, &draft, &staged_path, &scope, now).await?;
    let pinned = pin_staging_and_record_intent(cp, tx, &draft, record.id, scope, now).await?;

    let prepared_record_id = record.id;
    Ok(PreparedCommit {
        record,
        intent_id: pinned.intent_id,
        artifact_handle_id: input.artifact_handle_id,
        finalize: super::CommitFinalizeInput {
            record_id: prepared_record_id,
            artifact_handle_id: input.artifact_handle_id,
            source_file_asset_id: inputs.source.source_file_asset_id,
            source_file_version_id: inputs.source.source_file_version_id,
            staging_artifact_location_id: inputs.verified_staging.staging.id,
            staging_file_location: Some((
                pinned.staging_location_id,
                pinned.staging_location_epoch,
            )),
            target_storage_root_id: inputs.target_storage_root_id,
            target_relative_locator: inputs.target_relative_locator,
            target_path: inputs.resolved_target_path,
            promotion_started_at: now,
            gate_evaluated_lease_ids: inputs.gate_evaluated_lease_ids,
        },
    })
}

/// Create the pending commit record plus its started event, carrying the
/// prepared report that names both rooted addresses and the pinned facts.
///
/// # Errors
/// Pre-mutation failures before the row exists; `AfterPending` after.
async fn create_prepared_record(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    draft: &PendingIntentDraft,
    staged_path: &Path,
    scope: &AgreedCommitScope,
    now: time::OffsetDateTime,
) -> Result<voom_store::repo::media::artifacts::ArtifactCommitRecord, PrepareCommitError> {
    let resolved_target_path = &scope.resolved_target_path;
    let pending_input = NewArtifactCommitRecord {
        artifact_handle_id: draft.artifact_handle_id,
        source_file_version_id: draft.source_file_version_id,
        verification_id: draft.verification_id,
        target_path: resolved_target_path.display().to_string(),
        temp_path: None,
        report: json!({
            "phase": "prepared",
            "staging_path": staged_path.display().to_string(),
            "target_path": resolved_target_path.display().to_string(),
            "expected_size_bytes": draft.expected_facts.size_bytes,
            "expected_checksum": draft.expected_facts.content_hash,
            "rooted_target": {
                "storage_root_id": scope.target_storage_root_id.0,
                "provider_relative_locator": scope.target_locator.as_str(),
            },
            "staging_rooted_address": {
                "storage_root_id": scope.staging_root_id.0,
                "provider_relative_locator": scope.staging_locator.as_str(),
            },
        }),
        started_at: now,
    };
    let record = create_pending_commit_with_started_event_in_tx(
        &cp.artifacts,
        &cp.events,
        tx,
        pending_input,
        |commit_record_id| {
            Event::ArtifactCommitStarted(ArtifactCommitStartedPayload {
                commit_record_id,
                artifact_handle_id: draft.artifact_handle_id,
                source_file_version_id: draft.source_file_version_id,
                verification_id: draft.verification_id,
                target_path: resolved_target_path.display().to_string(),
                temp_path: String::new(),
            })
        },
    )
    .await
    .map_err(|err| match err {
        PendingCommitRecordError::BeforePending(err) => {
            PrepareCommitError::PreMutation(pre_mutation(&draft.context, &err))
        }
        PendingCommitRecordError::AfterPending(err) => PrepareCommitError::AfterPending(err),
    })?;
    Ok(record)
}

/// Everything prepare reads before any durable mutation: source facts, the
/// resolved rooted target, verified staging facts, pinned expected facts, and
/// the commit safety gate verdict.
struct PreparedInputs {
    context: PreMutationContext,
    source: CommitSourceFacts,
    verified_staging: VerifiedStagingFacts,
    expected_facts: voom_store::repo::media::artifact_commit_intents::CommitExpectedFacts,
    gate_evaluated_lease_ids: Vec<voom_core::UseLeaseId>,
    target_storage_root_id: StorageRootId,
    target_relative_locator: voom_core::ProviderRelativeLocator,
    resolved_target_path: PathBuf,
}

/// Read every pre-mutation input for one prepare generation.
///
/// # Errors
/// Pre-mutation failures for unreadable sources, unresolved targets,
/// unverifiable staging, or a fail-closed gate error.
async fn read_prepare_inputs(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: &CommitArtifactInput,
    now: time::OffsetDateTime,
) -> Result<PreparedInputs, PrepareCommitError> {
    let context = PreMutationContext {
        artifact_handle_id: input.artifact_handle_id,
        verification_id: None,
        target_path: input.target_path.clone(),
    };
    let source = read_commit_source_facts(cp, tx, input.artifact_handle_id, &context).await?;
    let (target_storage_root_id, target_relative_locator, resolved_target_path) =
        crate::operation_source::resolve_artifact_target(
            cp,
            "artifact commit",
            source.source_storage_root_id,
            &input.target_path,
        )
        .await
        .map_err(|err| pre_mutation_error(&context, &err))?;
    let verified_staging = read_verified_staging_facts(
        cp,
        tx,
        input.artifact_handle_id,
        &input.target_path,
        &context,
    )
    .await?;
    // Expected facts are pinned by the successful verification — the control
    // plane never re-observes the staged bytes here (ADR 0074).
    let expected_facts =
        expected_facts_from_verification(&source.handle, &verified_staging.verification)
            .map_err(|err| pre_mutation_error(&verified_staging.context, &err))?;
    // Commit safety gate: a blocking use lease live at commit time on the
    // affected scope fails the commit here, before any durable authorization
    // exists. Any gate-check error is fail-closed — the commit does not
    // proceed.
    let gate_evaluated_lease_ids =
        check_commit_safety_gate(cp, tx, &source, &verified_staging.context, now).await?;
    Ok(PreparedInputs {
        context,
        source,
        verified_staging,
        expected_facts,
        gate_evaluated_lease_ids,
        target_storage_root_id,
        target_relative_locator,
        resolved_target_path,
    })
}

/// The durable identity of one prepared commit generation's staged bytes.
#[derive(Debug)]
struct PinnedStagingIntent {
    staging_location_id: voom_core::ids::FileLocationId,
    staging_location_epoch: u64,
    intent_id: voom_core::ids::ArtifactCommitIntentId,
}

/// The rooted scope agreed between staging and target roots (spec step 1):
/// both addressable under active roots owned by the same node.
struct AgreedCommitScope {
    staging_root_id: StorageRootId,
    staging_locator: voom_core::ProviderRelativeLocator,
    owner_node_id: NodeId,
    target_storage_root_id: StorageRootId,
    target_root_epoch: u64,
    target_locator: voom_core::ProviderRelativeLocator,
    resolved_target_path: PathBuf,
}

/// Identity fields the pending intent pins from the verified source.
struct PendingIntentDraft {
    artifact_handle_id: ArtifactHandleId,
    source_file_version_id: FileVersionId,
    verification_id: ArtifactVerificationId,
    expected_facts: voom_store::repo::media::artifact_commit_intents::CommitExpectedFacts,
    /// Where the staged bytes come from: the source file version's live
    /// rooted address, pinned byte-free at prepare (ADR 0075).
    source_storage_root_id: StorageRootId,
    source_provider_relative_locator: voom_core::ProviderRelativeLocator,
    context: PreMutationContext,
}

/// Resolve the staged bytes' rooted address and require the staging root and
/// target root to share one active owner node (spec step 1).
///
/// # Errors
/// Pre-mutation failures for unresolvable addresses or disagreeing owners.
async fn agree_commit_scope(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    staged_path: &Path,
    context: &PreMutationContext,
    inputs_target_storage_root_id: StorageRootId,
    target_locator: voom_core::ProviderRelativeLocator,
    resolved_target_path: PathBuf,
) -> Result<AgreedCommitScope, PrepareCommitError> {
    let (staging_root_id, staging_locator, staging_root_owner) =
        resolve_staging_rooted_address(cp, staged_path, context).await?;
    let target_root = cp
        .libraries
        .get_library_root_in_tx(tx, inputs_target_storage_root_id)
        .await
        .map_err(|err| pre_mutation_error(context, &err))?
        .ok_or_else(|| {
            pre_mutation_error(
                context,
                &VoomError::NotFound(format!("library_roots {inputs_target_storage_root_id}")),
            )
        })?;
    let Some(owner_node_id) = staging_root_owner else {
        return Err(pre_mutation_error(
            context,
            &VoomError::Config(format!(
                "staging root {staging_root_id} has no owner node; the staged bytes cannot be \
                 committed through a fenced intent"
            )),
        ));
    };
    if Some(owner_node_id) != target_root.owner_node_id {
        return Err(pre_mutation_error(
            context,
            &VoomError::Config(format!(
                "staging root {staging_root_id} owner {owner_node_id} and target root \
                 {inputs_target_storage_root_id} owner {:?} must resolve to the same active \
                 local node",
                target_root.owner_node_id
            )),
        ));
    }
    Ok(AgreedCommitScope {
        staging_root_id,
        staging_locator,
        owner_node_id,
        target_storage_root_id: inputs_target_storage_root_id,
        target_root_epoch: target_root.root_epoch,
        target_locator,
        resolved_target_path,
    })
}

/// Create the staged bytes' rooted `file_locations` pin, the pending intent
/// row, and the `ArtifactCommitIntentRecorded` event — in one transaction.
///
/// # Errors
/// `AfterPending` storage failures once the commit record exists.
async fn pin_staging_and_record_intent(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    draft: &PendingIntentDraft,
    record_id: voom_core::ids::ArtifactCommitRecordId,
    scope: AgreedCommitScope,
    now: time::OffsetDateTime,
) -> Result<PinnedStagingIntent, PrepareCommitError> {
    let staging_location = cp
        .identity
        .create_file_location_in_tx(
            tx,
            NewFileLocation {
                file_version_id: draft.source_file_version_id,
                storage_root_id: scope.staging_root_id,
                provider_relative_locator: scope.staging_locator.clone(),
                proof: None,
                observed_at: now,
            },
        )
        .await
        .map_err(PrepareCommitError::AfterPending)?;
    let intent = cp
        .artifact_commit_intents
        .create_pending_in_tx(
            tx,
            NewArtifactCommitIntent {
                commit_record_id: record_id,
                artifact_handle_id: draft.artifact_handle_id,
                source_file_version_id: draft.source_file_version_id,
                verification_id: draft.verification_id,
                source_storage_root_id: draft.source_storage_root_id,
                source_provider_relative_locator: draft.source_provider_relative_locator.clone(),
                staging_location_id: staging_location.id,
                staging_location_epoch: staging_location.epoch,
                target_storage_root_id: scope.target_storage_root_id,
                target_root_epoch: scope.target_root_epoch,
                target_provider_relative_locator: scope.target_locator.as_str().to_owned(),
                owner_node_id: scope.owner_node_id,
                expected_facts: draft.expected_facts.clone(),
                requested_at: now,
            },
        )
        .await
        .map_err(PrepareCommitError::AfterPending)?;
    append_commit_event_in_tx(
        &cp.events,
        tx,
        draft.artifact_handle_id,
        now,
        Event::ArtifactCommitIntentRecorded(
            voom_events::payload::ArtifactCommitIntentRecordedPayload {
                commit_record_id: record_id,
                artifact_handle_id: draft.artifact_handle_id,
                verification_id: draft.verification_id,
                owner_node_id: intent.owner_node_id,
                target_root_id: scope.target_storage_root_id,
                target_provider_relative_locator: scope.target_locator.as_str().to_owned(),
                started_at: now,
            },
        ),
    )
    .await
    .map_err(PrepareCommitError::AfterPending)?;
    Ok(PinnedStagingIntent {
        staging_location_id: staging_location.id,
        staging_location_epoch: staging_location.epoch,
        intent_id: intent.id,
    })
}

/// Consult the commit safety gate for a lineage commit. Returns the use-lease
/// ids the gate evaluated (for the audit event) when no blocking lease is
/// live, or a pre-mutation failure when one is (`BlockedByUseLease`) or when
/// the check itself fails (fail-closed).
async fn check_commit_safety_gate(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    source: &CommitSourceFacts,
    context: &PreMutationContext,
    now: time::OffsetDateTime,
) -> Result<Vec<voom_core::UseLeaseId>, PrepareCommitError> {
    evaluate_commit_safety_gate(
        cp,
        tx,
        source.source_file_asset_id,
        source.source_file_version_id,
        now,
    )
    .await
    .map_err(|err| pre_mutation_error(context, &err))
}

/// Run the lineage commit safety gate on the caller's transaction. Returns the
/// use-lease ids the gate evaluated when no blocking lease is live,
/// `BlockedByUseLease` when one is, or the underlying storage error (fail-closed)
/// when the check itself fails. Callers treat any error as fail-closed — the
/// commit must not proceed.
pub(crate) async fn evaluate_commit_safety_gate(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    source_file_asset_id: FileAssetId,
    source_file_version_id: FileVersionId,
    now: time::OffsetDateTime,
) -> Result<Vec<voom_core::UseLeaseId>, VoomError> {
    let check = check_lineage_commit_leases_in_tx(
        tx,
        &cp.identity,
        source_file_asset_id,
        source_file_version_id,
        now,
    )
    .await?;
    if let Some((lease_id, scope)) = check.blocking {
        return Err(VoomError::BlockedByUseLease(format!(
            "commit blocked by active use lease {lease_id} on {} {}",
            scope.type_str(),
            scope.id_u64()
        )));
    }
    Ok(check.evaluated_lease_ids)
}

#[derive(Debug)]
pub(super) struct CommitSourceFacts {
    pub(super) handle: ArtifactExpectedFacts,
    pub(super) source_file_version_id: FileVersionId,
    pub(super) source_file_asset_id: FileAssetId,
    pub(super) source_storage_root_id: StorageRootId,
}

#[derive(Debug)]
pub(super) struct VerifiedStagingFacts {
    pub(super) staging: LiveArtifactLocation,
    pub(super) verification: ArtifactVerification,
    pub(super) context: PreMutationContext,
}

pub(super) async fn read_commit_source_facts(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    artifact_handle_id: ArtifactHandleId,
    context: &PreMutationContext,
) -> Result<CommitSourceFacts, PrepareCommitError> {
    let handle = cp
        .artifacts
        .require_expected_facts_in_tx(tx, artifact_handle_id)
        .await
        .map_err(|err| pre_mutation_error(context, &err))?;
    let Some(source_file_version_id) = handle.source_file_version_id else {
        return Err(pre_mutation_error(
            context,
            &VoomError::Config(format!(
                "artifact_handle {artifact_handle_id} is not linked to a source file_version"
            )),
        ));
    };
    let Some(source) = cp
        .identity
        .get_file_version_in_tx(tx, source_file_version_id)
        .await
        .map_err(|err| pre_mutation_error(context, &err))?
    else {
        return Err(pre_mutation_error(
            context,
            &VoomError::NotFound(format!("file_versions {source_file_version_id} missing")),
        ));
    };
    if source.retired_at.is_some() {
        return Err(pre_mutation_error(
            context,
            &VoomError::Config(format!("file_versions {source_file_version_id} is retired")),
        ));
    }
    let source_location_id = handle.source_file_location_id.ok_or_else(|| {
        pre_mutation_error(
            context,
            &VoomError::Config(format!(
                "artifact_handle {artifact_handle_id} has no source file_location"
            )),
        )
    })?;
    let source_location = cp
        .identity
        .get_file_location_in_tx(tx, source_location_id)
        .await
        .map_err(|err| pre_mutation_error(context, &err))?
        .ok_or_else(|| {
            pre_mutation_error(
                context,
                &VoomError::NotFound(format!("file_location {source_location_id}")),
            )
        })?;
    if source_location.file_version_id != source_file_version_id
        || source_location.retired_at.is_some()
    {
        return Err(pre_mutation_error(
            context,
            &VoomError::Config(format!(
                "artifact_handle {artifact_handle_id} source location {source_location_id} \
                 is not live on file_version {source_file_version_id}"
            )),
        ));
    }
    let (source_storage_root_id, _) = source_location
        .rooted_address()
        .map_err(|err| pre_mutation_error(context, &err))?;
    Ok(CommitSourceFacts {
        handle,
        source_file_version_id,
        source_file_asset_id: source.file_asset_id,
        source_storage_root_id,
    })
}

pub(super) async fn read_verified_staging_facts(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    artifact_handle_id: ArtifactHandleId,
    target_path: &Path,
    context: &PreMutationContext,
) -> Result<VerifiedStagingFacts, PrepareCommitError> {
    let staging = cp
        .artifacts
        .live_location_of_kind_in_tx(tx, artifact_handle_id, ArtifactLocationKind::Staging)
        .await
        .map_err(|err| pre_mutation_error(context, &err))?
        .ok_or_else(|| {
            pre_mutation_error(
                context,
                &VoomError::Config(format!(
                    "artifact_handle {artifact_handle_id} must have exactly one live staging \
                     location; found 0"
                )),
            )
        })?;
    let Some(verification) = cp
        .artifacts
        .latest_successful_verification_for_live_staging_in_tx(tx, artifact_handle_id)
        .await
        .map_err(|err| pre_mutation_error(context, &err))?
    else {
        return Err(pre_mutation_error(
            context,
            &VoomError::Config(format!(
                "artifact_handle {artifact_handle_id} has no successful verification for its live staging location"
            )),
        ));
    };
    let context = PreMutationContext {
        artifact_handle_id,
        verification_id: Some(verification.id),
        target_path: target_path.to_owned(),
    };
    if verification.artifact_location_id != staging.id || verification.path != staging.value {
        return Err(pre_mutation_error(
            &context,
            &VoomError::Config(format!(
                "artifact verification {} is stale for live staging location {}",
                verification.id, staging.id
            )),
        ));
    }

    Ok(VerifiedStagingFacts {
        staging,
        verification,
        context,
    })
}

/// Pin the expected facts from the successful verification. Every layer the
/// prior host observation cross-checked must still agree: handle facts,
/// verification expectations, and the verification's own observations.
pub(super) fn expected_facts_from_verification(
    handle: &ArtifactExpectedFacts,
    verification: &ArtifactVerification,
) -> Result<voom_store::repo::media::artifact_commit_intents::CommitExpectedFacts, VoomError> {
    if handle.size_bytes != verification.expected_size_bytes
        || handle.checksum != verification.expected_checksum
        || verification.observed_size_bytes != Some(verification.expected_size_bytes)
        || verification.observed_checksum.as_deref()
            != Some(verification.expected_checksum.as_str())
    {
        return Err(VoomError::ArtifactChecksumMismatch(
            "successful verification facts disagree with the artifact handle".to_owned(),
        ));
    }
    Ok(
        voom_store::repo::media::artifact_commit_intents::CommitExpectedFacts {
            size_bytes: verification.expected_size_bytes,
            content_hash: verification.expected_checksum.clone(),
        },
    )
}

/// Resolve which active local storage root contains the staged bytes, and the
/// staged bytes' provider-relative locator within it. Metadata-only: paths are
/// canonicalized but bytes are never opened.
async fn resolve_staging_rooted_address(
    cp: &ControlPlane,
    staged_path: &Path,
    context: &PreMutationContext,
) -> Result<
    (
        StorageRootId,
        voom_core::ProviderRelativeLocator,
        Option<NodeId>,
    ),
    PrepareCommitError,
> {
    let roots = cp
        .list_library_roots(None)
        .await
        .map_err(|err| pre_mutation_error(context, &err))?;
    let mut best: Option<(usize, &LibraryRoot)> = None;
    for root in &roots {
        if !matches!(root.state, voom_core::StorageRootState::Active) {
            continue;
        }
        let Ok(root_path) = tokio::fs::canonicalize(root.provider_locator.as_str()).await else {
            continue;
        };
        let Ok(relative) = staged_path.strip_prefix(&root_path) else {
            continue;
        };
        let depth = relative.components().count();
        if best.is_none_or(|(best_depth, _)| depth > best_depth) {
            best = Some((depth, root));
        }
    }
    let Some((_, root)) = best else {
        return Err(pre_mutation_error(
            context,
            &VoomError::Config(format!(
                "staged artifact {} is not inside any active storage root",
                staged_path.display()
            )),
        ));
    };
    let root_path = tokio::fs::canonicalize(root.provider_locator.as_str())
        .await
        .map_err(|err| {
            pre_mutation_error(
                context,
                &VoomError::ArtifactUnavailable(format!(
                    "cannot resolve staging storage root {}: {err}",
                    root.id
                )),
            )
        })?;
    let relative = staged_path
        .strip_prefix(&root_path)
        .map_err(|_| {
            pre_mutation_error(
                context,
                &VoomError::database(format!(
                    "staged path {} escaped storage root {} after prefix match",
                    staged_path.display(),
                    root.id
                )),
            )
        })?
        .to_str()
        .ok_or_else(|| {
            pre_mutation_error(
                context,
                &VoomError::Config(format!(
                    "staged artifact {} is not valid UTF-8 relative to storage root {}",
                    staged_path.display(),
                    root.id
                )),
            )
        })?
        .to_owned();
    let locator = voom_core::ProviderRelativeLocator::new(relative)
        .map_err(|err| pre_mutation_error(context, &err))?;
    Ok((root.id, locator, root.owner_node_id))
}

fn pre_mutation_error(context: &PreMutationContext, err: &VoomError) -> PrepareCommitError {
    PrepareCommitError::PreMutation(pre_mutation(context, err))
}

#[derive(Debug, Clone)]
pub(super) struct PreMutationContext {
    pub(super) artifact_handle_id: ArtifactHandleId,
    pub(super) verification_id: Option<ArtifactVerificationId>,
    pub(super) target_path: PathBuf,
}

fn pre_mutation(context: &PreMutationContext, err: &VoomError) -> CommitArtifactPreMutationReport {
    CommitArtifactPreMutationReport {
        artifact_handle_id: context.artifact_handle_id,
        verification_id: context.verification_id,
        target_path: context.target_path.clone(),
        error_code: err.error_code(),
        message: err.to_string(),
    }
}

async fn append_failed_pre_mutation(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    failure: &CommitArtifactPreMutationReport,
    occurred_at: time::OffsetDateTime,
) -> Result<(), VoomError> {
    append_commit_event_in_tx(
        &cp.events,
        tx,
        failure.artifact_handle_id,
        occurred_at,
        Event::ArtifactCommitFailedPreMutation(ArtifactCommitFailedPreMutationPayload {
            artifact_handle_id: failure.artifact_handle_id,
            commit_record_id: None,
            target_path: failure.target_path.display().to_string(),
            error_code: failure.error_code.as_str().to_owned(),
            message: failure.message.clone(),
        }),
    )
    .await
}
