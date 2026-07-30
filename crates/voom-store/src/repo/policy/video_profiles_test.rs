use super::*;
use crate::test_support::fresh_initialized_pool_at;

async fn repo() -> (
    SqliteVideoProfileRepo,
    sqlx::SqlitePool,
    voom_test_support::TempDatabase,
) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (SqliteVideoProfileRepo::new(pool.clone()), pool, tmp)
}

#[tokio::test]
async fn lists_all_seeded_builtins() {
    let (repo, _pool, _tmp) = repo().await;
    let profiles = repo.list().await.unwrap();
    let names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"default-hevc"));
    assert!(names.contains(&"av1-1080p"));
    assert_eq!(profiles.len(), 6);
}

#[tokio::test]
async fn every_seeded_builtin_is_valid_against_its_descriptor() {
    let (repo, _pool, _tmp) = repo().await;
    for profile in repo.list().await.unwrap() {
        let typed = profile.to_worker_profile();
        voom_core::validate_profile_against_descriptor(&typed)
            .unwrap_or_else(|e| panic!("seed `{}` invalid: {e}", profile.name));
    }
}

#[tokio::test]
async fn get_by_name_returns_profile_or_none() {
    let (repo, _pool, _tmp) = repo().await;
    let hit = repo.get_by_name("hevc-archive").await.unwrap().unwrap();
    assert_eq!(hit.codec_profile.as_deref(), Some("main10"));
    assert_eq!(hit.pixel_format.as_deref(), Some("yuv420p10le"));
    assert!(repo.get_by_name("does-not-exist").await.unwrap().is_none());
}

fn sample_new(name: &str) -> NewVideoProfile {
    NewVideoProfile {
        name: name.to_owned(),
        encoder: "libx265".to_owned(),
        crf: Some(22),
        cq: None,
        qp: None,
        preset: Some("slow".to_owned()),
        tune: None,
        codec_profile: Some("main10".to_owned()),
        codec_level: None,
        pixel_format: Some("yuv420p10le".to_owned()),
        max_width: Some(1920),
        max_height: Some(1080),
        output_container: "mkv".to_owned(),
        copy_compatible: false,
        decode: voom_core::VideoDecodeMode::default(),
    }
}

#[tokio::test]
async fn create_derives_target_codec_and_persists() {
    let (repo, _pool, _tmp) = repo().await;
    let created = repo.create(sample_new("home-hevc")).await.unwrap();
    assert_eq!(created.id, "vp-home-hevc");
    assert_eq!(created.target_codec, "hevc");
    let fetched = repo.get_by_name("home-hevc").await.unwrap().unwrap();
    assert_eq!(fetched, created);
    voom_core::validate_profile_against_descriptor(&fetched.to_worker_profile()).unwrap();
}

#[tokio::test]
async fn create_rejects_field_outside_encoder_vocabulary() {
    let (repo, _pool, _tmp) = repo().await;
    let mut bad = sample_new("bad-crf");
    bad.crf = Some(60); // outside libx265 0..=51
    let err = repo.create(bad).await.unwrap_err();
    assert_eq!(err.code(), "CONFIG_INVALID");
}

#[tokio::test]
async fn create_rejects_empty_name() {
    let (repo, _pool, _tmp) = repo().await;
    let mut bad = sample_new("placeholder");
    bad.name = "  ".to_owned();
    let err = repo.create(bad).await.unwrap_err();
    assert_eq!(err.code(), "CONFIG_INVALID");
}

#[tokio::test]
async fn create_rejects_duplicate_name_as_conflict() {
    let (repo, _pool, _tmp) = repo().await;
    repo.create(sample_new("dup")).await.unwrap();
    let err = repo.create(sample_new("dup")).await.unwrap_err();
    assert_eq!(err.code(), "CONFLICT");
}

