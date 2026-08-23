use serde_json::Map;
use serde_json::{Value, json};
use std::path::Path;
use voom_core::OperationKind;
use voom_core::{FileLocationId, FileVersionId, StorageRootId};
use voom_plan::planner::audio::{AudioOperationPayload, AudioOperationType};
use voom_plan::planner::remux::RemuxOperationPayload;
use voom_worker_protocol::{
    TranscodeVideoExpectedFacts, TranscodeVideoInput, TranscodeVideoOutput, TranscodeVideoProfile,
    TranscodeVideoRequest,
};

use crate::transcode::stage::{OutputName, output_file_name};
use crate::workflow::execution::timing::EffectiveTiming;
use crate::workflow::plan::access_declaration::TicketStorageSource;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchContext {
    pub branch_id: String,
    pub path: String,
    pub probe_codec: Option<String>,
    pub source_file: Option<Value>,
    /// The storage this branch's tickets are rendered against. This — not
    /// `node.policy_target()` — is how the source reaches this renderer, because
    /// the `ScanLibrary` arm never reads a policy target.
    pub storage_source: Option<TicketStorageSource>,
}

pub fn render_default_payload(
    operation: OperationKind,
    branch: &BranchContext,
    timing: EffectiveTiming,
) -> Result<Value, BindingError> {
    render_default_payload_with_fan_out(operation, branch, timing, 3)
}

pub fn render_default_payload_with_fan_out(
    operation: OperationKind,
    branch: &BranchContext,
    timing: EffectiveTiming,
    fan_out_count: usize,
) -> Result<Value, BindingError> {
    let mut payload = match operation {
        OperationKind::ScanLibrary => json!({
            "path": "/library",
            "fan_out_count": fan_out_count,
        }),
        OperationKind::ProbeFile => {
            let mut payload = json!({ "path": branch.path });
            if let Some(codec) = &branch.probe_codec {
                payload["codec"] = json!(codec);
            }
            payload
        }
        OperationKind::HashFile
        | OperationKind::IdentifyMedia
        | OperationKind::BackUpFile
        | OperationKind::VerifyArtifact
        | OperationKind::ExtractAudio
        | OperationKind::TranscodeAudio
        | OperationKind::DeleteArtifact => json!({ "path": branch.path }),
        OperationKind::ScoreQuality => {
            let codec = branch.probe_codec.as_ref().ok_or_else(|| {
                BindingError::new(format!(
                    "probe codec missing for branch `{}`",
                    branch.branch_id
                ))
            })?;
            json!({
                "path": branch.path,
                "profile": "default",
                "codec": codec,
            })
        }
        OperationKind::Remux => json!({
            "path": branch.path,
            "container": "mkv",
        }),
        OperationKind::TranscodeVideo => render_default_transcode_video_payload(branch)?,
        OperationKind::CommitArtifact => json!({
            "path": branch.path,
            "reason": "quality_regression",
        }),
        OperationKind::SyncExternalSystem => json!({
            "path": branch.path,
            "system": "plex",
            "action": "refresh",
        }),
        OperationKind::EditTracks => json!({
            "path": branch.path,
            "holder": "manual",
            "reason": "playback",
        }),
    };

    let Some(object) = payload.as_object_mut() else {
        return Err(BindingError::new("rendered payload must be a JSON object"));
    };
    object.insert("operation".to_owned(), json!(operation.as_str()));
    object.insert("branch_id".to_owned(), json!(branch.branch_id));
    object.insert("duration_ms".to_owned(), json!(timing.duration_ms));
    object.insert(
        "progress_interval_ms".to_owned(),
        json!(timing.progress_interval_ms),
    );
    match &branch.storage_source {
        Some(source) => insert_storage_source(object, source),
        // A byte-touching ticket without a source would be rejected at encode
        // anyway; failing here names the branch instead of the payload.
        None if operation.is_byte_touching() => {
            return Err(BindingError::new(format!(
                "byte-touching operation {} requires a storage source on branch `{}`",
                operation.as_str(),
                branch.branch_id
            )));
        }
        None => {}
    }
    Ok(payload)
}

