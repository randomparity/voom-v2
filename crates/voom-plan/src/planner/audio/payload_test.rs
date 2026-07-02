use serde_json::json;

use super::*;

#[test]
fn synthesize_payload_round_trips_target_channels() {
    let payload = AudioOperationPayload {
        operation_type: AudioOperationType::SynthesizeAudio,
        target_codec: "aac".to_owned(),
        container: "mkv".to_owned(),
        source_media_snapshot_id: Some(7),
        filter: None,
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
fn transcode_payload_omits_target_channels() {
    let value = AudioOperationPayload {
        operation_type: AudioOperationType::TranscodeAudio,
        target_codec: "aac".to_owned(),
        container: "mkv".to_owned(),
        source_media_snapshot_id: Some(7),
        filter: None,
        target_channels: None,
    }
    .into_value();
    assert!(value.get("target_channels").is_none());
}
