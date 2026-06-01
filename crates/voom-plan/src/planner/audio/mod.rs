use serde_json::json;
use voom_policy::{MediaSnapshotInput, TrackFilter};

mod payload;
mod selection;

pub use payload::{
    AUDIO_EXTRACT_CODEC, AUDIO_EXTRACT_CONTAINER, AUDIO_TRANSCODE_CONTAINER, AudioOperationPayload,
    AudioOperationType, AudioPayloadError,
};
pub use selection::{
    AudioBundleRole, AudioDispositionFact, AudioPlanShape, AudioPlanningBlock,
    SnapshotAudioStreamFact, evaluate_audio_filter, extract_audio_shape, extraction_role,
    has_transcode_preservation_facts, selected_audio_streams, stream_facts, transcode_audio_shape,
};

use crate::{NodeStatus, PlanOperationKind, PlanningDiagnostic, PlanningDiagnosticCode};

use super::OperationPlan;

pub(super) fn plan_transcode(
    phase_name: &str,
    snapshot: &MediaSnapshotInput,
    target_codec: &str,
    container: &str,
    filter: Option<&TrackFilter>,
) -> OperationPlan {
    let operation_kind = PlanOperationKind::TranscodeAudio;
    let payload = AudioOperationPayload {
        operation_type: AudioOperationType::TranscodeAudio,
        target_codec: target_codec.to_owned(),
        container: container.to_owned(),
        source_media_snapshot_id: snapshot.existing_media_snapshot_id.map(|id| id.0),
        filter: filter.cloned(),
    }
    .into_value();
    let observed_state = audio_observed_state(snapshot, filter);
    let (status, status_reason, capability, diagnostic) =
        match transcode_audio_shape(snapshot, target_codec, container, filter) {
            AudioPlanShape::NoOp => (
                NodeStatus::NoOp,
                format!("selected audio is already {target_codec} in {container}"),
                None,
                None,
            ),
            AudioPlanShape::Planned => (
                NodeStatus::Planned,
                format!("selected audio will be transcoded to {target_codec} in {container}"),
                Some("transcode_audio".to_owned()),
                None,
            ),
            AudioPlanShape::Blocked(block) => {
                let (code, message) = audio_block_diagnostic(block, operation_kind);
                (NodeStatus::Blocked, message.to_owned(), None, Some(code))
            }
        };

    let plan = OperationPlan::new(
        operation_kind,
        payload,
        observed_state,
        status,
        status_reason,
        capability,
    );
    with_optional_diagnostic(plan, diagnostic, phase_name, snapshot, operation_kind)
}

pub(super) fn plan_extract(
    phase_name: &str,
    snapshot: &MediaSnapshotInput,
    target_codec: &str,
    container: &str,
    filter: Option<&TrackFilter>,
) -> OperationPlan {
    let operation_kind = PlanOperationKind::ExtractAudio;
    let payload = AudioOperationPayload {
        operation_type: AudioOperationType::ExtractAudio,
        target_codec: target_codec.to_owned(),
        container: container.to_owned(),
        source_media_snapshot_id: snapshot.existing_media_snapshot_id.map(|id| id.0),
        filter: filter.cloned(),
    }
    .into_value();
    let observed_state = audio_observed_state(snapshot, filter);
    let (status, status_reason, capability, diagnostic) =
        match extract_audio_shape(snapshot, filter) {
            AudioPlanShape::NoOp => (
                NodeStatus::NoOp,
                format!("selected audio is already extracted as {target_codec} in {container}"),
                None,
                None,
            ),
            AudioPlanShape::Planned => (
                NodeStatus::Planned,
                format!("selected audio will be extracted as {target_codec} in {container}"),
                Some("extract_audio".to_owned()),
                None,
            ),
            AudioPlanShape::Blocked(block) => {
                let (code, message) = audio_block_diagnostic(block, operation_kind);
                (NodeStatus::Blocked, message.to_owned(), None, Some(code))
            }
        };

    let plan = OperationPlan::new(
        operation_kind,
        payload,
        observed_state,
        status,
        status_reason,
        capability,
    );
    with_optional_diagnostic(plan, diagnostic, phase_name, snapshot, operation_kind)
}

fn with_optional_diagnostic(
    plan: OperationPlan,
    code: Option<PlanningDiagnosticCode>,
    phase_name: &str,
    snapshot: &MediaSnapshotInput,
    operation_kind: PlanOperationKind,
) -> OperationPlan {
    let Some(code) = code else {
        return plan;
    };
    let message = plan.status_reason.clone();
    plan.with_diagnostic(
        PlanningDiagnostic::error(code, &message)
            .with_phase(phase_name)
            .with_operation_kind(operation_kind.as_str())
            .with_target(snapshot.target.clone()),
    )
}

fn audio_observed_state(
    snapshot: &MediaSnapshotInput,
    filter: Option<&TrackFilter>,
) -> Option<serde_json::Value> {
    let mut observed = serde_json::Map::new();
    if let Some(container) = &snapshot.container {
        observed.insert("container".to_owned(), json!(container));
    }
    if let Ok(selected) = selected_audio_streams(snapshot, filter) {
        observed.insert(
            "selected_audio_stream_count".to_owned(),
            json!(selected.len()),
        );
        let codecs = selected
            .iter()
            .filter_map(|stream| stream.codec.clone())
            .collect::<Vec<_>>();
        if !codecs.is_empty() {
            observed.insert("audio_codecs".to_owned(), json!(codecs));
        }
    }
    if observed.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(observed))
    }
}

fn audio_block_diagnostic(
    block: AudioPlanningBlock,
    operation_kind: PlanOperationKind,
) -> (PlanningDiagnosticCode, &'static str) {
    match block {
        AudioPlanningBlock::InsufficientSnapshotFacts => (
            PlanningDiagnosticCode::InsufficientSnapshotFacts,
            "snapshot stream facts are insufficient for audio planning",
        ),
        AudioPlanningBlock::UnsupportedSelector => (
            PlanningDiagnosticCode::UnsupportedMediaShape,
            "audio selector is not supported by audio planning",
        ),
        AudioPlanningBlock::ZeroMatches => (
            PlanningDiagnosticCode::UnsupportedMediaShape,
            if operation_kind == PlanOperationKind::ExtractAudio {
                "extract_audio selector matched zero audio streams"
            } else {
                "transcode_audio selector matched zero audio streams"
            },
        ),
        AudioPlanningBlock::MultipleMatches => (
            PlanningDiagnosticCode::UnsupportedMediaShape,
            "extract_audio selector matched multiple audio streams",
        ),
        AudioPlanningBlock::NoVideo => (
            PlanningDiagnosticCode::UnsupportedMediaShape,
            "audio planning requires at least one video stream",
        ),
        AudioPlanningBlock::UnsupportedMediaShape => (
            PlanningDiagnosticCode::UnsupportedMediaShape,
            "media shape is not supported by audio planning",
        ),
    }
}