fn render_default_transcode_video_payload(branch: &BranchContext) -> Result<Value, BindingError> {
    let profile = TranscodeVideoProfile::default_hevc();
    let staging_root = "/tmp/voom/default-workflow/transcode/staging";
    let output_name = OutputName {
        source_path: &branch.path,
        profile_id: &profile.name,
        codec: &profile.target_codec,
        container: "mkv",
    };
    let request = TranscodeVideoRequest {
        input: TranscodeVideoInput {
            path: source_file_string(branch, "path")?.to_owned(),
            expected: TranscodeVideoExpectedFacts {
                size_bytes: source_file_u64(branch, "size_bytes")?,
                content_hash: source_file_string(branch, "content_hash")?.to_owned(),
                modified_at: source_file_optional_string(branch, "modified_at")?,
                local_file_key: source_file_optional_string(branch, "local_file_key")?,
            },
            video_codec: branch.probe_codec.clone(),
            video_pixel_format: None,
        },
        output: TranscodeVideoOutput {
            staging_root: staging_root.to_owned(),
            path: format!(
                "{}/{}/{}",
                staging_root,
                branch.branch_id,
                output_file_name(&output_name)
            ),
            container: "mkv".to_owned(),
            video_codec: profile.target_codec.clone(),
            overwrite: true,
        },
        profile,
        hardware_assignment: None,
        copy_video: false,
    };
    serde_json::to_value(request)
        .map_err(|err| BindingError::new(format!("transcode_video payload encode: {err}")))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_field_names,
    reason = "every field is a typed id; a shorter name would lose which id it is"
)]
pub struct PolicyFileSource {
    pub file_version_id: FileVersionId,
    /// The root containing `location_id`. Both are required: a policy-rendered
    /// ticket is byte-touching, so its payload must carry the identity its
    /// declaration is checked against.
    pub storage_root_id: StorageRootId,
    pub location_id: FileLocationId,
}

pub fn render_policy_transcode_payload(
    source: PolicyFileSource,
    operation_payload: &Value,
    staging_root: &Path,
    target_dir: &Path,
    timing: EffectiveTiming,
) -> Result<Value, BindingError> {
    let target_codec = required_string(operation_payload, "target_codec")?;
    let container = required_string(operation_payload, "container")?;
    let profile = required_string(operation_payload, "profile")?;
    // The planner embeds the full typed profile as `resolved_profile` (pinned
    // Phase 5↔6 contract from Task 5.2). Thread it into the ticket payload so
    // the executor can build the TranscodeVideoRequest without re-running
    // resolution at dispatch time.
    let resolved_profile = operation_payload
        .get("resolved_profile")
        .cloned()
        .ok_or_else(|| {
            BindingError::new("transcode_video node payload missing `resolved_profile`")
        })?;
    if !resolved_profile.is_object() {
        return Err(BindingError::new(
            "transcode_video node payload `resolved_profile` must be an object",
        ));
    }
    let mut payload = json!({
        "operation": "transcode_video",
        "target_codec": target_codec,
        "container": container,
        "profile": profile,
        "resolved_profile": resolved_profile,
        "staging_root": staging_root,
        "target_dir": target_dir,
        "duration_ms": timing.duration_ms,
        "progress_interval_ms": timing.progress_interval_ms,
    });
    let Some(object) = payload.as_object_mut() else {
        return Err(BindingError::new("rendered payload must be a JSON object"));
    };
    if let Some(source_video_codec) = operation_payload
        .get("source_video_codec")
        .and_then(Value::as_str)
    {
        object.insert(
            "source_video_codec".to_owned(),
            Value::String(source_video_codec.to_owned()),
        );
    }
    if let Some(source_video_pixel_format) = operation_payload
        .get("source_video_pixel_format")
        .and_then(Value::as_str)
    {
        object.insert(
            "source_video_pixel_format".to_owned(),
            Value::String(source_video_pixel_format.to_owned()),
        );
    }
    insert_policy_file_source(object, source);
    Ok(payload)
}

pub fn render_policy_remux_payload(
    source: PolicyFileSource,
    operation_payload: &Value,
    staging_root: &Path,
    target_dir: &Path,
    timing: EffectiveTiming,
) -> Result<Value, BindingError> {
    let remux_payload = RemuxOperationPayload::try_from_execution_value(operation_payload)
        .map_err(|err| BindingError::new(err.to_string()))?
        .into_value();
    let mut payload = json!({
        "operation": "remux",
        "remux": remux_payload,
        "staging_root": staging_root,
        "target_dir": target_dir,
        "duration_ms": timing.duration_ms,
        "progress_interval_ms": timing.progress_interval_ms,
    });
    let Some(object) = payload.as_object_mut() else {
        return Err(BindingError::new("rendered payload must be a JSON object"));
    };
    insert_policy_file_source(object, source);
    Ok(payload)
}

