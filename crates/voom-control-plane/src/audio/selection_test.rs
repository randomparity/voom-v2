use super::*;

use serde_json::{Value, json};
use time::OffsetDateTime;
use voom_core::{ErrorCode, FileVersionId, MediaSnapshotId};
use voom_store::repo::media::identity::MediaSnapshot;

#[test]
fn transcode_selection_returns_selected_audio_refs_in_request_order() {
    let payload = transcode_payload(&json!({
        "type": "language_in",
        "values": ["eng", "jpn"]
    }));
    let snapshot = snapshot_with_streams(vec![
        audio("a-1", 1, "aac", Some("eng"), Some("Main"), Some(false)),
        audio("a-2", 2, "aac", Some("jpn"), Some("Dub"), Some(false)),
        audio("a-3", 3, "aac", Some("spa"), Some("Alt"), Some(false)),
    ]);

    let selection = transcode_selection_from_payload_and_snapshot(&payload, &snapshot).unwrap();

    assert_eq!(
        selection
            .selection
            .selected_streams
            .iter()
            .map(|stream| (
                stream.snapshot_stream_id.as_str(),
                stream.provider_stream_index
            ))
            .collect::<Vec<_>>(),
        vec![("a-1", 1), ("a-2", 2)]
    );
}

#[test]
fn transcode_rejects_zero_matches_and_sources_without_video() {
    let payload = transcode_payload(&json!({
        "type": "language_in",
        "values": ["fra"]
    }));
    let snapshot = snapshot_with_streams(vec![audio(
        "a-1",
        1,
        "aac",
        Some("eng"),
        Some("Main"),
        Some(false),
    )]);

    let err = transcode_selection_from_payload_and_snapshot(&payload, &snapshot).unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("zero streams"));

    let no_video = MediaSnapshot {
        payload: json!({"streams": [audio("a-1", 1, "aac", Some("eng"), Some("Main"), Some(false))]}),
        ..snapshot
    };
    let err =
        transcode_selection_from_payload_and_snapshot(&transcode_payload(&Value::Null), &no_video)
            .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("video stream"));
}

#[test]
fn transcode_video_absence_takes_precedence_over_malformed_audio_facts() {
    // No video stream, plus duplicate audio stream ids that `stream_facts`
    // would otherwise reject as insufficient. Video presence is a precondition,
    // so the runtime selection must surface NoVideo before parsing stream facts.
    // `base` supplies only the non-payload identity fields; its payload is
    // replaced below with the no-video, duplicate-id stream list under test.
    let base = snapshot_with_streams(Vec::new());
    let no_video_dup = MediaSnapshot {
        payload: json!({"streams": [
            audio("dup", 1, "aac", Some("eng"), Some("Main"), Some(false)),
            audio("dup", 2, "aac", Some("jpn"), Some("Alt"), Some(false)),
        ]}),
        ..base
    };

    let err = transcode_selection_from_payload_and_snapshot(
        &transcode_payload(&Value::Null),
        &no_video_dup,
    )
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("video stream"));
}

#[test]
fn extraction_selection_returns_exactly_one_stream_and_role() {
    let payload = extract_payload(&json!({"type": "commentary"}));
    let snapshot = snapshot_with_streams(vec![
        audio("main", 1, "aac", Some("eng"), Some("Main"), Some(false)),
        audio(
            "commentary",
            2,
            "aac",
            Some("eng"),
            Some("Commentary"),
            Some(true),
        ),
    ]);

    let selection = extract_selection_from_payload_and_snapshot(&payload, &snapshot).unwrap();

    assert_eq!(selection.outputs.len(), 1);
    assert_eq!(selection.outputs[0].stream.snapshot_stream_id, "commentary");
    assert_eq!(selection.outputs[0].role, AudioBundleRole::CommentaryAudio);
    assert_eq!(selection.operation_id, None);
    assert_eq!(selection.outputs[0].output_id, None);
    assert_eq!(selection.outputs[0].name_suffix, None);
}

