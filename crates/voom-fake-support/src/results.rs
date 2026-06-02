use std::path::Path;

use voom_worker_protocol::{
    AudioDispositionFact, AudioObservedFacts, AudioOutputStreamFact, ExtractAudioRequest,
    ExtractAudioResult, ExtractAudioStatus, OperationKind, ProtocolError, RemuxObservedFacts,
    RemuxRequest, RemuxResult, RemuxStatus, TranscodeAudioRequest, TranscodeAudioResult,
    TranscodeAudioStatus, TranscodeVideoObservedFacts, TranscodeVideoRequest, TranscodeVideoResult,
    TranscodeVideoStatus,
};

use crate::catalog::operation_name;
use crate::validation::{
    extract_audio_protocol_payload, invalid, optional_string_array_field, remux_protocol_payload,
    string_array_field, string_field, transcode_audio_protocol_payload,
    transcode_video_protocol_payload,
};

pub fn synthetic_artifact_access_evidence(
    payload: &serde_json::Value,
) -> Result<serde_json::Value, ProtocolError> {
    let Some(plan) = payload.get("artifact_access_plan") else {
        return Ok(serde_json::json!({}));
    };
    let selected_access_mode = string_field(plan, "selected_access_mode")?;
    let advertised_access_modes = advertised_access_modes(payload, plan)?;
    if !advertised_access_modes.contains(&selected_access_mode) {
        return Err(invalid(format!(
            "artifact access mode {selected_access_mode} is not advertised"
        )));
    }

    Ok(serde_json::json!({
        "artifact_access": {
            "inputs_consumed": string_array_field(plan, "input_handles")?,
            "outputs_declared": string_array_field(plan, "output_handles")?,
            "mode": selected_access_mode,
            "validated": true,
        }
    }))
}

pub(crate) fn result_payload(
    provider: &str,
    operation: OperationKind,
    scenario: &str,
    payload: &serde_json::Value,
    fan_out_count: Option<u32>,
) -> Result<serde_json::Value, ProtocolError> {
    let mut result = serde_json::json!({
        "provider": provider,
        "operation": operation_name(operation),
        "scenario": scenario,
    });
    let object = result
        .as_object_mut()
        .ok_or_else(|| invalid("internal result payload must be object"))?;
    match provider {
        "fake-scanner" => {
            object.insert("files".to_owned(), scanner_files(payload, fan_out_count)?);
        }
        "fake-prober" => {
            object.insert("duration_ms".to_owned(), serde_json::json!(7_200_000_u64));
            object.insert(
                "codec".to_owned(),
                payload
                    .get("codec")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!("h264")),
            );
            object.insert("hash".to_owned(), serde_json::json!("sha256:fake-prober"));
        }
        "fake-transcoder" => {
            if let Some(video_result) = fake_transcoder_video_payload(provider, operation, payload)?
            {
                return Ok(video_result);
            }
            if let Some(audio_result) = fake_transcoder_audio_payload(provider, operation, payload)?
            {
                return Ok(audio_result);
            }
            return Err(invalid(format!(
                "fake-transcoder {} requires typed transcode_video, transcode_audio, \
                 or extract_audio payload",
                operation_name(operation)
            )));
        }
        "fake-remuxer" => {
            if let Some(request) = remux_protocol_payload(payload)? {
                return serde_json::to_value(fake_remux_result(&request)?)
                    .map_err(|err| invalid(format!("fake remux result encode failed: {err}")));
            }
            object.insert(
                "output_path".to_owned(),
                serde_json::json!(transform_output_path(payload, "remuxed")),
            );
            object.insert(
                "container".to_owned(),
                payload
                    .get("container")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!("mkv")),
            );
        }
        "fake-backup-store" => {
            object.insert(
                "local_backup_id".to_owned(),
                serde_json::json!("backup-local-0001"),
            );
        }
        "fake-health-checker" => {
            object.insert("status".to_owned(), serde_json::json!("verified"));
        }
        "fake-identity-provider" => {
            object.insert(
                "canonical_media_id".to_owned(),
                serde_json::json!("media:fake:movie"),
            );
        }
        "fake-external-system" => {
            object.insert("refresh_status".to_owned(), serde_json::json!("queued"));
        }
        "fake-quality-scorer" => {
            object.insert("score".to_owned(), serde_json::json!(93));
            object.insert(
                "needs_transcode".to_owned(),
                serde_json::json!(needs_transcode(payload)),
            );
        }
        "fake-issue-provider" => {
            object.insert("issue_key".to_owned(), serde_json::json!("VOOM-FAKE-1"));
        }
        "fake-use-lease-provider" => {
            object.insert("decision".to_owned(), serde_json::json!("granted"));
        }
        _ => return Err(invalid(format!("unknown provider {provider}"))),
    }
    merge_artifact_access_evidence(&mut result, payload)?;
    Ok(result)
}