pub fn render_policy_transcode_audio_payload(
    source: PolicyFileSource,
    operation_payload: &Value,
    staging_root: &Path,
    target_dir: &Path,
    timing: EffectiveTiming,
) -> Result<Value, BindingError> {
    render_policy_audio_payload(
        source,
        operation_payload,
        AudioOperationType::TranscodeAudio,
        "transcode_audio",
        staging_root,
        target_dir,
        timing,
    )
}

pub fn render_policy_extract_audio_payload(
    source: PolicyFileSource,
    operation_payload: &Value,
    staging_root: &Path,
    target_dir: &Path,
    timing: EffectiveTiming,
) -> Result<Value, BindingError> {
    render_policy_audio_payload(
        source,
        operation_payload,
        AudioOperationType::ExtractAudio,
        "extract_audio",
        staging_root,
        target_dir,
        timing,
    )
}

pub fn render_policy_verify_artifact_payload(
    source: PolicyFileSource,
    timing: EffectiveTiming,
) -> Result<Value, BindingError> {
    let mut payload = json!({
        "operation": "verify_artifact",
        "duration_ms": timing.duration_ms,
        "progress_interval_ms": timing.progress_interval_ms,
    });
    let Some(object) = payload.as_object_mut() else {
        return Err(BindingError::new("rendered payload must be a JSON object"));
    };
    insert_policy_file_source(object, source);
    Ok(payload)
}

fn render_policy_audio_payload(
    source: PolicyFileSource,
    operation_payload: &Value,
    expected_type: AudioOperationType,
    operation: &str,
    staging_root: &Path,
    target_dir: &Path,
    timing: EffectiveTiming,
) -> Result<Value, BindingError> {
    let audio_payload = AudioOperationPayload::try_from_execution_value(operation_payload)
        .map_err(|err| BindingError::new(err.to_string()))?;
    let type_matches = audio_payload.operation_type == expected_type
        || (expected_type == AudioOperationType::TranscodeAudio
            && audio_payload.operation_type == AudioOperationType::SynthesizeAudio);
    if !type_matches {
        return Err(BindingError::new(format!(
            "{operation} payload has mismatched type"
        )));
    }
    let mut payload = json!({
        "operation": operation,
        "audio": audio_payload.into_value(),
        "staging_root": staging_root,
        "target_dir": target_dir,
        "duration_ms": timing.duration_ms,
        "progress_interval_ms": timing.progress_interval_ms,
    });
    let Some(object) = payload.as_object_mut() else {
        return Err(BindingError::new("rendered payload must be a JSON object"));
    };
    insert_policy_file_source(object, source);
    Ok(payload)
}

fn insert_policy_file_source(object: &mut Map<String, Value>, source: PolicyFileSource) {
    object.insert(
        "source_file_version_id".to_owned(),
        json!(source.file_version_id),
    );
    insert_storage_source(
        object,
        &TicketStorageSource::Location {
            storage_root_id: source.storage_root_id,
            file_location_id: source.location_id,
        },
    );
}

/// Write the identity a declaration is validated against.
///
/// Every workflow ticket carries this, byte-touching or not: a non-byte-touching
/// ticket's payload is what its byte-touching children thread their own source
/// from, so omitting it there would leave them with nothing to build.
fn insert_storage_source(object: &mut Map<String, Value>, source: &TicketStorageSource) {
    match *source {
        TicketStorageSource::Root { storage_root_id } => {
            object.insert("source_storage_root_id".to_owned(), json!(storage_root_id));
            // A root-addressed render must not inherit a location key from whatever
            // built the base payload; leaving one would describe a narrower access
            // than the ticket actually has.
            object.remove("source_location_id");
        }
        TicketStorageSource::Location {
            storage_root_id,
            file_location_id,
        } => {
            object.insert("source_storage_root_id".to_owned(), json!(storage_root_id));
            object.insert("source_location_id".to_owned(), json!(file_location_id));
        }
    }
}

