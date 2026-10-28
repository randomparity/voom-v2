use super::*;

#[test]
fn stored_compatibility_corruption_is_database_error() {
    let payload = json!({"media_dispatch": {"operation": "transcode_audio", "settings": {
        "compatibility": {"source_channels": -1}
    }}});
    let error = stored_compatibility_settings(&payload).unwrap_err();
    assert!(matches!(error, VoomError::Database { .. }));
    assert!(
        stored_compatibility_settings(&json!({"media_dispatch": {"operation": "remux"}}))
            .unwrap()
            .is_none()
    );
}

#[test]
fn required_reprobe_rejects_unusable_media_facts() {
    for snapshot in [
        json!({"streams": [{"index": 0, "kind": "video", "codec_name": "hevc"}]}),
        json!({"container": {"format_name": "matroska"}, "streams": [{"index": 0, "codec_name": "hevc"}]}),
        json!({"container": {"format_name": "matroska"}, "streams": [{"index": 0, "kind": "video"}]}),
        json!({"container": {"format_name": "matroska"}, "streams": [{"index": 0, "kind": "audio"}]}),
    ] {
        assert!(
            normalized_snapshot(snapshot).is_err(),
            "a required reprobe must be usable by the next phase"
        );
    }
}

#[test]
fn required_reprobe_preserves_metadata_and_assigns_stable_stream_ids() {
    let normalized = normalized_snapshot(json!({
        "container": {"format_name": "matroska"},
        "streams": [{"index": 2, "kind": "audio", "codec_name": "aac", "channels": 2}],
        "provider_extension": {"value": true},
    }))
    .unwrap();
    assert_eq!(normalized["streams"][0]["id"], "stream-2");
    assert_eq!(normalized["streams"][0]["channels"], 2);
    assert_eq!(normalized["provider_extension"]["value"], true);
    assert!(normalized_snapshot(json!({
        "container": {"format_name": "matroska"},
        "streams": [{"index": 1, "kind": "audio", "codec_name": "aac"}, {"index": 1, "kind": "audio", "codec_name": "aac"}],
    })).is_err());
}

#[test]
fn mutation_result_rejects_a_changed_worker_source() {
    let raw = json!({
        "status": "remuxed", "provider": "test", "provider_version": "1",
        "input_pre": {"size_bytes": 1, "content_hash": "before"},
        "input_post": {"size_bytes": 1, "content_hash": "after"},
        "output": {"size_bytes": 2, "content_hash": "output"},
        "output_container": "mkv", "kept_snapshot_stream_ids": ["stream-0"], "default_snapshot_stream_ids": [],
    });
    assert!(
        validate_worker_result(OperationKind::Remux, &raw).is_err(),
        "the worker and owner source facts must describe an unchanged input"
    );
}

#[test]
fn synthesis_probe_comparison_uses_exact_admitted_companion_projection() {
    use voom_store::repo::media::audio_synthesis_operations::AudioSynthesisCompanion;
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/owner-publication-synthesis-probe.json"
    ))
    .unwrap();
    let output: voom_worker_protocol::OwnerObservedOutput =
        serde_json::from_value(fixture["original_output"].clone()).unwrap();
    let probe: OwnerProbeEvidence =
        serde_json::from_value(fixture["canonical_probe"].clone()).unwrap();
    let result_id = fixture["worker_result"]["selected_output_streams"][0]["snapshot_stream_id"]
        .as_str()
        .unwrap();
    let companions = [AudioSynthesisCompanion {
        id: 1,
        operation_id: 1,
        ordinal: 0,
        companion_id: "companion".to_owned(),
        source_snapshot_stream_id: "stream-1".to_owned(),
        source_provider_stream_index: 1,
        result_snapshot_stream_id: result_id.to_owned(),
        result_provider_stream_index: Some(2),
        codec: Some("aac".to_owned()),
        channels: Some(2),
        language: Some("eng".to_owned()),
        title: None,
        disposition_default: Some(true),
        disposition_forced: Some(false),
        disposition_commentary: Some(false),
        lineage_id: None,
    }];
    validate_original_probe(
        &output,
        &probe,
        Some(&companions),
        &fixture["worker_result"],
        None,
    )
    .unwrap();
    assert!(
        validate_original_probe(&output, &probe, None, &fixture["worker_result"], None).is_err(),
        "a stable companion ID requires the exact admitted operation anchor"
    );
    let mut wrong_worker = probe.clone();
    wrong_worker.worker_id = WorkerId(probe.worker_id.0 + 1);
    assert!(
        validate_original_probe(
            &output,
            &wrong_worker,
            Some(&companions),
            &fixture["worker_result"],
            None,
        )
        .is_err()
    );
    let mut wrong_stream = probe.clone();
    wrong_stream.result.snapshot["streams"][2]["id"] = json!("foreign-companion");
    assert!(
        validate_original_probe(
            &output,
            &wrong_stream,
            Some(&companions),
            &fixture["worker_result"],
            None,
        )
        .is_err()
    );
    let mut wrong_facts = probe.clone();
    wrong_facts.result.snapshot["streams"][2]["channels"] = json!(6);
    assert!(
        validate_original_probe(
            &output,
            &wrong_facts,
            Some(&companions),
            &fixture["worker_result"],
            None,
        )
        .is_err()
    );
    let mut wrong_companions = companions.clone();
    wrong_companions[0].result_snapshot_stream_id = "foreign-operation-companion".to_owned();
    assert!(
        validate_original_probe(
            &output,
            &probe,
            Some(&wrong_companions),
            &fixture["worker_result"],
            None,
        )
        .is_err()
    );
}

#[test]
fn owner_result_projection_keeps_unknown_worker_fields_and_original_facts() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/owner-publication-synthesis-probe.json"
    ))
    .unwrap();
    let mut complete = fixture["worker_result"].clone();
    complete["agent_observed"] = json!({"owner": "extension"});
    complete["artifact_access"] = json!({"access": "extension"});
    let original = complete.clone();
    let projected = owner_worker_result(&complete).unwrap();
    serde_json::from_value::<voom_worker_protocol::TranscodeAudioResult>(projected.clone())
        .unwrap();
    assert_eq!(projected, fixture["worker_result"]);
    assert_eq!(
        complete, original,
        "projection must not rewrite retained completion evidence"
    );
    complete["unexpected_worker_field"] = json!(true);
    assert!(
        serde_json::from_value::<voom_worker_protocol::TranscodeAudioResult>(
            owner_worker_result(&complete).unwrap()
        )
        .is_err(),
        "owner projection must not erase an unaccepted worker field"
    );
}
