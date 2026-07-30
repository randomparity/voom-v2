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

fn vaapi_descriptor() -> VaapiVideoAcceleratorDescriptor {
    VaapiVideoAcceleratorDescriptor {
        pci_address: "0000:03:00.0".to_owned(),
        device_name: "AMD Radeon RX 7600".to_owned(),
        driver_version: "Mesa Gallium driver 25.1.7".to_owned(),
        encoders: vec!["hevc_vaapi".to_owned()],
        decoders: vec!["h264".to_owned(), "hevc".to_owned(), "av1".to_owned()],
        max_sessions: 2,
    }
}

fn nvidia_descriptor() -> NvidiaVideoAcceleratorDescriptor {
    NvidiaVideoAcceleratorDescriptor {
        hardware_token: "nvidia:GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned(),
        device_uuid: "GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned(),
        device_name: "RTX A6000".to_owned(),
        driver_version: "595.80".to_owned(),
        encoders: vec!["hevc_nvenc".to_owned()],
        decoders: vec!["h264_cuvid".to_owned(), "hevc_cuvid".to_owned()],
        max_sessions: 4,
    }
}

/// A VAAPI descriptor is identified by PCI address, never by render-node
/// number, because enumeration order can renumber `/dev/dri/renderD*` across
/// boots while the address behind it cannot (ADR 0052 §2).
#[test]
fn vaapi_descriptor_round_trips_and_rejects_unknown_fields() {
    let descriptor = vaapi_descriptor();
    let mut value = serde_json::to_value(&descriptor).unwrap();
    assert_eq!(value["pci_address"], "0000:03:00.0");
    assert_eq!(
        serde_json::from_value::<VaapiVideoAcceleratorDescriptor>(value.clone()).unwrap(),
        descriptor
    );

    value["render_node"] = serde_json::json!("/dev/dri/renderD128");
    assert!(serde_json::from_value::<VaapiVideoAcceleratorDescriptor>(value).is_err());
}

/// The `backend` tag is what routes a payload to a backend-specific code path,
/// so a VAAPI requirement and assignment must both carry `"vaapi"` and survive
/// a round-trip — a mistagged payload would be dispatched to the wrong device.
#[test]
fn vaapi_requirement_and_assignment_are_tagged_vaapi_and_round_trip() {
    let requirement = VideoHardwareRequirement::vaapi("hevc_vaapi", Some("hevc".to_owned()));
    let value = serde_json::to_value(&requirement).unwrap();
    assert_eq!(value["backend"], "vaapi");
    assert_eq!(
        serde_json::from_value::<VideoHardwareRequirement>(value).unwrap(),
        requirement
    );

    let assignment = VideoHardwareAssignment::vaapi("vaapi:pci-0000:03:00.0", "0000:03:00.0");
    let value = serde_json::to_value(&assignment).unwrap();
    assert_eq!(value["backend"], "vaapi");
    assert_eq!(
        serde_json::from_value::<VideoHardwareAssignment>(value).unwrap(),
        assignment
    );
}

/// The scheduler leases a device by token and the worker verifies an assignment
/// against the device it bound, so both must derive the token from the PCI address
/// the same way. A drift here would look like every assignment naming the wrong
/// device.
#[test]
fn vaapi_hardware_token_is_derived_from_the_pci_address() {
    assert_eq!(
        vaapi_hardware_token("0000:f4:00.0"),
        "vaapi:pci-0000:f4:00.0"
    );
    assert_eq!(
        VideoHardwareAssignment::vaapi(vaapi_hardware_token("0000:03:00.0"), "0000:03:00.0"),
        VideoHardwareAssignment::vaapi("vaapi:pci-0000:03:00.0", "0000:03:00.0")
    );
}

/// An unknown field on a VAAPI payload is a producer/consumer version skew, and
/// silently dropping it would let a device-identity field go unnoticed.
#[test]
fn vaapi_requirement_and_assignment_reject_unknown_fields() {
    let mut value =
        serde_json::to_value(VideoHardwareRequirement::vaapi("hevc_vaapi", None)).unwrap();
    value["render_node"] = serde_json::json!("/dev/dri/renderD128");
    assert!(serde_json::from_value::<VideoHardwareRequirement>(value).is_err());

    let mut value = serde_json::to_value(VideoHardwareAssignment::vaapi(
        "vaapi:pci-0000:03:00.0",
        "0000:03:00.0",
    ))
    .unwrap();
    value["device_uuid"] = serde_json::json!("GPU-aaaaaaaa");
    assert!(serde_json::from_value::<VideoHardwareAssignment>(value).is_err());
}

/// An unrecognized `backend` must fail loudly rather than fall through to a
/// software default — issue #409 forbids silent software fallback.
#[test]
fn unknown_backend_tag_is_rejected_for_every_vocabulary() {
    let requirement = serde_json::json!({ "backend": "qsv", "encoder": "hevc_qsv" });
    assert!(serde_json::from_value::<VideoHardwareRequirement>(requirement).is_err());

    let assignment = serde_json::json!({ "backend": "qsv", "hardware_token": "qsv:0" });
    assert!(serde_json::from_value::<VideoHardwareAssignment>(assignment).is_err());

    let mut descriptor = serde_json::to_value(vaapi_descriptor()).unwrap();
    descriptor["backend"] = serde_json::json!("qsv");
    assert!(serde_json::from_value::<VideoAcceleratorDescriptor>(descriptor).is_err());
}

