//! Audio admission uses the existing semantic operation ledgers before any
//! encoder receives work. Only the owner resolves the resulting rooted paths.

use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{Sqlite, Transaction};
use time::OffsetDateTime;
use voom_core::{FileVersionId, MediaSnapshotId, ProviderRelativeLocator, VoomError};
use voom_store::repo::execution::leases::Lease;
use voom_store::repo::execution::tickets::Ticket;
use voom_store::repo::media::audio_extract_operations::{
    AudioExtractOperationState, NewAudioExtractClaim, NewAudioExtractDispatchAttempt,
    NewAudioExtractOperation, NewAudioExtractOutput, SqliteAudioExtractOperationRepo,
};
use voom_store::repo::media::audio_synthesis_operations::{
    AudioSynthesisOperationState, NewAudioSynthesisClaim, NewAudioSynthesisCompanion,
    NewAudioSynthesisDispatchAttempt, NewAudioSynthesisOperation,
    SqliteAudioSynthesisOperationRepo,
};
use voom_store::repo::media::identity::{FileVersionRepo, MediaSnapshotRepo};
use voom_worker_protocol::{MediaDispatch, MediaPlannedOutput, decode_media_dispatch};

use crate::ControlPlane;
use crate::audio::selection::{
    extract_selection_from_payload_and_snapshot, transcode_selection_from_payload_and_snapshot,
};

pub(crate) enum AudioAdmission {
    Dispatch(Value),
    Committed(Value),
}

pub(crate) async fn admit_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    ticket: &Ticket,
    lease: &Lease,
    now: OffsetDateTime,
) -> Result<AudioAdmission, VoomError> {
    let mut payload = ticket.payload.clone();
    if !matches!(
        ticket.kind.normalize(),
        voom_core::NormalizedTicketOperation::Known {
            kind: voom_core::OperationKind::ExtractAudio | voom_core::OperationKind::TranscodeAudio,
            namespaced: true,
        }
    ) {
        return Ok(AudioAdmission::Dispatch(payload));
    }
    let Some(rendered) = payload.get_mut("rendered_payload") else {
        return Ok(AudioAdmission::Dispatch(payload));
    };
    let Some(encoded) = rendered.get("media_dispatch") else {
        return Ok(AudioAdmission::Dispatch(payload));
    };
    let mut dispatch = decode_media_dispatch(encoded)
        .map_err(|error| VoomError::database(format!("audio admission dispatch: {error}")))?;
    if !matches!(
        dispatch,
        MediaDispatch::ExtractAudio(_) | MediaDispatch::TranscodeAudio(_)
    ) {
        return Ok(AudioAdmission::Dispatch(payload));
    }
    let audio = rendered
        .get("audio")
        .ok_or_else(|| VoomError::database("audio admission lacks policy payload".to_owned()))?;
    let source_id = FileVersionId(super::required_id(rendered, "source_file_version_id")?);
    let snapshot_id = MediaSnapshotId(super::required_id(audio, "source_media_snapshot_id")?);
    let snapshot = cp
        .identity
        .get_media_snapshot_in_tx(tx, snapshot_id)
        .await?
        .ok_or_else(|| VoomError::database("audio admission source snapshot missing".to_owned()))?;
    if snapshot.file_version_id != source_id {
        return Err(VoomError::database(
            "audio admission source snapshot/version disagree".to_owned(),
        ));
    }
    let worker = cp.workers.require_live_in_tx(tx, lease.worker_id).await?;
    let epoch = u32::try_from(worker.epoch)
        .map_err(|error| VoomError::database_context("audio admission worker epoch", error))?;
    let admission = AdmissionInput {
        ticket,
        lease,
        source_id,
        snapshot: &snapshot,
        worker: &worker,
        worker_epoch: epoch,
    };
    let decision = match &mut dispatch {
        MediaDispatch::ExtractAudio(dispatch) => {
            admit_extraction_in_tx(cp, tx, &admission, audio, dispatch, now).await?
        }
        MediaDispatch::TranscodeAudio(dispatch) => {
            admit_synthesis_in_tx(cp, tx, &admission, audio, dispatch, now).await?
        }
        _ => AdmissionDecision::Unchanged,
    };
    match decision {
        AdmissionDecision::Committed(result) => return Ok(AudioAdmission::Committed(result)),
        AdmissionDecision::Unchanged => return Ok(AudioAdmission::Dispatch(payload)),
        AdmissionDecision::Dispatched => {}
    }
    rendered
        .as_object_mut()
        .ok_or_else(|| VoomError::database("rendered media payload is not object".to_owned()))?
        .insert(
            "media_dispatch".to_owned(),
            serde_json::to_value(dispatch).map_err(|error| {
                VoomError::Internal(format!("encode admitted audio dispatch: {error}"))
            })?,
        );
    Ok(AudioAdmission::Dispatch(payload))
}

struct AdmissionInput<'a> {
    ticket: &'a Ticket,
    lease: &'a Lease,
    source_id: FileVersionId,
    snapshot: &'a voom_store::repo::media::identity::MediaSnapshot,
    worker: &'a voom_store::repo::execution::workers::Worker,
    worker_epoch: u32,
}

fn dispatch_key(admission: &AdmissionInput<'_>) -> Result<String, VoomError> {
    let incarnation = admission.worker.node_incarnation_id.ok_or_else(|| {
        VoomError::database("audio dispatch worker lacks owner incarnation".to_owned())
    })?;
    Ok(voom_worker_protocol::owner_dispatch_idempotency_key(
        incarnation,
        admission.lease.id,
    ))
}

