use std::collections::{BTreeSet, HashSet};

use serde_json::Value;
use voom_core::VoomError;
use voom_plan::planner::remux::{
    RemuxOperationPayload, RemuxPlanningBlock, SnapshotFact, SnapshotStreamFact,
    resolve_track_keep_ids, stream_facts,
};
use voom_policy::{DefaultStrategy, TrackTarget};
use voom_store::repo::media::identity::MediaSnapshot;
use voom_worker_protocol::{RemuxSelection, RemuxStreamRef};

pub fn selection_from_payload_and_snapshot(
    payload: &Value,
    snapshot: &MediaSnapshot,
) -> Result<RemuxSelection, VoomError> {
    let payload = RemuxOperationPayload::try_from_execution_value(payload)
        .map_err(|err| VoomError::Config(format!("remux operation payload is invalid: {err}")))?;
    if !voom_worker_protocol::is_supported_remux_container(&payload.container) {
        return Err(VoomError::Config(format!(
            "remux container {} is unsupported",
            payload.container
        )));
    }
    if payload.source_media_snapshot_id != Some(snapshot.id.0) {
        return Err(VoomError::Config(format!(
            "remux payload pins media snapshot {}, but execution loaded snapshot {}",
            payload.source_media_snapshot_id.unwrap_or_default(),
            snapshot.id.0
        )));
    }
    let snapshot_input = crate::media_snapshot::planning_input(1, snapshot);
    let mut facts = stream_facts(&snapshot_input).map_err(remux_block_error)?;
    facts.sort_by_key(|stream| stream.provider_stream_index);
    if !facts.iter().any(|stream| stream.kind == TrackTarget::Video) {
        return Err(VoomError::Config(
            "remux selection requires at least one video stream".to_owned(),
        ));
    }
    if payload
        .track_actions
        .iter()
        .any(|action| action.target == TrackTarget::Video)
    {
        return Err(VoomError::Config(
            "video track policy is unsupported".to_owned(),
        ));
    }
    let mut keep_ids =
        resolve_track_keep_ids(&facts, &payload.track_actions).map_err(remux_block_error)?;

    for stream in facts
        .iter()
        .filter(|stream| stream.kind == TrackTarget::Video)
    {
        keep_ids.insert(stream.snapshot_stream_id.clone());
    }

    reject_empty_audio(&facts, &keep_ids)?;

    let keep_streams = facts
        .iter()
        .filter(|stream| keep_ids.contains(&stream.snapshot_stream_id))
        .map(stream_ref)
        .collect::<Vec<_>>();
    let defaults = effective_default_actions(&payload.defaults)?;
    let (default_streams, clear_default_streams) = default_refs(&defaults, &facts, &keep_ids)?;
    let (forced_streams, clear_forced_streams) = forced_refs(&facts, &keep_ids);
    let head_streams = head_refs(
        payload.head_snapshot_stream_id.as_deref(),
        &facts,
        &keep_ids,
    )?;

    Ok(RemuxSelection {
        keep_streams,
        default_streams,
        clear_default_streams,
        track_order: payload.track_order,
        head_streams,
        forced_streams,
        clear_forced_streams,
    })
}

fn forced_refs(
    facts: &[SnapshotStreamFact],
    keep_ids: &BTreeSet<String>,
) -> (Vec<RemuxStreamRef>, Vec<RemuxStreamRef>) {
    let mut forced_streams = Vec::new();
    let mut clear_forced_streams = Vec::new();
    for stream in facts.iter().filter(|stream| {
        stream.kind != TrackTarget::Attachment && keep_ids.contains(&stream.snapshot_stream_id)
    }) {
        match stream.is_forced {
            SnapshotFact::Value(true) => forced_streams.push(stream_ref(stream)),
            SnapshotFact::Value(false) => clear_forced_streams.push(stream_ref(stream)),
            SnapshotFact::Missing | SnapshotFact::Malformed => {}
        }
    }
    (forced_streams, clear_forced_streams)
}

