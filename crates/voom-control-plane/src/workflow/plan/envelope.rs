//! Byte-free `media_dispatch` envelope assembly at ticket-creation time
//! (ADR 0075 flip).
//!
//! Every helper here reads durable rows (identity, library roots, media
//! snapshots) or ticket-borne JSON; none of them stat, canonicalize, hash,
//! or open a byte. An envelope renders only when all of its inputs are
//! derivable: a live rooted source handle, expected facts, and — for
//! operations with planned outputs — the library's configured destination
//! root. When an input is unset the render fails: since the bundled
//! media adapters are gone (T8), a byte-touching ticket without an
//! envelope has no execution contract left, so a missing input aborts
//! ticket creation with a render error instead of silently falling back.

use serde_json::Value;
use voom_core::{FileLocationId, FileVersionId, OperationKind, StorageRootId, VoomError};
use voom_plan::planner::audio::AudioOperationType;

use voom_store::repo::library::library_roots::LibraryRoot;
use voom_store::repo::media::identity::{
    FileLocationRepo, FileVersion, FileVersionRepo, MediaSnapshot, MediaSnapshotRepo,
};
use voom_worker_protocol::{
    AudioExpectedFacts, ExpectedFileFacts, RemuxExpectedFacts, TranscodeVideoExpectedFacts,
    TranscodeVideoProfile, VerifyArtifactExpectedFacts,
};

use crate::ControlPlane;
use crate::workflow::plan::binding::{BindingError, PolicyFileSource, media_dispatch};

/// Size and hash pinned into an envelope's `expected` block.
///
/// `modified_at` / `local_file_key` stay `None` here: identity rows do not
/// carry them, and they are optional observation hints, not identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceFacts {
    pub size_bytes: u64,
    pub content_hash: String,
}

impl SourceFacts {
    /// Facts pinned from the source `file_versions` row.
    #[must_use]
    pub(crate) fn from_version(version: &FileVersion) -> Self {
        Self {
            size_bytes: version.size_bytes,
            content_hash: version.content_hash.clone(),
        }
    }

    /// Facts pinned from a scan-recorded `source_file` block.
    ///
    /// # Errors
    ///
    /// Fails when the block lacks a usable `size_bytes`/`content_hash` pair.
    pub(crate) fn from_source_file(source_file: &Value) -> Result<Self, VoomError> {
        let object = source_file.as_object().ok_or_else(|| {
            VoomError::Config("ticket source_file must be a JSON object".to_owned())
        })?;
        let size_bytes = object
            .get("size_bytes")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                VoomError::Config("ticket source_file requires size_bytes".to_owned())
            })?;
        let content_hash = object
            .get("content_hash")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                VoomError::Config("ticket source_file requires content_hash".to_owned())
            })?
            .to_owned();
        Ok(Self {
            size_bytes,
            content_hash,
        })
    }

    fn file(self) -> ExpectedFileFacts {
        ExpectedFileFacts {
            size_bytes: self.size_bytes,
            content_hash: self.content_hash,
            modified_at: None,
            local_file_key: None,
        }
    }

    fn audio(self) -> AudioExpectedFacts {
        AudioExpectedFacts {
            size_bytes: self.size_bytes,
            content_hash: self.content_hash,
            modified_at: None,
            local_file_key: None,
        }
    }

    fn remux(self) -> RemuxExpectedFacts {
        RemuxExpectedFacts {
            size_bytes: self.size_bytes,
            content_hash: self.content_hash,
            modified_at: None,
            local_file_key: None,
        }
    }

    fn video(self) -> TranscodeVideoExpectedFacts {
        TranscodeVideoExpectedFacts {
            size_bytes: self.size_bytes,
            content_hash: self.content_hash,
            modified_at: None,
            local_file_key: None,
        }
    }
}

