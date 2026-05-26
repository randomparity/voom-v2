use voom_policy::{MediaSnapshotInput, TargetKind, TargetRef, TrackFilter};

use super::{
    AudioBundleRole, AudioPlanningBlock, SnapshotAudioStreamFact, evaluate_audio_filter,
    extraction_role, stream_facts,
};

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
                "commentary": true
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
            disposition: super::AudioDispositionFact {
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

fn audio_fact(commentary: Option<bool>) -> SnapshotAudioStreamFact {
    SnapshotAudioStreamFact {
        snapshot_stream_id: "stream-1".to_owned(),
        provider_stream_index: 1,
        codec: Some("aac".to_owned()),
        language: Some("eng".to_owned()),
        title: None,
        channels: Some(2),
        default: false,
        disposition: super::AudioDispositionFact {
            default: false,
            forced: false,
            commentary,
        },
        commentary,
    }
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
