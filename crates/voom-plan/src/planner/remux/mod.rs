use std::collections::{BTreeSet, HashSet};

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
pub use selection::{
    RemuxFilterOperation, RemuxPlanningBlock, SnapshotFact, SnapshotStreamFact, evaluate_filter,
    resolve_track_keep_ids, stream_facts,
};

use crate::{NodeStatus, PlanOperationKind, PlanningDiagnostic, PlanningDiagnosticCode};

use super::{OperationPlan, video_stream_count};
pub(super) enum CandidateSupport {
    Supported,
    Unsupported(&'static str),
}

pub(super) fn candidate_kind(operation: &CompiledOperation) -> Option<PlanOperationKind> {
    match operation {
        CompiledOperation::SetContainer(voom_policy::compiled::CompiledSetContainerOperation {
            ..
        }) => Some(PlanOperationKind::SetContainer),
        CompiledOperation::KeepTracks(voom_policy::compiled::CompiledKeepTracksOperation {
            ..
        }) => Some(PlanOperationKind::KeepTracks),
        CompiledOperation::RemoveTracks(voom_policy::compiled::CompiledRemoveTracksOperation {
            ..
        }) => Some(PlanOperationKind::RemoveTracks),
        CompiledOperation::ReorderTracks(
            voom_policy::compiled::CompiledReorderTracksOperation { .. },
        ) => Some(PlanOperationKind::ReorderTracks),
        CompiledOperation::SetDefaults(voom_policy::compiled::CompiledSetDefaultsOperation {
            ..
        }) => Some(PlanOperationKind::SetDefaults),
        _ => None,
    }
}

pub(super) fn candidate_support(operation: &CompiledOperation) -> CandidateSupport {
    match operation {
        CompiledOperation::SetContainer(voom_policy::compiled::CompiledSetContainerOperation {
            container,
        }) if container.eq_ignore_ascii_case("mkv") => CandidateSupport::Supported,
        CompiledOperation::SetContainer(voom_policy::compiled::CompiledSetContainerOperation {
            ..
        }) => CandidateSupport::Unsupported("only mkv remux containers are supported"),
        CompiledOperation::KeepTracks(voom_policy::compiled::CompiledKeepTracksOperation {
            target,
            filter,
        })
        | CompiledOperation::RemoveTracks(voom_policy::compiled::CompiledRemoveTracksOperation {
            target,
            filter,
        }) => {
            if *target == TrackTarget::Video {
                return CandidateSupport::Unsupported(
                    "video track selection is not supported by remux planning",
                );
            }
            if filter.as_ref().is_some_and(filter_has_unsupported_shape) {
                CandidateSupport::Unsupported("track filter is not supported by remux planning")
            } else {
                CandidateSupport::Supported
            }
        }
        CompiledOperation::ReorderTracks(
            voom_policy::compiled::CompiledReorderTracksOperation { targets, .. },
        ) => {
            if duplicate_track_targets(targets) {
                CandidateSupport::Unsupported("track order contains duplicate target groups")
            } else {
                CandidateSupport::Supported
            }
        }
        CompiledOperation::SetDefaults(voom_policy::compiled::CompiledSetDefaultsOperation {
            ..
        }) => CandidateSupport::Supported,
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
    preferred_languages: &[String],
) -> OperationPlan {
    let resolution = resolve_remux_operations(snapshot, operations, preferred_languages);
    let best_evaluated_untagged_language = resolution
        .as_ref()
        .is_ok_and(|resolution| resolution.best_evaluated_untagged_language);
    let payload = match &resolution {
        Ok(resolution) => resolution.payload.clone().into_value(),
        Err(_) => remux_payload(snapshot, operations),
    };
    let observed_state = snapshot
        .container
        .as_ref()
        .map(|container| json!({ "container": container }));
    let target_container = payload
        .get("container")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("mkv");
    let shape = match resolution {
        Ok(resolution) => remux_group_shape(
            snapshot,
            target_container,
            resolution.track_selection_changed,
        ),
        Err(block) => remux_block_shape(block),
    };
    let (status, status_reason, capability, diagnostic) = match shape {
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
            message,
            None,
            Some(PlanningDiagnosticCode::InsufficientSnapshotFacts),
        ),
        RemuxGroupShape::UnsupportedShape(message) => (
            NodeStatus::Blocked,
            message,
            None,
            Some(PlanningDiagnosticCode::UnsupportedMediaShape),
        ),
        RemuxGroupShape::EmptyFilterSelection(message) => (
            NodeStatus::Blocked,
            message,
            None,
            Some(PlanningDiagnosticCode::EmptyTrackFilterSelection),
        ),
        RemuxGroupShape::AmbiguousFilterSelection(message) => (
            NodeStatus::Blocked,
            message,
            None,
            Some(PlanningDiagnosticCode::AmbiguousTrackFilterSelection),
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
    let plan = with_optional_diagnostic(
        plan,
        diagnostic,
        phase_name,
        snapshot,
        PlanOperationKind::Remux,
    );
    with_untagged_language_warning(
        plan,
        phase_name,
        snapshot,
        operations,
        best_evaluated_untagged_language,
    )
}

/// Attach a per-file `Warning` when remux language selection evaluates an
/// untagged track as `und` under ADR 0021. Skipped on blocked nodes.
fn with_untagged_language_warning(
    plan: OperationPlan,
    phase_name: &str,
    snapshot: &MediaSnapshotInput,
    operations: &[&CompiledOperation],
    best_evaluated_untagged_language: bool,
) -> OperationPlan {
    if plan.status == NodeStatus::Blocked
        || !(best_evaluated_untagged_language
            || remux_has_untagged_language_filter(snapshot, operations))
    {
        return plan;
    }
    plan.with_diagnostic(
        PlanningDiagnostic::warning(
            PlanningDiagnosticCode::UntaggedTrackLanguageDefaulted,
            "an untagged track was evaluated as language und by remux language selection",
        )
        .with_phase(phase_name)
        .with_operation_kind(PlanOperationKind::Remux.as_str())
        .with_target(snapshot.target.clone()),
    )
}

fn remux_has_untagged_language_filter(
    snapshot: &MediaSnapshotInput,
    operations: &[&CompiledOperation],
) -> bool {
    let Ok(facts) = stream_facts(snapshot) else {
        return false;
    };
    operations.iter().any(|operation| {
        let (target, filter, excludes_attachments) = match operation {
            CompiledOperation::KeepTracks(voom_policy::compiled::CompiledKeepTracksOperation {
                target,
                filter,
            })
            | CompiledOperation::RemoveTracks(
                voom_policy::compiled::CompiledRemoveTracksOperation { target, filter },
            )
            | CompiledOperation::SetDefaults(
                voom_policy::compiled::CompiledSetDefaultsOperation { target, filter, .. },
            ) => (Some(*target), filter.as_ref(), false),
            CompiledOperation::ReorderTracks(
                voom_policy::compiled::CompiledReorderTracksOperation { head_filter, .. },
            ) => (None, head_filter.as_ref(), true),
            _ => return false,
        };
        filter.is_some_and(|filter| {
            filter_references_language(filter)
                && facts.iter().any(|stream| {
                    target.is_none_or(|target| stream.kind == target)
                        && (!excludes_attachments || stream.kind != TrackTarget::Attachment)
                        && stream.language == SnapshotFact::Missing
                })
        })
    })
}

fn filter_references_language(filter: &TrackFilter) -> bool {
    match filter {
        TrackFilter::LanguageIn(voom_policy::compiled::LanguageInTrackFilter { .. }) => true,
        TrackFilter::Not(voom_policy::compiled::NotTrackFilter { inner }) => {
            filter_references_language(inner)
        }
        TrackFilter::And(voom_policy::compiled::AndTrackFilter { filters })
        | TrackFilter::Or(voom_policy::compiled::OrTrackFilter { filters }) => {
            filters.iter().any(filter_references_language)
        }
        TrackFilter::CodecIn(voom_policy::compiled::CodecInTrackFilter { .. })
        | TrackFilter::Channels(voom_policy::compiled::ChannelsTrackFilter { .. })
        | TrackFilter::Commentary(voom_policy::compiled::CommentaryTrackFilter {})
        | TrackFilter::Forced(voom_policy::compiled::ForcedTrackFilter {})
        | TrackFilter::Default(voom_policy::compiled::DefaultTrackFilter {})
        | TrackFilter::Font(voom_policy::compiled::FontTrackFilter {})
        | TrackFilter::TitleContains(voom_policy::compiled::TitleContainsTrackFilter { .. })
        | TrackFilter::TitleMatches(voom_policy::compiled::TitleMatchesTrackFilter { .. }) => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemuxGroupShape {
    NoOp,
    ContainerChange { current: String },
    TrackSelectionChange,
    InsufficientFacts(String),
    UnsupportedShape(String),
    EmptyFilterSelection(String),
    AmbiguousFilterSelection(String),
}

#[derive(Debug, Clone)]
struct RemuxResolution {
    payload: RemuxOperationPayload,
    track_selection_changed: bool,
    best_evaluated_untagged_language: bool,
}

struct RemuxDefaultsResolution {
    actions: Vec<RemuxDefaultAction>,
    evaluated_untagged_language: bool,
}

struct ResolvedBestDefault {
    selected_snapshot_stream_id: String,
    evaluated_untagged_language: bool,
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
        TrackFilter::TitleMatches(voom_policy::compiled::TitleMatchesTrackFilter { .. }) => true,
        TrackFilter::Not(voom_policy::compiled::NotTrackFilter { inner }) => {
            filter_has_unsupported_shape(inner)
        }
        TrackFilter::And(voom_policy::compiled::AndTrackFilter { filters })
        | TrackFilter::Or(voom_policy::compiled::OrTrackFilter { filters }) => {
            filters.iter().any(filter_has_unsupported_shape)
        }
        TrackFilter::LanguageIn(voom_policy::compiled::LanguageInTrackFilter { .. })
        | TrackFilter::CodecIn(voom_policy::compiled::CodecInTrackFilter { .. })
        | TrackFilter::Channels(voom_policy::compiled::ChannelsTrackFilter { .. })
        | TrackFilter::Commentary(voom_policy::compiled::CommentaryTrackFilter {})
        | TrackFilter::Forced(voom_policy::compiled::ForcedTrackFilter {})
        | TrackFilter::Default(voom_policy::compiled::DefaultTrackFilter {})
        | TrackFilter::Font(voom_policy::compiled::FontTrackFilter {})
        | TrackFilter::TitleContains(voom_policy::compiled::TitleContainsTrackFilter { .. }) => {
            false
        }
    }
}

fn remux_payload(
    snapshot: &MediaSnapshotInput,
    operations: &[&CompiledOperation],
) -> serde_json::Value {
    base_remux_payload(snapshot, operations).into_value()
}

fn base_remux_payload(
    snapshot: &MediaSnapshotInput,
    operations: &[&CompiledOperation],
) -> RemuxOperationPayload {
    let container = operations
        .iter()
        .find_map(|operation| match operation {
            CompiledOperation::SetContainer(
                voom_policy::compiled::CompiledSetContainerOperation { container },
            ) => Some(container.as_str()),
            _ => None,
        })
        .unwrap_or("mkv");
    let track_actions = operations
        .iter()
        .filter_map(|operation| match operation {
            CompiledOperation::KeepTracks(voom_policy::compiled::CompiledKeepTracksOperation {
                target,
                filter,
            }) => Some(track_action_payload(
                RemuxTrackActionKind::KeepTracks,
                *target,
                filter.clone(),
            )),
            CompiledOperation::RemoveTracks(
                voom_policy::compiled::CompiledRemoveTracksOperation { target, filter },
            ) => Some(track_action_payload(
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
            CompiledOperation::ReorderTracks(
                voom_policy::compiled::CompiledReorderTracksOperation { targets, .. },
            ) => Some(targets),
            _ => None,
        })
        .collect::<Vec<_>>();
    let has_set_container = operations.iter().any(|operation| {
        matches!(
            operation,
            CompiledOperation::SetContainer(
                voom_policy::compiled::CompiledSetContainerOperation { .. }
            )
        )
    });
    let track_order = match reorder_operations.as_slice() {
        [targets] => targets
            .iter()
            .map(|target| remux_track_group(*target))
            .collect::<Vec<_>>(),
        [] if has_set_container => payload::default_track_order(),
        _ => Vec::new(),
    };
    let defaults = operations
        .iter()
        .filter_map(|operation| match operation {
            CompiledOperation::SetDefaults(
                voom_policy::compiled::CompiledSetDefaultsOperation {
                    target, strategy, ..
                },
            ) => Some(RemuxDefaultAction {
                target: *target,
                strategy: *strategy,
                selected_snapshot_stream_id: None,
            }),
            _ => None,
        })
        .collect::<Vec<_>>();

    RemuxOperationPayload {
        container: container.to_owned(),
        source_media_snapshot_id: snapshot.existing_media_snapshot_id.map(|id| id.0),
        track_actions,
        track_order,
        head_snapshot_stream_id: None,
        defaults,
    }
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
    target_container: &str,
    track_selection_changed: bool,
) -> RemuxGroupShape {
    let Some(current_container) = snapshot.container.as_deref() else {
        return RemuxGroupShape::InsufficientFacts("snapshot container is unknown".to_owned());
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

fn resolve_remux_operations(
    snapshot: &MediaSnapshotInput,
    operations: &[&CompiledOperation],
    preferred_languages: &[String],
) -> Result<RemuxResolution, RemuxPlanningBlock> {
    let mut payload = base_remux_payload(snapshot, operations);
    let has_track_operation = operations.iter().any(|operation| {
        !matches!(
            operation,
            CompiledOperation::SetContainer(
                voom_policy::compiled::CompiledSetContainerOperation { .. }
            )
        )
    });
    let has_stream_facts = has_remux_stream_fact_shape(snapshot);
    if !has_track_operation && !has_stream_facts {
        if video_stream_count(snapshot) == Some(0) {
            return Err(RemuxPlanningBlock::UnsupportedMediaShape);
        }
        return Ok(RemuxResolution {
            payload,
            track_selection_changed: false,
            best_evaluated_untagged_language: false,
        });
    }

    let facts = stream_facts(snapshot)?;
    if !facts.iter().any(|stream| stream.kind == TrackTarget::Video) {
        return Err(RemuxPlanningBlock::UnsupportedMediaShape);
    }
    if !has_track_operation {
        return Ok(RemuxResolution {
            payload,
            track_selection_changed: false,
            best_evaluated_untagged_language: false,
        });
    }

    let keep_ids = resolve_track_keep_ids(&facts, &payload.track_actions)?;
    let mut changed = facts
        .iter()
        .any(|stream| !keep_ids.contains(&stream.snapshot_stream_id));
    let defaults = resolve_default_actions(operations, &facts, &keep_ids, preferred_languages)?;
    payload.defaults = defaults.actions;
    changed |= defaults_change(&payload.defaults, &facts, &keep_ids)?;
    let (track_order, head_snapshot_stream_id, order_changed) =
        resolve_track_order(operations, &facts, &keep_ids)?;
    let has_reorder = operations.iter().any(|operation| {
        matches!(
            operation,
            CompiledOperation::ReorderTracks(
                voom_policy::compiled::CompiledReorderTracksOperation { .. }
            )
        )
    });
    if has_reorder {
        payload.track_order = track_order;
    }
    payload.head_snapshot_stream_id = head_snapshot_stream_id;
    changed |= order_changed;

    Ok(RemuxResolution {
        payload,
        track_selection_changed: changed,
        best_evaluated_untagged_language: defaults.evaluated_untagged_language,
    })
}

fn resolve_default_actions(
    operations: &[&CompiledOperation],
    facts: &[SnapshotStreamFact],
    keep_ids: &BTreeSet<String>,
    preferred_languages: &[String],
) -> Result<RemuxDefaultsResolution, RemuxPlanningBlock> {
    let mut defaults = Vec::new();
    let mut evaluated_untagged_language = false;
    for operation in effective_default_operations(operations) {
        let CompiledOperation::SetDefaults(voom_policy::compiled::CompiledSetDefaultsOperation {
            target,
            strategy,
            filter,
        }) = operation
        else {
            continue;
        };
        match (filter, strategy) {
            (Some(filter), _) => {
                let selected = resolve_filter_matches(
                    retained_streams(facts, keep_ids, Some(*target)),
                    filter,
                    RemuxFilterOperation::Defaults(*target),
                )?;
                defaults.extend(selected.into_iter().map(|selected_snapshot_stream_id| {
                    RemuxDefaultAction {
                        target: *target,
                        strategy: *strategy,
                        selected_snapshot_stream_id: Some(selected_snapshot_stream_id),
                    }
                }));
            }
            (None, DefaultStrategy::Best) => {
                let Some(resolved) = resolve_best_default(
                    retained_streams(facts, keep_ids, Some(*target)),
                    preferred_languages,
                )?
                else {
                    continue;
                };
                evaluated_untagged_language |= resolved.evaluated_untagged_language;
                defaults.push(RemuxDefaultAction {
                    target: *target,
                    strategy: *strategy,
                    selected_snapshot_stream_id: Some(resolved.selected_snapshot_stream_id),
                });
            }
            (None, _) => defaults.push(RemuxDefaultAction {
                target: *target,
                strategy: *strategy,
                selected_snapshot_stream_id: None,
            }),
        }
    }
    Ok(RemuxDefaultsResolution {
        actions: defaults,
        evaluated_untagged_language,
    })
}

fn effective_default_operations<'a>(
    operations: &'a [&'a CompiledOperation],
) -> Vec<&'a CompiledOperation> {
    let explicit_targets = operations
        .iter()
        .filter_map(|operation| {
            let CompiledOperation::SetDefaults(
                voom_policy::compiled::CompiledSetDefaultsOperation {
                    target,
                    filter: Some(_),
                    ..
                },
            ) = operation
            else {
                return None;
            };
            Some(*target)
        })
        .collect::<Vec<_>>();
    let mut effective: Vec<&CompiledOperation> = Vec::new();
    for operation in operations {
        let CompiledOperation::SetDefaults(voom_policy::compiled::CompiledSetDefaultsOperation {
            target,
            filter,
            ..
        }) = operation
        else {
            continue;
        };
        if filter.is_none() && explicit_targets.contains(target) {
            continue;
        }
        if filter.is_some() {
            effective.push(*operation);
            continue;
        }
        effective.retain(|candidate| {
            let CompiledOperation::SetDefaults(
                voom_policy::compiled::CompiledSetDefaultsOperation {
                    target: candidate_target,
                    ..
                },
            ) = candidate
            else {
                return true;
            };
            candidate_target != target
        });
        effective.push(*operation);
    }
    effective
}

fn resolve_best_default(
    streams: Vec<&SnapshotStreamFact>,
    preferred_languages: &[String],
) -> Result<Option<ResolvedBestDefault>, RemuxPlanningBlock> {
    let Some(first) = streams.first() else {
        return Ok(None);
    };
    if preferred_languages.is_empty() {
        return Ok(Some(ResolvedBestDefault {
            selected_snapshot_stream_id: first.snapshot_stream_id.clone(),
            evaluated_untagged_language: false,
        }));
    }

    let mut selected = *first;
    let mut selected_rank = usize::MAX;
    let mut evaluated_untagged_language = false;
    for stream in streams {
        let language = match &stream.language {
            SnapshotFact::Value(language) => language.as_str(),
            SnapshotFact::Missing => {
                evaluated_untagged_language = true;
                "und"
            }
            SnapshotFact::Malformed => {
                return Err(RemuxPlanningBlock::InsufficientSnapshotFacts);
            }
        };
        let rank = preferred_languages
            .iter()
            .position(|preferred| preferred == language)
            .unwrap_or(preferred_languages.len());
        if rank < selected_rank {
            selected = stream;
            selected_rank = rank;
        }
    }
    Ok(Some(ResolvedBestDefault {
        selected_snapshot_stream_id: selected.snapshot_stream_id.clone(),
        evaluated_untagged_language,
    }))
}

fn resolve_track_order(
    operations: &[&CompiledOperation],
    facts: &[SnapshotStreamFact],
    keep_ids: &BTreeSet<String>,
) -> Result<(Vec<voom_core::RemuxTrackGroup>, Option<String>, bool), RemuxPlanningBlock> {
    let reorders = operations
        .iter()
        .filter_map(|operation| {
            let CompiledOperation::ReorderTracks(
                voom_policy::compiled::CompiledReorderTracksOperation {
                    targets,
                    head_filter,
                },
            ) = operation
            else {
                return None;
            };
            Some((targets, head_filter))
        })
        .collect::<Vec<_>>();
    let [] = reorders.as_slice() else {
        let [(targets, head_filter)] = reorders.as_slice() else {
            return Err(RemuxPlanningBlock::UnsupportedMediaShape);
        };
        if duplicate_track_targets(targets) || targets.is_empty() && head_filter.is_none() {
            return Err(RemuxPlanningBlock::UnsupportedMediaShape);
        }
        let head = head_filter
            .as_ref()
            .map(|filter| {
                resolve_unique_filter(
                    retained_streams(facts, keep_ids, None),
                    filter,
                    RemuxFilterOperation::OrderTracks,
                )
            })
            .transpose()?;
        let changed = desired_order_changes(facts, keep_ids, targets, head.as_deref());
        let order = targets
            .iter()
            .copied()
            .map(remux_track_group)
            .collect::<Vec<_>>();
        return Ok((order, head, changed));
    };
    Ok((Vec::new(), None, false))
}

fn retained_streams<'a>(
    facts: &'a [SnapshotStreamFact],
    keep_ids: &BTreeSet<String>,
    target: Option<TrackTarget>,
) -> Vec<&'a SnapshotStreamFact> {
    let mut streams = facts
        .iter()
        .filter(|stream| keep_ids.contains(&stream.snapshot_stream_id))
        .filter(|stream| target.is_none_or(|target| stream.kind == target))
        .filter(|stream| target.is_some() || stream.kind != TrackTarget::Attachment)
        .collect::<Vec<_>>();
    streams.sort_by_key(|stream| stream.provider_stream_index);
    streams
}

fn resolve_unique_filter(
    streams: Vec<&SnapshotStreamFact>,
    filter: &TrackFilter,
    operation: RemuxFilterOperation,
) -> Result<String, RemuxPlanningBlock> {
    let matches = resolve_filter_matches(streams, filter, operation)?;
    match matches.as_slice() {
        [snapshot_stream_id] => Ok(snapshot_stream_id.clone()),
        _ => Err(RemuxPlanningBlock::AmbiguousTrackFilterSelection {
            operation,
            match_count: matches.len(),
        }),
    }
}

fn resolve_filter_matches(
    streams: Vec<&SnapshotStreamFact>,
    filter: &TrackFilter,
    operation: RemuxFilterOperation,
) -> Result<Vec<String>, RemuxPlanningBlock> {
    let mut matches = Vec::new();
    for stream in streams {
        if evaluate_filter(filter, stream)? {
            matches.push(stream.snapshot_stream_id.clone());
        }
    }
    if matches.is_empty() {
        Err(RemuxPlanningBlock::EmptyTrackFilterSelection { operation })
    } else {
        Ok(matches)
    }
}

fn defaults_change(
    defaults: &[RemuxDefaultAction],
    facts: &[SnapshotStreamFact],
    keep_ids: &BTreeSet<String>,
) -> Result<bool, RemuxPlanningBlock> {
    let mut changed = false;
    let mut handled_targets = Vec::new();
    for action in defaults {
        if handled_targets.contains(&action.target) {
            continue;
        }
        handled_targets.push(action.target);
        let selected_ids = defaults
            .iter()
            .filter(|candidate| candidate.target == action.target)
            .filter_map(|candidate| candidate.selected_snapshot_stream_id.as_deref())
            .collect::<Vec<_>>();
        if !selected_ids.is_empty() {
            for stream in facts.iter().filter(|stream| {
                stream.kind == action.target && keep_ids.contains(&stream.snapshot_stream_id)
            }) {
                changed |= required_default(stream)?
                    != selected_ids.contains(&stream.snapshot_stream_id.as_str());
            }
            continue;
        }
        let streams = retained_streams(facts, keep_ids, Some(action.target));
        match action.strategy {
            DefaultStrategy::First => {
                let Some(first) = streams.first() else {
                    return Err(RemuxPlanningBlock::InsufficientSnapshotFacts);
                };
                let first_id = first.snapshot_stream_id.clone();
                for stream in streams {
                    changed |= required_default(stream)? != (stream.snapshot_stream_id == first_id);
                }
            }
            DefaultStrategy::None => {
                for stream in streams {
                    changed |= required_default(stream)?;
                }
            }
            DefaultStrategy::Preserve => {}
            DefaultStrategy::Best => return Err(RemuxPlanningBlock::UnsupportedMediaShape),
        }
    }
    Ok(changed)
}

fn required_default(stream: &SnapshotStreamFact) -> Result<bool, RemuxPlanningBlock> {
    match stream.is_default {
        SnapshotFact::Value(value) => Ok(value),
        SnapshotFact::Missing | SnapshotFact::Malformed => {
            Err(RemuxPlanningBlock::InsufficientSnapshotFacts)
        }
    }
}

fn desired_order_changes(
    facts: &[SnapshotStreamFact],
    keep_ids: &BTreeSet<String>,
    targets: &[TrackTarget],
    head_id: Option<&str>,
) -> bool {
    let source = retained_streams(facts, keep_ids, None);
    let mut desired = Vec::with_capacity(source.len());
    let mut used = HashSet::new();
    if let Some(head_id) = head_id {
        for stream in &source {
            if stream.snapshot_stream_id == head_id
                && used.insert(stream.snapshot_stream_id.as_str())
            {
                desired.push(*stream);
            }
        }
    }
    for target in targets {
        for stream in &source {
            if stream.kind == *target && used.insert(stream.snapshot_stream_id.as_str()) {
                desired.push(*stream);
            }
        }
    }
    for stream in &source {
        if used.insert(stream.snapshot_stream_id.as_str()) {
            desired.push(*stream);
        }
    }
    source != desired
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
            "snapshot stream facts are insufficient for remux planning".to_owned(),
        ),
        RemuxPlanningBlock::UnsupportedMediaShape => RemuxGroupShape::UnsupportedShape(
            "media shape is not supported by remux planning".to_owned(),
        ),
        RemuxPlanningBlock::EmptyTrackFilterSelection { operation } => {
            RemuxGroupShape::EmptyFilterSelection(filter_selection_message(operation, 0))
        }
        RemuxPlanningBlock::AmbiguousTrackFilterSelection {
            operation,
            match_count,
        } => RemuxGroupShape::AmbiguousFilterSelection(filter_selection_message(
            operation,
            match_count,
        )),
        RemuxPlanningBlock::ConflictingExplicitDefaults { target } => {
            RemuxGroupShape::UnsupportedShape(format!(
                "multiple explicit defaults operations target {}; keep exactly one `defaults {} \
                 where` operation",
                track_target_label(target),
                track_target_label(target)
            ))
        }
        RemuxPlanningBlock::ConflictingBestDefaultStrategies { target } => {
            RemuxGroupShape::UnsupportedShape(format!(
                "multiple defaults strategy operations target {} and include best; keep exactly \
                 one `defaults {}` strategy or one `defaults {} where` operation",
                track_target_label(target),
                track_target_label(target),
                track_target_label(target)
            ))
        }
    }
}

fn filter_selection_message(operation: RemuxFilterOperation, match_count: usize) -> String {
    match operation {
        RemuxFilterOperation::Defaults(target) => format!(
            "defaults {} filter matched {match_count} retained streams; update it to select \
             exactly one kept {} stream",
            track_target_label(target),
            track_target_label(target)
        ),
        RemuxFilterOperation::OrderTracks => format!(
            "order tracks filter matched {match_count} retained streams; update it to select \
             exactly one kept ordinary stream"
        ),
    }
}

fn track_target_label(target: TrackTarget) -> &'static str {
    match target {
        TrackTarget::Video => "video",
        TrackTarget::Audio => "audio",
        TrackTarget::Subtitle => "subtitle",
        TrackTarget::Attachment => "attachment",
    }
}