fn verify_facts(facts: SourceFacts) -> VerifyArtifactExpectedFacts {
    VerifyArtifactExpectedFacts {
        size_bytes: facts.size_bytes,
        content_hash: facts.content_hash,
        modified_at: None,
        local_file_key: None,
    }
}

/// The live rooted handle behind one recorded source identity, plus the file
/// version that location resolves to (remux/audio children need it to find
/// their planning snapshot).
///
/// # Errors
/// When the location row vanished or lost its rooted address between
/// declaration and render: an unresolvable source must not reach a lease.
pub(crate) async fn location_source(
    cp: &ControlPlane,
    storage_root_id: StorageRootId,
    file_location_id: FileLocationId,
) -> Result<
    (
        media_dispatch::MediaDispatchSource,
        voom_core::FileVersionId,
    ),
    VoomError,
> {
    let location = cp
        .identity
        .get_file_location(file_location_id)
        .await?
        .ok_or_else(|| {
            VoomError::Config(format!(
                "media dispatch envelope: file_location {file_location_id} vanished"
            ))
        })?;
    crate::operation_source::require_live_rooted(&location).map_err(|error| {
        VoomError::Config(format!(
            "media dispatch envelope: file_location {file_location_id} is not live rooted: {error}"
        ))
    })?;
    let (root, locator) = location.rooted_address().map_err(|error| {
        VoomError::Config(format!(
            "media dispatch envelope: file_location {file_location_id} has no rooted address: \
             {error}"
        ))
    })?;
    if root != storage_root_id {
        return Err(VoomError::Config(format!(
            "file_location {file_location_id} moved to storage root {root}, \
             but the ticket declares root {storage_root_id}"
        )));
    }
    Ok((
        media_dispatch::MediaDispatchSource::Location {
            storage_root_id: root,
            file_location_id,
            provider_relative_locator: locator.clone(),
        },
        location.file_version_id,
    ))
}

fn root_default(
    root: &LibraryRoot,
    role: media_dispatch::DestinationRole,
) -> Option<StorageRootId> {
    match role {
        media_dispatch::DestinationRole::Output => root.default_output_root_id,
        media_dispatch::DestinationRole::Staging => root.default_staging_root_id,
        media_dispatch::DestinationRole::Backup => root.default_backup_root_id,
    }
}

fn names_default(
    root: &LibraryRoot,
    configured: StorageRootId,
    role: media_dispatch::DestinationRole,
) -> bool {
    root_default(root, role) == Some(configured)
}

/// Resolve the default root `role` resolves to relative to
/// `configured_root_id`.
///
/// A library root resolves its own row first; a staging/output leaf resolves
/// through whichever library row assigns it as a default, because that owner
/// holds the sibling defaults.
///
/// # Errors
/// When nothing is configured: the bundled fallback that used to execute such
/// tickets is gone, so an unaddressable destination must fail the render.
pub(crate) async fn destination_root(
    cp: &ControlPlane,
    role: media_dispatch::DestinationRole,
    configured_root_id: StorageRootId,
) -> Result<StorageRootId, VoomError> {
    if let Some(root) = cp.libraries.get_library_root(configured_root_id).await?
        && let Some(direct) = root_default(&root, role)
    {
        return Ok(direct);
    }
    let roots = cp.libraries.list_library_roots(None).await?;
    roots
        .iter()
        .filter(|root| names_default(root, configured_root_id, role))
        .find_map(|owner| root_default(owner, role))
        .ok_or_else(|| {
            VoomError::Config(format!(
                "media dispatch envelope: no default {role:?} root configured for \
                 storage root {configured_root_id}"
            ))
        })
}

