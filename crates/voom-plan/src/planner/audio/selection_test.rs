use voom_policy::{MediaSnapshotInput, TargetKind, TargetRef, TrackFilter};

use super::{
    AudioBundleRole, AudioDispositionFact, AudioPlanShape, AudioPlanningBlock,
    SnapshotAudioStreamFact, evaluate_audio_filter, extract_audio_shape, extraction_role,
    has_transcode_preservation_facts, stream_facts, transcode_audio_shape,
};
use crate::planner::audio::AUDIO_TRANSCODE_CONTAINER;

#[test]
fn transcode_preservation_facts_require_language_title_channels_and_commentary() {
    assert!(has_transcode_preservation_facts(&audio_fact(Some(false))));

    let missing_language = SnapshotAudioStreamFact {
        language: None,
        ..audio_fact(Some(false))
    };
    let missing_title = SnapshotAudioStreamFact {
        title: None,
        ..audio_fact(Some(false))
    };
    let missing_channels = SnapshotAudioStreamFact {
        channels: None,
        ..audio_fact(Some(false))
    };
    let missing_commentary = audio_fact(None);

    assert!(!has_transcode_preservation_facts(&missing_language));
    assert!(!has_transcode_preservation_facts(&missing_title));
    assert!(!has_transcode_preservation_facts(&missing_channels));
    assert!(!has_transcode_preservation_facts(&missing_commentary));
}

#[test]
fn stream_facts_parse_audio_streams_with_disposition_commentary() {
    let facts = stream_facts(&snapshot_with_streams(&serde_json::json!([
        {
            "id": "stream-1",
            "index": 1,
            "kind": "audio",
            "codec_name": "aac",
            "language": "eng",
            "title": "Commentary",
            "channels": 2,
            "disposition": {
                "default": true,
                "forced": false,
                "comment": true
            }
        }
    ])))
    .unwrap();

    assert_eq!(
        facts,
        vec![SnapshotAudioStreamFact {
            snapshot_stream_id: "stream-1".to_owned(),
            provider_stream_index: 1,
            codec: Some("aac".to_owned()),
            language: Some("eng".to_owned()),
            title: Some("Commentary".to_owned()),
            channels: Some(2),
            default: true,
            disposition: AudioDispositionFact {
                default: true,
                forced: false,
                commentary: Some(true),
            },
            commentary: Some(true),
        }]
    );
}

#[test]
fn commentary_filter_requires_known_commentary_fact() {
    let commentary_stream = audio_fact(Some(true));
    let unknown_stream = audio_fact(None);

    assert_eq!(
        evaluate_audio_filter(&TrackFilter::Commentary, &commentary_stream),
        Ok(true)
    );
    assert_eq!(
        evaluate_audio_filter(&TrackFilter::Commentary, &unknown_stream),
        Err(AudioPlanningBlock::InsufficientSnapshotFacts)
    );
}

#[test]
fn extraction_role_maps_known_commentary_and_blocks_unknown_commentary() {
    let commentary_stream = audio_fact(Some(true));
    let unknown_stream = audio_fact(None);

    assert_eq!(
        extraction_role(&commentary_stream),
        Ok(AudioBundleRole::CommentaryAudio)
    );
    assert_eq!(
        extraction_role(&unknown_stream),
        Err(AudioPlanningBlock::InsufficientSnapshotFacts)
    );
}

#[test]
fn transcode_audio_shape_blocks_missing_preservation_facts() {
    let mut stream = audio_fact(Some(false));
    stream.title = None;
    let snapshot = snapshot_with_audio_facts(vec![stream]);

    assert_eq!(
        transcode_audio_shape(&snapshot, "opus", AUDIO_TRANSCODE_CONTAINER, None),
        AudioPlanShape::Blocked(AudioPlanningBlock::InsufficientSnapshotFacts)
    );
}

#[test]
fn extract_audio_shape_blocks_unknown_commentary_role() {
    let snapshot = snapshot_with_audio_facts(vec![audio_fact(None)]);

    assert_eq!(
        extract_audio_shape(&snapshot, None),
        AudioPlanShape::Blocked(AudioPlanningBlock::InsufficientSnapshotFacts)
    );
}

#[test]
fn audio_and_filter_false_branch_beats_later_missing_fact() {
    let mut stream = audio_fact(None);
    stream.language = Some("jpn".to_owned());
    let filter = TrackFilter::And {
        filters: vec![
            TrackFilter::LanguageIn {
                values: vec!["eng".to_owned()],
            },
            TrackFilter::Commentary,
        ],
    };

    assert_eq!(evaluate_audio_filter(&filter, &stream), Ok(false));
}

#[test]
fn audio_or_filter_does_not_mask_unsupported_selector() {
    let filter = TrackFilter::Or {
        filters: vec![
            TrackFilter::LanguageIn {
                values: vec!["eng".to_owned()],
            },
            TrackFilter::Font,
        ],
    };

    assert_eq!(
        evaluate_audio_filter(&filter, &audio_fact(Some(false))),
        Err(AudioPlanningBlock::UnsupportedSelector)
    );
}

fn audio_fact(commentary: Option<bool>) -> SnapshotAudioStreamFact {
    SnapshotAudioStreamFact {
        snapshot_stream_id: "stream-1".to_owned(),
        provider_stream_index: 1,
        codec: Some("aac".to_owned()),
        language: Some("eng".to_owned()),
        title: Some("Main".to_owned()),
        channels: Some(2),
        default: false,
        disposition: AudioDispositionFact {
            default: false,
            forced: false,
            commentary,
        },
        commentary,
    }
}

fn snapshot_with_audio_facts(streams: Vec<SnapshotAudioStreamFact>) -> MediaSnapshotInput {
    let json_streams = streams
        .into_iter()
        .map(|stream| {
            serde_json::json!({
                "id": stream.snapshot_stream_id,
                "index": stream.provider_stream_index,
                "kind": "audio",
                "codec_name": stream.codec,
                "language": stream.language,
                "title": stream.title,
                "channels": stream.channels,
                "disposition": {
                    "default": stream.disposition.default,
                    "forced": stream.disposition.forced,
                    "commentary": stream.disposition.commentary
                }
            })
        })
        .collect::<Vec<_>>();
    let mut all_streams = vec![serde_json::json!({
        "id": "video-1",
        "index": 0,
        "kind": "video",
        "codec_name": "h264"
    })];
    all_streams.extend(json_streams);
    let mut snapshot = snapshot_with_streams(&serde_json::Value::Array(all_streams));
    snapshot.stream_summary["video_stream_count"] = serde_json::json!(1);
    snapshot
}

fn snapshot_with_streams(streams: &serde_json::Value) -> MediaSnapshotInput {
    MediaSnapshotInput {
        ordinal: 0,
        target: TargetRef::Synthetic {
            key: "variant-1".to_owned(),
            kind: TargetKind::MediaVariant,
        },
        container: Some("mkv".to_owned()),
        stream_summary: serde_json::json!({ "streams": streams }),
        video_codec: None,
        width: None,
        height: None,
        hdr: None,
        bitrate: None,
        duration_millis: None,
        audio_languages: Vec::new(),
        subtitle_languages: Vec::new(),
        health_flags: Vec::new(),
        existing_media_snapshot_id: None,
    }
}
