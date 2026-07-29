use super::*;

#[test]
fn nvidia_descriptor_round_trips_and_rejects_unknown_fields() {
    let descriptor = NvidiaVideoAcceleratorDescriptor {
        hardware_token: "nvidia:GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned(),
        device_uuid: "GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned(),
        device_name: "RTX A6000".to_owned(),
        driver_version: "595.80".to_owned(),
        encoders: vec!["hevc_nvenc".to_owned()],
        decoders: vec!["h264_cuvid".to_owned(), "hevc_cuvid".to_owned()],
        max_sessions: 4,
    };
    let mut value = serde_json::to_value(&descriptor).unwrap();
    assert_eq!(
        serde_json::from_value::<NvidiaVideoAcceleratorDescriptor>(value.clone()).unwrap(),
        descriptor
    );

    value["ordinal"] = serde_json::json!(0);
    assert!(serde_json::from_value::<NvidiaVideoAcceleratorDescriptor>(value).is_err());
}

#[test]
fn requirements_are_tagged_and_strict() {
    let requirement = VideoHardwareRequirement::nvidia("hevc_nvenc", Some("h264_cuvid".to_owned()));
    let mut value = serde_json::to_value(&requirement).unwrap();
    assert_eq!(value["backend"], "nvidia");

    value["gpu"] = serde_json::json!(0);
    assert!(serde_json::from_value::<VideoHardwareRequirement>(value).is_err());
}