async fn snapshot_for(
    cp: &ControlPlane,
    file_version_id: FileVersionId,
    operation_payload: &Value,
) -> Result<MediaSnapshot, VoomError> {
    let id = operation_payload
        .get("source_media_snapshot_id")
        .and_then(Value::as_u64)
        .filter(|id| *id > 0)
        .ok_or_else(|| {
            VoomError::Config(format!(
                "media dispatch envelope: operation payload for file_version {file_version_id} \
                 pins no source_media_snapshot_id"
            ))
        })?;
    let snapshot = cp
        .identity
        .get_media_snapshot(voom_core::ids::MediaSnapshotId(id))
        .await?
        .ok_or_else(|| {
            VoomError::Config(format!(
                "media dispatch envelope: media_snapshot {id} does not resolve"
            ))
        })?;
    if snapshot.file_version_id != file_version_id {
        return Err(VoomError::Config(format!(
            "media_snapshot {id} does not belong to file_version {file_version_id}"
        )));
    }
    Ok(snapshot)
}

fn binding_result(result: Result<Value, BindingError>) -> Result<Option<Value>, VoomError> {
    result
        .map(Some)
        .map_err(|error| VoomError::Config(format!("media dispatch envelope binding: {error}")))
}

/// Render the `media_dispatch` envelope for a **policy-root** media ticket.
///
/// `operation_payload` is the planner node's raw payload (the `type:`-tagged
/// block). An unset planning input is a render error: with the bundled media
/// adapters removed there is no fallback contract to keep. `Ok(None)` is
/// reserved for operations outside the envelope family and for policy-root
/// verify tickets, which stay bundled until #528/#424 retargets them.
pub(crate) async fn policy_envelope(
    cp: &ControlPlane,
    branch_id: &str,
    operation: OperationKind,
    source: &PolicyFileSource,
    operation_payload: &Value,
) -> Result<Option<Value>, VoomError> {
    let (source_ref, _) = location_source(cp, source.storage_root_id, source.location_id).await?;
    let version = cp
        .identity
        .get_file_version(source.file_version_id)
        .await?
        .ok_or_else(|| {
            VoomError::Config(format!(
                "media dispatch envelope: file_version {} vanished",
                source.file_version_id.0
            ))
        })?;
    let facts = SourceFacts::from_version(&version);

    match operation {
        OperationKind::ProbeFile => binding_result(media_dispatch::render_media_dispatch_probe(
            &source_ref,
            facts.file(),
        )),
        OperationKind::BackUpFile => {
            let destination = destination_root(
                cp,
                media_dispatch::DestinationRole::Backup,
                source.storage_root_id,
            )
            .await?;
            binding_result(media_dispatch::render_media_dispatch_back_up_file(
                &source_ref,
                source.file_version_id,
                destination,
            ))
        }
        OperationKind::TranscodeVideo => {
            let profile = serde_json::from_value::<TranscodeVideoProfile>(
                operation_payload
                    .get("resolved_profile")
                    .cloned()
                    .unwrap_or(Value::Null),
            )
            .map_err(|error| {
                VoomError::Config(format!(
                    "media dispatch envelope: transcode-video profile does not resolve: {error}"
                ))
            })?;
            let destination = staging_destination(cp, source.storage_root_id).await?;
            binding_result(media_dispatch::render_media_dispatch_transcode_video(
                branch_id,
                &source_ref,
                facts.video(),
                destination,
                profile,
                None,
                false,
            ))
        }
        OperationKind::Remux => {
            let snapshot = snapshot_for(cp, source.file_version_id, operation_payload).await?;
            let selection = crate::remux::selection::selection_from_payload_and_snapshot(
                operation_payload,
                &snapshot,
            )
            .map_err(|error| {
                VoomError::Config(format!(
                    "media dispatch envelope: remux selection does not derive: {error}"
                ))
            })?;
            let destination = staging_destination(cp, source.storage_root_id).await?;
            binding_result(media_dispatch::render_media_dispatch_remux(
                branch_id,
                &source_ref,
                facts.remux(),
                selection,
                destination,
            ))
        }
        OperationKind::TranscodeAudio | OperationKind::ExtractAudio => {
            audio_envelope(
                cp,
                branch_id,
                &source_ref,
                source.file_version_id,
                operation_payload,
                facts,
            )
            .await
        }
        // A policy verification targets a live library location, but the
        // envelope family reserves verify-artifact for recorded staged
        // outputs; retargeting the policy surface stays with #528/#424. The
        // same fall-through covers every non-envelope operation.
        _ => Ok(None),
    }
}