// --- Node-local media dispatch envelopes (ADR 0075 flip) ---
//
// These helpers render the nested `media_dispatch` object a ticket payload
// carries alongside its existing scalar source keys. Production call sites
// live in `plan::envelope` (ticket creation) and `binding_test.rs` pins the
// wire shapes.
pub(crate) mod media_dispatch {
    use serde_json::Value;
    use std::path::Path;
    use voom_core::{
        FileLocationId, FileVersionId, PROTOCOL_VERSION, ProviderRelativeLocator, StorageRootId,
    };
    use voom_worker_protocol::{
        AudioExpectedFacts, AudioStreamRef, EXTRACT_AUDIO_CONTAINER, ExpectedFileFacts,
        MediaBackUpFileDispatch, MediaDispatch, MediaExtractAudioDispatch, MediaExtractOutput,
        MediaPlannedOutput, MediaProbeDispatch, MediaRemuxDispatch, MediaSourceRef,
        MediaTranscodeAudioDispatch, MediaTranscodeVideoDispatch, MediaVerifyArtifactDispatch,
        REMUX_CONTAINER_MKV, RemuxExpectedFacts, RemuxSelection, TRANSCODE_AUDIO_CONTAINER,
        TRANSCODE_VIDEO_CONTAINER, TranscodeAudioSelection, TranscodeAudioSettings,
        TranscodeVideoExpectedFacts, TranscodeVideoProfile, VerifyArtifactExpectedFacts,
        VideoHardwareAssignment,
    };

    use super::BindingError;
    use crate::transcode::stage::{OutputName, output_file_name};
    use crate::workflow::plan::access_declaration::TicketStorageSource;

    // --- Node-local media dispatch envelopes (ADR 0075) ---
    //
    // The helpers below render the nested `media_dispatch` object a ticket
    // payload carries alongside its existing scalar source keys. They are
    // pure; `plan::envelope` supplies the durable inputs and the unit tests
    // in `binding_test.rs` pin the wire shapes.

