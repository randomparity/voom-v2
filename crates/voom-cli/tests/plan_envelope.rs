#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::process::Command;

use serde_json::Value;
use tempfile::{NamedTempFile, tempdir};
use voom_policy::{FixtureName, load_fixture, load_policy_fixture};
use voom_store::test_support::sqlite_url_for;

#[test]
fn dry_run_noncompliant_succeeds_without_database() {
    let dir = tempdir().unwrap();
    let policy_path = dir.path().join("container-metadata.voom");
    std::fs::write(
        &policy_path,
        load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap(),
    )
    .unwrap();
    let db_path = dir.path().join("must-not-exist.sqlite");

    let output = Command::new(env!("CARGO_BIN_EXE_voom"))
        .env_remove("VOOM_DATABASE_URL")
        .env("VOOM_LOG_FORMAT", "json")
        .args([
            "--database-url",
            &format!("sqlite://{}", db_path.display()),
            "plan",
            "dry-run",
            "--policy-file",
            policy_path.to_str().unwrap(),
            "--input-fixture",
            "synthetic_noncompliant_transcode_needed",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert!(
        !db_path.exists(),
        "source-only dry-run must not create database files"
    );
    let json = envelope(output.stdout);
    assert_eq!(json["command"], "plan");
    assert_eq!(json["status"], "ok");
    assert_eq!(
        json["data"]["plan"]["input"]["source_label"],
        "synthetic_noncompliant_transcode_needed"
    );
    insta::assert_json_snapshot!("dry_run_noncompliant", json);
}

#[tokio::test]
async fn show_noncompliant_reads_durable_policy_and_input() {
    let tmp = NamedTempFile::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = voom_control_plane::ControlPlane::open_with_pool(
        pool,
        std::sync::Arc::new(voom_core::SystemClock),
    )
    .await
    .unwrap();
    let created = cp
        .create_policy_document(
            "container-metadata",
            &load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap(),
        )
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set(
            load_fixture(FixtureName::SyntheticNoncompliantTranscodeNeeded).unwrap(),
        )
        .await
        .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_voom"))
        .args([
            "--database-url",
            &url,
            "plan",
            "show",
            "--policy-version-id",
            &created.version.id.0.to_string(),
            "--input-set-id",
            &input.id.0.to_string(),
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let mut json = envelope(output.stdout);
    assert_eq!(
        json["data"]["plan"]["policy"]["version_id"],
        created.version.id.0
    );
    assert_eq!(json["data"]["plan"]["input"]["input_set_id"], input.id.0);
    redact_local(&mut json);
    insta::assert_json_snapshot!("show_noncompliant", json);
}

#[test]
fn parse_error_emits_plan_error_envelope() {
    let dir = tempdir().unwrap();
    let policy_path = dir.path().join("invalid.voom");
    std::fs::write(&policy_path, "policy").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_voom"))
        .args([
            "plan",
            "dry-run",
            "--policy-file",
            policy_path.to_str().unwrap(),
            "--input-fixture",
            "synthetic_noncompliant_transcode_needed",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let json = envelope(output.stdout);
    assert_eq!(json["command"], "plan");
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "POLICY_PARSE_ERROR");
    insta::assert_json_snapshot!("parse_error", json);
}

#[test]
fn plan_dry_run_missing_required_arg_emits_bad_args_envelope() {
    let output = Command::new(env!("CARGO_BIN_EXE_voom"))
        .args([
            "plan",
            "dry-run",
            "--input-fixture",
            "synthetic_noncompliant_transcode_needed",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let json = envelope(output.stdout);
    assert_eq!(json["command"], "cli");
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "BAD_ARGS");
    insta::assert_json_snapshot!("dry_run_missing_required_arg", json);
}

#[tokio::test]
async fn missing_input_set_emits_plan_error_envelope() {
    let tmp = NamedTempFile::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = voom_control_plane::ControlPlane::open_with_pool(
        pool,
        std::sync::Arc::new(voom_core::SystemClock),
    )
    .await
    .unwrap();
    let created = cp
        .create_policy_document(
            "container-metadata",
            &load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap(),
        )
        .await
        .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_voom"))
        .args([
            "--database-url",
            &url,
            "plan",
            "show",
            "--policy-version-id",
            &created.version.id.0.to_string(),
            "--input-set-id",
            "999999",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let mut json = envelope(output.stdout);
    assert_eq!(json["command"], "plan");
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "NOT_FOUND");
    redact_local(&mut json);
    insta::assert_json_snapshot!("missing_input_set", json);
}

fn envelope(stdout: Vec<u8>) -> Value {
    let stdout = String::from_utf8(stdout).unwrap();
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be one JSON envelope; got {stdout:?}: {e}"))
}

fn redact_local(json: &mut Value) {
    json["local"]["db_url"] = Value::String("[db-url]".to_owned());
    json["local"]["config_path"] = Value::String("[config-path]".to_owned());
}