async fn staging_destination(
    cp: &ControlPlane,
    storage_root_id: StorageRootId,
) -> Result<StorageRootId, VoomError> {
    destination_root(
        cp,
        media_dispatch::DestinationRole::Staging,
        storage_root_id,
    )
    .await
}

/// Audio synth/transcode/extract envelope from the planner audio block.
///
/// # Errors
/// When the audio planning block, snapshot, or staging destination does not
/// resolve — with the bundled adapters removed there is no fallback contract.
async fn audio_envelope(
    cp: &ControlPlane,
    branch_id: &str,
    source_ref: &media_dispatch::MediaDispatchSource,
    file_version_id: FileVersionId,
    operation_payload: &Value,
    facts: SourceFacts,
) -> Result<Option<Value>, VoomError> {
    let payload = voom_plan::planner::audio::AudioOperationPayload::try_from_execution_value(
        operation_payload,
    )
    .map_err(|error| {
        VoomError::Config(format!(
            "media dispatch envelope: audio planning block does not decode: {error}"
        ))
    })?;
    let snapshot = snapshot_for(cp, file_version_id, operation_payload).await?;
    let declared_root = match source_ref {
        media_dispatch::MediaDispatchSource::Location {
            storage_root_id, ..
        }
        | media_dispatch::MediaDispatchSource::RecordedStagedOutput {
            storage_root_id, ..
        } => *storage_root_id,
    };
    let destination = staging_destination(cp, declared_root).await?;
    match payload.operation_type {
        AudioOperationType::ExtractAudio => {
            let plan = crate::audio::selection::extract_selection_from_payload_and_snapshot(
                operation_payload,
                &snapshot,
            )
            .map_err(|error| {
                VoomError::Config(format!(
                    "media dispatch envelope: extract-audio selection does not derive: {error}"
                ))
            })?;
            let mut extractions = Vec::with_capacity(plan.outputs.len());
            for output in &plan.outputs {
                let output_id = output.output_id.clone().ok_or_else(|| {
                    VoomError::Config(
                        "media dispatch envelope: extract-audio output carries no stable \
                         output id"
                            .to_owned(),
                    )
                })?;
                extractions.push(media_dispatch::MediaExtractionRequest {
                    output_id,
                    selection: output.stream.clone(),
                    audio_codec: plan.target_codec.clone(),
                });
            }
            binding_result(media_dispatch::render_media_dispatch_extract_audio(
                branch_id,
                source_ref,
                facts.audio(),
                &extractions,
                destination,
            ))
        }
        AudioOperationType::TranscodeAudio | AudioOperationType::SynthesizeAudio => {
            let plan = crate::audio::selection::transcode_selection_from_payload_and_snapshot(
                operation_payload,
                &snapshot,
            )
            .map_err(|error| {
                VoomError::Config(format!(
                    "media dispatch envelope: transcode-audio selection does not derive: {error}"
                ))
            })?;
            let settings = voom_worker_protocol::TranscodeAudioSettings {
                target_codec: plan.target_codec.clone(),
                profile: "default".to_owned(),
                add_track: plan.add_track,
                target_channels: plan.target_channels,
            };
            binding_result(media_dispatch::render_media_dispatch_transcode_audio(
                branch_id,
                source_ref,
                facts.audio(),
                plan.selection.clone(),
                settings,
                destination,
            ))
        }
    }
}

