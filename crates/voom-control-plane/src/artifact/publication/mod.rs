//! Data-only owner-media publication. A ticket stays leased until its catalog,
//! real snapshot, and completion replay commit together (ADR0109).

use crate::ControlPlane;
use crate::cases::{append_event, commit_tx};
use crate::execution::{RemoteCompleteInput, RemoteCompleteOutcome};
use crate::workflow::plan::ticket_payload::WorkflowTicketPayload;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};
use time::OffsetDateTime;
use voom_core::{
    FileLocationId, FileVersionId, JobId, LeaseId, NodeId, NodeIncarnationId, OperationKind,
    ProviderRelativeLocator, TicketId, TicketOperation, VoomError, WorkerId,
};
use voom_events::payload::{
    ArtifactHandleCreatedPayload, ArtifactLocationRecordedPayload,
    ArtifactVerificationStartedPayload, ArtifactVerificationSucceededPayload,
};
use voom_events::{Event, SubjectType};
use voom_store::repo::execution::remote_idempotency::{IdempotencyOutcome, RemoteIdempotencyInput};
use voom_store::repo::execution::tickets::Ticket;
use voom_store::repo::media::artifact_access_plans::{
    ArtifactAccessPlan, ArtifactAccessPlanStatus,
};
use voom_store::repo::media::artifacts::{
    ArtifactCommitRecord, ArtifactHandleAccessMode, ArtifactLocationKind,
    ArtifactVerificationStatus, NewArtifactHandle, NewArtifactLocation, NewArtifactVerification,
};
use voom_store::repo::media::identity::{FileLocationRepo, FileVersionRepo, NewMediaSnapshot};
use voom_worker_protocol::{
    MediaDispatch, MediaPlannedOutput, OwnerObservedEvidence, OwnerProbeEvidence,
    decode_media_dispatch,
};

mod audio;
mod group;
mod reuse;

pub(crate) use audio::{AudioAdmission, admit_in_tx as admit_audio_in_tx};
pub(crate) use group::finish_extraction_group_in_tx;

/// Safe original request identity retained with the exact staged evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceLocationFence {
    pub epoch: u64,
    pub storage_root_id: voom_core::StorageRootId,
    pub provider_relative_locator: ProviderRelativeLocator,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoteMediaCompletionContext {
    pub node_id: NodeId,
    pub incarnation_id: NodeIncarnationId,
    pub worker_id: WorkerId,
    pub lease_id: LeaseId,
    pub ticket_id: TicketId,
    pub job_id: JobId,
    pub source_file_version_id: FileVersionId,
    pub source_location_id: FileLocationId,
    #[serde(default)]
    pub source_location_fence: Option<SourceLocationFence>,
    pub idempotency_key: String,
    pub request_hash: String,
    pub result: Value,
}

impl RemoteMediaCompletionContext {
    fn validate(&self) -> Result<(), VoomError> {
        for (name, value) in [
            ("node", self.node_id.0),
            ("worker", self.worker_id.0),
            ("lease", self.lease_id.0),
            ("ticket", self.ticket_id.0),
            ("job", self.job_id.0),
            ("source version", self.source_file_version_id.0),
            ("source location", self.source_location_id.0),
        ] {
            if value == 0 || i64::try_from(value).is_err() {
                return Err(VoomError::database(format!(
                    "owner media completion {name} is not a positive SQLite id"
                )));
            }
        }
        if self.idempotency_key.is_empty()
            || self.request_hash.is_empty()
            || !self.result.is_object()
        {
            return Err(VoomError::database(
                "owner media completion has invalid replay identity or result".to_owned(),
            ));
        }
        let fence = self.source_location_fence.as_ref().ok_or_else(|| {
            VoomError::database("owner media completion lacks its source location fence".to_owned())
        })?;
        if i64::try_from(fence.epoch).is_err()
            || fence.storage_root_id.0 == 0
            || i64::try_from(fence.storage_root_id.0).is_err()
        {
            return Err(VoomError::database(
                "owner media source fence is invalid".to_owned(),
            ));
        }
        Ok(())
    }

    fn replay_input(&self, now: OffsetDateTime) -> RemoteIdempotencyInput {
        RemoteIdempotencyInput {
            node_id: self.node_id,
            route_key: format!("POST /v1/execution/lease/{}/complete", self.lease_id),
            worker_id: Some(self.worker_id),
            idempotency_key: format!("{}:{}", self.incarnation_id, self.idempotency_key),
            request_hash: self.request_hash.clone(),
            created_at: now,
        }
    }
}

pub(crate) struct PendingMedia {
    pub context: RemoteMediaCompletionContext,
    commits: Vec<voom_core::ids::ArtifactCommitRecordId>,
}

struct ValidatedOwnerOutput {
    output: MediaPlannedOutput,
    observed: voom_worker_protocol::OwnerObservedOutput,
    snapshot: Value,
    path: std::path::PathBuf,
}

struct StagingPublication<'a> {
    context: &'a RemoteMediaCompletionContext,
    operation: OperationKind,
    audio: Option<&'a audio::AdmittedAudio>,
}

pub(crate) async fn stage_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    input: &RemoteCompleteInput,
    ticket: &Ticket,
    now: OffsetDateTime,
) -> Result<Option<PendingMedia>, VoomError> {
    // Only workflow mutations enter publication. Standalone probes also carry
    // media dispatch envelopes, but do not have workflow ticket payloads.
    if !matches!(
        crate::workflow::plan::ticket_payload::ticket_operation(ticket.kind.as_str()),
        Ok(OperationKind::Remux
            | OperationKind::TranscodeVideo
            | OperationKind::TranscodeAudio
            | OperationKind::ExtractAudio)
    ) {
        return Ok(None);
    }
    let Some(encoded) = ticket
        .payload
        .get("rendered_payload")
        .and_then(|v| v.get("media_dispatch"))
    else {
        return Err(VoomError::database(
            "workflow mutation is missing its media dispatch".to_owned(),
        ));
    };
    let payload = WorkflowTicketPayload::parse_ticket(ticket.kind.as_str(), ticket.payload.clone())
        .map_err(|e| VoomError::database(format!("media ticket payload: {e}")))?;
    let mut dispatch = decode_media_dispatch(encoded)
        .map_err(|error| VoomError::database(format!("persisted media dispatch: {error}")))?;
    let operation = match &dispatch {
        MediaDispatch::Remux(_) => OperationKind::Remux,
        MediaDispatch::TranscodeVideo(_) => OperationKind::TranscodeVideo,
        MediaDispatch::TranscodeAudio(_) => OperationKind::TranscodeAudio,
        MediaDispatch::ExtractAudio(_) => OperationKind::ExtractAudio,
        _ => {
            return Err(VoomError::database(
                "workflow mutation has a non-mutation media dispatch".to_owned(),
            ));
        }
    };
    if operation != payload.operation {
        return Err(VoomError::database(
            "media dispatch differs from durable ticket operation".to_owned(),
        ));
    }
    let context = completion_context_in_tx(cp, tx, input, ticket, &payload).await?;
    let admitted_audio = audio::resolve_in_tx(cp, tx, &mut dispatch, &context).await?;
    let outputs: Vec<MediaPlannedOutput> = match &dispatch {
        MediaDispatch::Remux(d) => vec![d.output.clone()],
        MediaDispatch::TranscodeVideo(d) => vec![d.output.clone()],
        MediaDispatch::TranscodeAudio(d) => vec![d.output.clone()],
        MediaDispatch::ExtractAudio(d) => d.outputs.iter().map(|o| o.output.clone()).collect(),
        _ => return Err(malformed("unsupported mutation dispatch")),
    };
    if let Some(pending) =
        existing_pending_in_tx(cp, tx, input, &context, admitted_audio.as_ref()).await?
    {
        return Ok(Some(pending));
    }
    let publication = StagingPublication {
        context: &context,
        operation: payload.operation,
        audio: admitted_audio.as_ref(),
    };
    let (observed, raw) =
        validate_owner_evidence_in_tx(cp, tx, &publication, &dispatch, &outputs, encoded, now)
            .await?;
    let validated =
        validate_outputs_in_tx(cp, tx, &publication, &dispatch, &outputs, &observed, &raw).await?;
    let mut audio_stages = Vec::new();
    let mut commits = Vec::new();
    for (index, validated) in validated.iter().enumerate() {
        let stage = stage_output_in_tx(cp, tx, &publication, validated, index, now).await?;
        commits.push(stage.commit_id);
        if admitted_audio.is_some() {
            audio_stages.push(stage);
        }
    }
    if let Some(audio) = &admitted_audio {
        audio::bind_in_tx(tx, audio, &raw, &audio_stages, now).await?;
    }
    Ok(Some(PendingMedia { context, commits }))
}

