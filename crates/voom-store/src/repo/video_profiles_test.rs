use super::*;
use crate::test_support::fresh_initialized_pool_at;

async fn repo() -> (SqliteVideoProfileRepo, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = fresh_initialized_pool_at(tmp.path()).await.unwrap();
    (SqliteVideoProfileRepo::new(pool), tmp)
}

#[tokio::test]
async fn lists_all_seeded_builtins() {
    let (repo, _tmp) = repo().await;
    let profiles = repo.list().await.unwrap();
    let names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"default-hevc"));
    assert!(names.contains(&"av1-1080p"));
    assert_eq!(profiles.len(), 6);
}

#[tokio::test]
async fn every_seeded_builtin_is_valid_against_its_descriptor() {
    let (repo, _tmp) = repo().await;
    for profile in repo.list().await.unwrap() {
        let typed = profile.to_worker_profile();
        voom_worker_protocol::validate_profile_against_descriptor(&typed)
            .unwrap_or_else(|e| panic!("seed `{}` invalid: {e}", profile.name));
    }
}

#[tokio::test]
async fn get_by_name_returns_profile_or_none() {
    let (repo, _tmp) = repo().await;
    let hit = repo.get_by_name("hevc-archive").await.unwrap().unwrap();
    assert_eq!(hit.codec_profile.as_deref(), Some("main10"));
    assert_eq!(hit.pixel_format.as_deref(), Some("yuv420p10le"));
    assert!(repo.get_by_name("does-not-exist").await.unwrap().is_none());
}

#[tokio::test]
async fn strict_check_rejects_bad_target_codec() {
    let (repo, _tmp) = repo().await;
    let err = sqlx::query(
        "INSERT INTO video_profiles (id, name, target_codec, encoder, crf, preset) \
         VALUES ('x', 'x', 'vp9', 'libx265', 23, 'medium')",
    )
    .execute(repo.pool_for_test())
    .await;
    assert!(err.is_err());
}
