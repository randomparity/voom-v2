use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;

use serde_json::Value;
use tokio::process::Command;
use tokio::time::{Duration, timeout};
use voom_core::is_font_attachment_mime_type;
use voom_worker_protocol::{RemuxRequest, RemuxSelection, RemuxStreamRef, RemuxTrackGroup};

use crate::preflight::{MkvmergeConfig, MkvtoolnixError};

pub const DEFAULT_PROCESS_TIMEOUT: Duration = Duration::from_hours(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct MkvmergeTrackFingerprint(String);

impl MkvmergeTrackFingerprint {
    fn from_identify(track: &Value) -> Self {
        let mut fields = Vec::new();
        collect_fingerprint_fields(track, &mut fields);
        fields.sort_unstable();
        Self(fields.join("\n"))
    }

    fn synthetic(kind: MkvmergeTrackKind) -> Self {
        Self(format!("type={kind:?}"))
    }

    fn from_attachment(attachment: &Value) -> Result<Self, MkvtoolnixError> {
        let file_name = required_attachment_string(attachment, "file_name")?;
        let content_type = required_attachment_string(attachment, "content_type")?;
        let mime_identity = if is_font_attachment_mime_type(content_type) {
            "class:font".to_owned()
        } else {
            format!("exact:{content_type}")
        };
        let size = attachment
            .get("size")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                MkvtoolnixError::IdentifyFailed("identify attachment missing size".to_owned())
            })?;
        Ok(Self(format!(
            "/file_name={file_name:?}\n/mime_identity={mime_identity:?}\n/size={size}"
        )))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MkvmergeTrack {
    pub(crate) id: u64,
    pub(crate) kind: MkvmergeTrackKind,
    pub(crate) default: bool,
    pub(crate) commentary: Option<bool>,
    pub(crate) fingerprint: MkvmergeTrackFingerprint,
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
                            commentary: None,
                            fingerprint: MkvmergeTrackFingerprint::synthetic(
                                MkvmergeTrackKind::Video,
                            ),
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

    pub(crate) fn track_for_provider_index(&self, provider_index: u32) -> Option<&MkvmergeTrack> {
        self.tracks_by_provider_index.get(&provider_index)
    }

    pub(crate) fn track_count(&self) -> usize {
        self.tracks_by_provider_index.len()
    }

    pub(crate) fn provider_indexes_for_group(&self, group: RemuxTrackGroup) -> Vec<u32> {
        self.tracks_by_provider_index
            .iter()
            .filter_map(|(provider_index, track)| {
                track.kind.matches_group(group).then_some(*provider_index)
            })
            .collect()
    }

    pub(crate) fn provider_indexes_for_kind(&self, kind: MkvmergeTrackKind) -> Vec<u32> {
        self.tracks_by_provider_index
            .iter()
            .filter_map(|(provider_index, track)| (track.kind == kind).then_some(*provider_index))
            .collect()
    }

    pub(crate) fn provider_indexes_matching_identity(
        &self,
        kind: MkvmergeTrackKind,
        fingerprint: &MkvmergeTrackFingerprint,
    ) -> Vec<u32> {
        self.tracks_by_provider_index
            .iter()
            .filter_map(|(provider_index, track)| {
                (track.kind == kind && &track.fingerprint == fingerprint).then_some(*provider_index)
            })
            .collect()
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
        let identify_type = track.get("type").and_then(Value::as_str);
        if identify_type.is_some_and(|value| matches!(value, "attachment" | "attachments")) {
            return Err(MkvtoolnixError::IdentifyFailed(
                "identify attachments must use the top-level attachments array".to_owned(),
            ));
        }
        let kind = identify_type.map_or(MkvmergeTrackKind::Other, MkvmergeTrackKind::from_identify);
        let default = track
            .pointer("/properties/default_track")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let commentary = track
            .pointer("/properties/flag_commentary")
            .and_then(Value::as_bool);
        mapped.insert(
            u32::try_from(provider_index)
                .map_err(|err| MkvtoolnixError::IdentifyFailed(err.to_string()))?,
            MkvmergeTrack {
                id,
                kind,
                default,
                commentary,
                fingerprint: MkvmergeTrackFingerprint::from_identify(track),
            },
        );
    }
    let attachments = optional_identify_attachments(identify)?;
    for (offset, attachment) in attachments.iter().enumerate() {
        let provider_index = tracks
            .len()
            .checked_add(offset)
            .and_then(|index| u32::try_from(index).ok())
            .ok_or_else(|| {
                MkvtoolnixError::IdentifyFailed(
                    "identify attachment provider index exceeds u32".to_owned(),
                )
            })?;
        let id = attachment
            .get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                MkvtoolnixError::IdentifyFailed("identify attachment missing id".to_owned())
            })?;
        mapped.insert(
            provider_index,
            MkvmergeTrack {
                id,
                kind: MkvmergeTrackKind::Attachment,
                default: false,
                commentary: None,
                fingerprint: MkvmergeTrackFingerprint::from_attachment(attachment)?,
            },
        );
    }
    Ok(MkvmergeTrackMapping {
        tracks_by_provider_index: mapped,
    })
}

