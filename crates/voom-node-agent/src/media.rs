//! Node-local media executor (ADR 0075 design C3).
//!
//! Every byte-touching media lease (`probe_file`, `transcode_audio`,
//! `extract_audio`, `transcode_video`, `remux`, `back_up_file`,
//! `verify_artifact`) runs through here instead of the plain dispatch
//! fall-through. The ticket payload carries location handles
//! ([`voom_worker_protocol::MediaDispatch`]), never absolute paths: this
//! executor strict-decodes the envelope before touching a child, resolves
//! every handle against the node's own configured storage roots, gates the
//! dispatch on a fresh observation of the source bytes, clears stale residue
//! at planned-output paths, and after the child finishes it independently
//! re-observes what the worker reported. The completion result carries a
//! typed `agent_observed` evidence block so the control plane can settle the
//! lease without opening any bytes itself.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value as JsonValue, json};

use crate::client::{CommitObservedFacts, LeaseDispatch};
use crate::commit::{canonical_root, remove_file_if_exists, rooted_path};
use crate::runtime::{
    ChildEndpoint, ChildEndpointRegistry, CoordinatorContext, LeaseOutcome,
    consume_progress_stream, duration_millis_u32, open_worker_stream,
};
use voom_core::{FailureClass, OperationKind, VoomError};
use voom_worker_protocol::{
    BackUpFileRequest, BackUpFileResult, ClientHandle, ExpectedFileFacts, ExtractAudioInput,
    ExtractAudioOutput, ExtractAudioOutputDescriptor, ExtractAudioRequest, ExtractAudioResult,
    MediaDispatch, MediaExtractAudioDispatch, MediaPlannedOutput, MediaSourceRef,
    MediaTranscodeAudioDispatch, MediaTranscodeVideoDispatch, NdjsonOutcome, ObservedFileFacts,
    OperationRequest, ProbeFileRequest, ProbeFileResult, ProgressFrame, RemuxInput, RemuxOutput,
    RemuxRequest, RemuxResult, TranscodeAudioInput, TranscodeAudioOutput, TranscodeAudioRequest,
    TranscodeAudioResult, TranscodeVideoInput, TranscodeVideoOutput, TranscodeVideoRequest,
    TranscodeVideoResult, VerifyArtifactRequest, VerifyArtifactResult, WorkerCredentials,
    decode_media_dispatch,
};

/// How long the post-dispatch staged-output probe waits for a live ffprobe
/// child. Mirrors the scan pump's endpoint budget; probe evidence is
/// optional, so expiry skips the attachment rather than failing the lease.
const PROBE_ENDPOINT_WAIT: Duration = Duration::from_secs(2);

/// Whether a leased operation routes through the media executor rather than
/// the plain single-child dispatch. Exactly the [`MediaDispatch`] envelope
/// operations; `scan_library` keeps its durable-pump priority upstream.
#[must_use]
pub(crate) fn is_media_dispatch_operation(operation: &str) -> bool {
    matches!(
        OperationKind::from_wire(operation),
        Some(
            OperationKind::BackUpFile
                | OperationKind::ExtractAudio
                | OperationKind::ProbeFile
                | OperationKind::Remux
                | OperationKind::TranscodeAudio
                | OperationKind::TranscodeVideo
                | OperationKind::VerifyArtifact
        )
    )
}

/// One planned output whose absolute path was resolved on this node.
#[derive(Debug)]
struct PlannedOutput {
    provider_relative_locator: String,
    path: PathBuf,
}

/// Every location handle of one decoded envelope, resolved to node-local
/// paths. Built before any child is touched so binding misses fail the lease
/// without letting a worker see another node's root.
#[derive(Debug)]
struct ResolvedPlan {
    /// The file the child reads (source, or the staged target for
    /// `verify_artifact`).
    source: PathBuf,
    /// Planned outputs in envelope order, residue already cleared.
    outputs: Vec<PlannedOutput>,
    /// Canonical containment root for `verify_artifact` requests; the worker
    /// rejects any path whose parent escapes it.
    verify_staging_root: Option<String>,
}

