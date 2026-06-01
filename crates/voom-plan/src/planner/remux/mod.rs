use serde_json::json;
use voom_policy::{
    CompiledOperation, DefaultStrategy, MediaSnapshotInput, TrackFilter, TrackTarget,
};

mod payload;
mod selection;

pub use payload::{
    RemuxDefaultAction, RemuxOperationPayload, RemuxPayloadError, RemuxTrackAction,
    RemuxTrackActionKind,
};
pub use selection::{RemuxPlanningBlock, SnapshotStreamFact, evaluate_filter, stream_facts};

use crate::{NodeStatus, PlanOperationKind, PlanningDiagnostic, PlanningDiagnosticCode};

use super::{OperationPlan, video_stream_count};
use payload::default_track_order;

pub(super) enum CandidateSupport {
    Supported,
    Unsupported(&'static str),
}

pub(super) fn candidate_kind(operation: &CompiledOperation) -> Option<PlanOperationKind> {
    match operation {
        CompiledOperation::SetContainer { .. } => Some(PlanOperationKind::SetContainer),
        CompiledOperation::KeepTracks { .. } => Some(PlanOperationKind::KeepTracks),
        CompiledOperation::RemoveTracks { .. } => Some(PlanOperationKind::RemoveTracks),
        CompiledOperation::ReorderTracks { .. } => Some(PlanOperationKind::ReorderTracks),
        CompiledOperation::SetDefaults { .. } => Some(PlanOperationKind::SetDefaults),
        _ => None,
    }
}

pub(super) fn candidate_support(operation: &CompiledOperation) -> CandidateSupport {
    match operation {
        CompiledOperation::SetContainer { container } if container.eq_ignore_ascii_case("mkv") => {
            CandidateSupport::Supported
        }
        CompiledOperation::SetContainer { .. } => {
            CandidateSupport::Unsupported("only mkv remux containers are supported")
        }
        CompiledOperation::KeepTracks { target, filter }
        | CompiledOperation::RemoveTracks { target, filter } => {
            if *target == TrackTarget::Video {
                return CandidateSupport::Unsupported(
                    "video track selection is not supported by remux planning",
                );
            }
            if *target == TrackTarget::Attachment {
                return CandidateSupport::Unsupported(
                    "attachment track selection is not supported by remux planning",
                );
            }
            if filter.as_ref().is_some_and(filter_has_unsupported_shape) {
                CandidateSupport::Unsupported("track filter is not supported by remux planning")
            } else {
                CandidateSupport::Supported
            }
        }
        CompiledOperation::SetDefaults {
            strategy: DefaultStrategy::Best,
            ..
        } => CandidateSupport::Unsupported(
            "default strategy best is not supported by remux planning",
        ),
        CompiledOperation::ReorderTracks { targets } => {
            if duplicate_track_targets(targets) {
                CandidateSupport::Unsupported("track order contains duplicate target groups")
            } else {
                CandidateSupport::Supported
            }
        }
        CompiledOperation::SetDefaults { .. } => CandidateSupport::Supported,
        _ => CandidateSupport::Unsupported("operation is not supported by remux planning"),
    }
}

pub(super) fn plan_set_container(
    phase_name: &str,
    snapshot: &MediaSnapshotInput,
    container: &str,
) -> OperationPlan {
    let (status, status_reason, capability, diagnostic) = match snapshot.container.as_deref() {
        Some(current) if current == container => (
            NodeStatus::NoOp,
            format!("container is already {container}"),
            None,
            None,
        ),
        Some(current) => (
            NodeStatus::Planned,
            format!("container {current} will be changed to {container}"),
            Some("remux_container".to_owned()),
            None,
        ),
        None => {
            let message = "snapshot container is unknown";
            (
                NodeStatus::Blocked,
                message.to_owned(),
                None,
                Some(PlanningDiagnosticCode::InsufficientSnapshotFacts),
            )
        }
    };

    let plan = OperationPlan::new(
        PlanOperationKind::SetContainer,
        json!({ "container": container }),
        snapshot
            .container
            .as_ref()
            .map(|container| json!({ "container": container })),
        status,
        status_reason,
        capability,
    );
    with_optional_diagnostic(
        plan,
        diagnostic,
        phase_name,
        snapshot,
        PlanOperationKind::SetContainer,
    )
}

pub(super) fn plan_blocked_candidate(
    phase_name: &str,
    snapshot: &MediaSnapshotInput,
    operation_kind: PlanOperationKind,
    operation_payload: serde_json::Value,
    message: &str,
) -> OperationPlan {
    OperationPlan::new(
        operation_kind,
        operation_payload,
        None,
        NodeStatus::Blocked,
        message.to_owned(),
        None,
    )
    .with_diagnostic(operation_diagnostic(
        PlanningDiagnosticCode::UnsupportedMediaShape,
        phase_name,
        snapshot,
        operation_kind,
        message,
    ))
}

pub(super) fn plan_group(
    phase_name: &str,
    snapshot: &MediaSnapshotInput,
    operations: &[&CompiledOperation],
) -> OperationPlan {
    let payload = remux_payload(snapshot, operations);
    let observed_state = snapshot
        .container
        .as_ref()
        .map(|container| json!({ "container": container }));
    let target_container = payload
        .get("container")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("mkv");
    let (status, status_reason, capability, diagnostic) =
        match remux_group_shape(snapshot, operations, target_container) {
            RemuxGroupShape::NoOp => (
                NodeStatus::NoOp,
                format!("container is already {target_container} and track selection is unchanged"),
                None,
                None,
            ),
            RemuxGroupShape::ContainerChange { current } => (
                NodeStatus::Planned,
                format!("container {current} will be changed to {target_container}"),
                Some("remux_container".to_owned()),
                None,
            ),
            RemuxGroupShape::TrackSelectionChange => (
                NodeStatus::Planned,
                "track selection will be changed".to_owned(),
                Some("remux".to_owned()),
                None,
            ),
            RemuxGroupShape::InsufficientFacts(message) => (
                NodeStatus::Blocked,
                message.to_owned(),
                None,
                Some(PlanningDiagnosticCode::InsufficientSnapshotFacts),
            ),
            RemuxGroupShape::UnsupportedShape(message) => (
                NodeStatus::Blocked,
                message.to_owned(),
                None,
                Some(PlanningDiagnosticCode::UnsupportedMediaShape),
            ),
        };

    let plan = OperationPlan::new(
        PlanOperationKind::Remux,
        payload,
        observed_state,
        status,
        status_reason,
        capability,
    );
    with_optional_diagnostic(
        plan,
        diagnostic,
        phase_name,
        snapshot,
        PlanOperationKind::Remux,
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemuxGroupShape {
    NoOp,
    ContainerChange { current: String },
    TrackSelectionChange,
    InsufficientFacts(&'static str),
    UnsupportedShape(&'static str),
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
    plan.with_diagnostic(operation_diagnostic(
        code,
        phase_name,
        snapshot,
        operation_kind,
        &message,
    ))
}

fn operation_diagnostic(
    code: PlanningDiagnosticCode,
    phase_name: &str,
    snapshot: &MediaSnapshotInput,
    operation_kind: PlanOperationKind,
    message: &str,
) -> PlanningDiagnostic {
    PlanningDiagnostic::error(code, message)
        .with_phase(phase_name)
        .with_operation_kind(operation_kind.as_str())
        .with_target(snapshot.target.clone())
}

fn duplicate_track_targets(targets: &[TrackTarget]) -> bool {
    let mut seen = Vec::new();
    for target in targets {
        if seen.contains(target) {
            return true;
        }
        seen.push(*target);
    }
    false
}

fn filter_has_unsupported_shape(filter: &TrackFilter) -> bool {
    match filter {
        TrackFilter::Commentary | TrackFilter::TitleMatches { .. } => true,
        TrackFilter::Not { inner } => filter_has_unsupported_shape(inner),
        TrackFilter::And { filters } | TrackFilter::Or { filters } => {
            filters.iter().any(filter_has_unsupported_shape)
        }
        TrackFilter::LanguageIn { .. }
        | TrackFilter::CodecIn { .. }
        | TrackFilter::Channels { .. }
        | TrackFilter::Forced
        | TrackFilter::Default
        | TrackFilter::Font
        | TrackFilter::TitleContains { .. } => false,
    }
}

fn remux_payload(
    snapshot: &MediaSnapshotInput,
    operations: &[&CompiledOperation],
) -> serde_json::Value {
    let container = operations
        .iter()
        .find_map(|operation| match operation {
            CompiledOperation::SetContainer { container } => Some(container.as_str()),
            _ => None,
        })
        .unwrap_or("mkv");
    let track_actions = operations
        .iter()
        .filter_map(|operation| match operation {
            CompiledOperation::KeepTracks { target, filter } => Some(track_action_payload(
                RemuxTrackActionKind::KeepTracks,
                *target,
                filter.clone(),
            )),
            CompiledOperation::RemoveTracks { target, filter } => Some(track_action_payload(
                RemuxTrackActionKind::RemoveTracks,
                *target,
                filter.clone(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    let reorder_operations = operations
        .iter()
        .filter_map(|operation| match operation {
            CompiledOperation::ReorderTracks { targets } => Some(targets),
            _ => None,
        })
        .collect::<Vec<_>>();
    let track_order = match reorder_operations.as_slice() {
        [targets] => targets
            .iter()
            .map(|target| remux_track_group(*target))
            .collect::<Vec<_>>(),
        _ => default_track_order(),
    };
    let defaults = operations
        .iter()
        .filter_map(|operation| match operation {
            CompiledOperation::SetDefaults { target, strategy } => Some(RemuxDefaultAction {
                target: *target,
                strategy: *strategy,
            }),
            _ => None,
        })
        .collect::<Vec<_>>();

    RemuxOperationPayload {
        container: container.to_owned(),
        source_media_snapshot_id: snapshot.existing_media_snapshot_id.map(|id| id.0),
        track_actions,
        track_order,
        defaults,
    }
    .into_value()
}

fn track_action_payload(
    kind: RemuxTrackActionKind,
    target: TrackTarget,
    filter: Option<TrackFilter>,
) -> RemuxTrackAction {
    RemuxTrackAction {
        kind,
        target,
        filter,
    }
}

fn remux_track_group(target: TrackTarget) -> voom_core::RemuxTrackGroup {
    match target {
        TrackTarget::Video => voom_core::RemuxTrackGroup::Video,
        TrackTarget::Audio => voom_core::RemuxTrackGroup::Audio,
        TrackTarget::Subtitle => voom_core::RemuxTrackGroup::Subtitle,
        TrackTarget::Attachment => voom_core::RemuxTrackGroup::Attachment,
    }
}

fn remux_group_shape(
    snapshot: &MediaSnapshotInput,
    operations: &[&CompiledOperation],
    target_container: &str,
) -> RemuxGroupShape {
    let track_selection_changed = match evaluate_remux_track_operations(snapshot, operations) {
        Ok(changed) => changed,
        Err(block) => return remux_block_shape(block),
    };

    let Some(current_container) = snapshot.container.as_deref() else {
        return RemuxGroupShape::InsufficientFacts("snapshot container is unknown");
    };
    if current_container.eq_ignore_ascii_case(target_container) && !track_selection_changed {
        RemuxGroupShape::NoOp
    } else if current_container.eq_ignore_ascii_case(target_container) {
        RemuxGroupShape::TrackSelectionChange
    } else {
        RemuxGroupShape::ContainerChange {
            current: current_container.to_owned(),
        }
    }
}

fn evaluate_remux_track_operations(
    snapshot: &MediaSnapshotInput,
    operations: &[&CompiledOperation],
) -> Result<bool, RemuxPlanningBlock> {
    let has_track_operation = operations
        .iter()
        .any(|operation| !matches!(operation, CompiledOperation::SetContainer { .. }));
    let has_stream_facts = has_remux_stream_fact_shape(snapshot);
    if !has_track_operation && !has_stream_facts {
        if video_stream_count(snapshot) == Some(0) {
            return Err(RemuxPlanningBlock::UnsupportedMediaShape);
        }
        return Ok(false);
    }

    let facts = stream_facts(snapshot)?;
    if !facts.iter().any(|stream| stream.kind == TrackTarget::Video) {
        return Err(RemuxPlanningBlock::UnsupportedMediaShape);
    }
    if facts
        .iter()
        .any(|stream| stream.kind == TrackTarget::Attachment)
    {
        return Err(RemuxPlanningBlock::UnsupportedMediaShape);
    }
    if !has_track_operation {
        return Ok(false);
    }

    let mut changed = false;
    let mut seen_reorder = false;
    for operation in operations {
        match operation {
            CompiledOperation::KeepTracks { target, filter } => {
                changed |= keep_tracks_changes(&facts, *target, filter.as_ref())?;
            }
            CompiledOperation::RemoveTracks { target, filter } => {
                changed |= remove_tracks_changes(&facts, *target, filter.as_ref())?;
            }
            CompiledOperation::SetDefaults { target, strategy } => {
                if !facts.iter().any(|stream| stream.kind == *target)
                    && !matches!(strategy, DefaultStrategy::None | DefaultStrategy::Preserve)
                {
                    return Err(RemuxPlanningBlock::InsufficientSnapshotFacts);
                }
                changed |= set_defaults_changes(&facts, *target, *strategy);
            }
            CompiledOperation::ReorderTracks { targets } => {
                if seen_reorder || targets.is_empty() || duplicate_track_targets(targets) {
                    return Err(RemuxPlanningBlock::UnsupportedMediaShape);
                }
                seen_reorder = true;
                changed |= reorder_tracks_changes(&facts, targets)?;
            }
            CompiledOperation::SetContainer { .. } => {}
            _ => return Err(RemuxPlanningBlock::UnsupportedMediaShape),
        }
    }

    Ok(changed)
}

fn keep_tracks_changes(
    facts: &[SnapshotStreamFact],
    target: TrackTarget,
    filter: Option<&TrackFilter>,
) -> Result<bool, RemuxPlanningBlock> {
    let Some(filter) = filter else {
        return Ok(false);
    };
    for stream in facts.iter().filter(|stream| stream.kind == target) {
        if !evaluate_filter(filter, stream)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn remove_tracks_changes(
    facts: &[SnapshotStreamFact],
    target: TrackTarget,
    filter: Option<&TrackFilter>,
) -> Result<bool, RemuxPlanningBlock> {
    let Some(filter) = filter else {
        return Ok(facts.iter().any(|stream| stream.kind == target));
    };
    for stream in facts.iter().filter(|stream| stream.kind == target) {
        if evaluate_filter(filter, stream)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn set_defaults_changes(
    facts: &[SnapshotStreamFact],
    target: TrackTarget,
    strategy: DefaultStrategy,
) -> bool {
    let target_streams = facts
        .iter()
        .filter(|stream| stream.kind == target)
        .collect::<Vec<_>>();
    match strategy {
        DefaultStrategy::First => target_streams
            .iter()
            .min_by_key(|stream| stream.provider_stream_index)
            .is_none_or(|first| {
                !first.is_default
                    || target_streams.iter().any(|stream| {
                        stream.provider_stream_index != first.provider_stream_index
                            && stream.is_default
                    })
            }),
        DefaultStrategy::None => target_streams.iter().any(|stream| stream.is_default),
        DefaultStrategy::Preserve => false,
        DefaultStrategy::Best => true,
    }
}

fn reorder_tracks_changes(
    facts: &[SnapshotStreamFact],
    targets: &[TrackTarget],
) -> Result<bool, RemuxPlanningBlock> {
    if targets.contains(&TrackTarget::Attachment) {
        return Err(RemuxPlanningBlock::UnsupportedMediaShape);
    }
    let mut streams = facts
        .iter()
        .filter(|stream| stream.kind != TrackTarget::Attachment)
        .collect::<Vec<_>>();
    streams.sort_by_key(|stream| stream.provider_stream_index);
    let mut current_order = Vec::new();
    for stream in streams {
        if current_order.last().copied() != Some(stream.kind) {
            current_order.push(stream.kind);
        }
    }
    let requested_present_order = targets
        .iter()
        .copied()
        .filter(|target| current_order.contains(target))
        .collect::<Vec<_>>();
    Ok(current_order != requested_present_order)
}

fn has_remux_stream_fact_shape(snapshot: &MediaSnapshotInput) -> bool {
    let Some(streams) = snapshot
        .stream_summary
        .get("streams")
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };
    streams.iter().all(|stream| {
        stream.as_object().is_some_and(|stream| {
            stream.contains_key("id") && stream.contains_key("index") && stream.contains_key("kind")
        })
    })
}

fn remux_block_shape(block: RemuxPlanningBlock) -> RemuxGroupShape {
    match block {
        RemuxPlanningBlock::InsufficientSnapshotFacts => RemuxGroupShape::InsufficientFacts(
            "snapshot stream facts are insufficient for remux planning",
        ),
        RemuxPlanningBlock::UnsupportedMediaShape => {
            RemuxGroupShape::UnsupportedShape("media shape is not supported by remux planning")
        }
    }
}
