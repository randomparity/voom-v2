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

#[test]
fn videotoolbox_descriptor_and_assignment_are_tagged_and_strict() {
    let descriptor =
        VideoAcceleratorDescriptor::VideoToolbox(VideoToolboxVideoAcceleratorDescriptor {
            hardware_token: "videotoolbox:abc123".to_owned(),
            resource_id: "abc123".to_owned(),
            model_identifier: "Mac17,6".to_owned(),
            chip_name: "Apple M5 Max".to_owned(),
            macos_version: "26.5.2".to_owned(),
            macos_build: "25F84".to_owned(),
            encoders: vec![
                "h264_videotoolbox".to_owned(),
                "hevc_videotoolbox".to_owned(),
            ],
            decoders: vec![VideoToolboxDecodeCapability {
                codec: "h264".to_owned(),
                pixel_formats: vec!["yuv420p".to_owned(), "nv12".to_owned()],
            }],
            max_sessions: 16,
        });
    let value = serde_json::to_value(&descriptor).unwrap();
    assert_eq!(value["backend"], "video_toolbox");
    assert_eq!(
        serde_json::from_value::<VideoAcceleratorDescriptor>(value).unwrap(),
        descriptor
    );

    let assignment = VideoHardwareAssignment::video_toolbox("videotoolbox:abc123", "abc123");
    let mut value = serde_json::to_value(&assignment).unwrap();
    assert_eq!(value["backend"], "video_toolbox");
    value["device_uuid"] = serde_json::json!("raw-platform-uuid");
    assert!(serde_json::from_value::<VideoHardwareAssignment>(value).is_err());
}

#[test]
fn videotoolbox_requirement_carries_exact_source_capability() {
    let requirement = VideoHardwareRequirement::video_toolbox(
        "hevc_videotoolbox",
        Some(VideoToolboxDecodeRequirement {
            codec: "av1".to_owned(),
            pixel_format: "p010le".to_owned(),
        }),
    );
    let value = serde_json::to_value(&requirement).unwrap();
    assert_eq!(value["backend"], "video_toolbox");
    assert_eq!(value["decoder"]["codec"], "av1");
    assert_eq!(value["decoder"]["pixel_format"], "p010le");
}
