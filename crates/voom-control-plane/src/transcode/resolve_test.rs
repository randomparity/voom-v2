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