enum AdmissionDecision {
    Unchanged,
    Dispatched,
    Committed(Value),
}

async fn admit_extraction_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    admission: &AdmissionInput<'_>,
    audio: &Value,
    dispatch: &mut voom_worker_protocol::MediaExtractAudioDispatch,
    now: OffsetDateTime,
) -> Result<AdmissionDecision, VoomError> {
    let ticket = admission.ticket;
    let lease = admission.lease;
    let worker = admission.worker;
    let epoch = admission.worker_epoch;
    let (operation, root) = plan_extraction_in_tx(cp, tx, admission, audio, dispatch, now).await?;
    let operation_key = operation.operation.operation_key.clone();
    if operation.operation.state == AudioExtractOperationState::Committed {
        return Ok(AdmissionDecision::Committed(
            super::reuse::extraction_in_tx(
                cp,
                tx,
                worker.node_id.ok_or_else(|| {
                    VoomError::database("audio reuse worker has no owner".to_owned())
                })?,
                ticket,
                lease,
                &operation,
            )
            .await?,
        ));
    }
    if operation.operation.state != AudioExtractOperationState::Planned
        || SqliteAudioExtractOperationRepo::get_dispatch_attempt_in_tx(
            tx,
            operation.operation.id,
            operation.operation.dispatch_generation,
        )
        .await?
        .is_some()
    {
        return Err(VoomError::Conflict("extraction already dispatched; reconcile exact prior generation before another encoder".to_owned()));
    }
    let claim = NewAudioExtractClaim {
        operation_key: operation_key.clone(),
        expected_generation: operation.operation.dispatch_generation,
        lease_id: lease.id,
        claim_token: crate::worker_process::random_hex_128(),
        expires_at: lease.expires_at,
    };
    SqliteAudioExtractOperationRepo::acquire_claim_in_tx(tx, &claim, now).await?;
    let token = blake3::hash(operation_key.as_bytes()).to_hex();
    let directory = format!(
        "operation-{}/generation-{}",
        &token[..16],
        claim.expected_generation
    );
    let mut paths = Vec::with_capacity(dispatch.outputs.len());
    for output in &mut dispatch.outputs {
        generation_output(&mut output.output, &directory)?;
        paths.push(
            std::path::Path::new(root.provider_locator.as_str())
                .join(output.output.provider_relative_locator.as_str())
                .to_string_lossy()
                .into_owned(),
        );
    }
    SqliteAudioExtractOperationRepo::record_dispatch_attempt_in_tx(
        tx,
        &claim,
        NewAudioExtractDispatchAttempt {
            worker_id: worker.id,
            worker_epoch: epoch,
            idempotency_key: dispatch_key(admission)?,
            attempt_directory: std::path::Path::new(root.provider_locator.as_str())
                .join(&directory)
                .to_string_lossy()
                .into_owned(),
            paths,
        },
        now,
    )
    .await?;
    Ok(AdmissionDecision::Dispatched)
}

async fn admit_synthesis_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    admission: &AdmissionInput<'_>,
    audio: &Value,
    dispatch: &mut voom_worker_protocol::MediaTranscodeAudioDispatch,
    now: OffsetDateTime,
) -> Result<AdmissionDecision, VoomError> {
    let ticket = admission.ticket;
    let lease = admission.lease;
    let snapshot = admission.snapshot;
    let worker = admission.worker;
    let epoch = admission.worker_epoch;
    let selection = transcode_selection_from_payload_and_snapshot(audio, snapshot)?;
    if !selection.add_track {
        return Ok(AdmissionDecision::Unchanged);
    }
    if selection.selection != dispatch.selection {
        return Err(VoomError::database(
            "synthesis selection differs from dispatch".to_owned(),
        ));
    }
    let (operation, root) =
        plan_synthesis_in_tx(cp, tx, admission, audio, dispatch, selection, now).await?;
    let key = operation.operation.operation_key.clone();
    let token = blake3::hash(key.as_bytes()).to_hex();
    if operation.operation.state == AudioSynthesisOperationState::Committed {
        return Ok(AdmissionDecision::Committed(
            super::reuse::synthesis_in_tx(
                cp,
                tx,
                worker.node_id.ok_or_else(|| {
                    VoomError::database("audio reuse worker has no owner".to_owned())
                })?,
                ticket,
                lease,
                &operation,
            )
            .await?,
        ));
    }
    if operation.operation.state != AudioSynthesisOperationState::Planned
        || SqliteAudioSynthesisOperationRepo::get_dispatch_attempt_in_tx(
            tx,
            operation.operation.id,
            operation.operation.dispatch_generation,
        )
        .await?
        .is_some()
    {
        return Err(VoomError::Conflict(
            "synthesis already dispatched; reconcile exact prior generation before another encoder"
                .to_owned(),
        ));
    }
    let claim = NewAudioSynthesisClaim {
        operation_key: key,
        expected_generation: operation.operation.dispatch_generation,
        lease_id: lease.id,
        claim_token: crate::worker_process::random_hex_128(),
        expires_at: lease.expires_at,
    };
    SqliteAudioSynthesisOperationRepo::acquire_claim_in_tx(tx, &claim, now).await?;
    let directory = format!(
        "operation-{}/generation-{}",
        &token[..16],
        claim.expected_generation
    );
    generation_output(&mut dispatch.output, &directory)?;
    SqliteAudioSynthesisOperationRepo::record_dispatch_attempt_in_tx(
        tx,
        &claim,
        &NewAudioSynthesisDispatchAttempt {
            dispatch_lease_id: lease.id,
            worker_id: worker.id.0,
            worker_epoch: epoch,
            idempotency_key: dispatch_key(admission)?,
            attempt_directory: std::path::Path::new(root.provider_locator.as_str())
                .join(&directory)
                .to_string_lossy()
                .into_owned(),
            staging_path: std::path::Path::new(root.provider_locator.as_str())
                .join(dispatch.output.provider_relative_locator.as_str())
                .to_string_lossy()
                .into_owned(),
        },
        now,
    )
    .await?;
    Ok(AdmissionDecision::Dispatched)
}