/// Typed agent-side evidence serialized into the completion result under the
/// top-level `agent_observed` key. Facts come from this agent's own
/// observations, never from the worker's report.
#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentObservedEvidence {
    pub(crate) input_pre: ObservedFileFacts,
    pub(crate) input_post: ObservedFileFacts,
    pub(crate) outputs: Vec<AgentObservedOutput>,
}

/// One independently observed output file, optionally carrying an ffprobe
/// snapshot gathered through the shared child-endpoint registry.
#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentObservedOutput {
    pub(crate) provider_relative_locator: String,
    pub(crate) facts: ObservedFileFacts,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) snapshot: Option<JsonValue>,
}

/// Worker-reported output facts the agent verifies byte-for-byte.
#[derive(Debug)]
struct ReportedOutput {
    size_bytes: u64,
    content_hash: String,
}

/// What the strictly parsed worker result asks of post-dispatch validation.
enum ParsedResult {
    /// A mutating or verifying op: these are the outputs to re-observe.
    Outputs(Vec<ReportedOutput>),
    /// `probe_file`: the worker's own post-probe facts must agree with ours.
    Probe(Box<ProbeFileResult>),
}

/// Execute one byte-touching media lease end to end (design C3).
///
/// Steps: strict-decode the envelope before any augmentation could pollute
/// it, resolve every handle through the shared rooted-path resolver, clear
/// stale planned-output residue, gate on a fresh source observation, dispatch
/// the path-based child request over the lease's own stream, then verify the
/// typed result against this agent's own observations and attach the
/// `agent_observed` evidence block.
pub(crate) async fn media_outcome(
    dispatch: &LeaseDispatch,
    child: Arc<dyn ClientHandle>,
    credentials: &WorkerCredentials,
    context: &CoordinatorContext,
    storage_roots: &HashMap<u64, PathBuf>,
    registry: &ChildEndpointRegistry,
) -> LeaseOutcome {
    // Step 1: strict decode BEFORE augmentation would inject sibling keys.
    // Wrong schema or unknown shape fails the lease without touching a child.
    // The control plane renders the envelope under a dedicated object so the
    // declaration-derivation scalar keys survive beside it (spec C2); a
    // payload without it is not a renderable media dispatch.
    let Some(envelope_payload) = dispatch.dispatch_payload.get("media_dispatch") else {
        return malformed(
            "media dispatch payload carries no `media_dispatch` envelope object".to_owned(),
            json!({}),
        );
    };
    let envelope = match decode_media_dispatch(envelope_payload) {
        Ok(envelope) => envelope,
        Err(reason) => {
            return malformed(format!("media dispatch decode failed: {reason}"), json!({}));
        }
    };

    // Step 2 + 4: resolve handles and clear stale residue. A binding miss or
    // locator escape fails here so a worker can never reach another root.
    let plan = match resolve_plan(storage_roots, &envelope).await {
        Ok(plan) => plan,
        Err(error) => {
            return malformed(
                format!("media dispatch resolution failed: {error}"),
                json!({}),
            );
        }
    };

    // Step 3: pre-dispatch source observation against the pinned facts.
    let input_pre = match observe(&plan.source).await {
        Ok(facts) => facts,
        Err(outcome) => return outcome,
    };
    if let Some((size_bytes, content_hash)) = expected_source_facts(&envelope)
        && (input_pre.size_bytes != size_bytes || input_pre.content_hash != content_hash)
    {
        let evidence = AgentObservedEvidence {
            input_pre: wire_facts(input_pre.clone()),
            input_post: wire_facts(input_pre),
            outputs: Vec::new(),
        };
        return mismatch_failure(
            "source",
            &format!(
                "expected {size_bytes} bytes hashed {content_hash}, observed {} bytes hashed {}",
                evidence.input_pre.size_bytes, evidence.input_pre.content_hash
            ),
            &evidence,
        );
    }

    // Step 5: dispatch the path-based child request over the lease's stream.
    let (operation, payload) = match child_request(&envelope, &plan) {
        Ok(request) => request,
        Err(error) => {
            return malformed(
                format!("media dispatch request build failed: {error}"),
                json!({}),
            );
        }
    };
    let request = OperationRequest {
        operation,
        lease_id: dispatch.lease_id,
        payload,
        heartbeat_deadline_ms: duration_millis_u32(context.lease_ttl),
        progress_idle_deadline_ms: duration_millis_u32(context.progress_timeout),
    };
    let stream =
        match open_worker_stream(dispatch, child.as_ref(), credentials, context, request).await {
            Ok(stream) => stream,
            Err(outcome) => return outcome,
        };
    let result_payload = match consume_progress_stream(stream, context.progress_timeout).await {
        LeaseOutcome::Complete(payload) => payload,
        other => return other,
    };

    // Step 6: parse, independently observe, verify, and attach evidence.
    settle_media_result(
        &envelope,
        &plan,
        result_payload,
        input_pre,
        dispatch,
        context,
        registry,
    )
    .await
}