    /// The handle-shaped byte source a media-dispatch envelope addresses.
    ///
    /// A location-sourced ticket renders from its live rooted address; a
    /// whole-root declaration ([`TicketStorageSource::Root`]) names no bytes, so
    /// the only envelope it can feed is verify-artifact's target — the producing
    /// operation's recorded staged-output address.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum MediaDispatchSource {
        /// A live rooted location: the declaration identity plus the rooted
        /// address recorded for `file_location_id`.
        Location {
            storage_root_id: StorageRootId,
            file_location_id: FileLocationId,
            provider_relative_locator: ProviderRelativeLocator,
        },
        /// A recorded staged-output address left by a producing operation.
        /// Constructed only by the backup->verify chain, which migrates with
        /// T8; until then only `binding_test.rs` pins its shapes.
        #[cfg_attr(not(test), expect(dead_code))]
        RecordedStagedOutput {
            storage_root_id: StorageRootId,
            provider_relative_locator: ProviderRelativeLocator,
        },
    }

    impl MediaDispatchSource {
        /// The scalar-key identity this source satisfies.
        #[must_use]
        #[cfg_attr(not(test), expect(dead_code))] // T8: backup->verify chain
        pub(crate) const fn ticket_storage_source(&self) -> TicketStorageSource {
            match self {
                Self::Location {
                    storage_root_id,
                    file_location_id,
                    ..
                } => TicketStorageSource::Location {
                    storage_root_id: *storage_root_id,
                    file_location_id: *file_location_id,
                },
                Self::RecordedStagedOutput {
                    storage_root_id, ..
                } => TicketStorageSource::Root {
                    storage_root_id: *storage_root_id,
                },
            }
        }

        /// The envelope reference for an operation reading a live location.
        ///
        /// # Errors
        ///
        /// Fails for [`MediaDispatchSource::RecordedStagedOutput`]: only artifact
        /// verification consumes a recorded staged-output address.
        pub(crate) fn location_ref(&self, operation: &str) -> Result<MediaSourceRef, BindingError> {
            match self {
                Self::Location {
                    storage_root_id,
                    provider_relative_locator,
                    ..
                } => Ok(MediaSourceRef {
                    storage_root_id: *storage_root_id,
                    provider_relative_locator: provider_relative_locator.clone(),
                }),
                Self::RecordedStagedOutput { .. } => Err(BindingError::new(format!(
                    "{operation} requires a live location source; a recorded \
                     staged-output address is reserved for artifact verification"
                ))),
            }
        }

        /// The staged artifact a verify-artifact envelope targets.
        ///
        /// # Errors
        ///
        /// Fails for [`MediaDispatchSource::Location`]: verification never reads a
        /// library location, only a recorded staged output.
        pub(crate) fn staged_target_ref(&self) -> Result<MediaSourceRef, BindingError> {
            match self {
                Self::RecordedStagedOutput {
                    storage_root_id,
                    provider_relative_locator,
                } => Ok(MediaSourceRef {
                    storage_root_id: *storage_root_id,
                    provider_relative_locator: provider_relative_locator.clone(),
                }),
                Self::Location { .. } => Err(BindingError::new(
                    "verify_artifact targets a recorded staged-output address, not a library location",
                )),
            }
        }
    }

    /// Which configured default root a planned destination resolves against.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum DestinationRole {
        /// `LibraryRoot.default_output_root_id`
        #[cfg_attr(not(test), expect(dead_code))] // T8: output-root destinations
        Output,
        /// `LibraryRoot.default_staging_root_id`
        Staging,
        /// `LibraryRoot.default_backup_root_id`
        Backup,
    }

    impl DestinationRole {
        #[must_use]
        pub(crate) const fn as_str(self) -> &'static str {
            match self {
                Self::Output => "output",
                Self::Staging => "staging",
                Self::Backup => "backup",
            }
        }
    }

    /// Resolve a planned destination to the library's configured default root for
    /// `role` (`LibraryRoot.default_output/staging/backup_root_id`).
    ///
    /// # Errors
    ///
    /// Fails descriptively when no default root is configured: a handle-shaped
    /// destination must name a concrete storage root.
    #[cfg_attr(not(test), expect(dead_code))] // T8: backup->verify chain
    pub(crate) fn resolve_destination_root(
        role: DestinationRole,
        default_root_id: Option<StorageRootId>,
    ) -> Result<StorageRootId, BindingError> {
        default_root_id.ok_or_else(|| {
            BindingError::new(format!(
                "library has no default {} root configured; cannot address a node-local {} destination",
                role.as_str(),
                role.as_str()
            ))
        })
    }

    /// Derive the deterministic provider-relative locator for one planned output.
    ///
    /// Format: `<branch-id>/<file-name>`. The branch-scoped first component
    /// mirrors today's path-based staging layout, which nests outputs under the
    /// branch id (`render_default_transcode_video_payload`); `<file-name>` comes
    /// from one of the `*_output_file_name` mirrors below so handle-shaped names
    /// stay comparable with the paths workers write today across the flip.
    ///
    /// # Errors
    ///
    /// Fails when the composed locator violates provider-relative-locator rules.
    pub(crate) fn planned_output_locator(
        branch_id: &str,
        file_name: &str,
    ) -> Result<ProviderRelativeLocator, BindingError> {
        ProviderRelativeLocator::new(format!("{branch_id}/{file_name}")).map_err(|error| {
            BindingError::new(format!(
                "planned output locator `{branch_id}/{file_name}` is not addressable: {error}"
            ))
        })
    }

    /// Mirror the bundled backup dispatcher's destination layout
    /// (`<backup root>/v<file-version-id>/<file-name>`) as a provider-relative
    /// locator under the resolved backup root.
    ///
    /// # Errors
    ///
    /// Fails when the composed locator violates provider-relative-locator rules.
    pub(crate) fn backup_destination_locator(
        file_version_id: FileVersionId,
        file_name: &str,
    ) -> Result<ProviderRelativeLocator, BindingError> {
        let composed = format!("v{}/{}", file_version_id.0, file_name);
        ProviderRelativeLocator::new(composed.clone()).map_err(|error| {
            BindingError::new(format!(
                "backup destination locator `{composed}` is not addressable: {error}"
            ))
        })
    }

    /// Build a planned output at `locator` on `storage_root_id`.
    ///
    /// `overwrite` is always `false`: real workers reject overwriting, and retry
    /// idempotence clears stale residue before dispatch instead (spec C3 step 4).
    #[must_use]
    pub(crate) fn planned_output(
        storage_root_id: StorageRootId,
        locator: ProviderRelativeLocator,
    ) -> MediaPlannedOutput {
        MediaPlannedOutput {
            storage_root_id,
            provider_relative_locator: locator,
            overwrite: false,
        }
    }

    /// The output-name stem the path-based workers use today — the source file's
    /// stem (`transcode::stage::output_file_name`) taken from the tail of the
    /// source's relative locator.
    ///
    /// # Errors
    ///
    /// Fails when the source locator has no usable final component.
    fn media_source_stem(source: &MediaDispatchSource) -> Result<String, BindingError> {
        let file_name = media_source_file_name(source)?;
        let stem = Path::new(&file_name)
            .file_stem()
            .and_then(std::ffi::OsStr::to_str)
            .filter(|stem| !stem.is_empty())
            .unwrap_or("output");
        Ok(stem.to_owned())
    }

    /// The source file's name as recorded in its relative locator (the bundled
    /// backup dispatcher preserves it verbatim under `v<file-version-id>/`).
    ///
    /// # Errors
    ///
    /// Fails when the source locator has no usable final component.
    fn media_source_file_name(source: &MediaDispatchSource) -> Result<String, BindingError> {
        let locator = match source {
            MediaDispatchSource::Location {
                provider_relative_locator,
                ..
            }
            | MediaDispatchSource::RecordedStagedOutput {
                provider_relative_locator,
                ..
            } => provider_relative_locator.as_str(),
        };
        locator
            .rsplit('/')
            .next()
            .filter(|tail| !tail.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                BindingError::new(format!(
                    "source locator `{locator}` has no file-name component"
                ))
            })
    }

    /// Mirrors `transcode::stage::{OutputName, output_file_name}`
    /// (`<stem>.<profile_id>.<codec>.<container>`); reuses that implementation so
    /// the two shapes cannot drift.
    #[must_use]
    pub(crate) fn transcode_video_output_file_name(
        source_stem: &str,
        profile_id: &str,
        codec: &str,
        container: &str,
    ) -> String {
        output_file_name(&OutputName {
            source_path: source_stem,
            profile_id,
            codec,
            container,
        })
    }

    /// Mirrors `remux::stage`'s naming: `<stem>.remux.<container>`.
    #[must_use]
    pub(crate) fn remux_output_file_name(source_stem: &str, container: &str) -> String {
        format!("{source_stem}.remux.{container}")
    }

    /// Mirrors `audio::stage::transcode_file_name`:
    /// `<stem>.audio-<codec>.<container>`.
    #[must_use]
    pub(crate) fn transcode_audio_output_file_name(
        source_stem: &str,
        codec: &str,
        container: &str,
    ) -> String {
        format!("{source_stem}.audio-{codec}.{container}")
    }

    /// Mirrors `audio::stage::extract_file_name`:
    /// `<stem>.<snapshot_stream_id>.<codec>.ogg`.
    #[must_use]
    pub(crate) fn extract_audio_output_file_name(
        source_stem: &str,
        snapshot_stream_id: &str,
        codec: &str,
    ) -> String {
        format!("{source_stem}.{snapshot_stream_id}.{codec}.{EXTRACT_AUDIO_CONTAINER}")
    }

    /// Render the nested `media_dispatch` object for a probe ticket.
    ///
    /// # Errors
    ///
    /// Fails when `source` cannot feed a probe (see
    /// [`MediaDispatchSource::location_ref`]) or the typed envelope fails to
    /// serialize.
    pub(crate) fn render_media_dispatch_probe(
        source: &MediaDispatchSource,
        expected: ExpectedFileFacts,
    ) -> Result<Value, BindingError> {
        encode_media_dispatch(MediaDispatch::Probe(MediaProbeDispatch {
            schema: PROTOCOL_VERSION,
            source: source.location_ref("probe")?,
            expected,
        }))
    }

    /// Render the nested `media_dispatch` object for a transcode-audio ticket.
    ///
    /// The output locator is derived deterministically from the branch id and the
    /// current audio-transcode naming; the destination root must already be
    /// resolved via [`resolve_destination_root`].
    ///
    /// # Errors
    ///
    /// Fails on an unusable source/destination pair or envelope serialization
    /// failure.
    pub(crate) fn render_media_dispatch_transcode_audio(
        branch_id: &str,
        source: &MediaDispatchSource,
        expected: AudioExpectedFacts,
        selection: TranscodeAudioSelection,
        settings: TranscodeAudioSettings,
        destination_root_id: StorageRootId,
    ) -> Result<Value, BindingError> {
        let stem = media_source_stem(source)?;
        let file_name = transcode_audio_output_file_name(
            &stem,
            &settings.target_codec,
            TRANSCODE_AUDIO_CONTAINER,
        );
        encode_media_dispatch(MediaDispatch::TranscodeAudio(MediaTranscodeAudioDispatch {
            schema: PROTOCOL_VERSION,
            source: source.location_ref("transcode_audio")?,
            expected,
            output_container: TRANSCODE_AUDIO_CONTAINER.to_owned(),
            output: planned_output(
                destination_root_id,
                planned_output_locator(branch_id, &file_name)?,
            ),
            selection,
            settings,
        }))
    }

    /// One requested extraction: which stream, encoded how, under which output id.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct MediaExtractionRequest {
        pub(crate) output_id: String,
        pub(crate) selection: AudioStreamRef,
        pub(crate) audio_codec: String,
    }

    /// Render the nested `media_dispatch` object for an extract-audio ticket,
    /// deriving one deterministic planned output per extraction in order.
    ///
    /// # Errors
    ///
    /// Fails on an unusable source or any unaddressable derived locator.
    pub(crate) fn render_media_dispatch_extract_audio(
        branch_id: &str,
        source: &MediaDispatchSource,
        expected: AudioExpectedFacts,
        extractions: &[MediaExtractionRequest],
        destination_root_id: StorageRootId,
    ) -> Result<Value, BindingError> {
        let stem = media_source_stem(source)?;
        let mut outputs = Vec::with_capacity(extractions.len());
        for extraction in extractions {
            let file_name = extract_audio_output_file_name(
                &stem,
                &extraction.selection.snapshot_stream_id,
                &extraction.audio_codec,
            );
            outputs.push(MediaExtractOutput {
                output_id: extraction.output_id.clone(),
                selection: extraction.selection.clone(),
                audio_codec: extraction.audio_codec.clone(),
                output: planned_output(
                    destination_root_id,
                    planned_output_locator(branch_id, &file_name)?,
                ),
            });
        }
        encode_media_dispatch(MediaDispatch::ExtractAudio(MediaExtractAudioDispatch {
            schema: PROTOCOL_VERSION,
            source: source.location_ref("extract_audio")?,
            expected,
            output_container: EXTRACT_AUDIO_CONTAINER.to_owned(),
            outputs,
        }))
    }

    /// Render the nested `media_dispatch` object for a transcode-video ticket.
    ///
    /// Mirrors `render_default_transcode_video_payload`: Matroska staging output
    /// named after the profile, target codec taken from the profile, no hardware
    /// assignment unless supplied.
    ///
    /// # Errors
    ///
    /// Fails on an unusable source or any unaddressable derived locator.
    pub(crate) fn render_media_dispatch_transcode_video(
        branch_id: &str,
        source: &MediaDispatchSource,
        expected: TranscodeVideoExpectedFacts,
        destination_root_id: StorageRootId,
        profile: TranscodeVideoProfile,
        hardware_assignment: Option<VideoHardwareAssignment>,
        copy_video: bool,
    ) -> Result<Value, BindingError> {
        let stem = media_source_stem(source)?;
        let file_name = transcode_video_output_file_name(
            &stem,
            &profile.name,
            &profile.target_codec,
            TRANSCODE_VIDEO_CONTAINER,
        );
        encode_media_dispatch(MediaDispatch::TranscodeVideo(MediaTranscodeVideoDispatch {
            schema: PROTOCOL_VERSION,
            source: source.location_ref("transcode_video")?,
            expected,
            output_container: TRANSCODE_VIDEO_CONTAINER.to_owned(),
            output_video_codec: profile.target_codec.clone(),
            output: planned_output(
                destination_root_id,
                planned_output_locator(branch_id, &file_name)?,
            ),
            profile,
            hardware_assignment,
            copy_video,
        }))
    }

    /// Render the nested `media_dispatch` object for a remux ticket.
    ///
    /// # Errors
    ///
    /// Fails on an unusable source or any unaddressable derived locator.
    pub(crate) fn render_media_dispatch_remux(
        branch_id: &str,
        source: &MediaDispatchSource,
        expected: RemuxExpectedFacts,
        selection: RemuxSelection,
        destination_root_id: StorageRootId,
    ) -> Result<Value, BindingError> {
        let stem = media_source_stem(source)?;
        let file_name = remux_output_file_name(&stem, REMUX_CONTAINER_MKV);
        encode_media_dispatch(MediaDispatch::Remux(MediaRemuxDispatch {
            schema: PROTOCOL_VERSION,
            source: source.location_ref("remux")?,
            expected,
            output_container: REMUX_CONTAINER_MKV.to_owned(),
            output: planned_output(
                destination_root_id,
                planned_output_locator(branch_id, &file_name)?,
            ),
            selection,
        }))
    }

    /// Render the nested `media_dispatch` object for a backup-file ticket. The
    /// destination preserves the source file's recorded name under
    /// `v<file-version-id>/`, mirroring today's bundled dispatcher layout.
    ///
    /// # Errors
    ///
    /// Fails on an unusable source or any unaddressable derived locator.
    pub(crate) fn render_media_dispatch_back_up_file(
        source: &MediaDispatchSource,
        file_version_id: FileVersionId,
        destination_root_id: StorageRootId,
    ) -> Result<Value, BindingError> {
        let file_name = media_source_file_name(source)?;
        encode_media_dispatch(MediaDispatch::BackUpFile(MediaBackUpFileDispatch {
            schema: PROTOCOL_VERSION,
            source: source.location_ref("back_up_file")?,
            destination: planned_output(
                destination_root_id,
                backup_destination_locator(file_version_id, &file_name)?,
            ),
        }))
    }

    /// Render the nested `media_dispatch` object for a verify-artifact ticket
    /// targeting a producing operation's recorded staged-output address.
    ///
    /// # Errors
    ///
    /// Fails when `source` is not a recorded staged-output address.
    #[cfg_attr(not(test), expect(dead_code))] // T8: backup->verify chain
    pub(crate) fn render_media_dispatch_verify_artifact(
        source: &MediaDispatchSource,
        expected: VerifyArtifactExpectedFacts,
    ) -> Result<Value, BindingError> {
        encode_media_dispatch(MediaDispatch::VerifyArtifact(MediaVerifyArtifactDispatch {
            schema: PROTOCOL_VERSION,
            target: source.staged_target_ref()?,
            expected,
        }))
    }

    fn encode_media_dispatch(dispatch: MediaDispatch) -> Result<Value, BindingError> {
        serde_json::to_value(dispatch)
            .map_err(|error| BindingError::new(format!("media dispatch encode: {error}")))
    }
}

