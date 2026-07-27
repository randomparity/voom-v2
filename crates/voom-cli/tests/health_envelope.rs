#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::borrow::Cow;
use std::io::Write;
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use sqlx::migrate::Migrator;
use tempfile::NamedTempFile;
use voom_cli::commands::health::{self, HealthData, HealthDb, HealthRuntime};
use voom_cli::envelope::Local;
use voom_control_plane::HealthPlane;
use voom_store::test_support::{insert_synthetic_migration, sqlite_url_for};
use voom_store::{MIGRATOR, connect_or_create};

const MIGRATION_ROLLBACK_RUNBOOK: &str =
    include_str!("../../../docs/runbooks/migration-rollback.md");
const CURRENT_HEALTH_JQ: &str = r#".schema_version == "0" and
.command == "health" and
.status == "ok" and
.data.db.status == "current" and
.error == null"#;
const ERROR_HEALTH_JQ: &str = r#".schema_version == "0" and
.command == "health" and
.status == "error" and
.data == null and
.error.code == $code"#;

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

fn run_health(url: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_voom"))
        .args(["--database-url", url, "health"])
        .output()
        .unwrap()
}

fn parse_stdout(output: &Output) -> (String, Value) {
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    let json = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be a JSON envelope; got {stdout:?}: {e}"));
    (stdout, json)
}

fn assert_jq_accepts(filter: &str, stdout: &str, error_code: Option<&str>) {
    let mut command = Command::new("jq");
    command.arg("-e");
    if let Some(error_code) = error_code {
        command.args(["--arg", "code", error_code]);
    }
    let mut child = command
        .arg(filter)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdout.as_bytes())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "documented jq predicate rejected envelope: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn migrator_before_latest() -> Migrator {
    let latest = MIGRATOR
        .iter()
        .map(|migration| migration.version)
        .max()
        .unwrap();
    Migrator {
        migrations: Cow::Owned(
            MIGRATOR
                .iter()
                .filter(|migration| migration.version < latest)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
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

#[tokio::test]
async fn migration_rollback_runbook_predicates_match_real_health_contract() {
    assert!(
        MIGRATION_ROLLBACK_RUNBOOK.contains(CURRENT_HEALTH_JQ),
        "runbook must contain the current-health jq predicate executed by this test"
    );
    assert!(
        MIGRATION_ROLLBACK_RUNBOOK.contains(ERROR_HEALTH_JQ),
        "runbook must contain the error-health jq predicate executed by this test"
    );
    assert!(
        !MIGRATION_ROLLBACK_RUNBOOK.contains("schema_state"),
        "runbook must not reference the nonexistent schema_state field"
    );
    assert!(
        !MIGRATION_ROLLBACK_RUNBOOK.contains("choose an older backup or"),
        "runbook must not direct partial-schema recovery toward older backups"
    );

    let current = NamedTempFile::new().unwrap();
    let current_url = sqlite_url_for(current.path());
    voom_store::init(&current_url).await.unwrap();
    let current_output = run_health(&current_url);
    assert_eq!(current_output.status.code(), Some(0));
    let (current_stdout, current_json) = parse_stdout(&current_output);
    assert_eq!(current_json["data"]["db"]["status"], "current");
    assert_jq_accepts(CURRENT_HEALTH_JQ, &current_stdout, None);

    let too_new = NamedTempFile::new().unwrap();
    let too_new_url = sqlite_url_for(too_new.path());
    voom_store::init(&too_new_url).await.unwrap();
    let too_new_pool = voom_store::connect(&too_new_url).await.unwrap();
    insert_synthetic_migration(&too_new_pool, 99999, true)
        .await
        .unwrap();
    drop(too_new_pool);
    let too_new_json = assert_health_error(&too_new_url, "DB_SCHEMA_TOO_NEW");
    assert_eq!(
        too_new_json["error"]["hint"],
        "Upgrade the server binary or roll the database back"
    );

    let partial = NamedTempFile::new().unwrap();
    let partial_url = sqlite_url_for(partial.path());
    let partial_pool = connect_or_create(&partial_url).await.unwrap();
    migrator_before_latest().run(&partial_pool).await.unwrap();
    drop(partial_pool);
    let partial_json = assert_health_error(&partial_url, "DB_PARTIAL_SCHEMA");
    assert_eq!(
        partial_json["error"]["hint"],
        "Run `voom init` against the current binary"
    );

    let dirty = NamedTempFile::new().unwrap();
    let dirty_url = sqlite_url_for(dirty.path());
    voom_store::init(&dirty_url).await.unwrap();
    let dirty_pool = voom_store::connect(&dirty_url).await.unwrap();
    sqlx::query("UPDATE _sqlx_migrations SET success = 0 WHERE version = 1")
        .execute(&dirty_pool)
        .await
        .unwrap();
    drop(dirty_pool);
    let dirty_json = assert_health_error(&dirty_url, "DB_DIRTY_MIGRATION");
    assert!(
        dirty_json["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("Manual recovery required")
    );
}

fn assert_health_error(url: &str, expected_code: &str) -> Value {
    let output = run_health(url);
    assert_eq!(output.status.code(), Some(2));
    let (stdout, json) = parse_stdout(&output);
    assert_eq!(json["error"]["code"], expected_code);
    assert_jq_accepts(ERROR_HEALTH_JQ, &stdout, Some(expected_code));
    json
}

/// End-to-end: invoke the compiled `voom` binary against a database whose
/// `schema_meta` has been dropped post-init. The CLI must emit a
/// `DB_PARTIAL_SCHEMA` envelope whose hint explicitly does NOT advise
/// re-running `voom init` (because init re-probes and would loop on the
/// same error).
#[tokio::test]
async fn health_against_corrupted_schema_meta_points_to_restore_not_init() {
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

    let output = run_health(&url);

    assert_eq!(output.status.code(), Some(2));
    let (_stdout, json) = parse_stdout(&output);
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
