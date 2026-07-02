#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

//! `voom lease` envelope goldens. Manual-lock acquire/release/force-release/list
//! over the existing use-lease control-plane cases. Acquire needs a live scope,
//! so the fixture seeds one `file_assets` row directly through the store before
//! shelling out to the CLI.

use std::process::Command;

use serde_json::Value;
use tempfile::NamedTempFile;
use voom_store::repo::identity::{IdentityRepo, SqliteIdentityRepo};

struct Fixture {
    _tmp: NamedTempFile,
    url: String,
}

async fn fixture() -> Fixture {
    let tmp = NamedTempFile::new().unwrap();
    let url = voom_store::test_support::sqlite_url_for(tmp.path());
    voom_store::init(&url).await.unwrap();
    Fixture { _tmp: tmp, url }
}

/// Seed one live `file_assets` row and return its id, so `lease acquire
/// --scope-type asset` has a live scope to attach to.
async fn seed_asset(url: &str) -> u64 {
    let pool = voom_store::connect(url).await.unwrap();
    let asset = SqliteIdentityRepo::new(pool)
        .create_file_asset(time::OffsetDateTime::now_utc())
        .await
        .unwrap();
    asset.id.0
}

fn cli(url: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
    command.args(["--database-url", url, "lease"]);
    command
}