fn owner_worker_result(result: &Value) -> Option<Value> {
    let mut worker = result.as_object()?.clone();
    worker.remove("agent_observed");
    worker.remove("artifact_access");
    Some(Value::Object(worker))
}

fn validate_worker_result(operation: OperationKind, raw: &Value) -> Result<(), VoomError> {
    let result = match operation {
        OperationKind::Remux => {
            serde_json::from_value::<voom_worker_protocol::RemuxResult>(raw.clone()).map(|_| ())
        }
        OperationKind::TranscodeVideo => {
            serde_json::from_value::<voom_worker_protocol::TranscodeVideoResult>(raw.clone())
                .map(|_| ())
        }
        OperationKind::TranscodeAudio => {
            serde_json::from_value::<voom_worker_protocol::TranscodeAudioResult>(raw.clone())
                .map(|_| ())
        }
        OperationKind::ExtractAudio => {
            serde_json::from_value::<voom_worker_protocol::ExtractAudioResult>(raw.clone())
                .map(|_| ())
        }
        _ => return Err(malformed("not a mutation result")),
    };
    result.map_err(|e| malformed(&format!("worker result: {e}")))?;
    for field in ["size_bytes", "content_hash"] {
        if raw.get("input_pre").and_then(|facts| facts.get(field))
            != raw.get("input_post").and_then(|facts| facts.get(field))
        {
            return Err(malformed("worker input facts changed during mutation"));
        }
    }
    Ok(())
}

async fn validate_extract_result_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    dispatch: &MediaDispatch,
    context: &RemoteMediaCompletionContext,
    raw: &Value,
) -> Result<(), VoomError> {
    use voom_worker_protocol::{
        ExtractAudioInput, ExtractAudioOutput, ExtractAudioOutputDescriptor, ExtractAudioRequest,
        ExtractAudioResult,
    };
    let MediaDispatch::ExtractAudio(dispatch) = dispatch else {
        return Ok(());
    };
    let (operation, claim) = voom_store::repo::media::audio_extract_operations::SqliteAudioExtractOperationRepo::claimed_for_lease_in_tx(tx, context.lease_id)
        .await?.ok_or_else(|| VoomError::database("extraction result has no durable operation".to_owned()))?;
    let attempt = voom_store::repo::media::audio_extract_operations::SqliteAudioExtractOperationRepo::get_dispatch_attempt_in_tx(tx, operation.operation.id, claim.expected_generation)
        .await?.ok_or_else(|| VoomError::database("extraction result has no durable attempt".to_owned()))?;
    if dispatch.outputs.len() != attempt.paths.len() {
        return Err(VoomError::database(
            "extraction dispatch/attempt output count differs".to_owned(),
        ));
    }
    let outputs: Vec<_> = dispatch
        .outputs
        .iter()
        .zip(&attempt.paths)
        .map(|(output, path)| ExtractAudioOutputDescriptor {
            output_id: output.output_id.clone(),
            selection: output.selection.clone(),
            output: ExtractAudioOutput {
                staging_root: std::path::Path::new(path)
                    .parent()
                    .and_then(std::path::Path::to_str)
                    .unwrap_or("")
                    .to_owned(),
                path: path.clone(),
                container: dispatch.output_container.clone(),
                audio_codec: output.audio_codec.clone(),
                overwrite: false,
            },
        })
        .collect();
    let first = outputs
        .first()
        .ok_or_else(|| malformed("extraction result has no outputs"))?;
    let root = cp
        .libraries
        .get_library_root_in_tx(tx, dispatch.source.storage_root_id)
        .await?
        .ok_or_else(|| VoomError::database("extraction source root missing".to_owned()))?;
    let source = std::path::Path::new(root.provider_locator.as_str())
        .join(dispatch.source.provider_relative_locator.as_str());
    let request = ExtractAudioRequest {
        input: ExtractAudioInput {
            path: source.to_string_lossy().into_owned(),
            expected: dispatch.expected.clone(),
        },
        output: first.output.clone(),
        selection: first.selection.clone(),
        outputs: Some(outputs),
    };
    let result: ExtractAudioResult = serde_json::from_value(raw.clone())
        .map_err(|error| malformed(&format!("extraction result: {error}")))?;
    voom_worker_protocol::validate_extract_audio_result(&request, &result)
        .map_err(|error| malformed(&error.to_string()))
}

fn validate_worker_input(raw: &Value, observed: &OwnerObservedEvidence) -> Result<(), VoomError> {
    for (name, facts) in [
        ("input_pre", &observed.input_pre),
        ("input_post", &observed.input_post),
    ] {
        if raw
            .get(name)
            .and_then(|value| value.get("size_bytes"))
            .and_then(Value::as_u64)
            != Some(facts.size_bytes)
            || raw
                .get(name)
                .and_then(|value| value.get("content_hash"))
                .and_then(Value::as_str)
                != Some(&facts.content_hash)
        {
            return Err(malformed(
                "worker input facts differ from owner observations",
            ));
        }
    }
    Ok(())
}