/// Resolve every handle of `envelope` against the node's bindings, clearing
/// stale residue at each planned-output path (a crashed prior attempt may
/// have left bytes there; retry stays idempotent because planned outputs hold
/// no durable identity until completion evidence lands).
async fn resolve_plan(
    storage_roots: &HashMap<u64, PathBuf>,
    envelope: &MediaDispatch,
) -> Result<ResolvedPlan, VoomError> {
    let plan = match envelope {
        MediaDispatch::Probe(dispatch) => ResolvedPlan {
            source: resolve_source(storage_roots, &dispatch.source).await?,
            outputs: Vec::new(),
            verify_staging_root: None,
        },
        MediaDispatch::VerifyArtifact(dispatch) => ResolvedPlan {
            source: resolve_source(storage_roots, &dispatch.target).await?,
            outputs: Vec::new(),
            verify_staging_root: Some(
                canonical_root(storage_roots, dispatch.target.storage_root_id)
                    .await?
                    .to_string_lossy()
                    .into_owned(),
            ),
        },
        MediaDispatch::TranscodeAudio(dispatch) => ResolvedPlan {
            source: resolve_source(storage_roots, &dispatch.source).await?,
            outputs: planned_outputs(storage_roots, [&dispatch.output]).await?,
            verify_staging_root: None,
        },
        MediaDispatch::ExtractAudio(dispatch) => {
            let locators: Vec<&MediaPlannedOutput> = dispatch
                .outputs
                .iter()
                .map(|output| &output.output)
                .collect();
            ResolvedPlan {
                source: resolve_source(storage_roots, &dispatch.source).await?,
                outputs: planned_outputs(storage_roots, locators).await?,
                verify_staging_root: None,
            }
        }
        MediaDispatch::TranscodeVideo(dispatch) => ResolvedPlan {
            source: resolve_source(storage_roots, &dispatch.source).await?,
            outputs: planned_outputs(storage_roots, [&dispatch.output]).await?,
            verify_staging_root: None,
        },
        MediaDispatch::Remux(dispatch) => ResolvedPlan {
            source: resolve_source(storage_roots, &dispatch.source).await?,
            outputs: planned_outputs(storage_roots, [&dispatch.output]).await?,
            verify_staging_root: None,
        },
        MediaDispatch::BackUpFile(dispatch) => ResolvedPlan {
            source: resolve_source(storage_roots, &dispatch.source).await?,
            outputs: planned_outputs(storage_roots, [&dispatch.destination]).await?,
            verify_staging_root: None,
        },
    };
    for output in &plan.outputs {
        remove_file_if_exists(&output.path).await?;
    }
    Ok(plan)
}

/// Resolve one source-style handle (an existing, live rooted location).
async fn resolve_source(
    storage_roots: &HashMap<u64, PathBuf>,
    source: &MediaSourceRef,
) -> Result<PathBuf, VoomError> {
    rooted_path(
        storage_roots,
        source.storage_root_id,
        source.provider_relative_locator.as_str(),
    )
    .await
}

/// Resolve planned outputs in order; callers clear their residue right after.
async fn planned_outputs(
    storage_roots: &HashMap<u64, PathBuf>,
    planned: impl IntoIterator<Item = &MediaPlannedOutput>,
) -> Result<Vec<PlannedOutput>, VoomError> {
    let mut outputs = Vec::new();
    for output in planned {
        outputs.push(PlannedOutput {
            provider_relative_locator: output.provider_relative_locator.as_str().to_owned(),
            path: rooted_path(
                storage_roots,
                output.storage_root_id,
                output.provider_relative_locator.as_str(),
            )
            .await?,
        });
    }
    Ok(outputs)
}