fn generation_output(output: &mut MediaPlannedOutput, directory: &str) -> Result<(), VoomError> {
    let name = output
        .provider_relative_locator
        .as_str()
        .rsplit('/')
        .next()
        .ok_or_else(|| VoomError::database("audio output name missing".to_owned()))?;
    output.provider_relative_locator = ProviderRelativeLocator::new(format!("{directory}/{name}"))?;
    Ok(())
}

pub(super) enum AdmittedAudio {
    Extraction(
        voom_store::repo::media::audio_extract_operations::AudioExtractOperationRecord,
        NewAudioExtractClaim,
    ),
    Synthesis(
        Box<voom_store::repo::media::audio_synthesis_operations::AudioSynthesisOperationRecord>,
        NewAudioSynthesisClaim,
    ),
}

pub(super) async fn guard_binding_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    context: &super::RemoteMediaCompletionContext,
    verification: &voom_store::repo::media::artifacts::ArtifactVerification,
    now: OffsetDateTime,
) -> Result<(), VoomError> {
    let extraction =
        SqliteAudioExtractOperationRepo::claimed_for_lease_in_tx(tx, context.lease_id).await?;
    let synthesis =
        SqliteAudioSynthesisOperationRepo::claimed_for_lease_in_tx(tx, context.lease_id).await?;
    if extraction.is_some() && synthesis.is_some() {
        return Err(VoomError::database(
            "source lease has ambiguous audio operation bindings".to_owned(),
        ));
    }
    if let Some((record, claim)) = extraction {
        let attempt = SqliteAudioExtractOperationRepo::get_dispatch_attempt_in_tx(
            tx,
            record.operation.id,
            claim.expected_generation,
        )
        .await?
        .ok_or_else(|| {
            VoomError::database("prepared extraction has no exact attempt".to_owned())
        })?;
        let matching: Vec<_> = record
            .outputs
            .iter()
            .filter(|output| output.verification_id == Some(verification.id))
            .collect();
        if matching.len() != 1 || attempt.paths.len() != record.outputs.len() {
            return Err(VoomError::database(
                "extraction verification/output association is ambiguous".to_owned(),
            ));
        }
        let output = matching[0];
        let ordinal = usize::try_from(output.ordinal)
            .map_err(|error| VoomError::database_context("extraction output ordinal", error))?;
        if record.operation.source_file_version_id != context.source_file_version_id
            || output.artifact_handle_id != Some(verification.artifact_handle_id)
            || output.artifact_location_id != Some(verification.artifact_location_id)
            || attempt.paths.get(ordinal) != Some(&verification.path)
            || attempt.worker_id != context.worker_id
            || attempt.status != voom_store::repo::media::audio_extract_operations::AudioExtractDispatchAttemptStatus::Terminal
            || !matches!(record.operation.state, AudioExtractOperationState::Prepared | AudioExtractOperationState::RecoveryRequired)
            || claim.expires_at <= now {
            return Err(VoomError::Conflict("extraction publication lost its exact prepared operation/attempt claim".to_owned()));
        }
    } else if let Some((record, claim)) = synthesis {
        let attempt = SqliteAudioSynthesisOperationRepo::get_dispatch_attempt_in_tx(
            tx,
            record.operation.id,
            claim.expected_generation,
        )
        .await?
        .ok_or_else(|| VoomError::database("staged synthesis has no exact attempt".to_owned()))?;
        if record.operation.source_file_version_id != context.source_file_version_id
            || record.operation.verification_id != Some(verification.id)
            || record.operation.artifact_handle_id != Some(verification.artifact_handle_id)
            || record.operation.artifact_location_id != Some(verification.artifact_location_id)
            || attempt.staging_path != verification.path || attempt.worker_id != context.worker_id.0
            || attempt.dispatch_lease_id != context.lease_id
            || attempt.status != voom_store::repo::media::audio_synthesis_operations::AudioSynthesisDispatchAttemptStatus::Terminal
            || record.operation.state != AudioSynthesisOperationState::Staged || claim.expires_at <= now {
            return Err(VoomError::Conflict("synthesis publication lost its exact staged operation/attempt claim".to_owned()));
        }
    } else if verification.workflow_ticket_id != Some(context.ticket_id)
        || verification.workflow_lease_id != Some(context.lease_id)
    {
        return Err(VoomError::database(
            "unbound owner verification has no exact audio operation anchor".to_owned(),
        ));
    }
    Ok(())
}

impl AdmittedAudio {
    pub(super) fn verification_id(&self) -> Option<voom_core::ids::ArtifactVerificationId> {
        match self {
            Self::Extraction(record, _) => record
                .outputs
                .first()
                .and_then(|output| output.verification_id),
            Self::Synthesis(record, _) => record.operation.verification_id,
        }
    }

    pub(super) fn normalize_snapshot(
        &self,
        raw: &Value,
        snapshot: &mut Value,
    ) -> Result<(), VoomError> {
        let Self::Synthesis(record, _) = self else {
            return Ok(());
        };
        normalize_synthesis_snapshot(&record.companions, raw, snapshot)
    }