/// A remux must never strip a source's audio to nothing: a file with audio that
/// keeps zero audio streams is unplayable (ADR 0021, issue #158). When the source
/// has audio but the resolved keep set retains none, this is a per-file failure —
/// the terminal-failure machinery opens an issue and skips the file — rather than
/// an audio-less artifact. Scoped to audio; a subtitle-less file is a valid outcome.
fn reject_empty_audio(
    facts: &[SnapshotStreamFact],
    keep_ids: &BTreeSet<String>,
) -> Result<(), VoomError> {
    let has_audio_source = facts.iter().any(|stream| stream.kind == TrackTarget::Audio);
    let keeps_audio = facts.iter().any(|stream| {
        stream.kind == TrackTarget::Audio && keep_ids.contains(&stream.snapshot_stream_id)
    });
    if has_audio_source && !keeps_audio {
        return Err(VoomError::Config(
            "remux would leave the file with no audio; no audio track survived the track filters"
                .to_owned(),
        ));
    }
    Ok(())
}

fn default_refs(
    defaults: &[EffectiveDefaultAction<'_>],
    facts: &[SnapshotStreamFact],
    keep_ids: &BTreeSet<String>,
) -> Result<(Vec<RemuxStreamRef>, Vec<RemuxStreamRef>), VoomError> {
    let mut default_streams = Vec::new();
    let mut clear_default_streams = Vec::new();
    for action in defaults {
        match *action {
            EffectiveDefaultAction::Resolved {
                target,
                selected_id,
            } => {
                let selected = resolved_kept_stream(selected_id, Some(target), facts, keep_ids)?;
                default_streams.push(stream_ref(selected));
                clear_default_streams.extend(
                    facts
                        .iter()
                        .filter(|stream| {
                            stream.kind == target
                                && stream.snapshot_stream_id != selected_id
                                && keep_ids.contains(&stream.snapshot_stream_id)
                        })
                        .map(stream_ref),
                );
            }
            EffectiveDefaultAction::First { target } => {
                let kept_target = kept_target_streams(facts, keep_ids, target);
                let Some(first) = kept_target
                    .iter()
                    .min_by_key(|stream| stream.provider_stream_index)
                else {
                    continue;
                };
                default_streams.push(stream_ref(first));
                clear_default_streams.extend(
                    kept_target
                        .iter()
                        .filter(|stream| stream.snapshot_stream_id != first.snapshot_stream_id)
                        .map(|stream| stream_ref(stream)),
                );
            }
            EffectiveDefaultAction::None { target } => {
                let kept_target = kept_target_streams(facts, keep_ids, target);
                clear_default_streams.extend(kept_target.into_iter().map(stream_ref));
            }
            EffectiveDefaultAction::Preserve { target } => {
                let kept_target = kept_target_streams(facts, keep_ids, target);
                for stream in kept_target {
                    match stream.is_default {
                        SnapshotFact::Value(true) => default_streams.push(stream_ref(stream)),
                        SnapshotFact::Value(false) => {
                            clear_default_streams.push(stream_ref(stream));
                        }
                        SnapshotFact::Missing | SnapshotFact::Malformed => {}
                    }
                }
            }
        }
    }
    for target in [
        TrackTarget::Video,
        TrackTarget::Audio,
        TrackTarget::Subtitle,
    ] {
        if defaults.iter().any(|action| action.target() == target) {
            continue;
        }
        for stream in facts
            .iter()
            .filter(|stream| stream.kind == target && keep_ids.contains(&stream.snapshot_stream_id))
        {
            match stream.is_default {
                SnapshotFact::Value(true) => default_streams.push(stream_ref(stream)),
                SnapshotFact::Value(false) => clear_default_streams.push(stream_ref(stream)),
                SnapshotFact::Missing | SnapshotFact::Malformed => {}
            }
        }
    }
    Ok((
        dedupe_refs(default_streams),
        dedupe_refs(clear_default_streams),
    ))
}

fn kept_target_streams<'a>(
    facts: &'a [SnapshotStreamFact],
    keep_ids: &BTreeSet<String>,
    target: TrackTarget,
) -> Vec<&'a SnapshotStreamFact> {
    facts
        .iter()
        .filter(|stream| stream.kind == target && keep_ids.contains(&stream.snapshot_stream_id))
        .collect()
}

#[derive(Clone, Copy)]
enum ClassifiedDefaultAction<'a> {
    ResolvedExplicit {
        target: TrackTarget,
        selected_id: &'a str,
    },
    ResolvedBest {
        target: TrackTarget,
        selected_id: &'a str,
    },
    UnresolvedBest {
        target: TrackTarget,
    },
    First {
        target: TrackTarget,
    },
    None {
        target: TrackTarget,
    },
    Preserve {
        target: TrackTarget,
    },
}

