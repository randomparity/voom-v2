#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use tempfile::NamedTempFile;
use voom_cli::commands::health::{self, HealthData, HealthDb, HealthRuntime};
use voom_cli::envelope::Local;
use voom_control_plane::ControlPlane;

#[test]
fn health_payload_current_state_shape() {
    let payload = HealthData {
        db: HealthDb {
            status: "current",
            schema_init_at: Some("2026-05-15T18:23:00.000Z".into()),
            migration_count: Some(1),
        },
        runtime: HealthRuntime { tokio_workers: 8 },
    };
    insta::assert_json_snapshot!("health_current", &payload);
}

fn local_for(url: &str) -> Local {
    Local {
        db_url: url.to_owned(),
        config_path: "/tmp/voom-test/config.toml".into(),
    }
}

#[tokio::test]
async fn health_against_uninitialized_db_returns_exit_code_2() {
    let tmp = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::connect_or_create(&url).await.unwrap();

    let cp = ControlPlane::open(url.clone()).await.unwrap();
    let code = health::run(&cp, local_for(&url)).await.unwrap();
    assert_eq!(code, 2, "uninitialized DB must surface as DB_UNINITIALIZED with exit code 2");
}

#[tokio::test]
async fn health_against_initialized_db_returns_exit_code_0() {
    let tmp = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::init(&url).await.unwrap();

    let cp = ControlPlane::open(url.clone()).await.unwrap();
    let code = health::run(&cp, local_for(&url)).await.unwrap();
    assert_eq!(code, 0);
}
