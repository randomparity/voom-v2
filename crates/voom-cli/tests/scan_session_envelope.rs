#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::process::{Command, Output};
use std::sync::Arc;

use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};
use time::OffsetDateTime;
use voom_control_plane::ControlPlane;
use voom_control_plane::execution::{RemoteActivateInput, RemoteWorkerDeclaration};
use voom_control_plane::scan::sessions::RemoteScanCompleteInput;
use voom_control_plane::scan::{
    RemoteScanBatchInput, RemoteScanFailInput, RemoteScanStartInput, ScanObservation,
};
use voom_control_plane::workers::RegisterNodeInput;
use voom_core::{
    ArtifactAccessMode, Clock, LibraryId, NodeId, NodeKind, OperationKind, ProviderLocator,
    ProviderRelativeLocator, ScanSessionId, ScanTerminalReason, StorageProviderKind, StorageRootId,
};
use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
use voom_store::repo::library::library_roots::{
    HiddenFilePolicy, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
};
use voom_test_support::TempDatabase;

const INCARNATION: &str = "0123456789abcdef0123456789abcdef";
const PRIVATE_PROVIDER_LOCATOR: &str = "SUPER_SECRET_PROVIDER_LOCATOR_419";
const PRIVATE_OBSERVATION_LOCATOR: &str = "SUPER_SECRET_OBSERVATION_LOCATOR_419";
const PRIVATE_OBJECT_IDENTITY: &str = "SUPER_SECRET_OBJECT_IDENTITY_419";
const PRIVATE_REQUEST_HASH: &str = "SUPER_SECRET_REQUEST_HASH_419";
const FIXED_TIME: i64 = 2_000_000_000;

#[derive(Debug)]
struct FixedClock(OffsetDateTime);

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        self.0
    }
}

struct Fixture {
    _database: TempDatabase,
    url: String,
    token: SecretString,
    request_root: StorageRootId,
    cancel_root: StorageRootId,
    running_id: ScanSessionId,
    succeeded_id: ScanSessionId,
    failed_id: ScanSessionId,
    cancelled_id: ScanSessionId,
    stale_id: ScanSessionId,
}

#[tokio::test]
async fn request_show_list_reconciliation_and_cancel_emit_stable_public_envelopes() {
    let fixture = fixture().await;

    snapshot_progress(&fixture);
    snapshot_terminal_states(&fixture);
    snapshot_reconciliation(&fixture);
    snapshot_request(&fixture);
    assert_cancel_command(&fixture);
}