fn normalized_snapshot(mut value: Value) -> Result<Value, VoomError> {
    value
        .get("container")
        .and_then(|container| container.get("format_name"))
        .and_then(Value::as_str)
        .filter(|format| !format.is_empty())
        .ok_or_else(|| malformed("staged snapshot lacks container format"))?;
    let streams = value
        .get_mut("streams")
        .and_then(Value::as_array_mut)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| malformed("staged snapshot has no streams"))?;
    let mut indices = std::collections::BTreeSet::new();
    let mut ids = std::collections::BTreeSet::new();
    for stream in streams {
        let kind = stream
            .get("kind")
            .and_then(Value::as_str)
            .filter(|kind| !kind.is_empty())
            .ok_or_else(|| malformed("staged snapshot stream kind missing"))?;
        if matches!(kind, "audio" | "video")
            && stream
                .get("codec_name")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        {
            return Err(malformed("staged snapshot audio/video codec missing"));
        }
        let index = stream
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| malformed("staged snapshot stream index invalid"))?;
        if !indices.insert(index) {
            return Err(malformed("duplicate staged stream index"));
        }
        let object = stream
            .as_object_mut()
            .ok_or_else(|| malformed("staged stream not object"))?;
        object
            .entry("id")
            .or_insert_with(|| json!(format!("stream-{index}")));
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| malformed("staged stream id invalid"))?;
        if !ids.insert(id.to_owned()) {
            return Err(malformed("duplicate staged stream id"));
        }
    }
    Ok(value)
}

fn malformed(reason: &str) -> VoomError {
    VoomError::MalformedWorkerResult(reason.to_owned())
}
fn required_id(value: &Value, field: &str) -> Result<u64, VoomError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .filter(|v| *v > 0 && i64::try_from(*v).is_ok())
        .ok_or_else(|| VoomError::database(format!("media ticket {field} is not a positive id")))
}
fn decode_report<T: serde::de::DeserializeOwned>(
    report: &Value,
    key: &str,
) -> Result<T, VoomError> {
    serde_json::from_value(
        report
            .get(key)
            .cloned()
            .ok_or_else(|| VoomError::database(format!("media verification lacks {key}")))?,
    )
    .map_err(|e| VoomError::database_context("owner media verification evidence", e))
}

async fn guard_plan(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    plan: &ArtifactAccessPlan,
    context: &RemoteMediaCompletionContext,
) -> Result<(), VoomError> {
    if plan.node_id != context.node_id
        || plan.owner_node_id != Some(context.node_id)
        || plan.worker_id != context.worker_id
        || plan.ticket_id != context.ticket_id
        || plan.lease_id != context.lease_id
        || plan.status != ArtifactAccessPlanStatus::Selected
    {
        return Err(VoomError::Conflict(
            "media access plan no longer matches held completion".to_owned(),
        ));
    }
    let evidence = plan
        .access_evidence
        .as_ref()
        .ok_or_else(|| malformed("media access plan lacks owner proof"))?;
    for epoch in &evidence.root_epochs {
        let root = cp
            .libraries
            .get_library_root_in_tx(tx, epoch.storage_root_id)
            .await?
            .ok_or_else(|| VoomError::NotFound("media access root".to_owned()))?;
        if root.root_epoch != epoch.root_epoch
            || root.owner_node_id != Some(context.node_id)
            || root.state != voom_core::StorageRootState::Active
            || !root.enabled
        {
            return Err(VoomError::Conflict(
                "media access root fence changed".to_owned(),
            ));
        }
    }
    Ok(())
}

/// Complete media state on the existing fenced finalization transaction.
/// Manual artifacts have no remote context and retain their previous behavior.
struct PublishedMediaIdentity {
    version: FileVersionId,
    location: FileLocationId,
    snapshot: voom_core::MediaSnapshotId,
}

pub(crate) async fn finish_scalar_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    record: &ArtifactCommitRecord,
    version_id: FileVersionId,
    location_id: FileLocationId,
) -> Result<(), VoomError> {
    let verification = cp
        .artifacts
        .get_verification_in_tx(tx, record.verification_id)
        .await?
        .ok_or_else(|| VoomError::database("commit verification missing".to_owned()))?;
    if verification.report.get("remote_completion").is_none() {
        if verification.report.get("owner_probe").is_some() {
            return Err(VoomError::database(
                "owner media verification lost its completion context".to_owned(),
            ));
        }
        return Ok(());
    }
    let context: RemoteMediaCompletionContext =
        decode_report(&verification.report, "remote_completion")?;
    context.validate()?;
    if verification.artifact_handle_id != record.artifact_handle_id
        || context.source_file_version_id != record.source_file_version_id
    {
        return Err(VoomError::Conflict(
            "media verification/source does not match its commit".to_owned(),
        ));
    }
    let probe: OwnerProbeEvidence = decode_report(&verification.report, "owner_probe")?;
    let now = cp.clock().now();
    let snapshot = crate::media_snapshot::record_with_event_in_tx(
        cp,
        tx,
        NewMediaSnapshot {
            file_version_id: version_id,
            probed_by: Some(probe.worker_id),
            probed_at: now,
            payload: probe.result.snapshot,
        },
    )
    .await?;
    let mut result = context.result.clone();
    let object = result
        .as_object_mut()
        .ok_or_else(|| VoomError::database("media result is not object".to_owned()))?;
    object.extend(
        serde_json::from_value::<serde_json::Map<String, Value>>(json!({
            "job_id": context.job_id, "ticket_id": context.ticket_id, "lease_id": context.lease_id,
            "source_file_version_id": context.source_file_version_id,
            "staged_artifact_handle_id": record.artifact_handle_id,
            "staged_artifact_location_id": verification.artifact_location_id,
            "verification_id": verification.id, "commit_record_id": record.id,
            "result_file_version_id": version_id, "result_file_location_id": location_id,
            "result_media_snapshot_id": snapshot.id,
        }))
        .map_err(|e| VoomError::Internal(format!("media result construction: {e}")))?,
    );
    audio::finish_synthesis_in_tx(
        cp,
        tx,
        &context,
        record,
        &PublishedMediaIdentity {
            version: version_id,
            location: location_id,
            snapshot: snapshot.id,
        },
        &mut result,
        now,
    )
    .await?;
    finish_in_tx(cp, tx, &context, result, now).await
}

pub(crate) async fn finish_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    context: &RemoteMediaCompletionContext,
    result: Value,
    now: OffsetDateTime,
) -> Result<(), VoomError> {
    let plan = guard_context_in_tx(cp, tx, context, now).await?;
    let evidence = crate::cases::execution::remote_execution::validated_artifact_complete_evidence(
        &context.result,
        &plan,
    )?;
    match cp
        .remote_idempotency
        .reserve_or_replay_in_tx(tx, context.replay_input(now))
        .await?
    {
        IdempotencyOutcome::Reserved => {}
        IdempotencyOutcome::Replay(_) => {
            return Err(VoomError::Conflict(
                "media completion already has a terminal replay".to_owned(),
            ));
        }
    }
    let consumed = cp
        .artifact_access_plans
        .mark_status_in_tx(
            tx,
            plan.id,
            ArtifactAccessPlanStatus::Consumed,
            Some("owner media committed and published".to_owned()),
            evidence,
            now,
        )
        .await?;
    let released = cp
        .release_lease_in_tx(tx, context.lease_id, result, now)
        .await?;
    let outcome = RemoteCompleteOutcome {
        lease_id: released.id,
        ticket_id: released.ticket_id,
        worker_id: released.worker_id,
        artifact_access_plan: crate::cases::execution::remote_execution::remote_plan(&consumed),
    };
    let replay = context.replay_input(now);
    cp.complete_remote_ok_in_tx(
        tx,
        context.node_id,
        &replay.route_key,
        Some(context.worker_id),
        &replay.idempotency_key,
        &outcome,
    )
    .await
}

