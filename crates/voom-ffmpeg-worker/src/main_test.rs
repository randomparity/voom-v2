use std::path::PathBuf;

use voom_ffmpeg_worker::preflight::FfmpegPreflight;

use super::*;

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
            videotoolbox: None,
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

    let descriptor = accelerator_descriptor(&preflight);
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