#[tokio::test]
async fn show_and_list_reject_a_mismatched_attributed_retirement_count() {
    let fixture = fixture().await;
    let pool = voom_store::connect(&fixture.url).await.unwrap();
    sqlx::query("UPDATE scan_sessions SET retired_location_count = 3 WHERE id = ?")
        .bind(i64::try_from(fixture.succeeded_id.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let id = fixture.succeeded_id.0.to_string();
    for args in [
        vec!["scan-session", "show", "--id", &id],
        vec!["scan-session", "list", "--limit", "50"],
    ] {
        let output = command(&fixture.url, &args).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let envelope = single_envelope(output);
        assert_eq!(envelope["command"], "scan-session");
        assert_eq!(envelope["status"], "error");
        assert_eq!(envelope["error"]["code"], "DB_UNREACHABLE");
        assert!(
            envelope["error"]["message"]
                .as_str()
                .unwrap()
                .contains("does not match 2 attributed locations")
        );
    }
}

#[test]
fn invalid_scan_session_arguments_emit_one_bad_args_envelope() {
    let too_long = "a".repeat(1025);
    let cases = vec![
        vec![
            "scan-session",
            "request",
            "--root",
            "7",
            "--idle-timeout-seconds",
            "0",
        ],
        vec![
            "scan-session",
            "request",
            "--root",
            "7",
            "--idle-timeout-seconds",
            "86401",
        ],
        vec!["scan-session", "list", "--limit", "0"],
        vec!["scan-session", "list", "--limit", "101"],
        vec![
            "scan-session",
            "reconciliation",
            "--id",
            "9",
            "--limit",
            "0",
        ],
        vec![
            "scan-session",
            "reconciliation",
            "--id",
            "9",
            "--limit",
            "101",
        ],
        vec!["scan-session", "list", "--status", "complete"],
        vec!["scan-session", "cancel", "--id", "9", "--reason", ""],
        vec!["scan-session"],
    ];
    for args in cases {
        assert_bad_args(args);
    }
    assert_bad_args(vec![
        "scan-session",
        "cancel",
        "--id",
        "9",
        "--reason",
        too_long.as_str(),
    ]);
}

fn snapshot_progress(fixture: &Fixture) {
    let running_id = fixture.running_id.0.to_string();
    let show = success(fixture, &["scan-session", "show", "--id", &running_id]);
    assert_eq!(show["data"]["session"]["status"], "running");
    assert_eq!(show["data"]["session"]["batch_count"], 1);
    assert_eq!(show["data"]["session"]["observation_count"], 1);
    let root = show["data"]["session"]["storage_root_id"]
        .as_u64()
        .unwrap()
        .to_string();
    let list = success(
        fixture,
        &[
            "scan-session",
            "list",
            "--root",
            &root,
            "--status",
            "running",
            "--after",
            "0",
            "--limit",
            "50",
        ],
    );
    assert_eq!(list["data"]["sessions"].as_array().unwrap().len(), 1);
    let first = success(fixture, &["scan-session", "list", "--limit", "1"]);
    let cursor = first["next_cursor"].as_u64().unwrap();
    let second = success(
        fixture,
        &[
            "scan-session",
            "list",
            "--after",
            &cursor.to_string(),
            "--limit",
            "1",
        ],
    );
    assert!(second["data"]["sessions"][0]["id"].as_u64().unwrap() > cursor);

    insta::assert_json_snapshot!(
        "progress",
        json!({
            "show": show,
            "filtered_list": list,
            "first_page": first,
            "second_page": second,
        })
    );
}

fn snapshot_terminal_states(fixture: &Fixture) {
    let states = [
        (fixture.succeeded_id, "succeeded"),
        (fixture.failed_id, "failed"),
        (fixture.cancelled_id, "cancelled"),
        (fixture.stale_id, "stale"),
    ];
    let mut envelopes = Vec::new();
    for (id, expected) in states {
        let id = id.0.to_string();
        let envelope = success(fixture, &["scan-session", "show", "--id", &id]);
        assert_eq!(envelope["data"]["session"]["status"], expected);
        assert_eq!(
            envelope["data"]["session"]["reconciliation_applied"],
            expected == "succeeded"
        );
        envelopes.push(envelope);
    }
    insta::assert_json_snapshot!("terminal_states", envelopes);
}

fn snapshot_reconciliation(fixture: &Fixture) {
    let id = fixture.succeeded_id.0.to_string();
    let first = success(
        fixture,
        &[
            "scan-session",
            "reconciliation",
            "--id",
            &id,
            "--limit",
            "1",
        ],
    );
    let cursor = first["next_cursor"].as_u64().unwrap();
    let second = success(
        fixture,
        &[
            "scan-session",
            "reconciliation",
            "--id",
            &id,
            "--after",
            &cursor.to_string(),
            "--limit",
            "1",
        ],
    );
    assert!(
        second["data"]["items"][0]["file_location_id"]
            .as_u64()
            .unwrap()
            > cursor
    );
    assert_public_reconciliation(&first);
    assert_public_reconciliation(&second);
    insta::assert_json_snapshot!("reconciliation", json!({"first": first, "second": second}));
}

fn snapshot_request(fixture: &Fixture) {
    let root = fixture.request_root.0.to_string();
    let mut request = success(
        fixture,
        &[
            "scan-session",
            "request",
            "--root",
            &root,
            "--idle-timeout-seconds",
            "300",
        ],
    );
    assert_eq!(request["data"]["session"]["status"], "requested");
    assert_iso_timestamp(&request["data"]["session"]["requested_at"]);
    assert_iso_timestamp(&request["data"]["session"]["progress_deadline_at"]);
    request["data"]["session"]["requested_at"] = Value::String("[timestamp]".to_owned());
    request["data"]["session"]["progress_deadline_at"] = Value::String("[timestamp]".to_owned());
    insta::assert_json_snapshot!("request", request);
}

fn assert_cancel_command(fixture: &Fixture) {
    let root = fixture.cancel_root.0.to_string();
    let requested = success(fixture, &["scan-session", "request", "--root", &root]);
    let id = requested["data"]["session"]["id"]
        .as_u64()
        .unwrap()
        .to_string();
    let cancelled = success(
        fixture,
        &[
            "scan-session",
            "cancel",
            "--id",
            &id,
            "--reason",
            "operator stopped scan",
        ],
    );
    assert_eq!(cancelled["data"]["session"]["status"], "cancelled");
    assert_eq!(
        cancelled["data"]["session"]["terminal_reason"],
        "operator stopped scan"
    );
    assert_eq!(
        cancelled["data"]["session"]["reconciliation_applied"],
        false
    );
}

async fn fixture() -> Fixture {
    let database = TempDatabase::new().unwrap();
    let url = voom_store::test_support::sqlite_url_for(database.path());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let clock = Arc::new(FixedClock(at(FIXED_TIME)));
    let cp = ControlPlane::open_with_pool(pool.clone(), clock)
        .await
        .unwrap();
    let registered = register_and_activate(&cp).await;
    let library_id = create_library(&cp).await;
    let roots = create_roots(&cp, library_id, registered.0).await;
    let sessions = create_sessions(&cp, &pool, registered.0, roots, &registered.1).await;
    Fixture {
        _database: database,
        url,
        token: registered.1,
        request_root: roots[5],
        cancel_root: roots[6],
        running_id: sessions[0],
        succeeded_id: sessions[1],
        failed_id: sessions[2],
        cancelled_id: sessions[3],
        stale_id: sessions[4],
    }
}

async fn register_and_activate(cp: &ControlPlane) -> (NodeId, SecretString) {
    let registered = cp
        .register_node(RegisterNodeInput {
            name: "scan-session-envelope-owner".to_owned(),
            kind: NodeKind::Remote,
            heartbeat_ttl_seconds: 60,
            metadata: json!({}),
        })
        .await
        .unwrap();
    cp.remote_activate(RemoteActivateInput {
        node_id: registered.node.id,
        token: registered.token.clone(),
        idempotency_key: "activate-scan-session-envelope".to_owned(),
        request_hash: PRIVATE_REQUEST_HASH.to_owned(),
        incarnation_id: INCARNATION.parse().unwrap(),
        workers: vec![RemoteWorkerDeclaration {
            logical_name: "scan".to_owned(),
            operations: vec![OperationKind::ProbeFile],
            artifact_access: vec![ArtifactAccessMode::SharedMount],
            max_parallel: 1,
        }],
    })
    .await
    .unwrap();
    (registered.node.id, registered.token)
}

async fn create_library(cp: &ControlPlane) -> LibraryId {
    cp.create_library(NewLibrary {
        slug: "scan-session-envelope".to_owned(),
        display_name: "Scan session envelope".to_owned(),
        media_kind: LibraryMediaKind::Movie,
        description: None,
        enabled: true,
    })
    .await
    .unwrap()
    .id
}

async fn create_roots(
    cp: &ControlPlane,
    library_id: LibraryId,
    owner: NodeId,
) -> [StorageRootId; 7] {
    let mut roots = Vec::new();
    for suffix in [
        "running",
        "succeeded",
        "failed",
        "cancelled",
        "stale",
        "request-command",
        "cancel-command",
    ] {
        let root = cp
            .create_library_root(new_root(library_id, owner, suffix))
            .await
            .unwrap();
        cp.activate_library_root(root.id, format!("scan-session-{suffix}"))
            .await
            .unwrap();
        roots.push(root.id);
    }
    roots.try_into().unwrap()
}

fn new_root(library_id: LibraryId, owner: NodeId, suffix: &str) -> NewLibraryRoot {
    let locator = format!("/{PRIVATE_PROVIDER_LOCATOR}/{suffix}");
    NewLibraryRoot {
        library_id,
        owner_node_id: owner,
        provider_kind: StorageProviderKind::LocalFilesystem,
        provider_locator: ProviderLocator::new(locator.clone()).unwrap(),
        display_locator: locator,
        include_globs: Vec::new(),
        exclude_globs: Vec::new(),
        extension_allowlist: Vec::new(),
        scan_mode: LibraryScanMode::ManualRecursive,
        symlink_policy: SymlinkPolicy::Reject,
        hidden_file_policy: HiddenFilePolicy::Ignore,
        max_depth: None,
        stability_seconds: 0,
        debounce_seconds: 0,
        default_output_root_id: None,
        default_staging_root_id: None,
        default_backup_root_id: None,
        enabled: true,
    }
}

async fn create_sessions(
    cp: &ControlPlane,
    pool: &sqlx::SqlitePool,
    node_id: NodeId,
    roots: [StorageRootId; 7],
    token: &SecretString,
) -> [ScanSessionId; 5] {
    let running = start(cp, node_id, roots[0], token, "running").await;
    cp.accept_scan_observation_batch(batch_input(node_id, running, token))
        .await
        .unwrap();

    seed_rooted_location(pool, roots[1], "retire/one.mkv").await;
    seed_rooted_location(pool, roots[1], "retire/two.mkv").await;
    let succeeded = start(cp, node_id, roots[1], token, "succeeded").await;
    cp.complete_scan_session(complete_input(node_id, succeeded, token))
        .await
        .unwrap();

    let failed = start(cp, node_id, roots[2], token, "failed").await;
    cp.fail_scan_session(fail_input(node_id, failed, token))
        .await
        .unwrap();

    let cancelled = cp.request_scan_session(roots[3], 300).await.unwrap().id;
    cp.cancel_scan_session(
        cancelled,
        ScanTerminalReason::new("operator cancelled fixture").unwrap(),
    )
    .await
    .unwrap();

    let stale = cp.request_scan_session(roots[4], 300).await.unwrap().id;
    sqlx::query("UPDATE library_roots SET root_epoch = root_epoch + 1 WHERE id = ?")
        .bind(i64::try_from(roots[4].0).unwrap())
        .execute(pool)
        .await
        .unwrap();
    cp.cancel_scan_session(stale, ScanTerminalReason::new("stale fixture").unwrap())
        .await
        .unwrap_err();

    [running, succeeded, failed, cancelled, stale]
}

async fn start(
    cp: &ControlPlane,
    node_id: NodeId,
    root: StorageRootId,
    token: &SecretString,
    key: &str,
) -> ScanSessionId {
    let id = cp.request_scan_session(root, 300).await.unwrap().id;
    cp.start_scan_session(RemoteScanStartInput {
        node_id,
        scan_session_id: id,
        incarnation_id: INCARNATION.parse().unwrap(),
        token: token.clone(),
        idempotency_key: format!("start-{key}"),
        request_hash: format!("{PRIVATE_REQUEST_HASH}-start-{key}"),
    })
    .await
    .unwrap();
    id
}

fn batch_input(node_id: NodeId, id: ScanSessionId, token: &SecretString) -> RemoteScanBatchInput {
    RemoteScanBatchInput {
        node_id,
        scan_session_id: id,
        incarnation_id: INCARNATION.parse().unwrap(),
        token: token.clone(),
        idempotency_key: "running-progress-batch".to_owned(),
        request_hash: "f".repeat(64),
        sequence: 0,
        observations: vec![ScanObservation {
            provider_relative_locator: ProviderRelativeLocator::new(format!(
                "private/{PRIVATE_OBSERVATION_LOCATOR}.mkv"
            ))
            .unwrap(),
            provider_object_identity: PRIVATE_OBJECT_IDENTITY.to_owned(),
            size_bytes: 419,
            modified_at: at(FIXED_TIME),
            stability_started_at: at(FIXED_TIME),
            stability_confirmed_at: at(FIXED_TIME),
        }],
    }
}

fn complete_input(
    node_id: NodeId,
    id: ScanSessionId,
    token: &SecretString,
) -> RemoteScanCompleteInput {
    RemoteScanCompleteInput {
        node_id,
        scan_session_id: id,
        incarnation_id: INCARNATION.parse().unwrap(),
        token: token.clone(),
        idempotency_key: "complete-succeeded".to_owned(),
        request_hash: format!("{PRIVATE_REQUEST_HASH}-complete"),
        last_sequence: None,
        observation_count: 0,
    }
}

fn fail_input(node_id: NodeId, id: ScanSessionId, token: &SecretString) -> RemoteScanFailInput {
    RemoteScanFailInput {
        node_id,
        scan_session_id: id,
        incarnation_id: INCARNATION.parse().unwrap(),
        token: token.clone(),
        idempotency_key: "fail-session".to_owned(),
        request_hash: format!("{PRIVATE_REQUEST_HASH}-fail"),
        reason: ScanTerminalReason::new("scanner reported failure").unwrap(),
    }
}

async fn seed_rooted_location(pool: &sqlx::SqlitePool, root_id: StorageRootId, locator: &str) {
    let asset = sqlx::query("INSERT INTO file_assets (created_at, epoch) VALUES (?, 0)")
        .bind("1970-01-01T00:00:00Z")
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
    let version = sqlx::query(
        "INSERT INTO file_versions (file_asset_id, content_hash, size_bytes, produced_by, \
         produced_from_version_id, created_at, retired_at, epoch) \
         VALUES (?, 'scan-session-envelope-hash', 1, 'ingest', NULL, ?, NULL, 0)",
    )
    .bind(asset)
    .bind("1970-01-01T00:00:00Z")
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    sqlx::query(
        "INSERT INTO file_locations (file_version_id, address_state, storage_root_id, \
         provider_relative_locator, observed_at, epoch) VALUES (?, 'rooted', ?, ?, ?, 0)",
    )
    .bind(version)
    .bind(i64::try_from(root_id.0).unwrap())
    .bind(locator)
    .bind("1970-01-01T00:00:00Z")
    .execute(pool)
    .await
    .unwrap();
}

fn success(fixture: &Fixture, args: &[&str]) -> Value {
    let output = command(&fixture.url, args).output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_no_private_facts(fixture, &output);
    let mut value = single_envelope(output);
    assert_eq!(value["schema_version"], "0");
    assert_eq!(value["command"], "scan-session");
    assert_eq!(value["status"], "ok");
    assert!(value["error"].is_null());
    redact_local(&mut value);
    value
}

fn command(url: &str, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
    command.args(["--database-url", url, "--log-level", "error"]);
    command.args(args);
    command
}

fn single_envelope(output: Output) -> Value {
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout.lines().count(),
        1,
        "stdout must contain exactly one JSON value: {stdout:?}"
    );
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|error| panic!("stdout must be one JSON envelope: {stdout:?}: {error}"))
}