    pub(super) fn target_path(&self, ordinal: usize) -> Result<&str, VoomError> {
        match self {
            Self::Extraction(record, _) => record
                .outputs
                .get(ordinal)
                .map(|output| output.target_path.as_str())
                .ok_or_else(|| VoomError::database("extraction ordinal absent".to_owned())),
            Self::Synthesis(record, _) if ordinal == 0 => Ok(&record.operation.target_path),
            Self::Synthesis(_, _) => Err(VoomError::database(
                "synthesis has multiple files".to_owned(),
            )),
        }
    }
}

pub(super) fn normalize_synthesis_snapshot(
    companions: &[voom_store::repo::media::audio_synthesis_operations::AudioSynthesisCompanion],
    raw: &Value,
    snapshot: &mut Value,
) -> Result<(), VoomError> {
    let result: voom_worker_protocol::TranscodeAudioResult = serde_json::from_value(raw.clone())
        .map_err(|error| super::malformed(&format!("synthesis worker result: {error}")))?;
    let streams = snapshot
        .get_mut("streams")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| super::malformed("synthesis probe streams absent"))?;
    if result.selected_output_streams.len() != companions.len() {
        return Err(super::malformed(
            "synthesis worker companion set differs from admission",
        ));
    }
    let mut used = std::collections::BTreeSet::new();
    for companion in companions {
        let output = result
            .selected_output_streams
            .iter()
            .find(|output| output.snapshot_stream_id == companion.result_snapshot_stream_id)
            .ok_or_else(|| super::malformed("synthesis companion worker correlation missing"))?;
        if !used.insert(output.output_provider_stream_index) {
            return Err(super::malformed(
                "synthesis companions share an output stream",
            ));
        }
        let stream = streams
            .iter_mut()
            .find(|stream| {
                stream.get("index").and_then(Value::as_u64)
                    == Some(u64::from(output.output_provider_stream_index))
            })
            .ok_or_else(|| super::malformed("synthesis companion absent from actual probe"))?;
        if stream.get("kind").and_then(Value::as_str) != Some("audio")
            || stream.get("codec_name").and_then(Value::as_str) != Some(output.codec.as_str())
            || stream.get("channels").and_then(Value::as_u64) != output.channels
        {
            return Err(super::malformed(
                "synthesis companion worker facts disagree with actual probe",
            ));
        }
        stream
            .as_object_mut()
            .ok_or_else(|| super::malformed("synthesis stream is not object"))?
            .insert(
                "id".to_owned(),
                Value::String(companion.result_snapshot_stream_id.clone()),
            );
    }
    Ok(())
}

pub(super) struct AudioStage {
    pub handle_id: voom_core::ArtifactHandleId,
    pub location_id: voom_core::ArtifactLocationId,
    pub verification_id: voom_core::ids::ArtifactVerificationId,
    pub commit_id: voom_core::ids::ArtifactCommitRecordId,
    pub path: String,
    pub facts: voom_worker_protocol::ObservedFileFacts,
    pub probe: voom_worker_protocol::OwnerProbeEvidence,
}

pub(super) async fn bind_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    admitted: &AdmittedAudio,
    raw: &Value,
    stages: &[AudioStage],
    now: OffsetDateTime,
) -> Result<(), VoomError> {
    match admitted {
        AdmittedAudio::Extraction(record, claim) => {
            bind_extraction_in_tx(tx, record, claim, raw, stages, now).await
        }
        AdmittedAudio::Synthesis(record, claim) => {
            bind_synthesis_in_tx(tx, record, claim, raw, stages, now).await
        }
    }
}

/// Reconstruct the exact generation paths admitted before dispatch from the
/// operation and attempt rows, rather than trusting completion-supplied paths.
pub(super) async fn resolve_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    dispatch: &mut MediaDispatch,
    context: &super::RemoteMediaCompletionContext,
) -> Result<Option<AdmittedAudio>, VoomError> {
    match dispatch {
        MediaDispatch::ExtractAudio(dispatch) => {
            let (record, claim) =
                SqliteAudioExtractOperationRepo::claimed_for_lease_in_tx(tx, context.lease_id)
                    .await?
                    .ok_or_else(|| {
                        VoomError::database(
                            "extraction completion has no admitted operation".to_owned(),
                        )
                    })?;
            let attempt = SqliteAudioExtractOperationRepo::get_dispatch_attempt_in_tx(
                tx,
                record.operation.id,
                claim.expected_generation,
            )
            .await?
            .ok_or_else(|| {
                VoomError::database("extraction completion has no admitted attempt".to_owned())
            })?;
            super::validate_dispatch_key(&attempt.idempotency_key, context)?;
            if record.operation.source_file_version_id != context.source_file_version_id
                || record.outputs.len() != dispatch.outputs.len()
                || attempt.paths.len() != dispatch.outputs.len()
                || attempt.worker_id != context.worker_id
                || claim.expires_at <= cp.clock().now()
            {
                return Err(VoomError::Conflict("extraction completion differs from admitted source, output set, worker or claim".to_owned()));
            }
            for ((output, stored), path) in dispatch
                .outputs
                .iter_mut()
                .zip(&record.outputs)
                .zip(&attempt.paths)
            {
                if stored
                    .output_id
                    .as_deref()
                    .is_some_and(|id| id != output.output_id)
                    || stored.source_snapshot_stream_id != output.selection.snapshot_stream_id
                    || stored.source_provider_stream_index != output.selection.provider_stream_index
                {
                    return Err(VoomError::database(
                        "extraction durable descriptor/dispatch mismatch".to_owned(),
                    ));
                }
                output.output.provider_relative_locator =
                    rooted_path_in_tx(cp, tx, output.output.storage_root_id, path).await?;
            }
            Ok(Some(AdmittedAudio::Extraction(record, claim)))
        }
        MediaDispatch::TranscodeAudio(dispatch) if dispatch.settings.add_track => {
            let (record, claim) =
                SqliteAudioSynthesisOperationRepo::claimed_for_lease_in_tx(tx, context.lease_id)
                    .await?
                    .ok_or_else(|| {
                        VoomError::database(
                            "synthesis completion has no admitted operation".to_owned(),
                        )
                    })?;
            let attempt = SqliteAudioSynthesisOperationRepo::get_dispatch_attempt_in_tx(
                tx,
                record.operation.id,
                claim.expected_generation,
            )
            .await?
            .ok_or_else(|| {
                VoomError::database("synthesis completion has no admitted attempt".to_owned())
            })?;
            super::validate_dispatch_key(&attempt.idempotency_key, context)?;
            if record.operation.source_file_version_id != context.source_file_version_id
                || attempt.worker_id != context.worker_id.0
                || attempt.dispatch_lease_id != context.lease_id
                || claim.expires_at <= cp.clock().now()
            {
                return Err(VoomError::Conflict(
                    "synthesis completion differs from admitted source, worker or claim".to_owned(),
                ));
            }
            dispatch.output.provider_relative_locator = rooted_path_in_tx(
                cp,
                tx,
                dispatch.output.storage_root_id,
                &attempt.staging_path,
            )
            .await?;
            Ok(Some(AdmittedAudio::Synthesis(Box::new(record), claim)))
        }
        _ => Ok(None),
    }
}

