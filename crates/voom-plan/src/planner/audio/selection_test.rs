use voom_policy::{MediaSnapshotInput, TargetKind, TargetRef, TrackFilter};

use voom_policy::ComparisonOp;

use super::{
    AudioBundleRole, AudioDispositionFact, AudioPlanShape, AudioPlanningBlock,
    SnapshotAudioStreamFact, evaluate_audio_filter, extract_audio_shape, extraction_role,
    stream_facts, synthesize_audio_shape, transcode_audio_shape,
};
use crate::planner::audio::AUDIO_TRANSCODE_CONTAINER;

fn surround_fact() -> SnapshotAudioStreamFact {
    SnapshotAudioStreamFact {
        codec: Some("eac3".to_owned()),
        channels: Some(6),
        ..audio_fact(Some(false))
    }
}

#[test]
fn synthesize_audio_shape_plans_stereo_downmix_of_surround_source() {
    // The 5.1 + stereo companion case (#276): a 6-channel source downmixed to 2.
    let snapshot = snapshot_with_audio_facts(vec![surround_fact()]);
    assert_eq!(
        synthesize_audio_shape(&snapshot, 2, None),
        AudioPlanShape::Planned
    );
}

#[test]
fn synthesize_audio_shape_blocks_when_target_is_not_a_downmix() {
    let snapshot = snapshot_with_audio_facts(vec![surround_fact()]);
    // Equal channel count is not a downmix.
    assert_eq!(
        synthesize_audio_shape(&snapshot, 6, None),
        AudioPlanShape::Blocked(AudioPlanningBlock::SynthesisNotDownmix)
    );
    // An upmix (more channels than the source) is likewise rejected.
    assert_eq!(
        synthesize_audio_shape(&snapshot, 8, None),
        AudioPlanShape::Blocked(AudioPlanningBlock::SynthesisNotDownmix)
    );
}

#[test]
fn synthesize_audio_shape_blocks_when_filter_matches_nothing() {
    let snapshot = snapshot_with_audio_facts(vec![surround_fact()]);
    let filter = TrackFilter::Channels {
        op: ComparisonOp::Gte,
        value: 8,
    };
    assert_eq!(
        synthesize_audio_shape(&snapshot, 2, Some(&filter)),
        AudioPlanShape::Blocked(AudioPlanningBlock::ZeroMatches)
    );
}

#[test]
fn synthesize_audio_shape_blocks_when_source_channels_unknown() {
    let stream = SnapshotAudioStreamFact {
        channels: None,
        ..surround_fact()
    };
    let snapshot = snapshot_with_audio_facts(vec![stream]);
    assert_eq!(
        synthesize_audio_shape(&snapshot, 2, None),
        AudioPlanShape::Blocked(AudioPlanningBlock::InsufficientSnapshotFacts)
    );
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
fn transcode_audio_shape_plans_streams_missing_descriptive_facts() {
    // No per-stream descriptive fact is a transcode build input (ADR-0011);
    // a stream with a known codec plans regardless of title/commentary/
    // language/channels presence.
    let stream = SnapshotAudioStreamFact {
        title: None,
        language: None,
        channels: None,
        disposition: AudioDispositionFact {
            default: false,
            forced: false,
            commentary: None,
        },
        commentary: None,
        ..audio_fact(Some(false))
    };
    let snapshot = snapshot_with_audio_facts(vec![stream]);

    assert_eq!(
        transcode_audio_shape(&snapshot, "opus", AUDIO_TRANSCODE_CONTAINER, None),
        AudioPlanShape::Planned
    );
}

#[test]
fn transcode_audio_shape_blocks_stream_without_codec() {
    // Codec is the real plannability floor: without it the shape cannot decide
    // no-op vs transcode, so it blocks.
    let mut stream = audio_fact(Some(false));
    stream.codec = None;
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

#[test]
fn audio_language_filter_untagged_matches_as_und() {
    // A missing language tag is treated as `und`, never a planning block
    // (issue #272): it is excluded by a non-`und` value set and kept by `und`.
    let untagged = SnapshotAudioStreamFact {
        language: None,
        ..audio_fact(Some(false))
    };

    assert_eq!(
        evaluate_audio_filter(
            &TrackFilter::LanguageIn {
                values: vec!["eng".to_owned()],
            },
            &untagged,
        ),
        Ok(false)
    );
    assert_eq!(
        evaluate_audio_filter(
            &TrackFilter::LanguageIn {
                values: vec!["und".to_owned()],
            },
            &untagged,
        ),
        Ok(true)
    );
}

#[test]
fn audio_language_filter_explicit_und_matches_like_untagged() {
    let explicit_und = SnapshotAudioStreamFact {
        language: Some("und".to_owned()),
        ..audio_fact(Some(false))
    };

    assert_eq!(
        evaluate_audio_filter(
            &TrackFilter::LanguageIn {
                values: vec!["und".to_owned()],
            },
            &explicit_und,
        ),
        Ok(true)
    );
    assert_eq!(
        evaluate_audio_filter(
            &TrackFilter::LanguageIn {
                values: vec!["eng".to_owned()],
            },
            &explicit_und,
        ),
        Ok(false)
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