#[tokio::test]
async fn update_replaces_fields_and_missing_name_is_none() {
    let (repo, _pool, _tmp) = repo().await;
    repo.create(sample_new("editme")).await.unwrap();
    let mut changed = sample_new("editme");
    changed.crf = Some(30);
    changed.encoder = "libsvtav1".to_owned();
    changed.preset = Some("8".to_owned());
    changed.codec_profile = Some("main".to_owned());
    changed.pixel_format = Some("yuv420p10le".to_owned());
    let updated = repo.update(changed).await.unwrap().unwrap();
    assert_eq!(updated.crf, Some(30));
    assert_eq!(updated.target_codec, "av1");
    assert!(repo.update(sample_new("ghost")).await.unwrap().is_none());
}

#[tokio::test]
async fn create_persists_nvidia_quality_and_decode_mode() {
    let (repo, _pool, _tmp) = repo().await;
    let mut profile = sample_new("nvidia-hevc");
    profile.encoder = "hevc_nvenc".to_owned();
    profile.crf = None;
    profile.cq = Some(23);
    profile.preset = Some("p4".to_owned());
    profile.tune = Some("uhq".to_owned());
    profile.decode = voom_core::VideoDecodeMode::nvidia();

    let created = repo.create(profile).await.unwrap();

    assert_eq!(created.cq, Some(23));
    assert!(created.crf.is_none());
    assert!(created.decode.is_nvidia());
    assert_eq!(
        repo.get_by_name("nvidia-hevc").await.unwrap(),
        Some(created)
    );
}

/// A VAAPI profile's durable shape is the inverse of a software one: `qp`
/// instead of `crf`, and no `preset` at all. The round-trip has to preserve
/// both the presence of `qp` and the *absence* of a preset — a repository that
/// substituted a default preset would produce a row the encoder descriptor
/// rejects, and one that dropped `qp` would leave the encode's quality
/// unspecified.
#[tokio::test]
async fn create_persists_vaapi_qp_and_absent_preset() {
    let (repo, _pool, _tmp) = repo().await;
    let mut profile = sample_new("vaapi-hevc");
    profile.encoder = "hevc_vaapi".to_owned();
    profile.crf = None;
    profile.qp = Some(23);
    profile.preset = None;
    profile.codec_profile = Some("main10".to_owned());
    profile.pixel_format = Some("p010".to_owned());
    profile.decode = voom_core::VideoDecodeMode::vaapi();

    let created = repo.create(profile).await.unwrap();

    assert_eq!(created.qp, Some(23));
    assert!(created.preset.is_none());
    assert!(created.crf.is_none() && created.cq.is_none());
    assert!(created.decode.is_vaapi());
    assert_eq!(repo.get_by_name("vaapi-hevc").await.unwrap(), Some(created));

    let fetched = repo.get_by_name("vaapi-hevc").await.unwrap().unwrap();
    let typed = fetched.to_worker_profile();
    assert_eq!(typed.qp, Some(23));
    assert!(typed.preset.is_none());
    voom_core::validate_profile_against_descriptor(&typed).unwrap();
}

/// `update` must bind `qp` too, or an operator's re-tune of a VAAPI profile
/// would silently keep the old quality target.
#[tokio::test]
async fn update_replaces_vaapi_qp() {
    let (repo, _pool, _tmp) = repo().await;
    let mut profile = sample_new("vaapi-tune");
    profile.encoder = "hevc_vaapi".to_owned();
    profile.crf = None;
    profile.qp = Some(23);
    profile.preset = None;
    profile.codec_profile = None;
    profile.pixel_format = Some("nv12".to_owned());
    profile.decode = voom_core::VideoDecodeMode::vaapi();
    repo.create(profile.clone()).await.unwrap();

    profile.qp = Some(30);
    let updated = repo.update(profile).await.unwrap().unwrap();

    assert_eq!(updated.qp, Some(30));
    assert!(updated.preset.is_none());
}