pub(super) async fn rooted_path_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    root_id: voom_core::StorageRootId,
    path: &str,
) -> Result<ProviderRelativeLocator, VoomError> {
    let root = cp
        .libraries
        .get_library_root_in_tx(tx, root_id)
        .await?
        .ok_or_else(|| VoomError::database("admitted audio root missing".to_owned()))?;
    let relative = std::path::Path::new(path)
        .strip_prefix(root.provider_locator.as_str())
        .map_err(|error| VoomError::database_context("admitted audio path outside root", error))?;
    let relative = relative
        .to_str()
        .ok_or_else(|| VoomError::database("admitted audio path is not UTF-8".to_owned()))?;
    ProviderRelativeLocator::parse_database("admitted audio relative path", relative)
}

pub(super) async fn finish_synthesis_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    context: &super::RemoteMediaCompletionContext,
    commit: &voom_store::repo::media::artifacts::ArtifactCommitRecord,
    identity: &super::PublishedMediaIdentity,
    result: &mut Value,
    now: OffsetDateTime,
) -> Result<(), VoomError> {
    let super::PublishedMediaIdentity {
        version: version_id,
        location: location_id,
        snapshot: snapshot_id,
    } = *identity;
    let Some((record, claim)) =
        SqliteAudioSynthesisOperationRepo::claimed_for_lease_in_tx(tx, context.lease_id).await?
    else {
        return Ok(());
    };
    if record.operation.source_file_version_id != context.source_file_version_id
        || record.operation.verification_id != Some(commit.verification_id)
        || record.operation.artifact_handle_id != Some(commit.artifact_handle_id)
        || claim.expires_at <= now
    {
        return Err(VoomError::Conflict(
            "synthesis finalization lost exact staged operation claim".to_owned(),
        ));
    }
    let version = cp
        .identity
        .get_file_version_in_tx(tx, version_id)
        .await?
        .ok_or_else(|| VoomError::database("synthesis committed version missing".to_owned()))?;
    SqliteAudioSynthesisOperationRepo::finalize_in_tx(
        tx,
        &voom_store::repo::media::audio_synthesis_operations::FinalizeAudioSynthesisOperation {
            operation_id: record.operation.id,
            commit_record_id: commit.id,
            result_file_asset_id: version.file_asset_id,
            result_file_version_id: version_id,
            result_file_location_id: location_id,
            result_media_snapshot_id: snapshot_id,
            recorded_at: now,
        },
    )
    .await?;
    let finalized =
        SqliteAudioSynthesisOperationRepo::get_by_key_in_tx(tx, &record.operation.operation_key)
            .await?
            .ok_or_else(|| VoomError::database("finalized synthesis missing".to_owned()))?;
    let companions = synthesis_reports(&finalized, commit, version_id, location_id, snapshot_id)?;
    let result = result.as_object_mut().ok_or_else(|| {
        VoomError::database("synthesis completion result is not object".to_owned())
    })?;
    result.insert(
        "synthesis_operation_id".to_owned(),
        Value::String(finalized.operation.planned_operation_id),
    );
    result.insert(
        "synthesis_operation_key".to_owned(),
        Value::String(finalized.operation.operation_key),
    );
    result.insert(
        "synthesized_companions".to_owned(),
        serde_json::to_value(companions).map_err(|error| {
            VoomError::Internal(format!("encode synthesis companions: {error}"))
        })?,
    );
    Ok(())
}

