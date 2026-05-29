use super::*;

use serde_json::{Value, json};
use time::OffsetDateTime;
use voom_core::{ErrorCode, FileVersionId, MediaSnapshotId};
use voom_store::repo::identity::MediaSnapshot;

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

    assert_eq!(selection.stream.snapshot_stream_id, "commentary");
    assert_eq!(selection.role, AudioBundleRole::CommentaryAudio);
}

#[test]
fn extraction_rejects_zero_multiple_or_unknown_commentary_state() {
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
    assert!(multiple.to_string().contains("multiple streams"));

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
fn missing_selected_language_title_default_facts_block_transcode_preservation() {
    for stream in [
        audio("a-1", 1, "aac", None, Some("Main"), Some(false)),
        audio("a-1", 1, "aac", Some("eng"), None, Some(false)),
        audio("a-1", 1, "aac", Some("eng"), Some("Main"), None),
    ] {
        let snapshot = snapshot_with_streams(vec![stream]);

        let err = transcode_selection_from_payload_and_snapshot(
            &transcode_payload(&Value::Null),
            &snapshot,
        )
        .unwrap_err();

        assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
        assert!(err.to_string().contains("insufficient stream facts"));
    }
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