/// The `(size, hash)` the envelope pins the source to, when the operation
/// declares expectations (`back_up_file` copies whatever is there).
fn expected_source_facts(envelope: &MediaDispatch) -> Option<(u64, &str)> {
    match envelope {
        MediaDispatch::Probe(dispatch) => Some((
            dispatch.expected.size_bytes,
            dispatch.expected.content_hash.as_str(),
        )),
        MediaDispatch::TranscodeAudio(dispatch) => Some((
            dispatch.expected.size_bytes,
            dispatch.expected.content_hash.as_str(),
        )),
        MediaDispatch::ExtractAudio(dispatch) => Some((
            dispatch.expected.size_bytes,
            dispatch.expected.content_hash.as_str(),
        )),
        MediaDispatch::TranscodeVideo(dispatch) => Some((
            dispatch.expected.size_bytes,
            dispatch.expected.content_hash.as_str(),
        )),
        MediaDispatch::Remux(dispatch) => Some((
            dispatch.expected.size_bytes,
            dispatch.expected.content_hash.as_str(),
        )),
        MediaDispatch::VerifyArtifact(dispatch) => Some((
            dispatch.expected.size_bytes,
            dispatch.expected.content_hash.as_str(),
        )),
        MediaDispatch::BackUpFile(_) => None,
    }
}

/// Build the existing path-based child request for the operation. Output
/// paths stay inside the resolved plan; `overwrite` is always `false` because
/// residue was cleared above and real workers reject replacement writes.
fn child_request(
    envelope: &MediaDispatch,
    plan: &ResolvedPlan,
) -> Result<(OperationKind, JsonValue), VoomError> {
    let source = text(&plan.source);
    Ok(match envelope {
        MediaDispatch::Probe(dispatch) => (
            OperationKind::ProbeFile,
            json!(ProbeFileRequest {
                path: source,
                expected: dispatch.expected.clone(),
            }),
        ),
        MediaDispatch::TranscodeAudio(dispatch) => (
            OperationKind::TranscodeAudio,
            transcode_audio_payload(dispatch, plan, source)?,
        ),
        MediaDispatch::ExtractAudio(dispatch) => (
            OperationKind::ExtractAudio,
            extract_audio_payload(dispatch, plan, source)?,
        ),
        MediaDispatch::TranscodeVideo(dispatch) => (
            OperationKind::TranscodeVideo,
            transcode_video_payload(dispatch, plan, source)?,
        ),
        MediaDispatch::Remux(dispatch) => {
            let output = sole_planned(plan)?;
            (
                OperationKind::Remux,
                json!(RemuxRequest {
                    input: RemuxInput {
                        path: source,
                        expected: dispatch.expected.clone(),
                    },
                    output: RemuxOutput {
                        staging_root: parent_text(&output.path),
                        path: text(&output.path),
                        container: dispatch.output_container.clone(),
                        overwrite: false,
                    },
                    selection: dispatch.selection.clone(),
                }),
            )
        }
        MediaDispatch::BackUpFile(_) => {
            let destination = sole_planned(plan)?;
            (
                OperationKind::BackUpFile,
                json!(BackUpFileRequest {
                    source_path: source,
                    destination_path: text(&destination.path),
                }),
            )
        }
        MediaDispatch::VerifyArtifact(dispatch) => (
            OperationKind::VerifyArtifact,
            json!(VerifyArtifactRequest {
                path: source,
                staging_root: plan.verify_staging_root.clone().ok_or_else(|| {
                    VoomError::Internal(
                        "verify_artifact plan resolved without a containment root".to_owned(),
                    )
                })?,
                expected: dispatch.expected.clone(),
            }),
        ),
    })
}

