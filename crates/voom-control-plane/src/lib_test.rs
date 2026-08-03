use super::*;
use voom_core::ErrorCode;

fn fresh_url() -> (voom_test_support::TempDatabase, String) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    (tmp, url)
}

#[tokio::test]
async fn open_exposes_seeded_video_profiles() {
    let (_keep, url) = fresh_url();
    voom_store::init(&url).await.unwrap();
    let cp = ControlPlane::open(&url).await.unwrap();
    let profiles = cp.video_profiles.list().await.unwrap();
    assert_eq!(profiles.len(), 6);
    let names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"default-hevc"));
}

#[tokio::test]
async fn control_plane_open_rejects_uninitialized_db() {
    let (_keep, url) = fresh_url();
    voom_store::test_support::create_uninitialized_pool(&url)
        .await
        .unwrap();
    let err = ControlPlane::open(&url).await.unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::DbUninitialized);
}

#[tokio::test]
async fn control_plane_open_rejects_too_new_schema() {
    let (_keep, url) = fresh_url();
    voom_store::init(&url).await.unwrap();

    {
        let pool = voom_store::connect(&url).await.unwrap();
        sqlx::query(
            "INSERT INTO _sqlx_migrations \
             (version, description, installed_on, success, checksum, execution_time) \
             VALUES (99999, 'synthetic-future', strftime('%s','now'), 1, X'00', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    let err = ControlPlane::open(&url).await.unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::DbSchemaTooNew);
}

#[tokio::test]
async fn control_plane_open_rejects_dirty_schema() {
    let (_keep, url) = fresh_url();
    voom_store::init(&url).await.unwrap();

    {
        let pool = voom_store::connect(&url).await.unwrap();
        sqlx::query("UPDATE _sqlx_migrations SET success = 0 WHERE version = 1")
            .execute(&pool)
            .await
            .unwrap();
    }

    let err = ControlPlane::open(&url).await.unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::DbDirtyMigration);
}

#[tokio::test]
async fn second_init_returns_already_initialized() {
    let (_keep, url) = fresh_url();
    voom_store::init(&url).await.unwrap();
    let second = voom_store::init(&url).await.unwrap();
    assert!(second.already_initialized);
    assert_eq!(second.migrations_applied, 0);
}