impl<'a> ClassifiedDefaultAction<'a> {
    fn target(self) -> TrackTarget {
        match self {
            Self::ResolvedExplicit { target, .. }
            | Self::ResolvedBest { target, .. }
            | Self::UnresolvedBest { target }
            | Self::First { target }
            | Self::None { target }
            | Self::Preserve { target } => target,
        }
    }

    fn is_resolved_explicit(self) -> bool {
        match self {
            Self::ResolvedExplicit { .. } => true,
            Self::ResolvedBest { .. }
            | Self::UnresolvedBest { .. }
            | Self::First { .. }
            | Self::None { .. }
            | Self::Preserve { .. } => false,
        }
    }

    fn is_best(self) -> bool {
        match self {
            Self::ResolvedBest { .. } | Self::UnresolvedBest { .. } => true,
            Self::ResolvedExplicit { .. }
            | Self::First { .. }
            | Self::None { .. }
            | Self::Preserve { .. } => false,
        }
    }

    fn into_effective(self) -> Result<EffectiveDefaultAction<'a>, VoomError> {
        match self {
            Self::ResolvedExplicit {
                target,
                selected_id,
            }
            | Self::ResolvedBest {
                target,
                selected_id,
            } => Ok(EffectiveDefaultAction::Resolved {
                target,
                selected_id,
            }),
            Self::UnresolvedBest { .. } => Err(VoomError::Config(
                "default strategy best requires a resolved selected_snapshot_stream_id".to_owned(),
            )),
            Self::First { target } => Ok(EffectiveDefaultAction::First { target }),
            Self::None { target } => Ok(EffectiveDefaultAction::None { target }),
            Self::Preserve { target } => Ok(EffectiveDefaultAction::Preserve { target }),
        }
    }
}

#[derive(Clone, Copy)]
enum EffectiveDefaultAction<'a> {
    Resolved {
        target: TrackTarget,
        selected_id: &'a str,
    },
    First {
        target: TrackTarget,
    },
    None {
        target: TrackTarget,
    },
    Preserve {
        target: TrackTarget,
    },
}

impl EffectiveDefaultAction<'_> {
    fn target(self) -> TrackTarget {
        match self {
            Self::Resolved { target, .. }
            | Self::First { target }
            | Self::None { target }
            | Self::Preserve { target } => target,
        }
    }
}

fn effective_default_actions(
    defaults: &[voom_plan::planner::remux::RemuxDefaultAction],
) -> Result<Vec<EffectiveDefaultAction<'_>>, VoomError> {
    let classified = defaults
        .iter()
        .map(classify_default_action)
        .collect::<Result<Vec<_>, _>>()?;
    let mut explicit_targets = Vec::new();
    for action in &classified {
        if !action.is_resolved_explicit() {
            continue;
        }
        let target = action.target();
        if explicit_targets.contains(&target) {
            return Err(VoomError::Config(format!(
                "multiple explicit defaults actions target {}",
                track_target_name(target)
            )));
        }
        explicit_targets.push(target);
    }
    let effective = classified
        .into_iter()
        .filter(|action| {
            action.is_resolved_explicit() || !explicit_targets.contains(&action.target())
        })
        .collect::<Vec<_>>();
    validate_best_default_strategy_conflicts(&effective)?;
    effective
        .into_iter()
        .map(ClassifiedDefaultAction::into_effective)
        .collect()
}

fn classify_default_action(
    action: &voom_plan::planner::remux::RemuxDefaultAction,
) -> Result<ClassifiedDefaultAction<'_>, VoomError> {
    match (
        action.strategy,
        action.selected_snapshot_stream_id.as_deref(),
    ) {
        (DefaultStrategy::Preserve, Some(selected_id)) => {
            Ok(ClassifiedDefaultAction::ResolvedExplicit {
                target: action.target,
                selected_id,
            })
        }
        (DefaultStrategy::Best, Some(selected_id)) => Ok(ClassifiedDefaultAction::ResolvedBest {
            target: action.target,
            selected_id,
        }),
        (DefaultStrategy::First, None) => Ok(ClassifiedDefaultAction::First {
            target: action.target,
        }),
        (DefaultStrategy::None, None) => Ok(ClassifiedDefaultAction::None {
            target: action.target,
        }),
        (DefaultStrategy::Preserve, None) => Ok(ClassifiedDefaultAction::Preserve {
            target: action.target,
        }),
        (DefaultStrategy::Best, None) => Ok(ClassifiedDefaultAction::UnresolvedBest {
            target: action.target,
        }),
        (DefaultStrategy::First, Some(_)) => Err(VoomError::Config(
            "selected_snapshot_stream_id is invalid with default strategy first".to_owned(),
        )),
        (DefaultStrategy::None, Some(_)) => Err(VoomError::Config(
            "selected_snapshot_stream_id is invalid with default strategy none".to_owned(),
        )),
    }
}