/// Acquire a manual lock on `asset 1`, returning its lease id. Assumes the
/// fixture already seeded the asset.
fn acquire_asset_lock(url: &str, asset_id: u64) -> u64 {
    let output = cli(url)
        .args([
            "acquire",
            "--scope-type",
            "asset",
            "--scope-id",
            &asset_id.to_string(),
            "--issuer-ref",
            "operator-alice",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "acquire must succeed");
    envelope(output.stdout)["data"]["id"].as_u64().unwrap()
}

fn envelope(stdout: Vec<u8>) -> Value {
    let stdout = String::from_utf8(stdout).unwrap();
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be one JSON envelope; got {stdout:?}: {e}"))
}

/// Replace clock-driven fields with placeholders so the snapshot is stable.
fn redact(json: &mut Value) {
    json["local"]["db_url"] = Value::String("[db-url]".to_owned());
    json["local"]["config_path"] = Value::String("[config-path]".to_owned());
    let data = &mut json["data"];
    if data.is_null() {
        return;
    }
    if let Some(locks) = data.get_mut("locks").and_then(Value::as_array_mut) {
        for lock in locks {
            stamp(lock);
            lock["age_seconds"] = Value::from(0);
        }
    } else {
        stamp(data);
    }
}

fn stamp(lease: &mut Value) {
    for field in ["acquired_at", "expires_at", "released_at"] {
        if lease.get(field).is_some_and(|v| !v.is_null()) {
            lease[field] = Value::String("[ts]".to_owned());
        }
    }
}

#[tokio::test]
async fn acquire_outputs_the_lock() {
    let fixture = fixture().await;
    let asset_id = seed_asset(&fixture.url).await;
    let output = cli(&fixture.url)
        .args([
            "acquire",
            "--scope-type",
            "asset",
            "--scope-id",
            &asset_id.to_string(),
            "--issuer-ref",
            "operator-alice",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let mut json = envelope(output.stdout);
    assert_eq!(json["command"], "lease");
    assert_eq!(json["status"], "ok");
    assert_eq!(json["data"]["kind"], "manual_lock");
    assert_eq!(json["data"]["blocking_mode"], "blocking");
    assert_eq!(json["data"]["ttl_bound"], false);
    redact(&mut json);
    insta::assert_json_snapshot!("acquire_outputs_the_lock", json);
}

#[tokio::test]
async fn list_outputs_live_locks_with_age() {
    let fixture = fixture().await;
    let asset_id = seed_asset(&fixture.url).await;
    acquire_asset_lock(&fixture.url, asset_id);
    let output = cli(&fixture.url).arg("list").output().unwrap();

    assert_eq!(output.status.code(), Some(0));
    let mut json = envelope(output.stdout);
    assert_eq!(json["status"], "ok");
    assert!(json["data"]["locks"][0]["age_seconds"].is_number());
    redact(&mut json);
    insta::assert_json_snapshot!("list_outputs_live_locks_with_age", json);
}

#[tokio::test]
async fn release_reports_the_terminal_lock() {
    let fixture = fixture().await;
    let asset_id = seed_asset(&fixture.url).await;
    let lease_id = acquire_asset_lock(&fixture.url, asset_id);
    let output = cli(&fixture.url)
        .args(["release", "--lease-id", &lease_id.to_string()])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let mut json = envelope(output.stdout);
    assert_eq!(json["status"], "ok");
    assert_eq!(json["data"]["release_reason"], "released");
    redact(&mut json);
    insta::assert_json_snapshot!("release_reports_the_terminal_lock", json);
}

#[tokio::test]
async fn force_release_records_the_audited_override() {
    let fixture = fixture().await;
    let asset_id = seed_asset(&fixture.url).await;
    let lease_id = acquire_asset_lock(&fixture.url, asset_id);
    let output = cli(&fixture.url)
        .args([
            "force-release",
            "--lease-id",
            &lease_id.to_string(),
            "--actor",
            "operator-bob",
            "--reason",
            "forgotten hold on a stuck job",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let mut json = envelope(output.stdout);
    assert_eq!(json["status"], "ok");
    assert_eq!(json["data"]["release_reason"], "force_released");
    redact(&mut json);
    insta::assert_json_snapshot!("force_release_records_the_audited_override", json);
}

#[tokio::test]
async fn list_is_empty_on_clean_db() {
    let fixture = fixture().await;
    let output = cli(&fixture.url).arg("list").output().unwrap();

    assert_eq!(output.status.code(), Some(0));
    let mut json = envelope(output.stdout);
    assert_eq!(json["status"], "ok");
    redact(&mut json);
    insta::assert_json_snapshot!("list_is_empty_on_clean_db", json);
}

#[tokio::test]
async fn acquire_on_unknown_scope_is_not_found() {
    let fixture = fixture().await;
    let output = cli(&fixture.url)
        .args([
            "acquire",
            "--scope-type",
            "asset",
            "--scope-id",
            "999",
            "--issuer-ref",
            "operator-alice",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let mut json = envelope(output.stdout);
    assert_eq!(json["error"]["code"], "NOT_FOUND");
    redact(&mut json);
    insta::assert_json_snapshot!("acquire_on_unknown_scope_is_not_found", json);
}

#[tokio::test]
async fn acquire_rejects_empty_issuer_ref() {
    let fixture = fixture().await;
    let asset_id = seed_asset(&fixture.url).await;
    let output = cli(&fixture.url)
        .args([
            "acquire",
            "--scope-type",
            "asset",
            "--scope-id",
            &asset_id.to_string(),
            "--issuer-ref",
            "   ",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let mut json = envelope(output.stdout);
    assert_eq!(json["error"]["code"], "CONFIG_INVALID");
    redact(&mut json);
    insta::assert_json_snapshot!("acquire_rejects_empty_issuer_ref", json);
}

#[tokio::test]
async fn force_release_rejects_empty_reason() {
    let fixture = fixture().await;
    let output = cli(&fixture.url)
        .args([
            "force-release",
            "--lease-id",
            "1",
            "--actor",
            "operator-bob",
            "--reason",
            "   ",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let mut json = envelope(output.stdout);
    assert_eq!(json["error"]["code"], "CONFIG_INVALID");
    redact(&mut json);
    insta::assert_json_snapshot!("force_release_rejects_empty_reason", json);
}

#[tokio::test]
async fn force_release_unknown_lease_is_not_found() {
    let fixture = fixture().await;
    let output = cli(&fixture.url)
        .args([
            "force-release",
            "--lease-id",
            "42",
            "--actor",
            "operator-bob",
            "--reason",
            "no such lease",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let mut json = envelope(output.stdout);
    assert_eq!(json["error"]["code"], "NOT_FOUND");
    redact(&mut json);
    insta::assert_json_snapshot!("force_release_unknown_lease_is_not_found", json);
}