impl PendingMedia {
    pub(crate) async fn wait(self, cp: &ControlPlane) -> Result<RemoteCompleteOutcome, VoomError> {
        loop {
            let mut tx =
                voom_store::tx::begin_read_only(&cp.pool, "owner media completion wait").await?;
            let replay = cp
                .remote_idempotency
                .lookup_replay_in_tx(&mut tx, &self.context.replay_input(cp.clock().now()))
                .await?;
            if let Some(replay) = replay {
                return match replay {
                    voom_store::repo::execution::remote_idempotency::RemoteMutationReplay::Ok {data} =>
                        serde_json::from_value(data).map_err(|e| VoomError::database_context("owner media complete replay", e)),
                    voom_store::repo::execution::remote_idempotency::RemoteMutationReplay::Error {message, ..} => Err(VoomError::CommitFailure(message)),
                };
            }
            guard_context_in_tx(cp, &mut tx, &self.context, cp.clock().now()).await?;
            for id in &self.commits {
                let record = cp
                    .artifacts
                    .get_commit_record_in_tx(&mut tx, *id)
                    .await?
                    .ok_or_else(|| {
                        VoomError::database("waiting owner commit disappeared".to_owned())
                    })?;
                let intent = cp
                    .artifact_commit_intents
                    .get_by_commit_record_in_tx(&mut tx, *id)
                    .await?
                    .ok_or_else(|| {
                        VoomError::database("waiting owner intent disappeared".to_owned())
                    })?;
                let verification = cp
                    .artifacts
                    .get_verification_in_tx(&mut tx, intent.verification_id)
                    .await?
                    .ok_or_else(|| {
                        VoomError::database("waiting owner verification disappeared".to_owned())
                    })?;
                audio::guard_binding_in_tx(&mut tx, &self.context, &verification, cp.clock().now())
                    .await?;
                if matches!(record.state, voom_store::repo::media::artifacts::ArtifactCommitState::Failed | voom_store::repo::media::artifacts::ArtifactCommitState::RecoveryRequired)
                    || matches!(intent.state, voom_store::repo::media::artifact_commit_intents::ArtifactCommitIntentState::Aborted | voom_store::repo::media::artifact_commit_intents::ArtifactCommitIntentState::RecoveryRequired)
                {
                    return Err(VoomError::Conflict("owner media publication stopped; prepared artifacts retained".to_owned()));
                }
            }
            commit_tx(tx).await?;
            let lease =
                cp.leases.get(self.context.lease_id).await?.ok_or_else(|| {
                    VoomError::database("waiting media lease disappeared".to_owned())
                })?;
            if lease.state != voom_store::repo::execution::leases::LeaseState::Held
                || lease.expires_at <= cp.clock().now()
            {
                return Err(VoomError::Conflict(
                    "owner media publication lost its source lease".to_owned(),
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}

pub(crate) async fn guard_verification_source_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    verification_id: voom_core::ids::ArtifactVerificationId,
    commit_id: voom_core::ids::ArtifactCommitRecordId,
) -> Result<(), VoomError> {
    let Some((verification, context)) =
        read_owner_verification_in_tx(cp, tx, verification_id, commit_id).await?
    else {
        return Ok(());
    };
    audio::guard_binding_in_tx(tx, &context, &verification, cp.clock().now()).await?;
    guard_context_in_tx(cp, tx, &context, cp.clock().now()).await?;
    Ok(())
}

pub(crate) async fn validate_verification_evidence_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    verification_id: voom_core::ids::ArtifactVerificationId,
    commit_id: voom_core::ids::ArtifactCommitRecordId,
) -> Result<(), VoomError> {
    read_owner_verification_in_tx(cp, tx, verification_id, commit_id).await?;
    Ok(())
}

async fn read_owner_verification_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    verification_id: voom_core::ids::ArtifactVerificationId,
    commit_id: voom_core::ids::ArtifactCommitRecordId,
) -> Result<
    Option<(
        voom_store::repo::media::artifacts::ArtifactVerification,
        RemoteMediaCompletionContext,
    )>,
    VoomError,
> {
    let verification = cp
        .artifacts
        .get_verification_in_tx(tx, verification_id)
        .await?
        .ok_or_else(|| VoomError::database("commit verification missing".to_owned()))?;
    if verification.report.get("remote_completion").is_none() {
        let extraction = voom_store::repo::media::audio_extract_operations::SqliteAudioExtractOperationRepo::operation_for_commit_in_tx(tx, commit_id).await?;
        let synthesis = voom_store::repo::media::audio_synthesis_operations::SqliteAudioSynthesisOperationRepo::for_verification_in_tx(tx, verification_id).await?;
        if verification.report.get("owner_probe").is_some()
            || extraction.is_some()
            || synthesis.is_some()
        {
            return Err(VoomError::database(
                "owner media verification lost its completion context".to_owned(),
            ));
        }
        return Ok(None);
    }
    let context: RemoteMediaCompletionContext =
        decode_report(&verification.report, "remote_completion")?;
    context.validate()?;
    let probe: OwnerProbeEvidence = decode_report(&verification.report, "owner_probe")?;
    if probe.worker_id.0 == 0 || i64::try_from(probe.worker_id.0).is_err() {
        return Err(VoomError::database(
            "owner probe worker id is invalid".to_owned(),
        ));
    }
    if probe.result.pre_probe != probe.result.post_probe {
        return Err(VoomError::database(
            "persisted owner probe facts changed".to_owned(),
        ));
    }
    if verification.status != ArtifactVerificationStatus::Succeeded
        || probe.result.pre_probe.size_bytes != verification.expected_size_bytes
        || probe.result.pre_probe.content_hash != verification.expected_checksum
        || verification.observed_size_bytes != Some(verification.expected_size_bytes)
        || verification.observed_checksum.as_ref() != Some(&verification.expected_checksum)
    {
        return Err(VoomError::database(
            "persisted owner probe differs from verification facts".to_owned(),
        ));
    }
    let normalized = normalized_snapshot(probe.result.snapshot.clone())
        .map_err(|error| VoomError::database(format!("persisted owner probe snapshot: {error}")))?;
    if normalized != probe.result.snapshot {
        return Err(VoomError::database(
            "persisted owner probe snapshot lacks canonical stream identities".to_owned(),
        ));
    }
    validate_stored_completion_in_tx(cp, tx, &verification, commit_id, &context, &probe).await?;
    validate_owner_metadata_in_tx(tx, &verification, commit_id, &context).await?;
    Ok(Some((verification, context)))
}

fn validate_dispatch_key(
    key: &str,
    context: &RemoteMediaCompletionContext,
) -> Result<(), VoomError> {
    if key
        != voom_worker_protocol::owner_dispatch_idempotency_key(
            context.incarnation_id,
            context.lease_id,
        )
    {
        return Err(VoomError::database(
            "owner audio dispatch key differs from original child request".to_owned(),
        ));
    }
    Ok(())
}

async fn validate_owner_metadata_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    verification: &voom_store::repo::media::artifacts::ArtifactVerification,
    commit_id: voom_core::ids::ArtifactCommitRecordId,
    context: &RemoteMediaCompletionContext,
) -> Result<(), VoomError> {
    let extraction = voom_store::repo::media::audio_extract_operations::SqliteAudioExtractOperationRepo::operation_for_commit_in_tx(tx, commit_id).await?;
    let synthesis = voom_store::repo::media::audio_synthesis_operations::SqliteAudioSynthesisOperationRepo::for_verification_in_tx(tx, verification.id).await?;
    if extraction.is_some() && synthesis.is_some() {
        return Err(VoomError::database(
            "owner verification has ambiguous audio operation ownership".to_owned(),
        ));
    }
    if let Some((record, claim)) = &extraction {
        let attempt = voom_store::repo::media::audio_extract_operations::SqliteAudioExtractOperationRepo::get_dispatch_attempt_in_tx(tx, record.operation.id, claim.expected_generation).await?
            .ok_or_else(|| VoomError::database("owner extraction attempt missing".to_owned()))?;
        validate_dispatch_key(&attempt.idempotency_key, context)?;
    }
    if let Some(record) = &synthesis {
        let attempt = voom_store::repo::media::audio_synthesis_operations::SqliteAudioSynthesisOperationRepo::get_dispatch_attempt_in_tx(tx, record.operation.id, record.operation.dispatch_generation).await?
            .ok_or_else(|| VoomError::database("owner synthesis attempt missing".to_owned()))?;
        validate_dispatch_key(&attempt.idempotency_key, context)?;
    }
    let worker_result = owner_worker_result(&context.result)
        .ok_or_else(|| VoomError::database("owner completion result is not object".to_owned()))?;
    if extraction.as_ref().is_some_and(|(record, _claim)| {
        record.operation.worker_result.as_ref() != Some(&worker_result)
    }) || synthesis
        .as_ref()
        .is_some_and(|record| record.operation.worker_result.as_ref() != Some(&worker_result))
    {
        return Err(VoomError::database(
            "audio ledger worker result differs from original owner completion".to_owned(),
        ));
    }
    if verification.worker_id != context.worker_id {
        return Err(VoomError::database(
            "owner verification worker differs from original mutation worker".to_owned(),
        ));
    }
    if (extraction.is_some() || synthesis.is_some())
        && (verification.workflow_ticket_id.is_some() || verification.workflow_lease_id.is_some())
    {
        return Err(VoomError::database(
            "owner audio verification carries a contradictory workflow binding".to_owned(),
        ));
    }
    Ok(())
}

fn validate_original_probe(
    output: &voom_worker_protocol::OwnerObservedOutput,
    probe: &OwnerProbeEvidence,
    companions: Option<
        &[voom_store::repo::media::audio_synthesis_operations::AudioSynthesisCompanion],
    >,
    raw: &Value,
) -> Result<(), VoomError> {
    let mut original = output
        .probe
        .clone()
        .ok_or_else(|| VoomError::database("original owner probe missing".to_owned()))?;
    original.result.snapshot = normalized_snapshot(original.result.snapshot)
        .map_err(|error| VoomError::database(format!("original owner probe snapshot: {error}")))?;
    if let Some(companions) = companions {
        let worker_result = owner_worker_result(raw)
            .ok_or_else(|| VoomError::database("original owner result is not object".to_owned()))?;
        audio::normalize_synthesis_snapshot(
            companions,
            &worker_result,
            &mut original.result.snapshot,
        )
        .map_err(|error| VoomError::database(format!("original synthesis probe: {error}")))?;
    }
    if &original != probe
        || output.facts.size_bytes != probe.result.pre_probe.size_bytes
        || output.facts.content_hash != probe.result.pre_probe.content_hash
    {
        return Err(VoomError::database(
            "persisted owner probe differs from original observed output".to_owned(),
        ));
    }
    Ok(())
}

async fn guard_context_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    context: &RemoteMediaCompletionContext,
    now: OffsetDateTime,
) -> Result<ArtifactAccessPlan, VoomError> {
    context.validate()?;
    let node = cp
        .nodes
        .require_active_incarnation_in_tx(tx, context.node_id, context.incarnation_id)
        .await?;
    crate::cases::execution::remote_execution::validate_remote_node_freshness(
        node.status,
        node.last_seen_at,
        node.heartbeat_ttl_seconds,
        node.id,
        now,
        false,
    )?;
    cp.workers
        .incarnation_owned_worker_in_tx(
            tx,
            context.worker_id,
            context.node_id,
            context.incarnation_id,
        )
        .await?;
    let lease = cp
        .leases
        .get_held_for_worker_in_tx(tx, context.lease_id, context.worker_id)
        .await?;
    if lease.ticket_id != context.ticket_id || lease.expires_at <= now {
        return Err(VoomError::Conflict(
            "media completion lost its held source lease".to_owned(),
        ));
    }
    let plan = cp
        .artifact_access_plans
        .get_by_lease_in_tx(tx, context.lease_id)
        .await?
        .ok_or_else(|| {
            VoomError::database("held media completion has no access plan".to_owned())
        })?;
    guard_plan(cp, tx, &plan, context).await?;

    let location = cp
        .identity
        .get_file_location_in_tx(tx, context.source_location_id)
        .await?
        .ok_or_else(|| VoomError::Conflict("media source location disappeared".to_owned()))?;
    let source = cp
        .identity
        .get_file_version_in_tx(tx, context.source_file_version_id)
        .await?
        .ok_or_else(|| VoomError::Conflict("media source version disappeared".to_owned()))?;
    let (root, locator) = location.rooted_address()?;
    let expected = context
        .source_location_fence
        .as_ref()
        .ok_or_else(|| VoomError::database("media source fence missing".to_owned()))?;
    if location.retired_at.is_some()
        || source.retired_at.is_some()
        || location.file_version_id != context.source_file_version_id
        || location.epoch != expected.epoch
        || root != expected.storage_root_id
        || locator != &expected.provider_relative_locator
    {
        return Err(VoomError::Conflict(
            "media source location fence changed".to_owned(),
        ));
    }

    let ticket = cp
        .tickets
        .get_in_tx(tx, context.ticket_id)
        .await?
        .ok_or_else(|| VoomError::database("media source ticket missing".to_owned()))?;
    let rendered = ticket.payload.get("rendered_payload").ok_or_else(|| {
        VoomError::database("media source ticket lost rendered payload".to_owned())
    })?;
    if ticket.job_id != Some(context.job_id)
        || FileVersionId(required_id(rendered, "source_file_version_id")?)
            != context.source_file_version_id
        || FileLocationId(required_id(rendered, "source_location_id")?)
            != context.source_location_id
    {
        return Err(VoomError::Conflict(
            "media source ticket context changed".to_owned(),
        ));
    }
    Ok(plan)
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

async fn completion_context_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    input: &RemoteCompleteInput,
    ticket: &Ticket,
    payload: &WorkflowTicketPayload,
) -> Result<RemoteMediaCompletionContext, VoomError> {
    let source_file_version_id = FileVersionId(required_id(
        &payload.rendered_payload,
        "source_file_version_id",
    )?);
    let source_location_id = FileLocationId(required_id(
        &payload.rendered_payload,
        "source_location_id",
    )?);
    let source_location = cp
        .identity
        .get_file_location_in_tx(tx, source_location_id)
        .await?
        .ok_or_else(|| VoomError::NotFound("media source location".to_owned()))?;
    let (source_root_id, source_locator) = source_location.rooted_address()?;
    let context = RemoteMediaCompletionContext {
        node_id: input.node_id,
        incarnation_id: input.incarnation_id,
        worker_id: input.worker_id,
        lease_id: input.lease_id,
        ticket_id: ticket.id,
        job_id: ticket
            .job_id
            .ok_or_else(|| malformed("media ticket has no job"))?,
        source_file_version_id,
        source_location_id,
        source_location_fence: Some(SourceLocationFence {
            epoch: source_location.epoch,
            storage_root_id: source_root_id,
            provider_relative_locator: source_locator.clone(),
        }),
        idempotency_key: input.idempotency_key.clone(),
        request_hash: input.request_hash.clone(),
        result: input.result.clone(),
    };
    Ok(context)
}