pub(super) fn synthesis_reports(
    finalized: &voom_store::repo::media::audio_synthesis_operations::AudioSynthesisOperationRecord,
    commit: &voom_store::repo::media::artifacts::ArtifactCommitRecord,
    version_id: FileVersionId,
    location_id: voom_core::FileLocationId,
    snapshot_id: MediaSnapshotId,
) -> Result<Vec<crate::audio::ExecuteSynthesisCompanionReport>, VoomError> {
    let mut companions = Vec::with_capacity(finalized.companions.len());
    for companion in &finalized.companions {
        let missing = || {
            VoomError::database(
                "finalized synthesis companion lacks validated facts or lineage".to_owned(),
            )
        };
        companions.push(crate::audio::ExecuteSynthesisCompanionReport {
            ordinal: companion.ordinal,
            companion_id: companion.companion_id.clone(),
            source_file_version_id: finalized.operation.source_file_version_id,
            source_media_snapshot_id: finalized.operation.source_media_snapshot_id,
            source_snapshot_stream_id: companion.source_snapshot_stream_id.clone(),
            source_provider_stream_index: companion.source_provider_stream_index,
            result_file_version_id: version_id,
            result_file_location_id: location_id,
            result_media_snapshot_id: snapshot_id,
            result_snapshot_stream_id: companion.result_snapshot_stream_id.clone(),
            result_provider_stream_index: companion
                .result_provider_stream_index
                .ok_or_else(missing)?,
            artifact_handle_id: commit.artifact_handle_id,
            artifact_location_id: finalized
                .operation
                .artifact_location_id
                .ok_or_else(missing)?,
            lineage_id: companion.lineage_id.ok_or_else(missing)?,
            location: std::path::PathBuf::from(&commit.target_path),
            codec: companion.codec.clone().ok_or_else(missing)?,
            channels: companion.channels.ok_or_else(missing)?,
            language: companion.language.clone(),
            title: companion.title.clone(),
            disposition_default: companion.disposition_default.ok_or_else(missing)?,
            disposition_forced: companion.disposition_forced.ok_or_else(missing)?,
            disposition_commentary: companion.disposition_commentary.ok_or_else(missing)?,
        });
    }
    Ok(companions)
}

async fn plan_extraction_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    admission: &AdmissionInput<'_>,
    audio: &Value,
    dispatch: &voom_worker_protocol::MediaExtractAudioDispatch,
    now: OffsetDateTime,
) -> Result<
    (
        voom_store::repo::media::audio_extract_operations::AudioExtractOperationRecord,
        voom_store::repo::library::library_roots::LibraryRoot,
    ),
    VoomError,
> {
    let source_id = admission.source_id;
    let snapshot = admission.snapshot;
    let snapshot_id = snapshot.id;
    let selection = extract_selection_from_payload_and_snapshot(audio, snapshot)?;
    if selection.outputs.len() != dispatch.outputs.len()
        || selection.outputs.is_empty()
        || selection.outputs.len() > voom_worker_protocol::MAX_COMMIT_GROUP_OUTPUTS
    {
        return Err(VoomError::database(
            "extraction dispatch output set differs from pinned selection or exceeds bound"
                .to_owned(),
        ));
    }
    let root_id = dispatch.outputs[0].output.storage_root_id;
    let root = cp
        .libraries
        .get_library_root_in_tx(tx, root_id)
        .await?
        .ok_or_else(|| VoomError::database("extraction staging root missing".to_owned()))?;
    let descriptors = extraction_descriptors(dispatch, &selection, &root)?;
    let mut hash = Sha256::new();
    for field in [
        source_id.0.to_string(),
        snapshot_id.0.to_string(),
        selection
            .operation_id
            .clone()
            .unwrap_or_else(|| "legacy".to_owned()),
    ]
    .iter()
    .chain(descriptors.iter().map(|output| &output.target_path))
    {
        hash.update(field.len().to_string());
        hash.update(b":");
        hash.update(field.as_bytes());
    }
    let operation_key = format!("{:x}", hash.finalize());
    let source_path = std::path::Path::new(dispatch.source.provider_relative_locator.as_str());
    let bundle = cp
        .resolve_or_create_primary_bundle_in_tx(tx, source_id, source_path, now)
        .await?;
    let operation = SqliteAudioExtractOperationRepo::create_planned_in_tx(
        tx,
        &NewAudioExtractOperation {
            operation_key: operation_key.clone(),
            operation_id: selection.operation_id,
            source_file_version_id: source_id,
            source_bundle_id: bundle.bundle_id,
            source_media_snapshot_id: snapshot_id,
        },
        &descriptors,
        now,
    )
    .await?;
    Ok((operation, root))
}

fn extraction_descriptors(
    dispatch: &voom_worker_protocol::MediaExtractAudioDispatch,
    selection: &crate::audio::selection::ExtractAudioSelectionPlan,
    root: &voom_store::repo::library::library_roots::LibraryRoot,
) -> Result<Vec<NewAudioExtractOutput>, VoomError> {
    let root_id = root.id;
    let mut descriptors = Vec::with_capacity(selection.outputs.len());
    for (selected, output) in selection.outputs.iter().zip(&dispatch.outputs) {
        if selected.stream != output.selection
            || selected
                .output_id
                .as_deref()
                .is_some_and(|id| id != output.output_id)
            || output.output.storage_root_id != root_id
        {
            return Err(VoomError::database(
                "extraction descriptor/dispatch correlation failed".to_owned(),
            ));
        }
        let target = ProviderRelativeLocator::new(format!(
            ".committed/audio/{}",
            output.output.provider_relative_locator.as_str()
        ))?;
        descriptors.push(NewAudioExtractOutput {
            output_id: selected.output_id.clone(),
            source_snapshot_stream_id: selected.stream.snapshot_stream_id.clone(),
            source_provider_stream_index: selected.stream.provider_stream_index,
            bundle_role: match selected.role {
                voom_plan::planner::audio::AudioBundleRole::CommentaryAudio => "commentary_audio",
                voom_plan::planner::audio::AudioBundleRole::ExternalAudio => "external_audio",
            }
            .to_owned(),
            target_path: std::path::Path::new(root.provider_locator.as_str())
                .join(target.as_str())
                .to_string_lossy()
                .into_owned(),
        });
    }
    Ok(descriptors)
}