fn transcode_audio_payload(
    dispatch: &MediaTranscodeAudioDispatch,
    plan: &ResolvedPlan,
    source: String,
) -> Result<JsonValue, VoomError> {
    let output = sole_planned(plan)?;
    Ok(json!(TranscodeAudioRequest {
        input: TranscodeAudioInput {
            path: source,
            expected: dispatch.expected.clone(),
        },
        output: TranscodeAudioOutput {
            staging_root: parent_text(&output.path),
            path: text(&output.path),
            container: dispatch.output_container.clone(),
            overwrite: false,
        },
        selection: dispatch.selection.clone(),
        audio: dispatch.settings.clone(),
    }))
}

fn extract_audio_payload(
    dispatch: &MediaExtractAudioDispatch,
    plan: &ResolvedPlan,
    source: String,
) -> Result<JsonValue, VoomError> {
    let descriptors: Vec<_> = dispatch
        .outputs
        .iter()
        .zip(&plan.outputs)
        .map(|(planned, resolved)| ExtractAudioOutputDescriptor {
            output_id: planned.output_id.clone(),
            selection: planned.selection.clone(),
            output: ExtractAudioOutput {
                staging_root: parent_text(&resolved.path),
                path: text(&resolved.path),
                container: dispatch.output_container.clone(),
                audio_codec: planned.audio_codec.clone(),
                overwrite: false,
            },
        })
        .collect();
    let first = descriptors.first().cloned().ok_or_else(|| {
        VoomError::Config("audio extraction dispatch must contain at least one output".to_owned())
    })?;
    Ok(json!(ExtractAudioRequest {
        input: ExtractAudioInput {
            path: source,
            expected: dispatch.expected.clone(),
        },
        output: first.output.clone(),
        selection: first.selection.clone(),
        outputs: Some(descriptors),
    }))
}

fn transcode_video_payload(
    dispatch: &MediaTranscodeVideoDispatch,
    plan: &ResolvedPlan,
    source: String,
) -> Result<JsonValue, VoomError> {
    let output = sole_planned(plan)?;
    Ok(json!(TranscodeVideoRequest {
        input: TranscodeVideoInput {
            path: source,
            expected: dispatch.expected.clone(),
            video_codec: None,
            video_pixel_format: None,
        },
        output: TranscodeVideoOutput {
            staging_root: parent_text(&output.path),
            path: text(&output.path),
            container: dispatch.output_container.clone(),
            video_codec: dispatch.output_video_codec.clone(),
            overwrite: false,
        },
        profile: dispatch.profile.clone(),
        hardware_assignment: dispatch.hardware_assignment.clone(),
        copy_video: dispatch.copy_video,
    }))
}

