//! Byte-free `media_dispatch` envelope assembly at ticket-creation time
//! (ADR 0075 flip).
//!
//! Every helper here reads durable rows (identity, library roots, media
//! snapshots) or ticket-borne JSON; none of them stat, canonicalize, hash,
//! or open a byte. An envelope renders only when all of its inputs are
//! derivable: a live rooted source handle, expected facts, and — for
//! operations with planned outputs — the library's configured destination
//! root. When an input is missing the caller keeps the pre-envelope payload
//! shape, which migration 0042 already anticipates for in-flight tickets;
//! the routing gate stays dormant for exactly those tickets until their
//! flow migrates (T8).

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

#[cfg_attr(not(test), expect(dead_code))] // T8: backup->verify chain
fn verify_facts(facts: SourceFacts) -> VerifyArtifactExpectedFacts {
    VerifyArtifactExpectedFacts {
        size_bytes: facts.size_bytes,
        content_hash: facts.content_hash,
        modified_at: None,
        local_file_key: None,
    }
}

/// The live rooted handle behind one recorded source identity.
///
/// `Ok(None)` means the location row vanished or lost its rooted address
/// between declaration and render; the ticket then stays on the legacy
/// contract rather than rendering an unresolvable envelope.
pub(crate) async fn location_source(
    cp: &ControlPlane,
    storage_root_id: StorageRootId,
    file_location_id: FileLocationId,
) -> Result<Option<media_dispatch::MediaDispatchSource>, VoomError> {
    let Some(location) = cp.identity.get_file_location(file_location_id).await? else {
        return Ok(None);
    };
    if crate::operation_source::require_live_rooted(&location).is_err() {
        return Ok(None);
    }
    let Ok((root, locator)) = location.rooted_address() else {
        return Ok(None);
    };
    if root != storage_root_id {
        return Err(VoomError::Config(format!(
            "file_location {file_location_id} moved to storage root {root}, \
             but the ticket declares root {storage_root_id}"
        )));
    }
    Ok(Some(media_dispatch::MediaDispatchSource::Location {
        storage_root_id: root,
        file_location_id,
        provider_relative_locator: locator.clone(),
    }))
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
/// `configured_root_id`, or `None` when nothing is configured.
///
/// A library root resolves its own row first; a staging/output leaf resolves
/// through whichever library row assigns it as a default, because that owner
/// holds the sibling defaults.
pub(crate) async fn destination_root(
    cp: &ControlPlane,
    role: media_dispatch::DestinationRole,
    configured_root_id: StorageRootId,
) -> Result<Option<StorageRootId>, VoomError> {
    if let Some(root) = cp.libraries.get_library_root(configured_root_id).await?
        && let Some(direct) = root_default(&root, role)
    {
        return Ok(Some(direct));
    }
    let roots = cp.libraries.list_library_roots(None).await?;
    Ok(roots
        .iter()
        .filter(|root| names_default(root, configured_root_id, role))
        .find_map(|owner| root_default(owner, role)))
}

async fn snapshot_for(
    cp: &ControlPlane,
    file_version_id: FileVersionId,
    operation_payload: &Value,
) -> Result<Option<MediaSnapshot>, VoomError> {
    let Some(id) = operation_payload
        .get("source_media_snapshot_id")
        .and_then(Value::as_u64)
        .filter(|id| *id > 0)
    else {
        return Ok(None);
    };
    let Some(snapshot) = cp
        .identity
        .get_media_snapshot(voom_core::ids::MediaSnapshotId(id))
        .await?
    else {
        // A payload pinning an unresolvable snapshot keeps the pre-envelope
        // contract here; the execution-time snapshot reader still rejects it,
        // so nothing silent survives to a lease.
        return Ok(None);
    };
    if snapshot.file_version_id != file_version_id {
        return Err(VoomError::Config(format!(
            "media_snapshot {id} does not belong to file_version {file_version_id}"
        )));
    }
    Ok(Some(snapshot))
}

fn binding_result(result: Result<Value, BindingError>) -> Result<Option<Value>, VoomError> {
    result
        .map(Some)
        .map_err(|error| VoomError::Config(format!("media dispatch envelope binding: {error}")))
}

/// Render the `media_dispatch` envelope for a **policy-root** media ticket.
///
/// `operation_payload` is the planner node's raw payload (the `type:`-tagged
/// block). Every arm fails closed into `Ok(None)` when a planning input is
/// absent — those tickets keep the pre-envelope contract instead of
/// rendering an envelope the agent could not execute.
pub(crate) async fn policy_envelope(
    cp: &ControlPlane,
    branch_id: &str,
    operation: OperationKind,
    source: &PolicyFileSource,
    operation_payload: &Value,
) -> Result<Option<Value>, VoomError> {
    let Some(source_ref) = location_source(cp, source.storage_root_id, source.location_id).await?
    else {
        return Ok(None);
    };
    let Some(version) = cp.identity.get_file_version(source.file_version_id).await? else {
        return Ok(None);
    };
    let facts = SourceFacts::from_version(&version);

    match operation {
        OperationKind::ProbeFile => binding_result(media_dispatch::render_media_dispatch_probe(
            &source_ref,
            facts.file(),
        )),
        OperationKind::BackUpFile => {
            let Some(destination) = destination_root(
                cp,
                media_dispatch::DestinationRole::Backup,
                source.storage_root_id,
            )
            .await?
            else {
                return Ok(None);
            };
            binding_result(media_dispatch::render_media_dispatch_back_up_file(
                &source_ref,
                source.file_version_id,
                destination,
            ))
        }
        OperationKind::TranscodeVideo => {
            let Ok(profile) = serde_json::from_value::<TranscodeVideoProfile>(
                operation_payload
                    .get("resolved_profile")
                    .cloned()
                    .unwrap_or(Value::Null),
            ) else {
                return Ok(None);
            };
            let Some(destination) = staging_destination(cp, source.storage_root_id).await? else {
                return Ok(None);
            };
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
            let Some(snapshot) =
                snapshot_for(cp, source.file_version_id, operation_payload).await?
            else {
                return Ok(None);
            };
            let Ok(selection) = crate::remux::selection::selection_from_payload_and_snapshot(
                operation_payload,
                &snapshot,
            ) else {
                return Ok(None);
            };
            let Some(destination) = staging_destination(cp, source.storage_root_id).await? else {
                return Ok(None);
            };
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
) -> Result<Option<StorageRootId>, VoomError> {
    destination_root(
        cp,
        media_dispatch::DestinationRole::Staging,
        storage_root_id,
    )
    .await
}

/// Audio synth/transcode/extract envelope from the planner audio block.
async fn audio_envelope(
    cp: &ControlPlane,
    branch_id: &str,
    source_ref: &media_dispatch::MediaDispatchSource,
    file_version_id: FileVersionId,
    operation_payload: &Value,
    facts: SourceFacts,
) -> Result<Option<Value>, VoomError> {
    let Ok(payload) = voom_plan::planner::audio::AudioOperationPayload::try_from_execution_value(
        operation_payload,
    ) else {
        return Ok(None);
    };
    let Some(snapshot) = snapshot_for(cp, file_version_id, operation_payload).await? else {
        return Ok(None);
    };
    let declared_root = match source_ref {
        media_dispatch::MediaDispatchSource::Location {
            storage_root_id, ..
        }
        | media_dispatch::MediaDispatchSource::RecordedStagedOutput {
            storage_root_id, ..
        } => *storage_root_id,
    };
    let Some(destination) = staging_destination(cp, declared_root).await? else {
        return Ok(None);
    };
    match payload.operation_type {
        AudioOperationType::ExtractAudio => {
            let Ok(plan) = crate::audio::selection::extract_selection_from_payload_and_snapshot(
                operation_payload,
                &snapshot,
            ) else {
                return Ok(None);
            };
            let mut extractions = Vec::with_capacity(plan.outputs.len());
            for output in &plan.outputs {
                let Some(output_id) = output.output_id.clone() else {
                    // Legacy single-output extractions carry no stable output
                    // id; they stay on the pre-envelope contract.
                    return Ok(None);
                };
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
            let Ok(plan) = crate::audio::selection::transcode_selection_from_payload_and_snapshot(
                operation_payload,
                &snapshot,
            ) else {
                return Ok(None);
            };
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
#[cfg_attr(not(test), expect(dead_code))] // T8: backup->verify chain
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
#[cfg_attr(not(test), expect(dead_code))] // T8: backup->verify chain
pub(crate) fn observed_output_facts(ticket_result: &Value) -> Option<SourceFacts> {
    let outputs = ticket_result
        .get("agent_observed")?
        .get("outputs")?
        .as_array()?;
    let first = outputs.first()?;
    Some(SourceFacts {
        size_bytes: first.get("size_bytes")?.as_u64()?,
        content_hash: first.get("content_hash")?.as_str()?.to_owned(),
    })
}

/// Render the `media_dispatch` envelope for an **expansion child** ticket.
///
/// `branch` carries the child's declared source and scan-recorded facts;
/// `rendered_payload` is the default payload already rendered for the node.
/// Only children whose inputs are fully derivable flip: probe children read
/// their facts off the scan result, transcode-video children reuse the
/// default profile the payload already pins. Everything else keeps the
/// pre-envelope contract until its flow migrates (T8).
pub(crate) async fn expansion_envelope(
    cp: &ControlPlane,
    operation: OperationKind,
    branch: &crate::workflow::plan::binding::BranchContext,
    rendered_payload: &Value,
) -> Result<Option<Value>, VoomError> {
    use crate::workflow::plan::access_declaration::TicketStorageSource;

    let Some(source_file) = branch.source_file.as_ref() else {
        return Ok(None);
    };
    let Some(TicketStorageSource::Location {
        storage_root_id,
        file_location_id,
    }) = branch.storage_source
    else {
        // A whole-root declaration names no bytes; envelopes address one.
        return Ok(None);
    };
    let Some(source_ref) = location_source(cp, storage_root_id, file_location_id).await? else {
        return Ok(None);
    };
    let Ok(facts) = SourceFacts::from_source_file(source_file) else {
        return Ok(None);
    };

    match operation {
        OperationKind::ProbeFile => binding_result(media_dispatch::render_media_dispatch_probe(
            &source_ref,
            facts.file(),
        )),
        OperationKind::TranscodeVideo => {
            let Ok(profile) = serde_json::from_value::<TranscodeVideoProfile>(
                rendered_payload
                    .get("profile")
                    .cloned()
                    .unwrap_or(Value::Null),
            ) else {
                return Ok(None);
            };
            let Some(destination) = staging_destination(cp, storage_root_id).await? else {
                return Ok(None);
            };
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
        _ => Ok(None),
    }
}

#[cfg(test)]
#[path = "envelope_test.rs"]
mod tests;