fn validate_best_default_strategy_conflicts(
    defaults: &[ClassifiedDefaultAction<'_>],
) -> Result<(), VoomError> {
    for action in defaults {
        if !action.is_best() {
            continue;
        }
        let target = action.target();
        if defaults
            .iter()
            .filter(|candidate| candidate.target() == target)
            .count()
            > 1
        {
            return Err(VoomError::Config(format!(
                "multiple defaults strategy actions target {} and include best",
                track_target_name(target)
            )));
        }
    }
    Ok(())
}

fn head_refs(
    head_id: Option<&str>,
    facts: &[SnapshotStreamFact],
    keep_ids: &BTreeSet<String>,
) -> Result<Vec<RemuxStreamRef>, VoomError> {
    let Some(head_id) = head_id else {
        return Ok(Vec::new());
    };
    let stream = resolved_kept_stream(head_id, None, facts, keep_ids)?;
    if stream.kind == TrackTarget::Attachment {
        return Err(VoomError::Config(format!(
            "resolved head stream `{head_id}` cannot be an attachment"
        )));
    }
    Ok(vec![stream_ref(stream)])
}

fn resolved_kept_stream<'a>(
    snapshot_stream_id: &str,
    expected_target: Option<TrackTarget>,
    facts: &'a [SnapshotStreamFact],
    keep_ids: &BTreeSet<String>,
) -> Result<&'a SnapshotStreamFact, VoomError> {
    let Some(stream) = facts
        .iter()
        .find(|stream| stream.snapshot_stream_id == snapshot_stream_id)
    else {
        return Err(VoomError::Config(format!(
            "resolved stream `{snapshot_stream_id}` is missing from the pinned snapshot"
        )));
    };
    if !keep_ids.contains(snapshot_stream_id) {
        return Err(VoomError::Config(format!(
            "resolved stream `{snapshot_stream_id}` did not survive remux track actions"
        )));
    }
    if let Some(target) = expected_target
        && stream.kind != target
    {
        return Err(VoomError::Config(format!(
            "resolved stream `{snapshot_stream_id}` is not an expected {} stream",
            track_target_name(target)
        )));
    }
    Ok(stream)
}

fn track_target_name(target: TrackTarget) -> &'static str {
    match target {
        TrackTarget::Video => "video",
        TrackTarget::Audio => "audio",
        TrackTarget::Subtitle => "subtitle",
        TrackTarget::Attachment => "attachment",
    }
}

fn dedupe_refs(streams: Vec<RemuxStreamRef>) -> Vec<RemuxStreamRef> {
    let mut seen = HashSet::new();
    streams
        .into_iter()
        .filter(|stream| seen.insert(stream.snapshot_stream_id.clone()))
        .collect()
}

fn stream_ref(stream: &SnapshotStreamFact) -> RemuxStreamRef {
    RemuxStreamRef {
        snapshot_stream_id: stream.snapshot_stream_id.clone(),
        provider_stream_index: stream.provider_stream_index,
    }
}

fn remux_block_error(block: RemuxPlanningBlock) -> VoomError {
    match block {
        RemuxPlanningBlock::InsufficientSnapshotFacts => {
            VoomError::Config("remux snapshot has insufficient stream facts".to_owned())
        }
        RemuxPlanningBlock::UnsupportedMediaShape => {
            VoomError::Config("remux selector is unsupported for this media shape".to_owned())
        }
        RemuxPlanningBlock::EmptyTrackFilterSelection { .. }
        | RemuxPlanningBlock::AmbiguousTrackFilterSelection { .. }
        | RemuxPlanningBlock::ConflictingExplicitDefaults { .. } => VoomError::Config(
            "remux payload contains unresolved filter-addressed selection".to_owned(),
        ),
        RemuxPlanningBlock::ConflictingBestDefaultStrategies { .. } => {
            VoomError::Config("remux payload contains conflicting defaults strategies".to_owned())
        }
    }
}

#[cfg(test)]
#[path = "selection_test.rs"]
mod tests;