/// Strictly parse the worker's terminal payload, re-observe everything it
/// claims, and assemble the completion result with the `agent_observed`
/// evidence block attached.
async fn settle_media_result(
    envelope: &MediaDispatch,
    plan: &ResolvedPlan,
    result_payload: JsonValue,
    input_pre: CommitObservedFacts,
    dispatch: &LeaseDispatch,
    context: &CoordinatorContext,
    registry: &ChildEndpointRegistry,
) -> LeaseOutcome {
    let operation = envelope_operation(envelope);
    let parsed = match parse_worker_result(envelope, &result_payload) {
        Ok(parsed) => parsed,
        Err(reason) => return malformed(reason, result_payload),
    };
    let input_post = match observe(&plan.source).await {
        Ok(facts) => facts,
        Err(outcome) => return outcome,
    };
    let mut evidence = AgentObservedEvidence {
        input_pre: wire_facts(input_pre),
        input_post: wire_facts(input_post),
        outputs: Vec::new(),
    };
    match parsed {
        ParsedResult::Probe(probe) => {
            if probe.post_probe.size_bytes != evidence.input_post.size_bytes
                || probe.post_probe.content_hash != evidence.input_post.content_hash
            {
                return mismatch_failure(
                    "probe",
                    &format!(
                        "worker reported {} bytes hashed {}, agent observed {} bytes hashed {}",
                        probe.post_probe.size_bytes,
                        probe.post_probe.content_hash,
                        evidence.input_post.size_bytes,
                        evidence.input_post.content_hash
                    ),
                    &evidence,
                );
            }
        }
        ParsedResult::Outputs(reported) => {
            if let Some(outcome) =
                verify_reported_outputs(operation, plan, &reported, &result_payload, &mut evidence)
                    .await
            {
                return outcome;
            }
        }
    }

    // Staged-output probes: best-effort identity snapshots through the shared
    // ffprobe child. A missing endpoint only thins the evidence, never fails
    // the lease.
    if is_staged_output_operation(operation) {
        for (index, output) in evidence.outputs.iter_mut().enumerate() {
            let path = plan.outputs[index].path.clone();
            if let Some(snapshot) =
                probe_output(registry, dispatch, context, &path, &output.facts, index).await
            {
                output.snapshot = Some(snapshot);
            }
        }
    }

    let mut result_payload = result_payload;
    if let Some(object) = result_payload.as_object_mut() {
        // The control plane only consumes a lease whose completion echoes the
        // dispatch plan's owner-local proof (issue #479, ADR 0073). Envelope
        // workers never see the plan, so the agent — which resolved and
        // enforced it — attaches the evidence itself, mirroring the worker
        // echo shape exactly: `validated` always, owner/evidence only when
        // the ticket declared byte work.
        let mut echo = serde_json::Map::new();
        echo.insert("validated".to_owned(), json!(true));
        if let Some(owner) = dispatch.artifact_access_plan.owner_node_id {
            echo.insert("owner_node_id".to_owned(), json!(owner));
        }
        if let Some(access_evidence) = dispatch.artifact_access_plan.access_evidence.as_ref() {
            match serde_json::to_value(access_evidence) {
                Ok(value) => {
                    echo.insert("access_evidence".to_owned(), value);
                }
                Err(error) => {
                    return malformed(
                        format!("serialize access_evidence evidence: {error}"),
                        json!({}),
                    );
                }
            }
        }
        object.insert("artifact_access".to_owned(), JsonValue::Object(echo));
        match serde_json::to_value(&evidence) {
            Ok(value) => {
                object.insert("agent_observed".to_owned(), value);
            }
            Err(error) => {
                return malformed(
                    format!("serialize agent_observed evidence: {error}"),
                    json!({}),
                );
            }
        }
    }
    LeaseOutcome::Complete(result_payload)
}

/// The wire operation an envelope decodes to; exhaustive over the envelope.
fn envelope_operation(envelope: &MediaDispatch) -> OperationKind {
    match envelope {
        MediaDispatch::Probe(_) => OperationKind::ProbeFile,
        MediaDispatch::TranscodeAudio(_) => OperationKind::TranscodeAudio,
        MediaDispatch::ExtractAudio(_) => OperationKind::ExtractAudio,
        MediaDispatch::TranscodeVideo(_) => OperationKind::TranscodeVideo,
        MediaDispatch::Remux(_) => OperationKind::Remux,
        MediaDispatch::BackUpFile(_) => OperationKind::BackUpFile,
        MediaDispatch::VerifyArtifact(_) => OperationKind::VerifyArtifact,
    }
}

/// Re-observe every planned output and compare against the worker's reported
/// facts, appending verified observations to `evidence`. Returns
/// `Some(outcome)` when cardinality or facts disagree.
async fn verify_reported_outputs(
    operation: OperationKind,
    plan: &ResolvedPlan,
    reported: &[ReportedOutput],
    result_payload: &JsonValue,
    evidence: &mut AgentObservedEvidence,
) -> Option<LeaseOutcome> {
    if reported.len() != plan.outputs.len() {
        return Some(malformed(
            format!(
                "{} result reports {} outputs for {} planned outputs",
                operation.as_str(),
                reported.len(),
                plan.outputs.len()
            ),
            result_payload.clone(),
        ));
    }
    for (resolved, worker_reported) in plan.outputs.iter().zip(reported) {
        let facts = match observe(&resolved.path).await {
            Ok(facts) => facts,
            Err(outcome) => return Some(outcome),
        };
        if facts.size_bytes != worker_reported.size_bytes
            || facts.content_hash != worker_reported.content_hash
        {
            return Some(mismatch_failure(
                "output",
                &format!(
                    "at {}: worker reported {} bytes hashed {}, agent observed {} bytes hashed {}",
                    resolved.provider_relative_locator,
                    worker_reported.size_bytes,
                    worker_reported.content_hash,
                    facts.size_bytes,
                    facts.content_hash
                ),
                evidence,
            ));
        }
        evidence.outputs.push(AgentObservedOutput {
            provider_relative_locator: resolved.provider_relative_locator.clone(),
            facts: wire_facts(facts),
            snapshot: None,
        });
    }
    None
}

