use super::*;
use serde_json::json;

fn settings() -> voom_worker_protocol::AudioCompatibilitySettings {
    serde_json::from_value(json!({
        "source_snapshot_stream_id": "source", "source_channels": 2,
        "source_channel_layout": "stereo", "target_channel_layout": "stereo",
        "retained_streams": [{"snapshot_stream_id": "video", "provider_stream_index": 0},
            {"snapshot_stream_id": "source", "provider_stream_index": 1}]
    }))
    .unwrap()
}

fn observed() -> (Value, Value) {
    let snapshot = json!({"container": {"format_name": "matroska"}, "streams": [
        {"id": "video", "index": 0, "kind": "video", "codec_name": "ffv1"},
        {"id": "source", "index": 1, "kind": "audio", "codec_name": "flac", "channels": 2,
            "channel_layout": "stereo", "language": "eng", "title": "Main",
            "disposition": {"default": true, "forced": false, "commentary": false}},
        {"id": "derived", "index": 2, "kind": "audio", "codec_name": "aac", "channels": 2,
            "channel_layout": "stereo", "language": "eng", "title": "Main",
            "compatibility_source_snapshot_stream_id": "source",
            "disposition": {"default": false, "forced": false, "commentary": false}}
    ]});
    let facts = json!({"size_bytes": 1, "content_hash": "blake3:input"});
    let raw = json!({"status": "transcoded", "provider": "ffmpeg", "provider_version": "8",
        "input_pre": facts, "input_post": facts, "output": facts, "output_container": "mkv",
        "selected_snapshot_stream_ids": ["derived"], "output_audio_codecs": ["aac"],
        "selected_output_streams": [{"snapshot_stream_id": "derived", "output_provider_stream_index": 2,
            "codec": "aac", "language": "eng", "title": "Main", "channels": 2, "channel_layout": "stereo",
            "default": false, "disposition": {"default": false, "forced": false, "commentary": false}}]
    });
    (snapshot, raw)
}

#[test]
fn compatibility_probe_must_match_admitted_source_and_result() {
    let (snapshot, raw) = observed();
    assert!(validate_compatibility_snapshot(&snapshot, &settings(), &raw).is_ok());
    for (field, invalid) in [
        ("id", json!("foreign")),
        ("compatibility_source_snapshot_stream_id", json!("video")),
        ("channel_layout", json!("mono")),
    ] {
        let mut forged = snapshot.clone();
        forged["streams"][2][field] = invalid;
        assert!(
            validate_compatibility_snapshot(&forged, &settings(), &raw).is_err(),
            "forged {field}"
        );
    }
    let mut missing_tag = snapshot.clone();
    missing_tag["streams"][1]
        .as_object_mut()
        .unwrap()
        .remove("id");
    assert!(
        validate_compatibility_snapshot(&missing_tag, &settings(), &raw).is_err(),
        "fallback IDs cannot conceal unobserved compatibility tags"
    );
    let mut wrong_default = snapshot;
    wrong_default["streams"][2]["disposition"]["default"] = json!(true);
    assert!(validate_compatibility_snapshot(&wrong_default, &settings(), &raw).is_err());
}

#[test]
fn compatibility_admission_binds_exact_settings() {
    let mut selection = crate::audio::selection::TranscodeAudioSelectionPlan {
        selection: voom_worker_protocol::TranscodeAudioSelection {
            selected_streams: Vec::new(),
        },
        selected_streams: Vec::new(),
        target_codec: "aac".to_owned(),
        container: "mkv".to_owned(),
        operation_id: Some("compat".to_owned()),
        add_track: true,
        target_channels: Some(2),
        compatibility: Some(settings()),
    };
    let wire = voom_worker_protocol::TranscodeAudioSettings {
        lossless: None,
        target_codec: "aac".to_owned(),
        profile: "default".to_owned(),
        add_track: true,
        target_channels: Some(2),
        compatibility: Some(settings()),
    };
    assert!(require_synthesis_settings(&selection, &wire).is_ok());
    let mut forged = wire.clone();
    forged.target_channels = Some(1);
    assert!(require_synthesis_settings(&selection, &forged).is_err());
    let mut forged = wire.clone();
    forged
        .compatibility
        .as_mut()
        .unwrap()
        .retained_streams
        .clear();
    assert!(require_synthesis_settings(&selection, &forged).is_err());
    selection.compatibility = None;
    assert!(
        require_synthesis_settings(&selection, &wire).is_err(),
        "ordinary synthesis cannot acquire compatibility mode"
    );
}

#[test]
fn normalized_audio_facts_preserve_checked_optional_observations() {
    let observed = json!({"sample_rate":48000,"sample_format":"s32","bits_per_sample":24});
    assert_eq!(
        optional_audio_output_fact::<u32>(&observed, "sample_rate").unwrap(),
        Some(48_000)
    );
    assert_eq!(
        optional_audio_output_fact::<String>(&observed, "sample_format")
            .unwrap()
            .as_deref(),
        Some("s32")
    );
    assert_eq!(
        optional_audio_output_fact::<u8>(&observed, "bits_per_sample").unwrap(),
        Some(24)
    );
    assert_eq!(
        optional_audio_output_fact::<u32>(&json!({}), "sample_rate").unwrap(),
        None
    );
    assert_eq!(
        optional_audio_output_fact::<u32>(&json!({"sample_rate":null}), "sample_rate").unwrap(),
        None
    );
    for invalid in [
        json!({"sample_rate":-1}),
        json!({"sample_rate":4_294_967_296_u64}),
        json!({"sample_rate":"48000"}),
    ] {
        let error = optional_audio_output_fact::<u32>(&invalid, "sample_rate").unwrap_err();
        assert!(error.to_string().contains("normalized audio sample_rate"));
    }
    assert!(
        optional_audio_output_fact::<u8>(&json!({"bits_per_sample":256}), "bits_per_sample")
            .is_err()
    );
    assert!(
        optional_audio_output_fact::<String>(&json!({"sample_format":32}), "sample_format")
            .is_err()
    );
}
