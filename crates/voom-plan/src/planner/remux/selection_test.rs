use serde_json::json;
use voom_policy::{
    ComparisonOp, MediaSnapshotInput, TargetKind, TargetRef, TrackFilter, TrackTarget,
};

use super::{RemuxPlanningBlock, SnapshotStreamFact, evaluate_filter, stream_facts};

#[test]
fn remux_stream_facts_parse_normalized_streams() {
    let streams = json!([
        {
            "id": "stream-0",
            "index": 0,
            "kind": "video",
            "codec_name": "h264",
            "disposition": {
                "default": true
            }
        },
        {
            "id": "stream-1",
            "index": 1,
            "kind": "audio",
            "codec_name": "aac",
            "language": "eng",
            "channels": 6,
            "title": "Main",
            "disposition": {
                "forced": true
            }
        }
    ]);
    let snapshot = snapshot_with_streams(&streams);

    let facts = stream_facts(&snapshot).unwrap();

    assert_eq!(
        facts,
        vec![
            SnapshotStreamFact {
                snapshot_stream_id: "stream-0".to_owned(),
                provider_stream_index: 0,
                kind: TrackTarget::Video,
                codec_name: Some("h264".to_owned()),
                language: None,
                channels: None,
                title: None,
                mime_type: None,
                filename: None,
                is_default: true,
                is_forced: false,
            },
            SnapshotStreamFact {
                snapshot_stream_id: "stream-1".to_owned(),
                provider_stream_index: 1,
                kind: TrackTarget::Audio,
                codec_name: Some("aac".to_owned()),
                language: Some("eng".to_owned()),
                channels: Some(6),
                title: Some("Main".to_owned()),
                mime_type: None,
                filename: None,
                is_default: false,
                is_forced: true,
            },
        ]
    );
}

#[test]
fn remux_stream_facts_missing_stream_id_blocks_planning() {
    let streams = json!([
        {
            "index": 0,
            "kind": "audio"
        }
    ]);
    let snapshot = snapshot_with_streams(&streams);

    let err = stream_facts(&snapshot).unwrap_err();

    assert_eq!(err, RemuxPlanningBlock::InsufficientSnapshotFacts);
}

#[test]
fn remux_language_filter_untagged_matches_as_und() {
    // A missing language tag is treated as `und` (ISO 639-2 undetermined) rather
    // than blocking planning (ADR 0021, issue #272): excluded by a non-`und` value
    // set, kept by `und`.
    let untagged = audio_stream(None);

    assert_eq!(
        evaluate_filter(
            &TrackFilter::LanguageIn {
                values: vec!["eng".to_owned()],
            },
            &untagged,
        ),
        Ok(false)
    );
    assert_eq!(
        evaluate_filter(
            &TrackFilter::LanguageIn {
                values: vec!["und".to_owned()],
            },
            &untagged,
        ),
        Ok(true)
    );
}

#[test]
fn remux_language_filter_explicit_und_matches_like_untagged() {
    let explicit_und = audio_stream(Some("und"));

    assert_eq!(
        evaluate_filter(
            &TrackFilter::LanguageIn {
                values: vec!["und".to_owned()],
            },
            &explicit_und,
        ),
        Ok(true)
    );
    assert_eq!(
        evaluate_filter(
            &TrackFilter::LanguageIn {
                values: vec!["eng".to_owned()],
            },
            &explicit_und,
        ),
        Ok(false)
    );
}

#[test]
fn remux_or_returns_true_before_later_insufficient_child() {
    // A title-less stream makes `title contains` insufficient; `Or` must return
    // true from the matching codec child without surfacing the later block.
    let stream = audio_stream(None);

    let matched = evaluate_filter(
        &TrackFilter::Or {
            filters: vec![
                TrackFilter::CodecIn {
                    values: vec!["aac".to_owned()],
                },
                TrackFilter::TitleContains {
                    value: "main".to_owned(),
                },
            ],
        },
        &stream,
    )
    .unwrap();

    assert!(matched);
}

#[test]
fn remux_and_evaluates_later_missing_facts_after_false_child() {
    // A title-less stream makes `title contains` insufficient; `And` must surface
    // that block even though an earlier child already evaluated false.
    let stream = audio_stream(None);

    let err = evaluate_filter(
        &TrackFilter::And {
            filters: vec![
                TrackFilter::CodecIn {
                    values: vec!["flac".to_owned()],
                },
                TrackFilter::TitleContains {
                    value: "main".to_owned(),
                },
            ],
        },
        &stream,
    )
    .unwrap_err();

    assert_eq!(err, RemuxPlanningBlock::InsufficientSnapshotFacts);
}

#[test]
fn remux_title_contains_is_case_sensitive() {
    let stream = SnapshotStreamFact {
        snapshot_stream_id: "stream-0".to_owned(),
        provider_stream_index: 0,
        kind: TrackTarget::Audio,
        codec_name: Some("aac".to_owned()),
        language: Some("eng".to_owned()),
        channels: Some(2),
        title: Some("Main Audio".to_owned()),
        mime_type: None,
        filename: None,
        is_default: false,
        is_forced: false,
    };

    let matched = evaluate_filter(
        &TrackFilter::TitleContains {
            value: "main".to_owned(),
        },
        &stream,
    )
    .unwrap();

    assert!(!matched);
}

#[test]
fn remux_channels_filter_uses_comparison_op() {
    let stream = SnapshotStreamFact {
        snapshot_stream_id: "stream-0".to_owned(),
        provider_stream_index: 0,
        kind: TrackTarget::Audio,
        codec_name: Some("aac".to_owned()),
        language: Some("eng".to_owned()),
        channels: Some(6),
        title: None,
        mime_type: None,
        filename: None,
        is_default: false,
        is_forced: false,
    };

    let matched = evaluate_filter(
        &TrackFilter::Channels {
            op: ComparisonOp::Gte,
            value: 6,
        },
        &stream,
    )
    .unwrap();

    assert!(matched);
}

#[test]
fn remux_font_filter_is_false_for_non_font_attachment() {
    let stream = SnapshotStreamFact {
        snapshot_stream_id: "stream-0".to_owned(),
        provider_stream_index: 0,
        kind: TrackTarget::Attachment,
        codec_name: None,
        language: None,
        channels: None,
        title: None,
        mime_type: None,
        filename: Some("cover.jpg".to_owned()),
        is_default: false,
        is_forced: false,
    };

    let matched = evaluate_filter(&TrackFilter::Font, &stream).unwrap();

    assert!(!matched);
}

fn audio_stream(language: Option<&str>) -> SnapshotStreamFact {
    SnapshotStreamFact {
        snapshot_stream_id: "stream-0".to_owned(),
        provider_stream_index: 0,
        kind: TrackTarget::Audio,
        codec_name: Some("aac".to_owned()),
        language: language.map(str::to_owned),
        channels: Some(2),
        title: None,
        mime_type: None,
        filename: None,
        is_default: false,
        is_forced: false,
    }
}

fn snapshot_with_streams(streams: &serde_json::Value) -> MediaSnapshotInput {
    MediaSnapshotInput {
        ordinal: 1,
        target: TargetRef::Synthetic {
            key: "media".to_owned(),
            kind: TargetKind::FileVersion,
        },
        container: None,
        stream_summary: json!({ "streams": streams }),
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
