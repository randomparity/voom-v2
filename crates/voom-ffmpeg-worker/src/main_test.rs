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
/// work the device cannot do (ADR 0051 §2).
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

/// One worker binds one device. Advertising both would give the control plane a
/// descriptor for hardware it holds no claim on.
#[test]
fn advertising_two_backends_at_once_is_a_startup_failure() {
    assert!(matches!(advertised_accelerator(None, None), Ok(None)));
    let vaapi = vaapi_accelerator_descriptor(vaapi_preflight());
    assert!(matches!(
        advertised_accelerator(None, Some(vaapi.clone())),
        Ok(Some(VideoAcceleratorDescriptor::Vaapi(_)))
    ));

    let nvidia = accelerator_descriptor(preflight::NvidiaPreflight {
        device_uuid: "GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned(),
        device_name: "RTX A6000".to_owned(),
        driver_version: "595.80".to_owned(),
        max_sessions: 1,
        decoders: Vec::new(),
        decoder_diagnostics: Vec::new(),
    });
    assert!(matches!(
        advertised_accelerator(Some(nvidia.clone()), None),
        Ok(Some(VideoAcceleratorDescriptor::Nvidia(_)))
    ));
    assert!(advertised_accelerator(Some(nvidia), Some(vaapi)).is_err());
}

#[test]
fn ffmpeg_config_from_preflight_advertises_only_detected_video_encoders() {
    let config = ffmpeg_config_from_preflight(
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
            nvidia: None,
            vaapi: None,
        },
        None,
    );

    assert_eq!(config.ffmpeg_path, PathBuf::from("/bin/ffmpeg-test"));
    assert_eq!(config.ffprobe_path, PathBuf::from("/bin/ffprobe-test"));
    assert_eq!(config.provider_version, "ffmpeg 7.1");
    assert_eq!(config.process_timeout, DEFAULT_PROCESS_TIMEOUT);
    assert!(config.has_video_encoder("libx265"));
    assert!(!config.has_video_encoder("libsvtav1"));
    assert!(config.has_video_encoder("libaom-av1"));
}