fn optional_identify_attachments(identify: &Value) -> Result<&[Value], MkvtoolnixError> {
    let Some(attachments) = identify.get("attachments") else {
        return Ok(&[]);
    };
    attachments.as_array().map(Vec::as_slice).ok_or_else(|| {
        MkvtoolnixError::IdentifyFailed("identify JSON attachments must be an array".to_owned())
    })
}

fn required_attachment_string<'a>(
    attachment: &'a Value,
    field: &str,
) -> Result<&'a str, MkvtoolnixError> {
    attachment
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            MkvtoolnixError::IdentifyFailed(format!("identify attachment missing {field}"))
        })
}

const TRACK_FINGERPRINT_FIELDS: &[&str] = &["type"];

const TRACK_PROPERTY_FINGERPRINT_FIELDS: &[&str] =
    &["flag_commentary", "forced_track", "language", "track_name"];

fn collect_fingerprint_fields(track: &Value, fields: &mut Vec<String>) {
    for key in TRACK_FINGERPRINT_FIELDS {
        if let Some(value) = track.get(*key) {
            fields.push(format!("/{key}={value}"));
        }
    }
    if let Some(properties) = track.get("properties").and_then(Value::as_object) {
        for key in TRACK_PROPERTY_FINGERPRINT_FIELDS {
            if let Some(value) = properties.get(*key) {
                fields.push(format!("/properties/{key}={value}"));
            }
        }
    }
}

pub fn build_mkvmerge_args(
    request: &RemuxRequest,
    mapping: &MkvmergeTrackMapping,
) -> Result<Vec<String>, MkvtoolnixError> {
    validate_attachment_selection_boundaries(&request.selection, mapping)?;
    let keep = selected_tracks(&request.selection.keep_streams, mapping)?;
    let mut args = vec![
        "--output".to_owned(),
        request.output.path.clone(),
        "--no-global-tags".to_owned(),
    ];
    extend_group_selection(&mut args, "--video-tracks", &keep, MkvmergeTrackKind::Video);
    extend_optional_group_selection(
        &mut args,
        "--audio-tracks",
        "--no-audio",
        &keep,
        MkvmergeTrackKind::Audio,
    );
    extend_optional_group_selection(
        &mut args,
        "--subtitle-tracks",
        "--no-subtitles",
        &keep,
        MkvmergeTrackKind::Subtitle,
    );
    extend_optional_group_selection(
        &mut args,
        "--attachments",
        "--no-attachments",
        &keep,
        MkvmergeTrackKind::Attachment,
    );
    extend_default_flags(&mut args, &request.selection, mapping)?;
    extend_forced_flags(&mut args, &request.selection, mapping)?;
    if let Some(track_order) = track_order(&request.selection, mapping)? {
        args.push("--track-order".to_owned());
        args.push(track_order);
    }
    args.push(request.input.path.clone());
    Ok(args)
}