fn assert_no_private_facts(fixture: &Fixture, output: &Output) {
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for secret in [
        fixture.token.expose_secret(),
        PRIVATE_PROVIDER_LOCATOR,
        PRIVATE_OBSERVATION_LOCATOR,
        PRIVATE_OBJECT_IDENTITY,
        PRIVATE_REQUEST_HASH,
    ] {
        assert!(!combined.contains(secret), "private value leaked: {secret}");
    }
}

fn assert_public_reconciliation(envelope: &Value) {
    for item in envelope["data"]["items"].as_array().unwrap() {
        let keys = item
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            [
                "file_location_id",
                "retired_at",
                "prior_epoch",
                "retired_epoch"
            ]
        );
    }
}

fn redact_local(value: &mut Value) {
    value["local"]["db_url"] = Value::String("[db-url]".to_owned());
    value["local"]["config_path"] = Value::String("[config-path]".to_owned());
}

fn assert_bad_args(args: Vec<&str>) {
    let output = Command::new(env!("CARGO_BIN_EXE_voom"))
        .args(args)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let json = single_envelope(output);
    assert_eq!(json["command"], "cli");
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "BAD_ARGS");
}

fn assert_iso_timestamp(value: &Value) {
    let timestamp = value.as_str().unwrap();
    OffsetDateTime::parse(timestamp, &time::format_description::well_known::Rfc3339).unwrap();
}

fn at(seconds: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(seconds).unwrap()
}