/// The recorded staged-output locator a producing parent left for its
/// byte-touching child, read off the parent's durable payload.
///
/// Returns `(storage_root_id, locator)` for the producing operation's single
/// planned output; `None` when the parent has no envelope (its child then
/// stays on the legacy contract too).
pub(crate) fn parent_envelope_output(
    parent_rendered_payload: &Value,
) -> Option<(StorageRootId, voom_core::ProviderRelativeLocator)> {
    let dispatch = parent_rendered_payload.get("media_dispatch")?;
    let output = dispatch
        .get("output")
        .or_else(|| dispatch.get("destination"))?;
    let storage_root_id = output
        .get("storage_root_id")
        .and_then(Value::as_u64)
        .map(StorageRootId)?;
    let locator = output
        .get("provider_relative_locator")
        .and_then(Value::as_str)
        .and_then(|text| voom_core::ProviderRelativeLocator::new(text.to_owned()).ok())?;
    Some((storage_root_id, locator))
}

/// The observed facts a completed parent reported for its staged output,
/// read data-only off the released lease result (`agent_observed.outputs`).
pub(crate) fn observed_output_facts(ticket_result: &Value) -> Option<SourceFacts> {
    let outputs = ticket_result
        .get("agent_observed")?
        .get("outputs")?
        .as_array()?;
    let facts = outputs.first()?.get("facts")?;
    Some(SourceFacts {
        size_bytes: facts.get("size_bytes")?.as_u64()?,
        content_hash: facts.get("content_hash")?.as_str()?.to_owned(),
    })
}

/// Render the `media_dispatch` `BackUpFile` envelope for the backup child of a
/// producing operation (transform->backup): the parent's recorded staged
/// output is what gets copied, onto the backup root its staging root names.
///
/// # Errors
/// When the parent rendered no envelope or no backup root is configured — the
/// bundled backup path that used to absorb both cases is gone.
pub(crate) async fn backup_child_envelope(
    cp: &ControlPlane,
    branch_id: &str,
    parent_rendered_payload: &Value,
) -> Result<Option<Value>, VoomError> {
    let (root, locator) = parent_envelope_output(parent_rendered_payload).ok_or_else(|| {
        VoomError::Config(format!(
            "media dispatch envelope: parent of backup child {branch_id} recorded no staged \
             output"
        ))
    })?;
    let destination = destination_root(cp, media_dispatch::DestinationRole::Backup, root).await?;
    let source = media_dispatch::MediaDispatchSource::RecordedStagedOutput {
        storage_root_id: root,
        provider_relative_locator: locator,
    };
    binding_result(media_dispatch::render_media_dispatch_back_up_staged_output(
        branch_id,
        &source,
        destination,
    ))
}

/// Render the `media_dispatch` `VerifyArtifact` envelope for the verify child of
/// a completed backup ticket (backup->verify): the target is the backup's
/// recorded staged output and the expected facts are what the agent observed
/// writing there.
///
/// # Errors
/// When the parent released no result or no observed output facts: agent
/// completions always carry `agent_observed` evidence, so a missing block is a
/// render error, not a fallback to the removed bundled verify path.
pub(crate) fn verify_child_envelope(
    parent_rendered_payload: &Value,
    parent_ticket_result: Option<&Value>,
) -> Result<Option<Value>, VoomError> {
    let result = parent_ticket_result.ok_or_else(|| {
        VoomError::Config(
            "media dispatch envelope: parent of verify child released no result".to_owned(),
        )
    })?;
    let (root, locator) = parent_envelope_output(parent_rendered_payload).ok_or_else(|| {
        VoomError::Config(
            "media dispatch envelope: parent of verify child recorded no staged output".to_owned(),
        )
    })?;
    let facts = observed_output_facts(result).ok_or_else(|| {
        VoomError::Config(
            "media dispatch envelope: parent of verify child observed no output facts".to_owned(),
        )
    })?;
    let source = media_dispatch::MediaDispatchSource::RecordedStagedOutput {
        storage_root_id: root,
        provider_relative_locator: locator,
    };
    binding_result(media_dispatch::render_media_dispatch_verify_artifact(
        &source,
        verify_facts(facts),
    ))
}