fn validate_attachment_selection_boundaries(
    selection: &RemuxSelection,
    mapping: &MkvmergeTrackMapping,
) -> Result<(), MkvtoolnixError> {
    if selection.track_order.contains(&RemuxTrackGroup::Attachment) {
        return Err(MkvtoolnixError::ConfigInvalid(
            "track_order cannot contain attachment".to_owned(),
        ));
    }
    let selection_fields = [
        ("default_streams", selection.default_streams.as_slice()),
        (
            "clear_default_streams",
            selection.clear_default_streams.as_slice(),
        ),
        ("head_streams", selection.head_streams.as_slice()),
        ("forced_streams", selection.forced_streams.as_slice()),
        (
            "clear_forced_streams",
            selection.clear_forced_streams.as_slice(),
        ),
    ];
    for (field, streams) in selection_fields {
        for stream in streams {
            let is_attachment = mapping
                .track_for_provider_index(stream.provider_stream_index)
                .is_some_and(|track| track.kind == MkvmergeTrackKind::Attachment);
            if is_attachment {
                return Err(MkvtoolnixError::ConfigInvalid(format!(
                    "{field} cannot contain attachments"
                )));
            }
        }
    }
    Ok(())
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
    let output = timeout(config.timeout, command_output(&mut command))
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
    let container = identify_container_type(&identify);
    if !container.eq_ignore_ascii_case("mkv") && !container.eq_ignore_ascii_case("matroska") {
        return Err(MkvtoolnixError::OutputFactsMismatch(format!(
            "output container is not mkv: {container}"
        )));
    }
    let mapping = track_mapping_from_identify(&identify)?;
    Ok(OutputProbe { mapping })
}