async fn existing_pending_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    input: &RemoteCompleteInput,
    context: &RemoteMediaCompletionContext,
    admitted_audio: Option<&audio::AdmittedAudio>,
) -> Result<Option<PendingMedia>, VoomError> {
    // Remux/video use the lease-bound verification. Audio uses its exact
    // operation/output/attempt ledger and keeps both workflow fields unbound.
    let existing = if let Some(audio) = admitted_audio {
        if let Some(verification_id) = audio.verification_id() {
            Some(
                cp.artifacts
                    .get_verification_in_tx(tx, verification_id)
                    .await?
                    .ok_or_else(|| {
                        VoomError::database("admitted audio verification missing".to_owned())
                    })?,
            )
        } else {
            None
        }
    } else {
        cp.artifacts
            .verification_for_workflow_lease_in_tx(tx, input.lease_id)
            .await?
    };
    if let Some(existing) = existing {
        let saved: RemoteMediaCompletionContext =
            decode_report(&existing.report, "remote_completion")?;
        saved.validate()?;
        if saved != *context {
            return Err(VoomError::Conflict(
                "media completion differs from its staged request".to_owned(),
            ));
        }
        let commits = if let Some(audio::AdmittedAudio::Extraction(record, _)) = admitted_audio {
            record
                .outputs
                .iter()
                .map(|output| {
                    output.commit_record_id.ok_or_else(|| {
                        VoomError::database(
                            "staged extraction output lost commit binding".to_owned(),
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            let records = cp
                .artifacts
                .list_commit_records_in_tx(tx, existing.artifact_handle_id)
                .await?;
            let matching: Vec<_> = records
                .into_iter()
                .filter(|record| record.verification_id == existing.id)
                .map(|record| record.id)
                .collect();
            if matching.len() != 1 {
                return Err(VoomError::database(
                    "staged owner verification commit binding is ambiguous".to_owned(),
                ));
            }
            matching
        };
        return Ok(Some(PendingMedia {
            context: context.clone(),
            commits,
        }));
    }
    Ok(None)
}

async fn stage_output_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    publication: &StagingPublication<'_>,
    validated: &ValidatedOwnerOutput,
    index: usize,
    now: OffsetDateTime,
) -> Result<audio::AudioStage, VoomError> {
    let context = publication.context;
    let admitted_audio = publication.audio;
    let ValidatedOwnerOutput {
        output,
        observed,
        path,
        ..
    } = validated;
    let (handle_id, staged) = create_staged_handle_in_tx(cp, tx, context, validated, now).await?;
    let (verification, probe) = create_owner_verification_in_tx(
        cp,
        tx,
        publication,
        validated,
        (handle_id, &staged),
        index,
        now,
    )
    .await?;
    let name = output
        .provider_relative_locator
        .as_str()
        .rsplit('/')
        .next()
        .ok_or_else(|| malformed("output filename missing"))?;
    let family = match publication.operation {
        OperationKind::Remux => "remux",
        OperationKind::TranscodeVideo => "transcode",
        _ => "audio",
    };
    let target = if let Some(audio) = &admitted_audio {
        audio::rooted_path_in_tx(cp, tx, output.storage_root_id, audio.target_path(index)?).await?
    } else {
        ProviderRelativeLocator::new(format!(".committed/{family}/t{}-{name}", context.ticket_id))?
    };
    let commit_id = super::commit::prepare_owner_media_in_tx(
        cp,
        tx,
        handle_id,
        output.storage_root_id,
        output.provider_relative_locator.clone(),
        target,
        now,
    )
    .await?;
    Ok(audio::AudioStage {
        handle_id,
        location_id: staged.id,
        verification_id: verification.id,
        commit_id,
        path: path.to_string_lossy().into_owned(),
        facts: observed.facts.clone(),
        probe,
    })
}

async fn create_staged_handle_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    context: &RemoteMediaCompletionContext,
    validated: &ValidatedOwnerOutput,
    now: OffsetDateTime,
) -> Result<
    (
        voom_core::ArtifactHandleId,
        voom_store::repo::media::artifacts::ArtifactLocation,
    ),
    VoomError,
> {
    let ValidatedOwnerOutput { observed, path, .. } = validated;
    let handle = cp
        .artifacts
        .create_handle_in_tx(
            tx,
            NewArtifactHandle {
                size_bytes: Some(
                    i64::try_from(observed.facts.size_bytes)
                        .map_err(|_| malformed("output size exceeds SQLite range"))?,
                ),
                checksum: Some(observed.facts.content_hash.clone()),
                privacy_class: "internal".to_owned(),
                durability_class: "staging".to_owned(),
                allowed_access_modes: vec![ArtifactHandleAccessMode::LocalPath],
                mutability: "immutable".to_owned(),
                source_lineage: Some(
                    json!({"source_file_version_id": context.source_file_version_id,
                "source_file_location_id": context.source_location_id}),
                ),
                file_version_id: Some(context.source_file_version_id),
                created_at: now,
            },
        )
        .await?;
    append_event(
        &cp.events,
        tx,
        SubjectType::ArtifactHandle,
        Some(handle.id.0),
        now,
        Event::ArtifactHandleCreated(ArtifactHandleCreatedPayload {
            artifact_handle_id: handle.id,
            privacy_class: handle.privacy_class,
            durability_class: handle.durability_class,
            mutability: handle.mutability,
        }),
    )
    .await?;
    let staged = cp
        .artifacts
        .record_location_in_tx(
            tx,
            NewArtifactLocation {
                artifact_handle_id: handle.id,
                kind: ArtifactLocationKind::Staging,
                value: path.to_string_lossy().into_owned(),
                observed_at: now,
            },
        )
        .await?;
    append_event(
        &cp.events,
        tx,
        SubjectType::ArtifactLocation,
        Some(staged.id.0),
        now,
        Event::ArtifactLocationRecorded(ArtifactLocationRecordedPayload {
            artifact_location_id: staged.id,
            artifact_handle_id: handle.id,
            kind: "staging".to_owned(),
            value: staged.value.clone(),
        }),
    )
    .await?;
    Ok((handle.id, staged))
}

async fn create_owner_verification_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    publication: &StagingPublication<'_>,
    validated: &ValidatedOwnerOutput,
    staged_handle: (
        voom_core::ArtifactHandleId,
        &voom_store::repo::media::artifacts::ArtifactLocation,
    ),
    index: usize,
    now: OffsetDateTime,
) -> Result<
    (
        voom_store::repo::media::artifacts::ArtifactVerification,
        OwnerProbeEvidence,
    ),
    VoomError,
> {
    let context = publication.context;
    let admitted_audio = publication.audio;
    let (handle_id, staged) = staged_handle;
    let ValidatedOwnerOutput {
        observed, snapshot, ..
    } = validated;
    append_event(
        &cp.events,
        tx,
        SubjectType::ArtifactHandle,
        Some(handle_id.0),
        now,
        Event::ArtifactVerificationStarted(ArtifactVerificationStartedPayload {
            artifact_handle_id: handle_id,
            artifact_location_id: staged.id,
            worker_id: context.worker_id,
            path: staged.value.clone(),
        }),
    )
    .await?;
    let mut probe = observed
        .probe
        .clone()
        .ok_or_else(|| malformed("validated probe disappeared"))?;
    probe.result.snapshot = snapshot.clone();
    let verification = cp
        .artifacts
        .record_verification_in_tx(
            tx,
            NewArtifactVerification {
                artifact_handle_id: handle_id,
                artifact_location_id: staged.id,
                path: staged.value.clone(),
                worker_id: context.worker_id,
                workflow_ticket_id: (admitted_audio.is_none() && index == 0)
                    .then_some(context.ticket_id),
                workflow_lease_id: (admitted_audio.is_none() && index == 0)
                    .then_some(context.lease_id),
                status: ArtifactVerificationStatus::Succeeded,
                expected_size_bytes: observed.facts.size_bytes,
                expected_checksum: observed.facts.content_hash.clone(),
                observed_size_bytes: Some(observed.facts.size_bytes),
                observed_checksum: Some(observed.facts.content_hash.clone()),
                failure_class: None,
                error_code: None,
                message: None,
                report: json!({"owner_probe": probe, "remote_completion": context}),
                started_at: now,
                finished_at: now,
            },
        )
        .await?;
    append_event(
        &cp.events,
        tx,
        SubjectType::ArtifactHandle,
        Some(handle_id.0),
        now,
        Event::ArtifactVerificationSucceeded(ArtifactVerificationSucceededPayload {
            verification_id: verification.id,
            artifact_handle_id: handle_id,
            artifact_location_id: staged.id,
            worker_id: context.worker_id,
            observed_size_bytes: observed.facts.size_bytes,
            observed_checksum: observed.facts.content_hash.clone(),
        }),
    )
    .await?;
    Ok((verification, probe))
}

async fn validate_owner_evidence_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    publication: &StagingPublication<'_>,
    dispatch: &MediaDispatch,
    outputs: &[MediaPlannedOutput],
    encoded: &Value,
    now: OffsetDateTime,
) -> Result<(OwnerObservedEvidence, Value), VoomError> {
    let context = publication.context;
    let observed: OwnerObservedEvidence = serde_json::from_value(
        context
            .result
            .get("agent_observed")
            .cloned()
            .ok_or_else(|| malformed("missing owner output observations"))?,
    )
    .map_err(|e| malformed(&format!("owner observations: {e}")))?;
    if outputs.is_empty() || outputs.len() != observed.outputs.len() {
        return Err(malformed("owner output count differs from dispatch"));
    }
    for key in [
        "staged_artifact_handle_id",
        "commit_record_id",
        "result_file_version_id",
        "result_file_location_id",
    ] {
        if context.result.get(key).is_some() {
            return Err(malformed("worker supplied reserved publication identity"));
        }
    }
    let source = cp
        .identity
        .get_file_version_in_tx(tx, context.source_file_version_id)
        .await?
        .ok_or_else(|| VoomError::NotFound("media source version".to_owned()))?;
    let location = cp
        .identity
        .get_file_location_in_tx(tx, context.source_location_id)
        .await?
        .ok_or_else(|| VoomError::NotFound("media source location".to_owned()))?;
    if source.retired_at.is_some()
        || location.retired_at.is_some()
        || location.file_version_id != source.id
    {
        return Err(VoomError::Conflict(
            "media source identity is no longer live".to_owned(),
        ));
    }
    let (source_root, source_locator) = location.rooted_address()?;
    let expected_source = encoded
        .get("source")
        .ok_or_else(|| malformed("dispatch lacks source"))?;
    if expected_source
        .get("storage_root_id")
        .and_then(Value::as_u64)
        != Some(source_root.0)
        || expected_source
            .get("provider_relative_locator")
            .and_then(Value::as_str)
            != Some(source_locator.as_str())
    {
        return Err(VoomError::Conflict(
            "media dispatch source differs from its durable source location".to_owned(),
        ));
    }
    for facts in [&observed.input_pre, &observed.input_post] {
        if facts.size_bytes != source.size_bytes || facts.content_hash != source.content_hash {
            return Err(malformed("owner input facts differ from source version"));
        }
    }
    guard_context_in_tx(cp, tx, context, now).await?;
    let raw = owner_worker_result(&context.result)
        .ok_or_else(|| malformed("worker result must be an object"))?;
    validate_worker_result(publication.operation, &raw)?;
    validate_extract_result_in_tx(cp, tx, dispatch, context, &raw).await?;
    validate_worker_input(&raw, &observed)?;
    Ok((observed, raw))
}

async fn validate_outputs_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    publication: &StagingPublication<'_>,
    dispatch: &MediaDispatch,
    outputs: &[MediaPlannedOutput],
    observed: &OwnerObservedEvidence,
    raw: &Value,
) -> Result<Vec<ValidatedOwnerOutput>, VoomError> {
    let context = publication.context;
    let mut validated = Vec::with_capacity(outputs.len());
    for (index, (output, observed)) in outputs.iter().zip(&observed.outputs).enumerate() {
        i64::try_from(observed.facts.size_bytes)
            .map_err(|_| malformed("output size exceeds SQLite range"))?;
        if observed.provider_relative_locator != output.provider_relative_locator {
            return Err(malformed(
                "owner output locator differs from dispatch order",
            ));
        }
        let reported = if outputs.len() == 1 && !matches!(dispatch, MediaDispatch::ExtractAudio(_))
        {
            raw.get("output")
        } else {
            raw.get("outputs")
                .and_then(Value::as_array)
                .and_then(|a| a.get(index))
                .and_then(|v| v.get("output"))
        }
        .ok_or_else(|| malformed("worker output facts missing"))?;
        if reported.get("size_bytes").and_then(Value::as_u64) != Some(observed.facts.size_bytes)
            || reported.get("content_hash").and_then(Value::as_str)
                != Some(&observed.facts.content_hash)
        {
            return Err(malformed("worker output differs from owner observation"));
        }
        let probe = observed
            .probe
            .as_ref()
            .ok_or_else(|| malformed("required output has no attributed probe"))?;
        cp.workers
            .incarnation_owned_worker_in_tx(
                tx,
                probe.worker_id,
                context.node_id,
                context.incarnation_id,
            )
            .await?;
        if !cp
            .workers
            .operation_eligibility_in_tx(tx, probe.worker_id, &TicketOperation::new("probe_file")?)
            .await?
            .is_eligible()
        {
            return Err(malformed("attributed child cannot probe files"));
        }
        if probe.result.pre_probe != probe.result.post_probe
            || [&probe.result.pre_probe, &probe.result.post_probe]
                .iter()
                .any(|facts| {
                    facts.size_bytes != observed.facts.size_bytes
                        || facts.content_hash != observed.facts.content_hash
                })
            || observed
                .snapshot
                .as_ref()
                .is_some_and(|s| s != &probe.result.snapshot)
        {
            return Err(malformed(
                "staged probe does not describe the observed output",
            ));
        }
        let mut snapshot = normalized_snapshot(probe.result.snapshot.clone())?;
        if let Some(audio) = publication.audio {
            audio.normalize_snapshot(raw, &mut snapshot)?;
            snapshot = normalized_snapshot(snapshot)?;
        }
        let root = cp
            .libraries
            .get_library_root_in_tx(tx, output.storage_root_id)
            .await?
            .ok_or_else(|| VoomError::NotFound("media output root".to_owned()))?;
        if root.owner_node_id != Some(context.node_id)
            || root.state != voom_core::StorageRootState::Active
            || !root.enabled
        {
            return Err(VoomError::Conflict(
                "media output root owner/state changed".to_owned(),
            ));
        }
        let path = std::path::PathBuf::from(root.provider_locator.as_str())
            .join(output.provider_relative_locator.as_str());
        validated.push(ValidatedOwnerOutput {
            output: output.clone(),
            observed: observed.clone(),
            snapshot,
            path,
        });
    }
    Ok(validated)
}

