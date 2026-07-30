use std::path::PathBuf;

use voom_ffmpeg_worker::preflight::{FfmpegPreflight, VaapiPreflight};

use super::*;

fn vaapi_preflight() -> VaapiPreflight {
    VaapiPreflight {
        pci_address: "0000:f4:00.0".to_owned(),
        render_node: PathBuf::from("/dev/dri/renderD128"),
        device_name: "AMD Radeon 8060S Graphics".to_owned(),
        driver_version: "Mesa Gallium driver 26.1.5".to_owned(),
        max_sessions: 2,
        encoders: vec!["hevc_vaapi".to_owned()],
        decoders: vec!["h264".to_owned(), "av1".to_owned()],
        decoder_diagnostics: vec!["hevc: probe failed".to_owned()],
    }
}

/// The advertised descriptor is the probe's result, not the candidate list: a
/// decoder whose probe failed must not appear, or the scheduler would dispatch
/// work the device cannot do (ADR 0052 §2).
#[test]
fn vaapi_descriptor_advertises_only_what_the_probe_proved() {
    let descriptor = vaapi_accelerator_descriptor(vaapi_preflight());

    assert_eq!(descriptor.pci_address, "0000:f4:00.0");
    assert_eq!(descriptor.device_name, "AMD Radeon 8060S Graphics");
    assert_eq!(descriptor.driver_version, "Mesa Gallium driver 26.1.5");
    assert_eq!(descriptor.encoders, vec!["hevc_vaapi".to_owned()]);
    assert_eq!(
        descriptor.decoders,
        vec!["h264".to_owned(), "av1".to_owned()],
        "the codec whose probe failed must not be advertised"
    );
    assert_eq!(descriptor.max_sessions, 2);
}

fn nvidia_preflight() -> preflight::NvidiaPreflight {
    preflight::NvidiaPreflight {
        device_uuid: "GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned(),
        device_name: "RTX A6000".to_owned(),
        driver_version: "595.80".to_owned(),
        max_sessions: 1,
        decoders: Vec::new(),
        decoder_diagnostics: Vec::new(),
    }
}

fn preflight_with(
    nvidia: Option<preflight::NvidiaPreflight>,
    vaapi: Option<VaapiPreflight>,
) -> FfmpegPreflight {
    FfmpegPreflight {
        ffmpeg_path: PathBuf::from("/bin/ffmpeg-test"),
        ffprobe_path: PathBuf::from("/bin/ffprobe-test"),
        ffmpeg_version: "ffmpeg 7.1".to_owned(),
        ffprobe_version: "ffprobe 7.1".to_owned(),
        hevc_encoder: "libx265".to_owned(),
        svtav1_encoder: String::new(),
        libaom_encoder: "libaom-av1".to_owned(),
        aac_encoder: "aac".to_owned(),
        opus_encoder: "libopus".to_owned(),
        matroska_muxer: "matroska".to_owned(),
        mp4_muxer: "mp4".to_owned(),
        ogg_muxer: "ogg".to_owned(),
        nvidia,
        vaapi,
        videotoolbox: None,
    }
}

/// One worker binds one device. Advertising both would give the control plane a
/// descriptor for hardware it holds no claim on.
#[test]
fn advertising_two_backends_at_once_is_a_startup_failure() {
    assert!(matches!(
        bound_accelerator(&preflight_with(None, None)),
        Ok(None)
    ));
    assert!(matches!(
        bound_accelerator(&preflight_with(None, Some(vaapi_preflight()))),
        Ok(Some(AcceleratorBinding::Vaapi(_)))
    ));
    assert!(matches!(
        bound_accelerator(&preflight_with(Some(nvidia_preflight()), None)),
        Ok(Some(AcceleratorBinding::Nvidia(_)))
    ));
    assert!(
        bound_accelerator(&preflight_with(
            Some(nvidia_preflight()),
            Some(vaapi_preflight())
        ))
        .is_err()
    );
}

/// The advertised payload is the descriptor alone. The render node is a local
/// detail of the worker that resolved it — the control plane schedules on the PCI
/// address (ADR 0052 §1), so leaking a node path into the wire contract would
/// invite scheduling against an enumeration-order artifact.
#[test]
fn advertised_descriptor_tags_the_backend_and_omits_the_render_node() {
    let vaapi = advertised_accelerator(&AcceleratorBinding::Vaapi(vaapi_device_binding(
        vaapi_preflight(),
    )));
    let value = serde_json::to_value(&vaapi).unwrap();

    assert_eq!(value["backend"], "vaapi");
    assert_eq!(value["pci_address"], "0000:f4:00.0");
    assert!(value.get("render_node").is_none(), "{value}");

    let nvidia = advertised_accelerator(&AcceleratorBinding::Nvidia(nvidia_descriptor(
        nvidia_preflight(),
    )));
    assert!(matches!(nvidia, VideoAcceleratorDescriptor::Nvidia(_)));
}

