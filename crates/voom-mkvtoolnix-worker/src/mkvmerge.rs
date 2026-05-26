use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;
use tokio::process::Command;
use tokio::time::{Duration, timeout};
use voom_worker_protocol::{RemuxRequest, RemuxSelection, RemuxStreamRef, RemuxTrackGroup};

use crate::preflight::{MkvmergeConfig, MkvtoolnixError};

pub const DEFAULT_PROCESS_TIMEOUT: Duration = Duration::from_hours(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MkvmergeTrackKind {
    Video,
    Audio,
    Subtitle,
    Attachment,
    Other,
}

impl MkvmergeTrackKind {
    fn from_identify(value: &str) -> Self {
        match value {
            "video" => Self::Video,
            "audio" => Self::Audio,
            "subtitles" | "subtitle" => Self::Subtitle,
            "attachments" | "attachment" => Self::Attachment,
            _ => Self::Other,
        }
    }

    pub(crate) fn matches_group(self, group: RemuxTrackGroup) -> bool {
        matches!(
            (self, group),
            (Self::Video, RemuxTrackGroup::Video)
                | (Self::Audio, RemuxTrackGroup::Audio)
                | (Self::Subtitle, RemuxTrackGroup::Subtitle)
                | (Self::Attachment, RemuxTrackGroup::Attachment)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MkvmergeTrack {
    pub(crate) id: u64,
    pub(crate) kind: MkvmergeTrackKind,
    pub(crate) default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MkvmergeTrackMapping {
    tracks_by_provider_index: BTreeMap<u32, MkvmergeTrack>,
}

impl MkvmergeTrackMapping {
    #[must_use]
    pub fn from_pairs(pairs: impl IntoIterator<Item = (u32, u64)>) -> Self {
        Self {
            tracks_by_provider_index: pairs
                .into_iter()
                .map(|(provider_index, id)| {
                    (
                        provider_index,
                        MkvmergeTrack {
                            id,
                            kind: MkvmergeTrackKind::Video,
                            default: false,
                        },
                    )
                })
                .collect(),
        }
    }

    #[must_use]
    pub fn mkvmerge_track_id_for_provider_index(&self, provider_index: u32) -> Option<u64> {
        self.tracks_by_provider_index
            .get(&provider_index)
            .map(|track| track.id)
    }

    pub(crate) fn track_for_provider_index(&self, provider_index: u32) -> Option<MkvmergeTrack> {
        self.tracks_by_provider_index.get(&provider_index).copied()
    }
}

pub fn track_mapping_from_identify(
    identify: &serde_json::Value,
) -> Result<MkvmergeTrackMapping, MkvtoolnixError> {
    let tracks = identify
        .get("tracks")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            MkvtoolnixError::IdentifyFailed("identify JSON missing tracks".to_owned())
        })?;
    let mut mapped = BTreeMap::new();
    for (provider_index, track) in tracks.iter().enumerate() {
        let id = track.get("id").and_then(Value::as_u64).ok_or_else(|| {
            MkvtoolnixError::IdentifyFailed("identify track missing id".to_owned())
        })?;
        let kind = track
            .get("type")
            .and_then(Value::as_str)
            .map_or(MkvmergeTrackKind::Other, MkvmergeTrackKind::from_identify);
        let default = track
            .pointer("/properties/default_track")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        mapped.insert(
            u32::try_from(provider_index)
                .map_err(|err| MkvtoolnixError::IdentifyFailed(err.to_string()))?,
            MkvmergeTrack { id, kind, default },
        );
    }
    Ok(MkvmergeTrackMapping {
        tracks_by_provider_index: mapped,
    })
}

pub fn build_mkvmerge_args(
    request: &RemuxRequest,
    mapping: &MkvmergeTrackMapping,
) -> Result<Vec<String>, MkvtoolnixError> {
    let keep = selected_tracks(&request.selection.keep_streams, mapping)?;
    let mut args = vec![
        "--output".to_owned(),
        request.output.path.clone(),
        "--no-global-tags".to_owned(),
    ];
    extend_group_selection(&mut args, "--video-tracks", &keep, MkvmergeTrackKind::Video);
    extend_group_selection(&mut args, "--audio-tracks", &keep, MkvmergeTrackKind::Audio);
    extend_group_selection(
        &mut args,
        "--subtitle-tracks",
        &keep,
        MkvmergeTrackKind::Subtitle,
    );
    extend_group_selection(
        &mut args,
        "--attachments",
        &keep,
        MkvmergeTrackKind::Attachment,
    );
    extend_default_flags(&mut args, &request.selection, mapping)?;
    if let Some(track_order) = track_order(&request.selection, mapping)? {
        args.push("--track-order".to_owned());
        args.push(track_order);
    }
    args.push(request.input.path.clone());
    Ok(args)
}

pub async fn identify_tracks(
    config: &MkvmergeConfig,
    path: &Path,
) -> Result<MkvmergeTrackMapping, MkvtoolnixError> {
    let identify = identify_json(config, path).await?;
    track_mapping_from_identify(&identify)
}

pub async fn run_mkvmerge_remux(
    config: &MkvmergeConfig,
    request: &RemuxRequest,
    mapping: &MkvmergeTrackMapping,
) -> Result<(), MkvtoolnixError> {
    let args = build_mkvmerge_args(request, mapping)?;
    let mut command = Command::new(&config.command);
    command.args(args).kill_on_drop(true);
    let output = timeout(config.timeout, command.output())
        .await
        .map_err(|_| MkvtoolnixError::MkvmergeFailed("mkvmerge timed out".to_owned()))?
        .map_err(|err| MkvtoolnixError::MkvmergeFailed(err.to_string()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(MkvtoolnixError::MkvmergeFailed(command_error(&output)))
    }
}

pub async fn identify_output(
    config: &MkvmergeConfig,
    path: &Path,
) -> Result<OutputProbe, MkvtoolnixError> {
    let identify = identify_json(config, path).await?;
    let container = identify
        .pointer("/container/properties/container_type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !container.eq_ignore_ascii_case("mkv") && !container.eq_ignore_ascii_case("matroska") {
        return Err(MkvtoolnixError::OutputFactsMismatch(format!(
            "output container is not mkv: {container}"
        )));
    }
    let mapping = track_mapping_from_identify(&identify)?;
    Ok(OutputProbe { mapping })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputProbe {
    pub mapping: MkvmergeTrackMapping,
}

fn selected_tracks(
    refs: &[RemuxStreamRef],
    mapping: &MkvmergeTrackMapping,
) -> Result<Vec<MkvmergeTrack>, MkvtoolnixError> {
    refs.iter()
        .map(|stream| {
            mapping
                .track_for_provider_index(stream.provider_stream_index)
                .ok_or_else(|| {
                    MkvtoolnixError::ConfigInvalid(format!(
                        "missing mkvmerge track id for provider stream index {}",
                        stream.provider_stream_index
                    ))
                })
        })
        .collect()
}

fn extend_group_selection(
    args: &mut Vec<String>,
    option: &str,
    keep: &[MkvmergeTrack],
    kind: MkvmergeTrackKind,
) {
    let ids = keep
        .iter()
        .filter(|track| track.kind == kind)
        .map(|track| track.id.to_string())
        .collect::<Vec<_>>();
    if !ids.is_empty() {
        args.push(option.to_owned());
        args.push(ids.join(","));
    }
}

fn extend_default_flags(
    args: &mut Vec<String>,
    selection: &RemuxSelection,
    mapping: &MkvmergeTrackMapping,
) -> Result<(), MkvtoolnixError> {
    let mut seen = BTreeSet::new();
    for stream in &selection.default_streams {
        let id = mapping
            .mkvmerge_track_id_for_provider_index(stream.provider_stream_index)
            .ok_or_else(|| {
                MkvtoolnixError::ConfigInvalid(format!(
                    "missing mkvmerge track id for provider stream index {}",
                    stream.provider_stream_index
                ))
            })?;
        seen.insert(id);
        args.push("--default-track-flag".to_owned());
        args.push(format!("{id}:1"));
    }
    for stream in &selection.clear_default_streams {
        let id = mapping
            .mkvmerge_track_id_for_provider_index(stream.provider_stream_index)
            .ok_or_else(|| {
                MkvtoolnixError::ConfigInvalid(format!(
                    "missing mkvmerge track id for provider stream index {}",
                    stream.provider_stream_index
                ))
            })?;
        if !seen.contains(&id) {
            args.push("--default-track-flag".to_owned());
            args.push(format!("{id}:0"));
        }
    }
    Ok(())
}

fn track_order(
    selection: &RemuxSelection,
    mapping: &MkvmergeTrackMapping,
) -> Result<Option<String>, MkvtoolnixError> {
    if selection.track_order.is_empty() {
        return Ok(None);
    }
    let keep = selection
        .keep_streams
        .iter()
        .map(|stream| {
            mapping
                .track_for_provider_index(stream.provider_stream_index)
                .ok_or_else(|| {
                    MkvtoolnixError::ConfigInvalid(format!(
                        "missing mkvmerge track id for provider stream index {}",
                        stream.provider_stream_index
                    ))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut ordered = Vec::new();
    let mut used = BTreeSet::new();
    for group in &selection.track_order {
        for track in &keep {
            if track.kind.matches_group(*group) && used.insert(track.id) {
                ordered.push(format!("0:{}", track.id));
            }
        }
    }
    for track in &keep {
        if used.insert(track.id) {
            ordered.push(format!("0:{}", track.id));
        }
    }
    Ok(Some(ordered.join(",")))
}

async fn identify_json(config: &MkvmergeConfig, path: &Path) -> Result<Value, MkvtoolnixError> {
    let mut command = Command::new(&config.command);
    command
        .arg("--identify")
        .arg("--identification-format")
        .arg("json")
        .arg(path)
        .kill_on_drop(true);
    let output = timeout(config.timeout, command.output())
        .await
        .map_err(|_| MkvtoolnixError::IdentifyFailed("mkvmerge identify timed out".to_owned()))?
        .map_err(|err| MkvtoolnixError::IdentifyFailed(err.to_string()))?;
    if !output.status.success() {
        return Err(MkvtoolnixError::IdentifyFailed(command_error(&output)));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|err| MkvtoolnixError::IdentifyFailed(format!("invalid identify JSON: {err}")))
}

fn command_error(output: &std::process::Output) -> String {
    format!(
        "status {}: {}{}",
        output
            .status
            .code()
            .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[cfg(test)]
#[path = "mkvmerge_test.rs"]
mod tests;
