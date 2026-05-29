use serde_json::json;
use voom_core::FileVersionId;
use voom_policy::MediaSnapshotInput;
use voom_policy::TargetRef;

use super::*;

async fn seeded_repo() -> (SqliteVideoProfileRepo, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    (SqliteVideoProfileRepo::new(pool), tmp)
}

fn inline_av1_settings() -> VideoProfileSettings {
    VideoProfileSettings {
        encoder: "libsvtav1".to_owned(),
        crf: 28,
        preset: "6".to_owned(),
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: None,
        max_height: None,
        output_container: Some("mp4".to_owned()),
        copy_compatible: None,
    }
}

#[tokio::test]
async fn resolves_named_profile_to_typed_settings() {
    let (repo, _tmp) = seeded_repo().await;
    let resolved = resolve_video_profile_ref(
        &repo,
        &voom_policy::VideoProfileRef::Named("hevc-archive".to_owned()),
    )
    .await
    .unwrap();
    assert_eq!(resolved.profile.name, "hevc-archive");
    assert_eq!(
        resolved.profile.pixel_format.as_deref(),
        Some("yuv420p10le")
    );
    assert_eq!(resolved.output_container, "mkv");
}

#[tokio::test]
async fn unknown_named_profile_is_config_invalid() {
    let (repo, _tmp) = seeded_repo().await;
    let err = resolve_video_profile_ref(
        &repo,
        &voom_policy::VideoProfileRef::Named("nope".to_owned()),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), "CONFIG_INVALID");
}

#[tokio::test]
async fn descriptor_invalid_named_profile_is_config_invalid() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    // crf 60 passes the migration's coarse `crf >= 0` CHECK but exceeds
    // libx265's descriptor crf_max (51): a row the SQL constraints accept yet
    // the encoder descriptor refuses.
    sqlx::query(
        "INSERT INTO video_profiles \
         (id, name, target_codec, encoder, crf, preset, output_container, copy_compatible) \
         VALUES ('vp-bad-crf', 'bad-crf', 'hevc', 'libx265', 60, 'medium', 'mkv', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let repo = SqliteVideoProfileRepo::new(pool);

    let err = resolve_video_profile_ref(
        &repo,
        &voom_policy::VideoProfileRef::Named("bad-crf".to_owned()),
    )
    .await
    .unwrap_err();

    assert_eq!(err.code(), "CONFIG_INVALID");
    assert!(err.to_string().contains("crf 60"));
}

#[tokio::test]
async fn inline_profile_gets_synthetic_identity() {
    let (repo, _tmp) = seeded_repo().await;
    let settings = inline_av1_settings(); // libsvtav1, crf 28, preset 6, mp4
    let resolved =
        resolve_video_profile_ref(&repo, &voom_policy::VideoProfileRef::Inline(settings))
            .await
            .unwrap();
    assert!(resolved.profile.name.starts_with("inline-"));
    assert_eq!(resolved.profile.target_codec, "av1");
    assert_eq!(resolved.output_container, "mp4");
}

// -- decide_copy_video tests --

fn profile_hevc_mp4_copy_compatible() -> TranscodeVideoProfile {
    TranscodeVideoProfile {
        name: "hevc-1080p".to_owned(),
        target_codec: "hevc".to_owned(),
        encoder: "libx265".to_owned(),
        crf: 23,
        preset: "medium".to_owned(),
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: Some(1920),
        max_height: Some(1080),
        copy_compatible: true,
    }
}

fn profile_hevc_mp4_no_copy() -> TranscodeVideoProfile {
    TranscodeVideoProfile {
        copy_compatible: false,
        ..profile_hevc_mp4_copy_compatible()
    }
}

fn snapshot_with_video(
    codec: &str,
    width: u32,
    height: u32,
    container: &str,
) -> MediaSnapshotInput {
    MediaSnapshotInput {
        ordinal: 1,
        target: TargetRef::FileVersion {
            id: FileVersionId(1),
        },
        container: Some(container.to_owned()),
        stream_summary: json!({
            "video_stream_count": 1,
            "streams": [{
                "kind": "video",
                "codec_name": codec,
                "width": width,
                "height": height,
                "pixel_format": "yuv420p"
            }]
        }),
        video_codec: Some(codec.to_owned()),
        width: Some(width),
        height: Some(height),
        hdr: None,
        bitrate: None,
        duration_millis: None,
        audio_languages: Vec::new(),
        subtitle_languages: Vec::new(),
        health_flags: Vec::new(),
        existing_media_snapshot_id: None,
    }
}

#[test]
fn copy_video_true_only_for_conforming_source_with_copy_compatible_profile() {
    let profile = profile_hevc_mp4_copy_compatible();
    // Source already hevc, within caps → true
    let conforming = snapshot_with_video("hevc", 1280, 720, "mkv");
    assert!(decide_copy_video(&profile, &conforming));
}

#[test]
fn copy_video_false_when_wrong_codec() {
    let profile = profile_hevc_mp4_copy_compatible();
    let h264_source = snapshot_with_video("h264", 1280, 720, "mkv");
    assert!(!decide_copy_video(&profile, &h264_source));
}

#[test]
fn copy_video_false_when_not_copy_compatible() {
    let profile = profile_hevc_mp4_no_copy();
    let conforming = snapshot_with_video("hevc", 1280, 720, "mkv");
    assert!(!decide_copy_video(&profile, &conforming));
}

#[test]
fn copy_video_false_when_exceeds_dimension_cap() {
    let profile = profile_hevc_mp4_copy_compatible();
    let oversized = snapshot_with_video("hevc", 3840, 2160, "mkv");
    assert!(!decide_copy_video(&profile, &oversized));
}

#[test]
fn copy_video_false_when_dimension_unknown_but_capped() {
    let profile = profile_hevc_mp4_copy_compatible();
    let mut snapshot = snapshot_with_video("hevc", 1280, 720, "mkv");
    snapshot.width = None; // unknown width
    assert!(!decide_copy_video(&profile, &snapshot));
}

#[test]
fn copy_video_false_when_pixel_format_constrained_and_mismatched() {
    let mut profile = profile_hevc_mp4_copy_compatible();
    profile.pixel_format = Some("yuv420p10le".to_owned());
    // source has yuv420p (8-bit) in stream, not 10-bit
    let snapshot = snapshot_with_video("hevc", 1280, 720, "mkv");
    assert!(!decide_copy_video(&profile, &snapshot));
}

#[test]
fn copy_video_true_when_pixel_format_matches_constraint() {
    let mut profile = profile_hevc_mp4_copy_compatible();
    profile.pixel_format = Some("yuv420p".to_owned());
    let snapshot = snapshot_with_video("hevc", 1280, 720, "mkv");
    assert!(decide_copy_video(&profile, &snapshot));
}

#[test]
fn h265_alias_is_recognized_as_hevc() {
    let profile = profile_hevc_mp4_copy_compatible();
    // ffprobe may report "h265" alias
    let h265_source = snapshot_with_video("h265", 1280, 720, "mkv");
    assert!(decide_copy_video(&profile, &h265_source));
}