async fn plan_synthesis_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    admission: &AdmissionInput<'_>,
    audio: &Value,
    dispatch: &voom_worker_protocol::MediaTranscodeAudioDispatch,
    selection: crate::audio::selection::TranscodeAudioSelectionPlan,
    now: OffsetDateTime,
) -> Result<
    (
        voom_store::repo::media::audio_synthesis_operations::AudioSynthesisOperationRecord,
        voom_store::repo::library::library_roots::LibraryRoot,
    ),
    VoomError,
> {
    let source_id = admission.source_id;
    let snapshot_id = admission.snapshot.id;
    let operation_id = selection
        .operation_id
        .ok_or_else(|| VoomError::database("synthesis operation id missing".to_owned()))?;
    let key = format!("synthesize:{}:{operation_id}", source_id.0);
    let token = blake3::hash(key.as_bytes()).to_hex();
    let root = cp
        .libraries
        .get_library_root_in_tx(tx, dispatch.output.storage_root_id)
        .await?
        .ok_or_else(|| VoomError::database("synthesis staging root missing".to_owned()))?;
    let name = dispatch
        .output
        .provider_relative_locator
        .as_str()
        .rsplit('/')
        .next()
        .ok_or_else(|| VoomError::database("synthesis output name missing".to_owned()))?;
    let target = ProviderRelativeLocator::new(format!(
        ".committed/audio/operation-{}/{name}",
        &token[..16]
    ))?;
    let descriptors: Vec<voom_plan::planner::audio::SynthesizeAudioCompanionDescriptor> =
        serde_json::from_value(
            audio
                .get("companions")
                .cloned()
                .ok_or_else(|| VoomError::database("synthesis companions missing".to_owned()))?,
        )
        .map_err(|error| VoomError::database_context("synthesis companion descriptors", error))?;
    let companions: Vec<_> = descriptors
        .into_iter()
        .map(|descriptor| NewAudioSynthesisCompanion {
            companion_id: descriptor.companion_id,
            source_snapshot_stream_id: descriptor.source_snapshot_stream_id,
            source_provider_stream_index: descriptor.source_provider_stream_index,
            result_snapshot_stream_id: descriptor.result_snapshot_stream_id,
        })
        .collect();
    let operation = SqliteAudioSynthesisOperationRepo::create_planned_in_tx(
        tx,
        &NewAudioSynthesisOperation {
            operation_key: key.clone(),
            planned_operation_id: operation_id,
            source_file_version_id: source_id,
            source_media_snapshot_id: snapshot_id,
            target_codec: selection.target_codec,
            target_channels: u32::try_from(selection.target_channels.ok_or_else(|| {
                VoomError::database("synthesis target channels missing".to_owned())
            })?)
            .map_err(|error| VoomError::database_context("synthesis channels", error))?,
            container: selection.container,
            target_path: std::path::Path::new(root.provider_locator.as_str())
                .join(target.as_str())
                .to_string_lossy()
                .into_owned(),
        },
        &companions,
        now,
    )
    .await?;
    Ok((operation, root))
}

async fn bind_extraction_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    record: &voom_store::repo::media::audio_extract_operations::AudioExtractOperationRecord,
    claim: &NewAudioExtractClaim,
    raw: &Value,
    stages: &[AudioStage],
    now: OffsetDateTime,
) -> Result<(), VoomError> {
    let attempt = SqliteAudioExtractOperationRepo::get_dispatch_attempt_in_tx(
        tx,
        record.operation.id,
        claim.expected_generation,
    )
    .await?
    .ok_or_else(|| VoomError::database("extraction staged receipt has no attempt".to_owned()))?;
    SqliteAudioExtractOperationRepo::mark_dispatch_terminal_in_tx(tx, claim, attempt.id, now)
        .await?;
    if record.outputs.len() != stages.len() {
        return Err(VoomError::database(
            "extraction stage count differs from admission".to_owned(),
        ));
    }
    let staged: Vec<_> = record
        .outputs
        .iter()
        .zip(stages)
        .map(|(output, stage)| {
            voom_store::repo::media::audio_extract_operations::NewStagedAudioExtractOutput {
                operation_output_id: output.id,
                staging_path: stage.path.clone(),
                expected_size_bytes: stage.facts.size_bytes,
                expected_checksum: stage.facts.content_hash.clone(),
                artifact_handle_id: stage.handle_id,
                artifact_location_id: stage.location_id,
                result_facts: serde_json::json!(stage.facts),
            }
        })
        .collect();
    SqliteAudioExtractOperationRepo::stage_operation_in_tx(
        tx,
        voom_store::repo::media::audio_extract_operations::StageAudioExtractOperation {
            operation_id: record.operation.id,
            claim,
            worker_result: raw,
            outputs: &staged,
            observed_at: now,
        },
    )
    .await?;
    let prepared: Vec<_> = record
        .outputs
        .iter()
        .zip(stages)
        .map(|(output, stage)| {
            voom_store::repo::media::audio_extract_operations::NewFencedPreparedAudioExtractOutput {
                operation_output_id: output.id,
                staging_path: stage.path.clone(),
                artifact_handle_id: stage.handle_id,
                artifact_location_id: stage.location_id,
                verification_id: stage.verification_id,
                commit_record_id: stage.commit_id,
                probe_worker_id: stage.probe.worker_id,
                probe_payload: stage.probe.result.snapshot.clone(),
            }
        })
        .collect();
    SqliteAudioExtractOperationRepo::prepare_fenced_operation_in_tx(
        tx,
        record.operation.id,
        claim,
        &prepared,
        now,
    )
    .await
}

