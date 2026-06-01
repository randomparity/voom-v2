use std::path::PathBuf;

use voom_ffmpeg_worker::preflight::FfmpegPreflight;

use super::*;

#[test]
fn ffmpeg_config_from_preflight_advertises_only_detected_video_encoders() {
    let config = ffmpeg_config_from_preflight(FfmpegPreflight {
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
    });

    assert_eq!(config.ffmpeg_path, PathBuf::from("/bin/ffmpeg-test"));
    assert_eq!(config.ffprobe_path, PathBuf::from("/bin/ffprobe-test"));
    assert_eq!(config.provider_version, "ffmpeg 7.1");
    assert_eq!(config.process_timeout, DEFAULT_PROCESS_TIMEOUT);
    assert!(config.has_video_encoder("libx265"));
    assert!(!config.has_video_encoder("libsvtav1"));
    assert!(config.has_video_encoder("libaom-av1"));
}