#[test]
fn extraction_rejects_zero_legacy_multiple_or_unknown_commentary_state() {
    let snapshot = snapshot_with_streams(vec![
        audio("main", 1, "aac", Some("eng"), Some("Main"), Some(false)),
        audio("alt", 2, "aac", Some("jpn"), Some("Alt"), Some(false)),
    ]);

    let zero = extract_selection_from_payload_and_snapshot(
        &extract_payload(&json!({"type": "language_in", "values": ["fra"]})),
        &snapshot,
    )
    .unwrap_err();
    assert!(zero.to_string().contains("zero streams"));

    let multiple =
        extract_selection_from_payload_and_snapshot(&extract_payload(&Value::Null), &snapshot)
            .unwrap_err();
    assert!(multiple.to_string().contains("regenerate the plan"));

    let unknown = snapshot_with_streams(vec![audio(
        "main",
        1,
        "aac",
        Some("eng"),
        Some("Main"),
        None,
    )]);
    let err = extract_selection_from_payload_and_snapshot(&extract_payload(&Value::Null), &unknown)
        .unwrap_err();
    assert!(err.to_string().contains("insufficient stream facts"));
}

#[test]
fn extraction_validates_ordered_planned_outputs_against_pinned_snapshot() {
    let operation_id = "node_extract_audio_1";
    let first_output_id = voom_plan::planner::audio::extract_output_id(operation_id, "main");
    let snapshot = snapshot_with_streams(vec![
        audio("main", 1, "aac", Some("eng"), Some("Main"), Some(false)),
        audio("alt", 2, "aac", Some("jpn"), Some("Alt"), Some(false)),
    ]);
    let mut payload = extract_payload(&Value::Null);
    payload["operation_id"] = json!(operation_id);
    payload["outputs"] = json!([
        {
            "output_id": first_output_id.clone(),
            "source_snapshot_stream_id": "main",
            "source_provider_stream_index": 1,
            "name_suffix": "main.opus.ogg",
            "bundle_role": "external_audio"
        },
        {
            "output_id": voom_plan::planner::audio::extract_output_id(operation_id, "alt"),
            "source_snapshot_stream_id": "alt",
            "source_provider_stream_index": 2,
            "name_suffix": "alt.opus.ogg",
            "bundle_role": "external_audio"
        }
    ]);

    let selection = extract_selection_from_payload_and_snapshot(&payload, &snapshot).unwrap();

    assert_eq!(selection.operation_id.as_deref(), Some(operation_id));
    assert_eq!(
        selection.outputs[0].output_id.as_deref(),
        Some(first_output_id.as_str())
    );
    assert_eq!(
        selection.outputs[0].name_suffix.as_deref(),
        Some("main.opus.ogg")
    );
    assert_eq!(selection.outputs.len(), 2);
    assert_eq!(selection.outputs[0].stream.snapshot_stream_id, "main");
    assert_eq!(selection.outputs[1].stream.snapshot_stream_id, "alt");

    payload["outputs"][0]["name_suffix"] = json!("wrong.opus.ogg");
    let error = extract_selection_from_payload_and_snapshot(&payload, &snapshot).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("do not match the pinned source snapshot")
    );
}