/// Decode the terminal frame into the operation's typed result and project it
/// to the outputs needing independent observation.
fn parse_worker_result(
    envelope: &MediaDispatch,
    payload: &JsonValue,
) -> Result<ParsedResult, String> {
    // Each worker family reports its own fact struct; they all share the
    // (size, hash) pair this executor verifies independently.
    let reported = |size_bytes: u64, content_hash: &str| ReportedOutput {
        size_bytes,
        content_hash: content_hash.to_owned(),
    };
    match envelope {
        MediaDispatch::TranscodeAudio(_) => {
            let result: TranscodeAudioResult = serde_json::from_value(payload.clone())
                .map_err(|error| format!("decode transcode_audio result: {error}"))?;
            Ok(ParsedResult::Outputs(vec![reported(
                result.output.size_bytes,
                &result.output.content_hash,
            )]))
        }
        MediaDispatch::ExtractAudio(_) => {
            let result: ExtractAudioResult = serde_json::from_value(payload.clone())
                .map_err(|error| format!("decode extract_audio result: {error}"))?;
            match &result.outputs {
                Some(outputs) => Ok(ParsedResult::Outputs(
                    outputs
                        .iter()
                        .map(|output| {
                            reported(output.output.size_bytes, &output.output.content_hash)
                        })
                        .collect(),
                )),
                None => Ok(ParsedResult::Outputs(vec![reported(
                    result.output.size_bytes,
                    &result.output.content_hash,
                )])),
            }
        }
        MediaDispatch::TranscodeVideo(_) => {
            let result: TranscodeVideoResult = serde_json::from_value(payload.clone())
                .map_err(|error| format!("decode transcode_video result: {error}"))?;
            Ok(ParsedResult::Outputs(vec![reported(
                result.output.size_bytes,
                &result.output.content_hash,
            )]))
        }
        MediaDispatch::Remux(_) => {
            let result: RemuxResult = serde_json::from_value(payload.clone())
                .map_err(|error| format!("decode remux result: {error}"))?;
            Ok(ParsedResult::Outputs(vec![reported(
                result.output.size_bytes,
                &result.output.content_hash,
            )]))
        }
        MediaDispatch::BackUpFile(_) => {
            let result: BackUpFileResult = serde_json::from_value(payload.clone())
                .map_err(|error| format!("decode back_up_file result: {error}"))?;
            Ok(ParsedResult::Outputs(vec![ReportedOutput {
                size_bytes: result.size_bytes,
                content_hash: result.checksum,
            }]))
        }
        MediaDispatch::VerifyArtifact(_) => {
            let result: VerifyArtifactResult = serde_json::from_value(payload.clone())
                .map_err(|error| format!("decode verify_artifact result: {error}"))?;
            Ok(ParsedResult::Outputs(vec![ReportedOutput {
                size_bytes: result.observed.size_bytes,
                content_hash: result.observed.content_hash,
            }]))
        }
        MediaDispatch::Probe(_) => {
            let result: ProbeFileResult = serde_json::from_value(payload.clone())
                .map_err(|error| format!("decode probe_file result: {error}"))?;
            Ok(ParsedResult::Probe(Box::new(result)))
        }
    }
}

/// Whether the operation writes staged outputs worth ffprobe snapshots.
fn is_staged_output_operation(operation: OperationKind) -> bool {
    matches!(
        operation,
        OperationKind::TranscodeAudio
            | OperationKind::ExtractAudio
            | OperationKind::TranscodeVideo
            | OperationKind::Remux
    )
}

