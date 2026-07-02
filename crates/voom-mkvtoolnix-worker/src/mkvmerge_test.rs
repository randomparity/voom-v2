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

    let before_mapping = track_mapping_from_identify(&before).unwrap();
    let before = before_mapping.track_for_provider_index(0).unwrap();
    let after_mapping = track_mapping_from_identify(&after).unwrap();
    let after = after_mapping.track_for_provider_index(0).unwrap();

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
            head_streams: vec![],
            forced_streams: vec![],
            clear_forced_streams: vec![],
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
            head_streams: vec![],
            forced_streams: vec![],
            clear_forced_streams: vec![],
        },
    };
    let mapping = MkvmergeTrackMapping::from_pairs([(0, 7)]);

    let args = build_mkvmerge_args(&request, &mapping).unwrap();

    assert!(args.contains(&"--no-audio".to_owned()));
    assert!(args.contains(&"--no-subtitles".to_owned()));
    assert!(args.contains(&"--no-attachments".to_owned()));
}

fn stream(provider_stream_index: u32) -> RemuxStreamRef {
    RemuxStreamRef {
        snapshot_stream_id: format!("stream-{provider_stream_index}"),
        provider_stream_index,
    }
}

fn remux_request(selection: RemuxSelection) -> RemuxRequest {
    RemuxRequest {
        input: RemuxInput {
            path: "/tmp/input.mkv".to_owned(),
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
        selection,
    }
}

fn base_selection(keep: Vec<RemuxStreamRef>) -> RemuxSelection {
    RemuxSelection {
        keep_streams: keep,
        default_streams: vec![],
        clear_default_streams: vec![],
        track_order: vec![],
        head_streams: vec![],
        forced_streams: vec![],
        clear_forced_streams: vec![],
    }
}

fn flag_pair(args: &[String], flag: &str, value: &str) -> bool {
    args.windows(2).any(|w| w[0] == flag && w[1] == value)
}

fn first_arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|w| w[0] == flag)
        .map(|w| w[1].as_str())
}

#[test]
fn build_args_emits_forced_track_flags_for_set_and_clear() {
    let mut selection = base_selection(vec![stream(0), stream(1), stream(2)]);
    selection.forced_streams = vec![stream(1)];
    selection.clear_forced_streams = vec![stream(2)];
    let request = remux_request(selection);
    let mapping = MkvmergeTrackMapping::from_pairs([(0, 7), (1, 12), (2, 14)]);

    let args = build_mkvmerge_args(&request, &mapping).unwrap();

    assert!(
        flag_pair(&args, "--forced-track-flag", "12:1"),
        "forced set flag missing: {args:?}"
    );
    assert!(
        flag_pair(&args, "--forced-track-flag", "14:0"),
        "forced clear flag missing: {args:?}"
    );
}

#[test]
fn build_args_forced_set_wins_over_clear_on_collision() {
    let mut selection = base_selection(vec![stream(0), stream(1)]);
    selection.forced_streams = vec![stream(1)];
    selection.clear_forced_streams = vec![stream(1)];
    let request = remux_request(selection);
    let mapping = MkvmergeTrackMapping::from_pairs([(0, 7), (1, 12)]);

    let args = build_mkvmerge_args(&request, &mapping).unwrap();

    assert!(flag_pair(&args, "--forced-track-flag", "12:1"));
    assert!(
        !flag_pair(&args, "--forced-track-flag", "12:0"),
        "clear must not fire when the same id is set forced: {args:?}"
    );
}

#[test]
fn build_args_missing_forced_stream_mapping_errors() {
    let mut selection = base_selection(vec![stream(0)]);
    selection.forced_streams = vec![stream(9)];
    let request = remux_request(selection);
    let mapping = MkvmergeTrackMapping::from_pairs([(0, 7)]);

    let err = build_mkvmerge_args(&request, &mapping).unwrap_err();

    assert!(err.to_string().contains("missing mkvmerge track id"));
}

#[test]
fn build_args_pins_head_stream_first_in_track_order() {
    let mut selection = base_selection(vec![stream(0), stream(1), stream(2)]);
    selection.head_streams = vec![stream(2)];
    let request = remux_request(selection);
    let mapping = MkvmergeTrackMapping::from_pairs([(0, 7), (1, 12), (2, 14)]);

    let args = build_mkvmerge_args(&request, &mapping).unwrap();

    let order = first_arg_value(&args, "--track-order").unwrap();
    assert!(
        order.starts_with("0:14"),
        "head stream not pinned first: {order}"
    );
}

#[test]
fn build_args_head_stream_precedes_group_order() {
    let mut selection = base_selection(vec![stream(0), stream(1), stream(2)]);
    selection.head_streams = vec![stream(2)];
    selection.track_order = vec![RemuxTrackGroup::Video];
    let request = remux_request(selection);
    let mapping = MkvmergeTrackMapping::from_pairs([(0, 7), (1, 12), (2, 14)]);

    let args = build_mkvmerge_args(&request, &mapping).unwrap();

    let order = first_arg_value(&args, "--track-order").unwrap();
    assert_eq!(order, "0:14,0:7,0:12");
}

#[test]
fn build_args_ignores_head_stream_not_kept() {
    let mut selection = base_selection(vec![stream(0)]);
    selection.head_streams = vec![stream(1)];
    let request = remux_request(selection);
    let mapping = MkvmergeTrackMapping::from_pairs([(0, 7), (1, 12)]);

    let args = build_mkvmerge_args(&request, &mapping).unwrap();

    let order = first_arg_value(&args, "--track-order").unwrap();
    assert_eq!(
        order, "0:7",
        "unkept head stream must not be pinned: {order}"
    );
}
