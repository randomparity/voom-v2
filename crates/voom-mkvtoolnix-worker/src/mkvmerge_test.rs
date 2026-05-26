use voom_worker_protocol::{
    RemuxExpectedFacts, RemuxInput, RemuxOutput, RemuxRequest, RemuxSelection, RemuxStreamRef,
    RemuxTrackGroup,
};

use super::*;

#[test]
fn maps_snapshot_provider_indexes_to_mkvmerge_track_ids() {
    let identify = serde_json::json!({
        "tracks": [
            {"id": 7, "type": "video", "properties": {"number": 1}},
            {"id": 12, "type": "audio", "properties": {"number": 2}},
            {"id": 14, "type": "subtitles", "properties": {"number": 3}}
        ]
    });

    let mapping = track_mapping_from_identify(&identify).unwrap();

    assert_eq!(mapping.mkvmerge_track_id_for_provider_index(0), Some(7));
    assert_eq!(mapping.mkvmerge_track_id_for_provider_index(1), Some(12));
    assert_eq!(mapping.mkvmerge_track_id_for_provider_index(2), Some(14));
}

#[test]
fn track_fingerprint_ignores_remux_changed_fields() {
    let before = serde_json::json!({
        "tracks": [
            {"id": 12, "type": "audio", "properties": {
                "default_track": false,
                "language": "eng",
                "number": 2
            }}
        ]
    });
    let after = serde_json::json!({
        "tracks": [
            {"id": 21, "type": "audio", "properties": {
                "default_track": true,
                "language": "eng",
                "number": 1
            }}
        ]
    });

    let before = track_mapping_from_identify(&before)
        .unwrap()
        .track_for_provider_index(0)
        .unwrap();
    let after = track_mapping_from_identify(&after)
        .unwrap()
        .track_for_provider_index(0)
        .unwrap();

    assert_eq!(before.fingerprint, after.fingerprint);
}

#[test]
fn track_fingerprint_distinguishes_same_kind_languages() {
    let identify = serde_json::json!({
        "tracks": [
            {"id": 12, "type": "audio", "properties": {"language": "eng", "number": 2}},
            {"id": 13, "type": "audio", "properties": {"language": "spa", "number": 3}}
        ]
    });

    let mapping = track_mapping_from_identify(&identify).unwrap();
    let english = mapping.track_for_provider_index(0).unwrap();
    let spanish = mapping.track_for_provider_index(1).unwrap();

    assert_ne!(english.fingerprint, spanish.fingerprint);
}

#[test]
fn reads_real_mkvmerge_container_type_string() {
    let identify = serde_json::json!({
        "container": {
            "properties": {
                "container_type": 17
            },
            "type": "Matroska"
        },
        "tracks": []
    });

    assert_eq!(identify_container_type(&identify), "Matroska");
}

#[test]
fn build_args_rejects_missing_track_mapping() {
    let request = RemuxRequest {
        input: RemuxInput {
            path: "/tmp/input.mp4".to_owned(),
            expected: RemuxExpectedFacts {
                size_bytes: 1,
                content_hash: "blake3:abc".to_owned(),
                modified_at: None,
                local_file_key: None,
            },
        },
        output: RemuxOutput {
            staging_root: "/tmp/stage".to_owned(),
            path: "/tmp/stage/out.mkv".to_owned(),
            container: "mkv".to_owned(),
            overwrite: false,
        },
        selection: RemuxSelection {
            keep_streams: vec![RemuxStreamRef {
                snapshot_stream_id: "stream-0".to_owned(),
                provider_stream_index: 0,
            }],
            default_streams: vec![],
            clear_default_streams: vec![],
            track_order: vec![RemuxTrackGroup::Video],
        },
    };
    let mapping = MkvmergeTrackMapping::from_pairs([(1, 7)]);

    let err = build_mkvmerge_args(&request, &mapping).unwrap_err();

    assert!(err.to_string().contains("missing mkvmerge track id"));
}

#[test]
fn build_args_disable_unselected_audio_subtitles_and_attachments() {
    let request = RemuxRequest {
        input: RemuxInput {
            path: "/tmp/input.mp4".to_owned(),
            expected: RemuxExpectedFacts {
                size_bytes: 1,
                content_hash: "blake3:abc".to_owned(),
                modified_at: None,
                local_file_key: None,
            },
        },
        output: RemuxOutput {
            staging_root: "/tmp/stage".to_owned(),
            path: "/tmp/stage/out.mkv".to_owned(),
            container: "mkv".to_owned(),
            overwrite: false,
        },
        selection: RemuxSelection {
            keep_streams: vec![RemuxStreamRef {
                snapshot_stream_id: "stream-0".to_owned(),
                provider_stream_index: 0,
            }],
            default_streams: vec![],
            clear_default_streams: vec![],
            track_order: vec![RemuxTrackGroup::Video],
        },
    };
    let mapping = MkvmergeTrackMapping::from_pairs([(0, 7)]);

    let args = build_mkvmerge_args(&request, &mapping).unwrap();

    assert!(args.contains(&"--no-audio".to_owned()));
    assert!(args.contains(&"--no-subtitles".to_owned()));
    assert!(args.contains(&"--no-attachments".to_owned()));
}