fn fake_remux_result(request: &RemuxRequest) -> Result<RemuxResult, ProtocolError> {
    let bytes = include_bytes!("../../voom-ffprobe-worker/fixtures/media/tiny.mp4");
    std::fs::write(&request.output.path, bytes)
        .map_err(|err| invalid(format!("fake remux output write failed: {err}")))?;
    Ok(RemuxResult {
        status: RemuxStatus::Remuxed,
        provider: "fake-remuxer".to_owned(),
        provider_version: "test".to_owned(),
        input_pre: RemuxObservedFacts {
            size_bytes: request.input.expected.size_bytes,
            content_hash: request.input.expected.content_hash.clone(),
            modified_at: None,
            local_file_key: None,
        },
        input_post: RemuxObservedFacts {
            size_bytes: request.input.expected.size_bytes,
            content_hash: request.input.expected.content_hash.clone(),
            modified_at: None,
            local_file_key: None,
        },
        output: RemuxObservedFacts {
            size_bytes: u64::try_from(bytes.len()).unwrap_or(0),
            content_hash: blake3_checksum(bytes),
            modified_at: None,
            local_file_key: Some(request.output.path.clone()),
        },
        output_container: request.output.container.clone(),
        kept_snapshot_stream_ids: request
            .selection
            .keep_streams
            .iter()
            .map(|stream| stream.snapshot_stream_id.clone())
            .collect(),
        default_snapshot_stream_ids: request
            .selection
            .default_streams
            .iter()
            .map(|stream| stream.snapshot_stream_id.clone())
            .collect(),
    })
}

fn fake_transcode_video_result(
    provider: &str,
    request: &TranscodeVideoRequest,
) -> Result<TranscodeVideoResult, ProtocolError> {
    let bytes = include_bytes!("../../voom-ffprobe-worker/fixtures/media/tiny.mp4");
    if let Some(parent) = Path::new(&request.output.path).parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            invalid(format!(
                "fake transcode_video output parent create failed: {err}"
            ))
        })?;
    }
    std::fs::write(&request.output.path, bytes)
        .map_err(|err| invalid(format!("fake transcode_video output write failed: {err}")))?;
    let input = video_observed_from_expected(
        request.input.expected.size_bytes,
        &request.input.expected.content_hash,
        request.input.expected.local_file_key.clone(),
    );
    Ok(TranscodeVideoResult {
        status: TranscodeVideoStatus::Transcoded,
        provider: provider.to_owned(),
        provider_version: "test".to_owned(),
        input_pre: input.clone(),
        input_post: input,
        output: TranscodeVideoObservedFacts {
            size_bytes: u64::try_from(bytes.len()).unwrap_or(0),
            content_hash: blake3_checksum(bytes),
            modified_at: None,
            local_file_key: Some(request.output.path.clone()),
        },
        output_container: request.output.container.clone(),
        output_video_codec: request.output.video_codec.clone(),
        output_width: 1_920,
        output_height: 1_080,
        output_pixel_format: request
            .profile
            .pixel_format
            .clone()
            .unwrap_or_else(|| "yuv420p".to_owned()),
        copied_video: request.copy_video,
    })
}