/// The software path is unchanged by the nullable `preset` and the new `qp`
/// column: a `libx265` row still round-trips its preset and carries no `qp`.
/// This is the byte-for-byte-unchanged guarantee at the storage layer.
#[tokio::test]
async fn create_leaves_software_profile_round_trip_unchanged() {
    let (repo, _pool, _tmp) = repo().await;
    let created = repo.create(sample_new("software-hevc")).await.unwrap();

    assert_eq!(created.preset.as_deref(), Some("slow"));
    assert_eq!(created.crf, Some(22));
    assert!(created.qp.is_none());
    assert!(created.decode.is_software());

    let fetched = repo.get_by_name("software-hevc").await.unwrap().unwrap();
    assert_eq!(fetched, created);
    let typed = fetched.to_worker_profile();
    assert_eq!(typed.preset.as_deref(), Some("slow"));
    assert!(typed.qp.is_none());
    voom_core::validate_profile_against_descriptor(&typed).unwrap();
}

/// Every seeded builtin predates `qp`, so migration 0030 must leave each one
/// with its preset and a null `qp` — the read path sees exactly what 0029
/// wrote.
#[tokio::test]
async fn seeded_builtins_keep_their_preset_and_carry_no_qp() {
    let (repo, _pool, _tmp) = repo().await;
    for profile in repo.list().await.unwrap() {
        assert!(
            profile.preset.is_some(),
            "seed `{}` lost its preset",
            profile.name
        );
        assert!(profile.qp.is_none(), "seed `{}` gained a qp", profile.name);
    }
}

#[tokio::test]
async fn retire_hides_from_list_but_keeps_resolvable_and_is_idempotent() {
    let (repo, _pool, _tmp) = repo().await;
    repo.create(sample_new("gone")).await.unwrap();
    let now = time::OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
    let retired = repo.retire("gone", now).await.unwrap().unwrap();
    assert!(retired.retired_at.is_some());
    assert!(!repo.list().await.unwrap().iter().any(|p| p.name == "gone"));
    assert!(repo.get_by_name("gone").await.unwrap().is_some());
    // Idempotent: the second retire preserves the first stamp.
    let later = time::OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
    let again = repo.retire("gone", later).await.unwrap().unwrap();
    assert_eq!(again.retired_at, retired.retired_at);
    assert!(repo.retire("never", now).await.unwrap().is_none());
}

#[tokio::test]
async fn strict_check_rejects_bad_target_codec() {
    let (_repo, pool, _tmp) = repo().await;
    let err = sqlx::query(
        "INSERT INTO video_profiles (id, name, target_codec, encoder, crf, preset) \
         VALUES ('x', 'x', 'vp9', 'libx265', 23, 'medium')",
    )
    .execute(&pool)
    .await;
    assert!(err.is_err());
}

#[tokio::test]
async fn strict_check_rejects_bad_encoder() {
    let (_repo, pool, _tmp) = repo().await;
    let err = sqlx::query(
        "INSERT INTO video_profiles (id, name, target_codec, encoder, crf, preset) \
         VALUES ('x', 'x', 'hevc', 'mpeg4', 23, 'medium')",
    )
    .execute(&pool)
    .await;
    assert!(err.is_err());
}

#[tokio::test]
async fn strict_check_rejects_bad_output_container() {
    let (_repo, pool, _tmp) = repo().await;
    let err = sqlx::query(
        "INSERT INTO video_profiles \
         (id, name, target_codec, encoder, crf, preset, output_container) \
         VALUES ('x', 'x', 'hevc', 'libx265', 23, 'medium', 'avi')",
    )
    .execute(&pool)
    .await;
    assert!(err.is_err());
}

#[tokio::test]
async fn strict_check_rejects_bad_copy_compatible() {
    let (_repo, pool, _tmp) = repo().await;
    let err = sqlx::query(
        "INSERT INTO video_profiles \
         (id, name, target_codec, encoder, crf, preset, copy_compatible) \
         VALUES ('x', 'x', 'hevc', 'libx265', 23, 'medium', 2)",
    )
    .execute(&pool)
    .await;
    assert!(err.is_err());
}
