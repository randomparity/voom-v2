use serde_json::json;

use super::*;

#[test]
fn synthesize_payload_round_trips_target_channels() {
    let payload = AudioOperationPayload {
        operation_type: AudioOperationType::SynthesizeAudio,
        operation_id: Some("node-1".to_owned()),
        target_codec: "aac".to_owned(),
        container: "mkv".to_owned(),
        source_media_snapshot_id: Some(7),
        filter: None,
        outputs: None,
        companions: Some(vec![SynthesizeAudioCompanionDescriptor {
            companion_id: synthesis_companion_id("node-1", "stream-1"),
            source_snapshot_stream_id: "stream-1".to_owned(),
            source_provider_stream_index: 1,
            result_snapshot_stream_id: synthesis_companion_id("node-1", "stream-1"),
        }]),
        target_channels: Some(2),
    };
    let value = payload.clone().into_value();
    assert_eq!(value["type"], "synthesize_audio");
    assert_eq!(value["target_channels"], 2);
    let parsed = AudioOperationPayload::try_from_execution_value(&value).unwrap();
    assert_eq!(parsed, payload);
}

#[test]
fn synthesize_payload_requires_target_channels() {
    let value = json!({
        "type": "synthesize_audio",
        "target_codec": "aac",
        "container": "mkv",
        "source_media_snapshot_id": 7
    });
    assert!(AudioOperationPayload::try_from_execution_value(&value).is_err());
}

#[test]
fn synthesize_payload_requires_operation_and_companion_descriptors() {
    let value = json!({
        "type": "synthesize_audio",
        "target_codec": "aac",
        "container": "mkv",
        "target_channels": 2,
        "source_media_snapshot_id": 7
    });
    let error = AudioOperationPayload::try_from_execution_value(&value).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("requires `operation_id` and `companions`")
    );
}

#[test]
fn transcode_payload_omits_target_channels() {
    let value = AudioOperationPayload {
        operation_type: AudioOperationType::TranscodeAudio,
        operation_id: None,
        target_codec: "aac".to_owned(),
        container: "mkv".to_owned(),
        source_media_snapshot_id: Some(7),
        filter: None,
        outputs: None,
        companions: None,
        target_channels: None,
    }
    .into_value();
    assert!(value.get("target_channels").is_none());
}

#[test]
fn extract_output_id_pins_domain_separated_preimage() {
    assert_eq!(
        extract_output_id("node_0123456789abcdef", "audio-1"),
        "extract_output_ac3505382b9bfd4f"
    );
}

#[test]
fn synthesis_companion_id_pins_domain_separated_preimage() {
    assert_eq!(
        synthesis_companion_id("node_0123456789abcdef", "audio-1"),
        "synth_companion_26daba3dd2f8074c"
    );
}

#[test]
fn synthesize_payload_rejects_drifted_and_unordered_companions() {
    let operation_id = "node_0123456789abcdef";
    let mut value = json!({
        "type": "synthesize_audio",
        "operation_id": operation_id,
        "target_codec": "aac",
        "container": "mkv",
        "target_channels": 2,
        "source_media_snapshot_id": 7,
        "companions": [
            {
                "companion_id": synthesis_companion_id(operation_id, "audio-2"),
                "source_snapshot_stream_id": "audio-2",
                "source_provider_stream_index": 2,
                "result_snapshot_stream_id": synthesis_companion_id(operation_id, "audio-2")
            },
            {
                "companion_id": synthesis_companion_id(operation_id, "audio-1"),
                "source_snapshot_stream_id": "audio-1",
                "source_provider_stream_index": 1,
                "result_snapshot_stream_id": synthesis_companion_id(operation_id, "audio-1")
            }
        ]
    });
    assert!(AudioOperationPayload::try_from_execution_value(&value).is_err());

    value["companions"][0]["source_provider_stream_index"] = json!(1);
    value["companions"][1]["source_provider_stream_index"] = json!(2);
    value["companions"][0]["result_snapshot_stream_id"] = json!("drifted");
    assert!(AudioOperationPayload::try_from_execution_value(&value).is_err());
}

#[test]
fn extract_payload_round_trips_outputs_and_reads_legacy_singleton() {
    let legacy = json!({
        "type": "extract_audio",
        "target_codec": "opus",
        "container": "ogg",
        "source_media_snapshot_id": 7
    });
    let parsed = AudioOperationPayload::try_from_execution_value(&legacy).unwrap();
    assert_eq!(parsed.operation_id, None);
    assert_eq!(parsed.outputs, None);

    let value = AudioOperationPayload {
        operation_type: AudioOperationType::ExtractAudio,
        operation_id: Some("node_0123456789abcdef".to_owned()),
        target_codec: "opus".to_owned(),
        container: "ogg".to_owned(),
        source_media_snapshot_id: Some(7),
        filter: None,
        outputs: Some(vec![ExtractAudioOutputDescriptor {
            output_id: "extract_output_ac3505382b9bfd4f".to_owned(),
            source_snapshot_stream_id: "audio-1".to_owned(),
            source_provider_stream_index: 1,
            name_suffix: "audio-1.opus.ogg".to_owned(),
            bundle_role: AudioBundleRole::ExternalAudio,
        }]),
        companions: None,
        target_channels: None,
    }
    .into_value();

    let parsed = AudioOperationPayload::try_from_execution_value(&value).unwrap();
    assert_eq!(
        parsed.operation_id.as_deref(),
        Some("node_0123456789abcdef")
    );
    assert_eq!(parsed.outputs.as_ref().map(Vec::len), Some(1));
}