fn fake_transcode_audio_result(
    provider: &str,
    request: &TranscodeAudioRequest,
) -> Result<TranscodeAudioResult, ProtocolError> {
    let output = fake_audio_output_facts(&request.output.path)?;
    let input = audio_observed_from_expected(
        request.input.expected.size_bytes,
        &request.input.expected.content_hash,
    );
    let selected_output_streams = request
        .selection
        .selected_streams
        .iter()
        .enumerate()
        .map(|(ordinal, stream)| AudioOutputStreamFact {
            snapshot_stream_id: stream.snapshot_stream_id.clone(),
            output_provider_stream_index: u32::try_from(ordinal).unwrap_or(0),
            codec: request.audio.target_codec.clone(),
            language: None,
            title: None,
            default: Some(false),
            disposition: Some(AudioDispositionFact {
                default: Some(false),
                forced: Some(false),
                commentary: Some(false),
            }),
            channels: None,
        })
        .collect::<Vec<_>>();
    Ok(TranscodeAudioResult {
        status: TranscodeAudioStatus::Transcoded,
        provider: provider.to_owned(),
        provider_version: "test".to_owned(),
        input_pre: input.clone(),
        input_post: input,
        output,
        output_container: request.output.container.clone(),
        selected_snapshot_stream_ids: request
            .selection
            .selected_streams
            .iter()
            .map(|stream| stream.snapshot_stream_id.clone())
            .collect(),
        output_audio_codecs: selected_output_streams
            .iter()
            .map(|stream| stream.codec.clone())
            .collect(),
        selected_output_streams,
    })
}

fn fake_extract_audio_result(
    provider: &str,
    request: &ExtractAudioRequest,
) -> Result<ExtractAudioResult, ProtocolError> {
    let output = fake_audio_output_facts(&request.output.path)?;
    let input = audio_observed_from_expected(
        request.input.expected.size_bytes,
        &request.input.expected.content_hash,
    );
    Ok(ExtractAudioResult {
        status: ExtractAudioStatus::Extracted,
        provider: provider.to_owned(),
        provider_version: "test".to_owned(),
        input_pre: input.clone(),
        input_post: input,
        output,
        output_container: request.output.container.clone(),
        output_audio_codec: request.output.audio_codec.clone(),
        selected_snapshot_stream_id: request.selection.snapshot_stream_id.clone(),
        output_language: None,
        output_title: None,
    })
}

fn fake_audio_output_facts(path: &str) -> Result<AudioObservedFacts, ProtocolError> {
    let bytes = include_bytes!("../../voom-ffprobe-worker/fixtures/media/tiny.mp4");
    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| invalid(format!("fake audio output parent create failed: {err}")))?;
    }
    std::fs::write(path, bytes)
        .map_err(|err| invalid(format!("fake audio output write failed: {err}")))?;
    Ok(AudioObservedFacts {
        size_bytes: u64::try_from(bytes.len()).unwrap_or(0),
        content_hash: blake3_checksum(bytes),
        modified_at: None,
        local_file_key: Some(path.to_owned()),
    })
}

fn video_observed_from_expected(
    size_bytes: u64,
    content_hash: &str,
    local_file_key: Option<String>,
) -> TranscodeVideoObservedFacts {
    TranscodeVideoObservedFacts {
        size_bytes,
        content_hash: content_hash.to_owned(),
        modified_at: None,
        local_file_key,
    }
}

fn audio_observed_from_expected(size_bytes: u64, content_hash: &str) -> AudioObservedFacts {
    AudioObservedFacts {
        size_bytes,
        content_hash: content_hash.to_owned(),
        modified_at: None,
        local_file_key: None,
    }
}

fn fake_transcoder_video_payload(
    provider: &str,
    operation: OperationKind,
    payload: &serde_json::Value,
) -> Result<Option<serde_json::Value>, ProtocolError> {
    if operation == OperationKind::TranscodeVideo
        && let Some(request) = transcode_video_protocol_payload(payload)?
    {
        let mut result = serde_json::to_value(fake_transcode_video_result(provider, &request)?)
            .map_err(|err| invalid(format!("fake transcode_video result encode failed: {err}")))?;
        merge_artifact_access_evidence(&mut result, payload)?;
        return Ok(Some(result));
    }
    Ok(None)
}