/// The render node the probes ran on must reach the command builder: that path is
/// what `-vaapi_device` / `-hwaccel_device` name, and nothing downstream re-derives
/// it from the PCI address.
#[test]
fn the_bound_render_node_reaches_the_ffmpeg_config() {
    let preflight = preflight_with(None, Some(vaapi_preflight()));
    let binding = bound_accelerator(&preflight).unwrap();

    let config = ffmpeg_config_from_preflight(preflight, binding);

    assert_eq!(
        config.vaapi().map(|vaapi| vaapi.render_node.clone()),
        Some(PathBuf::from("/dev/dri/renderD128"))
    );
    assert!(config.has_video_encoder("hevc_vaapi"));
}

/// Capability tracks the loaded driver build, so a bound device whose probe encode
/// never proved `hevc_vaapi` must not advertise it (ADR 0052 §2).
#[test]
fn a_bound_device_with_no_proven_encoder_does_not_advertise_hevc_vaapi() {
    let mut vaapi = vaapi_preflight();
    vaapi.encoders = Vec::new();
    let preflight = preflight_with(None, Some(vaapi));
    let binding = bound_accelerator(&preflight).unwrap();

    let config = ffmpeg_config_from_preflight(preflight, binding);

    assert!(!config.has_video_encoder("hevc_vaapi"));
}

#[test]
fn ffmpeg_config_from_preflight_advertises_only_detected_video_encoders() {
    let config = ffmpeg_config_from_preflight(preflight_with(None, None), None);

    assert_eq!(config.ffmpeg_path, PathBuf::from("/bin/ffmpeg-test"));
    assert_eq!(config.ffprobe_path, PathBuf::from("/bin/ffprobe-test"));
    assert_eq!(config.provider_version, "ffmpeg 7.1");
    assert_eq!(config.process_timeout, DEFAULT_PROCESS_TIMEOUT);
    assert!(config.has_video_encoder("libx265"));
    assert!(!config.has_video_encoder("libsvtav1"));
    assert!(config.has_video_encoder("libaom-av1"));
    assert!(!config.has_video_encoder("hevc_vaapi"));
}

#[test]
fn videotoolbox_preflight_becomes_tagged_readiness_descriptor() {
    let mut preflight = FfmpegPreflight {
        ffmpeg_path: PathBuf::from("/bin/ffmpeg-test"),
        ffprobe_path: PathBuf::from("/bin/ffprobe-test"),
        ffmpeg_version: "ffmpeg 8.1".to_owned(),
        ffprobe_version: "ffprobe 8.1".to_owned(),
        hevc_encoder: "libx265".to_owned(),
        svtav1_encoder: "libsvtav1".to_owned(),
        libaom_encoder: "libaom-av1".to_owned(),
        aac_encoder: "aac".to_owned(),
        opus_encoder: "libopus".to_owned(),
        matroska_muxer: "matroska".to_owned(),
        mp4_muxer: "mp4".to_owned(),
        ogg_muxer: "ogg".to_owned(),
        nvidia: None,
        vaapi: None,
        videotoolbox: None,
    };
    preflight.videotoolbox = Some(preflight::VideoToolboxPreflight {
        resource_id: "0123456789abcdef".to_owned(),
        model_identifier: "Mac17,6".to_owned(),
        chip_name: "Apple M5 Max".to_owned(),
        macos_version: "26.5.2".to_owned(),
        macos_build: "25F90".to_owned(),
        max_sessions: 4,
        encoders: vec![
            "h264_videotoolbox".to_owned(),
            "hevc_videotoolbox".to_owned(),
        ],
        decoders: Vec::new(),
        decoder_diagnostics: Vec::new(),
    });

    let descriptor = bound_accelerator(&preflight)
        .unwrap()
        .as_ref()
        .map(advertised_accelerator);
    assert!(matches!(
        descriptor,
        Some(VideoAcceleratorDescriptor::VideoToolbox(_))
    ));
    if let Some(VideoAcceleratorDescriptor::VideoToolbox(descriptor)) = descriptor {
        assert_eq!(descriptor.hardware_token, "videotoolbox:0123456789abcdef");
        assert_eq!(descriptor.resource_id, "0123456789abcdef");
        assert_eq!(descriptor.max_sessions, 4);
    }
}