fn identify_container_type(identify: &Value) -> &str {
    identify
        .pointer("/container/type")
        .and_then(Value::as_str)
        .or_else(|| {
            identify
                .pointer("/container/properties/container_type")
                .and_then(Value::as_str)
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputProbe {
    pub mapping: MkvmergeTrackMapping,
}

fn selected_tracks<'a>(
    refs: &[RemuxStreamRef],
    mapping: &'a MkvmergeTrackMapping,
) -> Result<Vec<&'a MkvmergeTrack>, MkvtoolnixError> {
    let mut refs = refs.iter().collect::<Vec<_>>();
    refs.sort_by_key(|stream| stream.provider_stream_index);
    refs.into_iter()
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
    keep: &[&MkvmergeTrack],
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

fn extend_optional_group_selection(
    args: &mut Vec<String>,
    option: &str,
    none_option: &str,
    keep: &[&MkvmergeTrack],
    kind: MkvmergeTrackKind,
) {
    let ids = keep
        .iter()
        .filter(|track| track.kind == kind)
        .map(|track| track.id.to_string())
        .collect::<Vec<_>>();
    if ids.is_empty() {
        args.push(none_option.to_owned());
    } else {
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

fn extend_forced_flags(
    args: &mut Vec<String>,
    selection: &RemuxSelection,
    mapping: &MkvmergeTrackMapping,
) -> Result<(), MkvtoolnixError> {
    let mut seen = BTreeSet::new();
    for stream in &selection.forced_streams {
        let id = mapping
            .mkvmerge_track_id_for_provider_index(stream.provider_stream_index)
            .ok_or_else(|| {
                MkvtoolnixError::ConfigInvalid(format!(
                    "missing mkvmerge track id for provider stream index {}",
                    stream.provider_stream_index
                ))
            })?;
        seen.insert(id);
        args.push("--forced-track-flag".to_owned());
        args.push(format!("{id}:1"));
    }
    for stream in &selection.clear_forced_streams {
        let id = mapping
            .mkvmerge_track_id_for_provider_index(stream.provider_stream_index)
            .ok_or_else(|| {
                MkvtoolnixError::ConfigInvalid(format!(
                    "missing mkvmerge track id for provider stream index {}",
                    stream.provider_stream_index
                ))
            })?;
        if !seen.contains(&id) {
            args.push("--forced-track-flag".to_owned());
            args.push(format!("{id}:0"));
        }
    }
    Ok(())
}

pub(crate) fn ordered_keep_streams<'a>(
    selection: &'a RemuxSelection,
    mapping: &MkvmergeTrackMapping,
) -> Result<Vec<&'a RemuxStreamRef>, MkvtoolnixError> {
    let mut source = selection.keep_streams.iter().collect::<Vec<_>>();
    source.sort_by_key(|stream| stream.provider_stream_index);
    for stream in &source {
        if mapping
            .track_for_provider_index(stream.provider_stream_index)
            .is_none()
        {
            return Err(MkvtoolnixError::ConfigInvalid(format!(
                "missing mkvmerge track id for provider stream index {}",
                stream.provider_stream_index
            )));
        }
    }

    validate_head_refs(selection)?;
    let mut heads = selection.head_streams.iter().collect::<Vec<_>>();
    heads.sort_by_key(|stream| stream.provider_stream_index);
    let mut ordered = Vec::with_capacity(source.len());
    let mut used = BTreeSet::new();
    for head in heads {
        push_matching_stream(&source, &mut ordered, &mut used, |stream| {
            stream.snapshot_stream_id == head.snapshot_stream_id
                && stream.provider_stream_index == head.provider_stream_index
        });
    }
    for group in &selection.track_order {
        push_matching_stream(&source, &mut ordered, &mut used, |stream| {
            mapping
                .track_for_provider_index(stream.provider_stream_index)
                .is_some_and(|track| track.kind.matches_group(*group))
        });
    }
    push_matching_stream(&source, &mut ordered, &mut used, |_| true);
    Ok(ordered)
}

fn validate_head_refs(selection: &RemuxSelection) -> Result<(), MkvtoolnixError> {
    let mut snapshot_ids = BTreeSet::new();
    let mut provider_indexes = BTreeSet::new();
    for head in &selection.head_streams {
        if !snapshot_ids.insert(head.snapshot_stream_id.as_str())
            || !provider_indexes.insert(head.provider_stream_index)
        {
            return Err(MkvtoolnixError::ConfigInvalid(
                "duplicate stream reference in head_streams".to_owned(),
            ));
        }
        if !selection.keep_streams.iter().any(|kept| kept == head) {
            return Err(MkvtoolnixError::ConfigInvalid(
                "head_streams must be a subset of keep_streams".to_owned(),
            ));
        }
    }
    Ok(())
}

fn push_matching_stream<'a>(
    source: &[&'a RemuxStreamRef],
    ordered: &mut Vec<&'a RemuxStreamRef>,
    used: &mut BTreeSet<u32>,
    predicate: impl Fn(&RemuxStreamRef) -> bool,
) {
    for stream in source {
        if predicate(stream) && used.insert(stream.provider_stream_index) {
            ordered.push(*stream);
        }
    }
}

fn track_order(
    selection: &RemuxSelection,
    mapping: &MkvmergeTrackMapping,
) -> Result<Option<String>, MkvtoolnixError> {
    if selection.track_order.is_empty() && selection.head_streams.is_empty() {
        return Ok(None);
    }
    let mut ordered = Vec::new();
    for stream in ordered_keep_streams(selection, mapping)? {
        let track = mapping
            .track_for_provider_index(stream.provider_stream_index)
            .ok_or_else(|| {
                MkvtoolnixError::ConfigInvalid(format!(
                    "missing mkvmerge track id for provider stream index {}",
                    stream.provider_stream_index
                ))
            })?;
        if track.kind != MkvmergeTrackKind::Attachment {
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
    let output = timeout(config.timeout, command_output(&mut command))
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

async fn command_output(command: &mut Command) -> io::Result<std::process::Output> {
    let mut attempts_remaining = 3;
    loop {
        attempts_remaining -= 1;
        match command.output().await {
            Err(err) if is_text_file_busy(&err) && attempts_remaining > 0 => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            result => return result,
        }
    }
}

fn is_text_file_busy(err: &io::Error) -> bool {
    err.raw_os_error() == Some(26)
}

#[cfg(test)]
#[path = "mkvmerge_test.rs"]
mod tests;