fn fake_transcoder_audio_payload(
    provider: &str,
    operation: OperationKind,
    payload: &serde_json::Value,
) -> Result<Option<serde_json::Value>, ProtocolError> {
    if operation == OperationKind::TranscodeAudio
        && let Some(request) = transcode_audio_protocol_payload(payload)?
    {
        let mut result = serde_json::to_value(fake_transcode_audio_result(provider, &request)?)
            .map_err(|err| invalid(format!("fake transcode_audio result encode failed: {err}")))?;
        merge_artifact_access_evidence(&mut result, payload)?;
        return Ok(Some(result));
    }
    if operation == OperationKind::ExtractAudio
        && let Some(request) = extract_audio_protocol_payload(payload)?
    {
        let mut result = serde_json::to_value(fake_extract_audio_result(provider, &request)?)
            .map_err(|err| invalid(format!("fake extract_audio result encode failed: {err}")))?;
        merge_artifact_access_evidence(&mut result, payload)?;
        return Ok(Some(result));
    }
    Ok(None)
}

fn merge_artifact_access_evidence(
    result: &mut serde_json::Value,
    payload: &serde_json::Value,
) -> Result<(), ProtocolError> {
    let result_object = result
        .as_object_mut()
        .ok_or_else(|| invalid("result payload must be object"))?;
    let artifact_access_evidence = synthetic_artifact_access_evidence(payload)?;
    let artifact_access_object = artifact_access_evidence
        .as_object()
        .ok_or_else(|| invalid("artifact access evidence must be object"))?;
    for (key, value) in artifact_access_object {
        result_object.insert(key.clone(), value.clone());
    }
    Ok(())
}

fn advertised_access_modes<'a>(
    payload: &'a serde_json::Value,
    plan: &'a serde_json::Value,
) -> Result<Vec<&'a str>, ProtocolError> {
    for (source, field) in [
        (payload, "advertised_artifact_access"),
        (payload, "advertised_access_modes"),
        (plan, "advertised_artifact_access"),
        (plan, "advertised_access_modes"),
    ] {
        if let Some(modes) = optional_string_array_field(source, field)? {
            return Ok(modes);
        }
    }
    if payload
        .get("artifact_access")
        .is_some_and(serde_json::Value::is_array)
    {
        return string_array_field(payload, "artifact_access");
    }
    Ok(Vec::new())
}

fn scanner_files(
    payload: &serde_json::Value,
    fan_out_count: Option<u32>,
) -> Result<serde_json::Value, ProtocolError> {
    let base = string_field(payload, "path")?.trim_end_matches('/');
    let count = fan_out_count.unwrap_or(1);
    let files = (0..count)
        .map(|index| {
            let path = format!("{base}/file-{index:03}.mkv");
            serde_json::json!({
                "path": path.clone(),
                "size_bytes": 4_200_000_000_u64 + u64::from(index),
                "content_hash": blake3_checksum(path.as_bytes()),
                "local_file_key": path,
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::Value::Array(files))
}

fn needs_transcode(payload: &serde_json::Value) -> bool {
    payload
        .get("codec")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|codec| codec != "h265")
}

fn transform_output_path(payload: &serde_json::Value, marker: &str) -> String {
    let path = payload
        .get("path")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("/library/movie.mkv");
    let input = Path::new(path);
    let parent = input
        .parent()
        .and_then(Path::to_str)
        .filter(|parent| !parent.is_empty())
        .unwrap_or("/library");
    let stem = input
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("movie");
    if parent == "/" {
        format!("/{stem}.{marker}.mkv")
    } else {
        format!("{}/{stem}.{marker}.mkv", parent.trim_end_matches('/'))
    }
}

fn blake3_checksum(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}