/// Wait briefly for a live ffprobe child, probe the staged output once, and
/// return its snapshot. Any failure along the way simply drops the evidence.
async fn probe_output(
    registry: &ChildEndpointRegistry,
    dispatch: &LeaseDispatch,
    context: &CoordinatorContext,
    path: &Path,
    facts: &ObservedFileFacts,
    index: usize,
) -> Option<JsonValue> {
    let endpoint = wait_for_probe_endpoint(registry).await?;
    let request = OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id: dispatch.lease_id,
        payload: json!(ProbeFileRequest {
            path: text(path),
            expected: ExpectedFileFacts {
                size_bytes: facts.size_bytes,
                content_hash: facts.content_hash.clone(),
                modified_at: None,
                local_file_key: None,
            },
        }),
        heartbeat_deadline_ms: duration_millis_u32(context.lease_ttl),
        progress_idle_deadline_ms: duration_millis_u32(context.progress_timeout),
    };
    let key = format!(
        "{}-{}-probe-{}",
        context.incarnation_id, dispatch.lease_id.0, index
    );
    let stream = tokio::time::timeout(
        context.progress_timeout,
        endpoint
            .client
            .dispatch(&endpoint.credentials, &key, request),
    )
    .await
    .ok()?
    .ok()?;
    let mut frames = stream.frames;
    loop {
        match tokio::time::timeout(context.progress_timeout, frames.next_frame()).await {
            Ok(Ok(NdjsonOutcome::Terminated(ProgressFrame::Result { payload, .. }))) => {
                return serde_json::from_value::<ProbeFileResult>(payload)
                    .ok()
                    .map(|p| p.snapshot);
            }
            Ok(Ok(NdjsonOutcome::Frame(ProgressFrame::Progress { .. }))) => {}
            Ok(Ok(_) | Err(_)) | Err(_) => return None,
        }
    }
}

/// Bounded wait for any live child declaring `probe_file`, mirroring the scan
/// pump's endpoint budget.
async fn wait_for_probe_endpoint(registry: &ChildEndpointRegistry) -> Option<ChildEndpoint> {
    let deadline = tokio::time::Instant::now() + PROBE_ENDPOINT_WAIT;
    loop {
        if let Some(endpoint) = registry.resolve(OperationKind::ProbeFile) {
            return Some(endpoint);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Observe a regular file, turning absence and observation errors into lease
/// failures so the caller never sees partial facts.
async fn observe(path: &Path) -> Result<CommitObservedFacts, LeaseOutcome> {
    match crate::commit::try_observe_regular_file(path).await {
        Ok(Some(facts)) => Ok(facts),
        Ok(None) => Err(LeaseOutcome::Failure(
            FailureClass::ArtifactUnavailable,
            format!("media file is missing: {}", path.display()),
            json!({}),
        )),
        Err(error) => {
            let class = match &error {
                VoomError::ArtifactChecksumMismatch(_) => FailureClass::ArtifactChecksumMismatch,
                _ => FailureClass::ArtifactUnavailable,
            };
            Err(LeaseOutcome::Failure(class, error.to_string(), json!({})))
        }
    }
}

/// Project agent-side facts onto the wire fact shape.
fn wire_facts(facts: CommitObservedFacts) -> ObservedFileFacts {
    ObservedFileFacts {
        size_bytes: facts.size_bytes,
        content_hash: facts.content_hash,
        modified_at: None,
        local_file_key: None,
    }
}

fn sole_planned(plan: &ResolvedPlan) -> Result<&PlannedOutput, VoomError> {
    plan.outputs.first().ok_or_else(|| {
        VoomError::Internal("media dispatch resolved without its planned output".to_owned())
    })
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn parent_text(path: &Path) -> String {
    path.parent().map_or_else(
        || text(Path::new(".")),
        |parent| parent.to_string_lossy().into_owned(),
    )
}

/// Pre-child validation failure: the payload never reached a worker, so
/// there is no worker evidence to preserve.
fn malformed(reason: String, payload: JsonValue) -> LeaseOutcome {
    LeaseOutcome::Failure(FailureClass::MalformedWorkerResult, reason, payload)
}

/// Fact-mismatch failure carrying the agent's own observations as evidence.
fn mismatch_failure(stage: &str, detail: &str, evidence: &AgentObservedEvidence) -> LeaseOutcome {
    LeaseOutcome::Failure(
        FailureClass::ArtifactChecksumMismatch,
        format!("{stage} facts disagree: {detail}"),
        json!({"agent_observed": evidence}),
    )
}

#[cfg(test)]
#[path = "media_test.rs"]
mod tests;
