#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use tempfile::NamedTempFile;
use voom_cli::commands::health::{self, HealthData, HealthDb, HealthRuntime};
use voom_cli::envelope::Local;
use voom_control_plane::HealthPlane;
use voom_store::test_support::sqlite_url_for;

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
    let url = sqlite_url_for(tmp.path());
    voom_store::connect_or_create(&url).await.unwrap();

    let hp = HealthPlane::open(&url).await.unwrap();
    let code = health::run(&hp, local_for(&url)).await.unwrap();
    assert_eq!(
        code, 2,
        "uninitialized DB must surface as DB_UNINITIALIZED with exit code 2"
    );
}

#[tokio::test]
async fn health_against_initialized_db_returns_exit_code_0() {
    let tmp = NamedTempFile::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    voom_store::init(&url).await.unwrap();

    let hp = HealthPlane::open(&url).await.unwrap();
    let code = health::run(&hp, local_for(&url)).await.unwrap();
    assert_eq!(code, 0);
}

/// End-to-end: invoke the compiled `voom` binary against a database whose
/// `schema_meta` has been dropped post-init. The CLI must emit a
/// `DB_PARTIAL_SCHEMA` envelope whose hint explicitly does NOT advise
/// re-running `voom init` (because init re-probes and would loop on the
/// same error).
#[tokio::test]
async fn health_against_corrupted_schema_meta_points_to_restore_not_init() {
    use std::process::Command;

    use serde_json::Value;

    let tmp = NamedTempFile::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    voom_store::init(&url).await.unwrap();
    {
        let pool = voom_store::connect(&url).await.unwrap();
        sqlx::query("DROP TABLE schema_meta")
            .execute(&pool)
            .await
            .unwrap();
    }

    let bin = env!("CARGO_BIN_EXE_voom");
    let output = Command::new(bin)
        .args(["--database-url", &url, "health"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be a JSON envelope; got {stdout:?}: {e}"));
    assert_eq!(json["error"]["code"], "DB_PARTIAL_SCHEMA");
    let hint = json["error"]["hint"].as_str().unwrap_or_default();
    assert!(
        !hint.contains("Run: voom init") && !hint.contains("run `voom init`"),
        "hint must NOT advise re-running voom init for corrupted schema_meta: {hint:?}"
    );
    assert!(
        hint.contains("restore") || hint.contains("repair") || hint.contains("schema_meta"),
        "hint must point operators at manual recovery: {hint:?}"
    );
}