async fn bind_synthesis_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    record: &voom_store::repo::media::audio_synthesis_operations::AudioSynthesisOperationRecord,
    claim: &NewAudioSynthesisClaim,
    raw: &Value,
    stages: &[AudioStage],
    now: OffsetDateTime,
) -> Result<(), VoomError> {
    let attempt = SqliteAudioSynthesisOperationRepo::get_dispatch_attempt_in_tx(
        tx,
        record.operation.id,
        claim.expected_generation,
    )
    .await?
    .ok_or_else(|| VoomError::database("synthesis staged receipt has no attempt".to_owned()))?;
    SqliteAudioSynthesisOperationRepo::mark_dispatch_terminal_in_tx(tx, claim, attempt.id, now)
        .await?;
    let stage = stages
        .first()
        .filter(|_| stages.len() == 1)
        .ok_or_else(|| VoomError::database("synthesis stage must contain one file".to_owned()))?;
    let companions = observed_synthesis_companions(record, stage)?;
    SqliteAudioSynthesisOperationRepo::bind_staged_in_tx(
        tx,
        &voom_store::repo::media::audio_synthesis_operations::BindAudioSynthesisOperation {
            operation_id: record.operation.id,
            claim: claim.clone(),
            staging_path: stage.path.clone(),
            expected_size_bytes: stage.facts.size_bytes,
            expected_checksum: stage.facts.content_hash.clone(),
            worker_result: raw.clone(),
            artifact_handle_id: stage.handle_id,
            artifact_location_id: stage.location_id,
            companions,
        },
        now,
    )
    .await?;
    SqliteAudioSynthesisOperationRepo::record_validation_in_tx(
        tx,
        &voom_store::repo::media::audio_synthesis_operations::ValidateAudioSynthesisOperation {
            operation_id: record.operation.id,
            verification_id: stage.verification_id,
            probe_worker_id: stage.probe.worker_id,
            probe_payload: stage.probe.result.snapshot.clone(),
        },
    )
    .await
}

fn observed_synthesis_companions(
    record: &voom_store::repo::media::audio_synthesis_operations::AudioSynthesisOperationRecord,
    stage: &AudioStage,
) -> Result<
    Vec<voom_store::repo::media::audio_synthesis_operations::StagedAudioSynthesisCompanion>,
    VoomError,
> {
    let streams = stage
        .probe
        .result
        .snapshot
        .get("streams")
        .and_then(Value::as_array)
        .ok_or_else(|| super::malformed("synthesis probe streams missing"))?;
    let mut companions = Vec::with_capacity(record.companions.len());
    for companion in &record.companions {
        let stream = streams
            .iter()
            .find(|stream| {
                stream.get("id").and_then(Value::as_str)
                    == Some(&companion.result_snapshot_stream_id)
            })
            .ok_or_else(|| super::malformed("normalized synthesis companion missing"))?;
        let index = u32::try_from(super::required_id(stream, "index").or_else(|_| {
            if stream.get("index").and_then(Value::as_u64) == Some(0) {
                Ok(0)
            } else {
                Err(super::malformed("synthesis stream index invalid"))
            }
        })?)
        .map_err(|error| super::malformed(&format!("synthesis stream index: {error}")))?;
        let channels = u32::try_from(
            stream
                .get("channels")
                .and_then(Value::as_u64)
                .filter(|channels| *channels > 0)
                .ok_or_else(|| super::malformed("synthesis actual channels missing"))?,
        )
        .map_err(|error| super::malformed(&format!("synthesis actual channels: {error}")))?;
        let codec = stream
            .get("codec_name")
            .and_then(Value::as_str)
            .filter(|codec| !codec.is_empty())
            .ok_or_else(|| super::malformed("synthesis actual codec missing"))?
            .to_owned();
        let disposition = stream
            .get("disposition")
            .ok_or_else(|| super::malformed("synthesis actual disposition missing"))?;
        let flag = |name| {
            disposition
                .get(name)
                .and_then(Value::as_bool)
                .ok_or_else(|| super::malformed("synthesis actual disposition flag missing"))
        };
        let language = stream
            .get("language")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let title = stream
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let default = flag("default")?;
        let forced = flag("forced")?;
        let commentary = flag("commentary")?;
        let facts = voom_worker_protocol::AudioOutputStreamFact {
            snapshot_stream_id: companion.result_snapshot_stream_id.clone(),
            output_provider_stream_index: index,
            codec: codec.clone(),
            language: language.clone(),
            title: title.clone(),
            default: Some(default),
            disposition: Some(voom_worker_protocol::AudioDispositionFact {
                default: Some(default),
                forced: Some(forced),
                commentary: Some(commentary),
            }),
            channels: Some(u64::from(channels)),
        };
        companions.push(
            voom_store::repo::media::audio_synthesis_operations::StagedAudioSynthesisCompanion {
                companion_id: companion.companion_id.clone(),
                result_provider_stream_index: index,
                codec,
                channels,
                language,
                title,
                disposition_default: default,
                disposition_forced: forced,
                disposition_commentary: commentary,
                result_facts: serde_json::json!(facts),
            },
        );
    }
    Ok(companions)
}