async fn validate_stored_completion_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    verification: &voom_store::repo::media::artifacts::ArtifactVerification,
    commit_id: voom_core::ids::ArtifactCommitRecordId,
    context: &RemoteMediaCompletionContext,
    probe: &OwnerProbeEvidence,
) -> Result<(), VoomError> {
    let ticket = cp
        .tickets
        .get_in_tx(tx, context.ticket_id)
        .await?
        .ok_or_else(|| VoomError::database("owner completion source ticket missing".to_owned()))?;
    let payload = WorkflowTicketPayload::parse_ticket(ticket.kind.as_str(), ticket.payload)
        .map_err(|error| {
            VoomError::database(format!("owner completion workflow payload: {error}"))
        })?;
    let observed: OwnerObservedEvidence = serde_json::from_value(
        context
            .result
            .get("agent_observed")
            .cloned()
            .ok_or_else(|| {
                VoomError::database("persisted owner completion observations missing".to_owned())
            })?,
    )
    .map_err(|error| {
        VoomError::database_context("persisted owner completion observations", error)
    })?;
    if observed.outputs.is_empty()
        || observed.outputs.len() > voom_worker_protocol::MAX_COMMIT_GROUP_OUTPUTS
    {
        return Err(VoomError::database(
            "persisted owner completion output count invalid".to_owned(),
        ));
    }
    for output in &observed.outputs {
        let child = output.probe.as_ref().ok_or_else(|| {
            VoomError::database("persisted owner completion probe missing".to_owned())
        })?;
        if child.worker_id.0 == 0 || i64::try_from(child.worker_id.0).is_err() {
            return Err(VoomError::database(
                "persisted owner completion probe worker invalid".to_owned(),
            ));
        }
    }
    let intent = cp
        .artifact_commit_intents
        .get_by_commit_record_in_tx(tx, commit_id)
        .await?
        .ok_or_else(|| VoomError::database("owner verification intent missing".to_owned()))?;
    let staging = cp
        .identity
        .get_file_location_in_tx(tx, intent.staging_location_id)
        .await?
        .ok_or_else(|| VoomError::database("owner verification staging pin missing".to_owned()))?;
    let (_, relative) = staging.rooted_address()?;
    let mut matching = observed
        .outputs
        .iter()
        .filter(|output| &output.provider_relative_locator == relative);
    let original = matching.next().ok_or_else(|| {
        VoomError::database("owner verification output association missing".to_owned())
    })?;
    if matching.next().is_some() {
        return Err(VoomError::database(
            "owner verification output association ambiguous".to_owned(),
        ));
    }
    let synthesis = voom_store::repo::media::audio_synthesis_operations::SqliteAudioSynthesisOperationRepo::for_verification_in_tx(tx, verification.id).await?;
    validate_original_probe(
        original,
        probe,
        synthesis
            .as_ref()
            .map(|record| record.companions.as_slice()),
        &context.result,
    )?;
    let raw = owner_worker_result(&context.result)
        .ok_or_else(|| VoomError::database("persisted owner result is not object".to_owned()))?;
    validate_worker_result(payload.operation, &raw)
        .map_err(|error| VoomError::database(format!("persisted owner worker result: {error}")))?;
    let dispatch = decode_media_dispatch(
        payload
            .rendered_payload
            .get("media_dispatch")
            .ok_or_else(|| VoomError::database("persisted owner dispatch missing".to_owned()))?,
    )
    .map_err(|error| VoomError::database(format!("persisted owner dispatch: {error}")))?;
    validate_extract_result_in_tx(cp, tx, &dispatch, context, &raw)
        .await
        .map_err(|error| VoomError::database(format!("persisted extraction result: {error}")))?;
    validate_worker_input(&raw, &observed)
        .map_err(|error| VoomError::database(format!("persisted owner worker input: {error}")))?;
    Ok(())
}