#[must_use]
#[cfg(test)]
pub fn branch_context_with_probe_codec(branch_id: &str, codec: &str) -> BranchContext {
    BranchContext {
        branch_id: branch_id.to_owned(),
        path: format!("/library/{branch_id}.mkv"),
        probe_codec: Some(codec.to_owned()),
        source_file: Some(test_source_file(branch_id)),
        storage_source: Some(TicketStorageSource::Location {
            storage_root_id: StorageRootId(3),
            file_location_id: FileLocationId(7),
        }),
    }
}

fn required_string<'a>(payload: &'a Value, field: &str) -> Result<&'a str, BindingError> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| BindingError::new(format!("transcode_video payload missing `{field}`")))
}

fn source_file(branch: &BranchContext) -> Result<&Value, BindingError> {
    branch.source_file.as_ref().ok_or_else(|| {
        BindingError::new(format!(
            "transcode_video branch `{}` missing source_file facts",
            branch.branch_id
        ))
    })
}

fn source_file_string<'a>(
    branch: &'a BranchContext,
    field: &'static str,
) -> Result<&'a str, BindingError> {
    source_file(branch)?
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| {
            BindingError::new(format!(
                "transcode_video source_file for branch `{}` missing string `{field}`",
                branch.branch_id
            ))
        })
}

fn source_file_optional_string(
    branch: &BranchContext,
    field: &'static str,
) -> Result<Option<String>, BindingError> {
    match source_file(branch)?.get(field) {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(BindingError::new(format!(
            "transcode_video source_file for branch `{}` field `{field}` must be a string",
            branch.branch_id
        ))),
    }
}

fn source_file_u64(branch: &BranchContext, field: &'static str) -> Result<u64, BindingError> {
    source_file(branch)?
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            BindingError::new(format!(
                "transcode_video source_file for branch `{}` missing unsigned `{field}`",
                branch.branch_id
            ))
        })
}

#[cfg(test)]
fn test_source_file(branch_id: &str) -> Value {
    json!({
        "path": format!("/library/{branch_id}.mkv"),
        "size_bytes": 4_200_000_000_u64,
        "content_hash": format!("blake3:{branch_id}"),
        "local_file_key": format!("/library/{branch_id}.mkv")
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingError {
    detail: String,
}

impl BindingError {
    pub(crate) fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for BindingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for BindingError {}

#[cfg(test)]
#[path = "binding_test.rs"]
mod tests;