/// Pins the exact bytes of every pre-#409 payload. A field rename, reorder, or
/// newly emitted field would break a worker binary on the other side of the
/// change, so this test is the guard that the VAAPI slice stayed additive for
/// the software and NVIDIA vocabularies.
#[test]
fn software_and_nvidia_payloads_are_byte_for_byte_unchanged() {
    assert_eq!(
        serde_json::to_string(&VideoHardwareRequirement::software()).unwrap(),
        r#"{"backend":"software"}"#
    );
    assert_eq!(
        serde_json::to_string(&VideoHardwareRequirement::nvidia(
            "hevc_nvenc",
            Some("h264_cuvid".to_owned())
        ))
        .unwrap(),
        r#"{"backend":"nvidia","encoder":"hevc_nvenc","decoder":"h264_cuvid"}"#
    );
    assert_eq!(
        serde_json::to_string(&VideoHardwareRequirement::nvidia("hevc_nvenc", None)).unwrap(),
        r#"{"backend":"nvidia","encoder":"hevc_nvenc"}"#
    );
    assert_eq!(
        serde_json::to_string(&VideoHardwareAssignment::software()).unwrap(),
        r#"{"backend":"software"}"#
    );
    assert_eq!(
        serde_json::to_string(&VideoHardwareAssignment::nvidia(
            "nvidia:GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
        ))
        .unwrap(),
        concat!(
            r#"{"backend":"nvidia","#,
            r#""hardware_token":"nvidia:GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","#,
            r#""device_uuid":"GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"}"#
        )
    );
    assert_eq!(
        serde_json::to_string(&nvidia_descriptor()).unwrap(),
        concat!(
            r#"{"hardware_token":"nvidia:GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","#,
            r#""device_uuid":"GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","#,
            r#""device_name":"RTX A6000","driver_version":"595.80","#,
            r#""encoders":["hevc_nvenc"],"decoders":["h264_cuvid","hevc_cuvid"],"#,
            r#""max_sessions":4}"#
        )
    );
}

/// The descriptor enum is the retyped `LocalWorkerBound.accelerator` payload
/// (ADR 0013 coordinated change). The NVIDIA content struct itself is unchanged,
/// so a descriptor already stored bare in `worker_capabilities.extra` still
/// deserializes without the tag.
#[test]
fn accelerator_descriptor_enum_tags_each_backend_and_keeps_nvidia_content_readable() {
    let nvidia = VideoAcceleratorDescriptor::Nvidia(nvidia_descriptor());
    let value = serde_json::to_value(&nvidia).unwrap();
    assert_eq!(value["backend"], "nvidia");
    assert_eq!(
        serde_json::from_value::<VideoAcceleratorDescriptor>(value.clone()).unwrap(),
        nvidia
    );

    let mut bare = value;
    assert_eq!(
        bare["hardware_token"],
        "nvidia:GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
    );
    bare.as_object_mut().unwrap().remove("backend");
    assert_eq!(
        serde_json::from_value::<NvidiaVideoAcceleratorDescriptor>(bare).unwrap(),
        nvidia_descriptor()
    );

    let vaapi = VideoAcceleratorDescriptor::Vaapi(vaapi_descriptor());
    let value = serde_json::to_value(&vaapi).unwrap();
    assert_eq!(value["backend"], "vaapi");
    assert_eq!(
        serde_json::from_value::<VideoAcceleratorDescriptor>(value).unwrap(),
        vaapi
    );
}

/// `LocalWorkerBound` is the worker's readiness handshake; the control plane
/// must be able to tell which backend the worker bound itself to, which is why
/// `accelerator` carries the tagged descriptor rather than the NVIDIA struct.
#[test]
fn local_worker_bound_carries_a_tagged_accelerator_descriptor() {
    let bound = LocalWorkerBound {
        addr: "127.0.0.1:9000".parse().unwrap(),
        accelerator: Some(VideoAcceleratorDescriptor::Vaapi(vaapi_descriptor())),
    };
    let value = serde_json::to_value(&bound).unwrap();
    assert_eq!(value["accelerator"]["backend"], "vaapi");
    assert_eq!(
        serde_json::from_value::<LocalWorkerBound>(value).unwrap(),
        bound
    );

    let software = LocalWorkerBound {
        addr: "127.0.0.1:9000".parse().unwrap(),
        accelerator: None,
    };
    assert_eq!(
        serde_json::to_string(&software).unwrap(),
        r#"{"addr":"127.0.0.1:9000"}"#
    );
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

#[test]
fn vaapi_supervisor_budget_outlasts_the_worker_readiness_deadline() {
    // The supervisor starts timing at spawn and the worker inside its own
    // preflight, so the supervisor's elapsed time always exceeds the worker's. If
    // these two ever became equal the supervisor would abandon the child first and
    // report a generic bound-address timeout, and the worker's expiry — the only
    // message naming the stage that did not prove — would be unreachable through
    // `voom worker run-local --vaapi-device`.
    assert!(
        VAAPI_PREFLIGHT_BUDGET > VAAPI_READINESS_DEADLINE,
        "supervisor budget {VAAPI_PREFLIGHT_BUDGET:?} must exceed worker deadline \
         {VAAPI_READINESS_DEADLINE:?}"
    );
    assert_eq!(
        VAAPI_PREFLIGHT_BUDGET.checked_sub(VAAPI_READINESS_DEADLINE),
        Some(Duration::from_secs(VAAPI_PREFLIGHT_COORDINATION_SECONDS))
    );
}