/// Render the `media_dispatch` envelope for an **expansion child** ticket.
/// An unset planning input is a render error; with the bundled media adapters
/// removed there is no legacy contract for an expansion child to fall back to.
pub(crate) async fn expansion_envelope(
    cp: &ControlPlane,
    operation: OperationKind,
    branch: &crate::workflow::plan::binding::BranchContext,
    rendered_payload: &Value,
) -> Result<Option<Value>, VoomError> {
    use crate::workflow::plan::access_declaration::TicketStorageSource;

    let source_file = branch.source_file.as_ref().ok_or_else(|| {
        VoomError::Config(format!(
            "media dispatch envelope: expansion child {op} carries no scan-recorded facts",
            op = operation.as_str()
        ))
    })?;
    let Some(TicketStorageSource::Location {
        storage_root_id,
        file_location_id,
    }) = branch.storage_source
    else {
        // A whole-root declaration names no bytes; envelopes address one.
        return Err(VoomError::Config(format!(
            "media dispatch envelope: expansion child {op} declares no rooted location",
            op = operation.as_str()
        )));
    };
    let (source_ref, version_id) = location_source(cp, storage_root_id, file_location_id).await?;
    let facts = SourceFacts::from_source_file(source_file).map_err(|error| {
        VoomError::Config(format!(
            "media dispatch envelope: expansion child {op} facts do not decode: {error}",
            op = operation.as_str()
        ))
    })?;

    match operation {
        OperationKind::ProbeFile => binding_result(media_dispatch::render_media_dispatch_probe(
            &source_ref,
            facts.file(),
        )),
        OperationKind::TranscodeVideo => {
            let profile = serde_json::from_value::<TranscodeVideoProfile>(
                rendered_payload
                    .get("profile")
                    .cloned()
                    .unwrap_or(Value::Null),
            )
            .map_err(|error| {
                VoomError::Config(format!(
                    "media dispatch envelope: transcode-video profile does not resolve: {error}"
                ))
            })?;
            let destination = staging_destination(cp, storage_root_id).await?;
            binding_result(media_dispatch::render_media_dispatch_transcode_video(
                &branch.branch_id,
                &source_ref,
                facts.video(),
                destination,
                profile,
                None,
                false,
            ))
        }
        OperationKind::Remux => {
            let snapshot = cp
                .identity
                .list_media_snapshots_by_version(version_id)
                .await?
                .into_iter()
                .next_back()
                .ok_or_else(|| {
                    VoomError::Config(format!(
                        "media dispatch envelope: remux child of file_version {} has no \
                         recorded snapshot",
                        version_id.0
                    ))
                })?;
            let mut operation_payload = rendered_payload.clone();
            if let Some(object) = operation_payload.as_object_mut() {
                // The default child payload carries no planner tag and pins no
                // snapshot; both are required to derive a selection.
                object.insert("type".to_owned(), Value::from("remux"));
                object.insert(
                    "source_media_snapshot_id".to_owned(),
                    Value::from(snapshot.id.0),
                );
            }
            let selection = crate::remux::selection::selection_from_payload_and_snapshot(
                &operation_payload,
                &snapshot,
            )
            .map_err(|error| {
                VoomError::Config(format!(
                    "media dispatch envelope: remux selection does not derive: {error}"
                ))
            })?;
            let destination = staging_destination(cp, storage_root_id).await?;
            binding_result(media_dispatch::render_media_dispatch_remux(
                &branch.branch_id,
                &source_ref,
                facts.remux(),
                selection,
                destination,
            ))
        }
        // A policy verification targets a live library location, but the
        // envelope family reserves verify-artifact for recorded staged
        // outputs; retargeting the policy surface stays with #528/#424.
        OperationKind::VerifyArtifact => Ok(None),
        other => Err(VoomError::Config(format!(
            "media dispatch envelope: expansion child operation {} has no envelope arm",
            other.as_str()
        ))),
    }
}

#[cfg(test)]
#[path = "envelope_test.rs"]
mod tests;
