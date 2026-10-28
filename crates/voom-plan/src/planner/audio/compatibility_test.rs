use super::*;
use serde_json::{Value, json};
use voom_policy::{MediaSnapshotInput, TargetKind, TargetRef};

fn audio(id: &str, index: u32, language: Option<&str>) -> Value {
    let mut stream = json!({
        "id": id, "index": index, "kind": "audio", "codec_name": "flac",
        "channels": 2, "channel_layout": "stereo",
        "disposition": {"commentary": false, "default": false, "forced": false}
    });
    if let Some(language) = language {
        stream["language"] = json!(language);
    }
    stream
}

fn snapshot(audio: Vec<Value>) -> MediaSnapshotInput {
    let mut streams =
        vec![json!({"id": "video", "index": 0, "kind": "video", "codec_name": "h264"})];
    streams.extend(audio);
    MediaSnapshotInput {
        ordinal: 0,
        target: TargetRef::Synthetic {
            key: "compatibility".to_owned(),
            kind: TargetKind::MediaVariant,
        },
        container: Some("mkv".to_owned()),
        stream_summary: json!({"streams": streams, "video_stream_count": 1}),
        video_codec: None,
        width: None,
        height: None,
        hdr: None,
        bitrate: None,
        duration_millis: None,
        audio_languages: Vec::new(),
        subtitle_languages: Vec::new(),
        health_flags: Vec::new(),
        existing_media_snapshot_id: Some(voom_core::MediaSnapshotId(7)),
    }
}

#[test]
fn compatibility_ranking_is_stable() {
    let mut commentary = audio("commentary", 1, Some("eng"));
    commentary["disposition"]["commentary"] = json!(true);
    let candidates = vec![
        audio("late", 8, Some("eng")),
        audio("unknown", 2, None),
        audio("foreign", 3, Some("fra")),
        commentary,
        audio("primary", 5, Some("eng")),
    ];
    for candidates in [candidates.clone(), candidates.into_iter().rev().collect()] {
        assert_eq!(
            resolve_audio_compatibility(&snapshot(candidates))
                .unwrap()
                .source_snapshot_stream_id,
            "primary"
        );
    }
    assert_eq!(
        resolve_audio_compatibility(&snapshot(vec![audio("unknown", 2, None)]))
            .unwrap()
            .source_snapshot_stream_id,
        "unknown"
    );
    assert_eq!(
        resolve_audio_compatibility(&snapshot(vec![audio("foreign", 1, Some("fra"))])),
        Err(AudioCompatibilityError::NoEligibleSource)
    );
}

#[test]
fn compatibility_target_matrix_is_exact() {
    for (channels, layouts) in [
        (1, vec!["mono"]),
        (2, vec!["stereo"]),
        (3, vec!["2.1", "3.0", "3.0(back)"]),
        (
            4,
            vec![
                "3.1",
                "4.0",
                "quad",
                "quad(side)",
                "4 channels (FL+FR+LFE+BC)",
            ],
        ),
        (5, vec!["4.1", "5.0", "5.0(side)"]),
        (6, vec!["5.1", "5.1(side)"]),
        (8, vec!["7.1"]),
    ] {
        for layout in layouts {
            let target = compatibility_target(channels, layout).unwrap();
            assert_eq!(target.codec, if channels <= 2 { "aac" } else { "eac3" });
            assert_eq!(target.channels, if channels == 8 { 6 } else { channels });
            assert_eq!(
                target.channel_layout,
                match layout {
                    "quad" => "quad(side)",
                    "5.0" => "5.0(side)",
                    "5.1" | "7.1" => "5.1(side)",
                    other => other,
                }
            );
        }
    }
    for (channels, layout) in [
        (0, "stereo"),
        (2, "mono"),
        (6, "stereo"),
        (7, "6.1"),
        (8, "7.1(wide)"),
        (9, "unknown"),
    ] {
        assert!(matches!(
            compatibility_target(channels, layout),
            Err(AudioCompatibilityError::UnsupportedLayout { .. })
        ));
    }
}

#[test]
fn compatibility_rejects_invalid_facts_and_associations() {
    let original = audio("source", 1, Some("eng"));
    for (field, invalid) in [
        ("channels", json!(-1)),
        ("channels", json!(u64::MAX)),
        ("index", json!(u64::MAX)),
        ("language", json!(42)),
        ("language", json!("EN")),
    ] {
        let mut stream = original.clone();
        stream[field] = invalid;
        assert!(
            matches!(
                resolve_audio_compatibility(&snapshot(vec![stream])),
                Err(AudioCompatibilityError::InvalidFacts(_))
            ),
            "invalid {field}"
        );
    }
    let mut missing = original.clone();
    missing.as_object_mut().unwrap().remove("channel_layout");
    assert!(matches!(
        resolve_audio_compatibility(&snapshot(vec![missing])),
        Err(AudioCompatibilityError::InvalidFacts(_))
    ));
    let mut marked = original.clone();
    marked["compatibility_invalid_language"] = json!(true);
    assert!(resolve_audio_compatibility(&snapshot(vec![marked])).is_err());
    let mut duplicate = original.clone();
    duplicate["index"] = json!(2);
    assert!(resolve_audio_compatibility(&snapshot(vec![original.clone(), duplicate])).is_err());
    let mut derived = audio("derived", 2, Some("eng"));
    derived["compatibility_source_snapshot_stream_id"] = json!("absent");
    assert!(matches!(
        resolve_audio_compatibility(&snapshot(vec![original.clone(), derived.clone()])),
        Err(AudioCompatibilityError::InvalidAssociation(_))
    ));
    derived["compatibility_source_snapshot_stream_id"] = json!("source");
    derived["codec_name"] = json!("aac");
    let mut second = derived.clone();
    second["id"] = json!("second");
    second["index"] = json!(3);
    assert!(matches!(
        resolve_audio_compatibility(&snapshot(vec![original, derived, second])),
        Err(AudioCompatibilityError::InvalidAssociation(_))
    ));
    // Corrupt numeric facts cannot turn into domain absence, even on a foreign track.
    let mut corrupt_foreign = audio("foreign", 1, Some("fra"));
    corrupt_foreign["channels"] = json!(-1);
    assert!(matches!(
        resolve_audio_compatibility(&snapshot(vec![corrupt_foreign])),
        Err(AudioCompatibilityError::InvalidFacts(_))
    ));
}

#[test]
fn compatibility_reuses_only_resolved_identity() {
    let original = audio("source", 3, Some("eng"));
    let mut peer = audio("peer", 4, Some("eng"));
    peer["codec_name"] = json!("aac");
    let resolved =
        resolve_audio_compatibility(&snapshot(vec![original.clone(), peer.clone()])).unwrap();
    assert!(
        resolved.resolved_snapshot_stream_id.is_none(),
        "another track cannot stand in for the ranked source"
    );
    peer["compatibility_source_snapshot_stream_id"] = json!("source");
    peer["index"] = json!(1);
    let resolved = resolve_audio_compatibility(&snapshot(vec![peer.clone(), original])).unwrap();
    assert_eq!(
        resolved.source_snapshot_stream_id, "source",
        "placed companions are excluded from original ranking"
    );
    assert_eq!(
        resolved.resolved_snapshot_stream_id.as_deref(),
        Some("peer")
    );
    let mut compatible = audio("existing", 1, Some("eng"));
    compatible["codec_name"] = json!("aac");
    assert_eq!(
        resolve_audio_compatibility(&snapshot(vec![compatible]))
            .unwrap()
            .resolved_snapshot_stream_id
            .as_deref(),
        Some("existing")
    );
}