#[test]
fn transcode_selection_admits_streams_missing_descriptive_facts() {
    // No per-stream descriptive fact gates runtime selection (ADR-0011):
    // title-less, language-less, and commentary-less streams all select.
    for stream in [
        audio("a-1", 1, "aac", None, Some("Main"), Some(false)),
        audio("a-1", 1, "aac", Some("eng"), None, Some(false)),
        audio("a-1", 1, "aac", Some("eng"), Some("Main"), None),
    ] {
        let snapshot = snapshot_with_streams(vec![stream]);

        let selection = transcode_selection_from_payload_and_snapshot(
            &transcode_payload(&Value::Null),
            &snapshot,
        )
        .unwrap();

        assert_eq!(
            selection
                .selection
                .selected_streams
                .iter()
                .map(|stream| stream.snapshot_stream_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a-1"]
        );
    }
}

#[test]
fn transcode_untagged_language_selects_under_und_and_excludes_under_eng() {
    // The execution selector calls the shared evaluator, so the `und` fallback for
    // an untagged track is inherited (ADR 0021): it matches `und`, not `eng`.
    let snapshot = snapshot_with_streams(vec![audio(
        "a-1",
        1,
        "aac",
        None,
        Some("Main"),
        Some(false),
    )]);

    let selection = transcode_selection_from_payload_and_snapshot(
        &transcode_payload(&json!({"type": "language_in", "values": ["und"]})),
        &snapshot,
    )
    .unwrap();
    assert_eq!(
        selection
            .selection
            .selected_streams
            .iter()
            .map(|stream| stream.snapshot_stream_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a-1"]
    );

    let err = transcode_selection_from_payload_and_snapshot(
        &transcode_payload(&json!({"type": "language_in", "values": ["eng"]})),
        &snapshot,
    )
    .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("zero streams"), "{err}");
}

#[test]
fn transcode_selection_resolves_synthesize_companion_against_pinned_source() {
    let mut payload = payload(
        "synthesize_audio",
        "aac",
        "mkv",
        &json!({"type": "channels", "op": "gte", "value": 6}),
    );
    payload["target_channels"] = json!(2);
    let operation_id = "node_test_synthesis";
    let companion_id = voom_plan::planner::audio::synthesis_companion_id(operation_id, "a-1");
    payload["operation_id"] = json!(operation_id);
    payload["companions"] = json!([{
        "companion_id": companion_id,
        "source_snapshot_stream_id": "a-1",
        "source_provider_stream_index": 1,
        "result_snapshot_stream_id": companion_id
    }]);
    let mut source = audio("a-1", 1, "eac3", Some("eng"), Some("Main"), Some(false));
    source["channels"] = json!(6);
    let snapshot = snapshot_with_streams(vec![source]);

    let selection = transcode_selection_from_payload_and_snapshot(&payload, &snapshot).unwrap();

    assert!(selection.add_track);
    assert_eq!(selection.operation_id.as_deref(), Some(operation_id));
    assert_eq!(selection.target_channels, Some(2));
    assert_eq!(
        selection.selection.selected_streams[0].snapshot_stream_id,
        companion_id
    );
    assert_eq!(
        selection.selected_streams[0].source.snapshot_stream_id,
        "a-1"
    );
}

fn transcode_payload(filter: &Value) -> Value {
    payload("transcode_audio", "aac", "mkv", filter)
}

fn extract_payload(filter: &Value) -> Value {
    payload("extract_audio", "opus", "ogg", filter)
}

fn payload(operation_type: &str, codec: &str, container: &str, filter: &Value) -> Value {
    json!({
        "type": operation_type,
        "target_codec": codec,
        "container": container,
        "source_media_snapshot_id": 1,
        "filter": filter
    })
}

fn snapshot_with_streams(audio_streams: Vec<Value>) -> MediaSnapshot {
    let mut streams = vec![json!({
        "id": "v-1",
        "index": 0,
        "kind": "video",
        "codec_name": "h264"
    })];
    streams.extend(audio_streams);
    MediaSnapshot {
        id: MediaSnapshotId(1),
        file_version_id: FileVersionId(1),
        probed_by: None,
        probed_at: OffsetDateTime::UNIX_EPOCH,
        payload: json!({ "container": "mkv", "streams": streams }),
    }
}

fn audio(
    id: &str,
    index: u32,
    codec: &str,
    language: Option<&str>,
    title: Option<&str>,
    commentary: Option<bool>,
) -> Value {
    let mut stream = json!({
        "id": id,
        "index": index,
        "kind": "audio",
        "codec_name": codec,
        "channels": 2,
        "disposition": {
            "default": index == 1,
            "forced": false,
            "commentary": commentary
        }
    });
    if let Some(language) = language {
        stream["language"] = json!(language);
    }
    if let Some(title) = title {
        stream["title"] = json!(title);
    }
    stream
}
