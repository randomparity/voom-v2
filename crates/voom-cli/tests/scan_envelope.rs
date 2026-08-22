#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

//! `voom scan` wire contracts after ADR 0077: the CLI requests a durable scan
//! session (and, without `--no-wait`, polls it to its terminal state). The
//! bytes are read by owner-node workers, so identity rows for downstream
//! policy commands are seeded through the real session chain (`scan_seed`).

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;
use voom_control_plane::ControlPlane;
use voom_control_plane::execution::{RemoteActivateInput, RemoteWorkerDeclaration};
use voom_control_plane::scan::{RemoteScanCompleteInput, RemoteScanStartInput};
use voom_control_plane::workers::RegisterNodeInput;
use voom_core::{ArtifactAccessMode, NodeIncarnationId, NodeKind, OperationKind};
use voom_policy::load_policy_fixture;
use voom_store::test_support::sqlite_url_for;
use voom_test_support::TempDatabase;
use voom_test_support::scan_seed::{SeedFile, seed_scanned_files};

/// Fixed incarnation for the waited-scan driver node.
const DRIVER_INCARNATION: &str = "0123456789abcdef0123456789abcdef";
/// Route-level `request_hash` inputs must be lowercase SHA-256-shaped; any
/// stable 64-char lowercase-hex digest satisfies the format gate.
const DRIVER_REQUEST_HASH: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[tokio::test]
async fn scan_request_outputs_durable_session_and_ticket() {
    let seeded = seed().await;

    let output = scan_command(&seeded).arg("--no-wait").output().unwrap();

    assert_status(&output, Some(0));
    let mut json = envelope(output.stdout);
    assert_eq!(json["command"], "scan");
    assert_eq!(json["status"], "ok");
    assert!(json["data"]["scan_session_id"].as_u64().unwrap() > 0);
    assert!(json["data"]["ticket_id"].as_u64().unwrap() > 0);
    redact_common(&mut json);
    insta::assert_json_snapshot!("scan_request_outputs_durable_session_and_ticket", json);
}

#[tokio::test]
async fn scan_blocked_root_emits_blocked_envelope() {
    let seeded = seed().await;
    let pool = voom_store::connect(&seeded.url).await.unwrap();
    sqlx::query("UPDATE library_roots SET enabled = 0 WHERE id = ?")
        .bind(i64::try_from(voom_store::test_support::TEST_STORAGE_ROOT_ID.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();

    let output = scan_command(&seeded).arg("--no-wait").output().unwrap();

    assert_status(&output, Some(2));
    let mut json = envelope(output.stdout);

    assert_eq!(json["command"], "scan");
    assert_eq!(json["error"]["code"], "BLOCKED");
    assert_eq!(json["data"]["status"], "blocked");
    assert_eq!(json["data"]["reason"], "root_disabled");
    assert_eq!(json["data"]["library_id"], 9_000_001);
    assert_eq!(
        json["data"]["storage_root_id"],
        voom_store::test_support::TEST_STORAGE_ROOT_ID.0
    );
    json["data"]["provider_locator"] = Value::String("[provider-locator]".to_owned());
    redact_common(&mut json);
    insta::assert_json_snapshot!("scan_blocked_root_emits_blocked_envelope", json);
}

#[tokio::test]
async fn scan_wait_reports_terminal_outcome() {
    let seeded = seed().await;
    let cp = ControlPlane::open(&seeded.url).await.unwrap();
    // Claim the root to a driver node this test holds credentials for, so the
    // CLI-requested session can be pumped to completion from here.
    let registered = cp
        .register_node(RegisterNodeInput {
            name: "scan-envelope-driver".to_owned(),
            kind: NodeKind::Remote,
            heartbeat_ttl_seconds: 600,
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap();
    let incarnation: NodeIncarnationId = DRIVER_INCARNATION.parse().unwrap();
    cp.remote_activate(RemoteActivateInput {
        node_id: registered.node.id,
        token: registered.token.clone(),
        idempotency_key: "scan-envelope-driver-activate".to_owned(),
        request_hash: DRIVER_REQUEST_HASH.to_owned(),
        incarnation_id: incarnation,
        workers: vec![RemoteWorkerDeclaration {
            logical_name: "scan-envelope-driver".to_owned(),
            operations: vec![OperationKind::ScanLibrary],
            artifact_access: vec![ArtifactAccessMode::SharedMount],
            max_parallel: 1,
        }],
    })
    .await
    .unwrap();
    let pool = voom_store::connect(&seeded.url).await.unwrap();
    sqlx::query("UPDATE library_roots SET owner_node_id = ? WHERE id = ?")
        .bind(i64::try_from(registered.node.id.0).unwrap())
        .bind(i64::try_from(voom_store::test_support::TEST_STORAGE_ROOT_ID.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();

    // The waited CLI polls until the session reaches a terminal state; the
    // test plays the owner-node agent and completes it under the CLI's feet.
    let child = scan_command(&seeded)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let scan_session_id = wait_for_requested_session(&pool).await;
    let token = registered.token;
    cp.start_scan_session(RemoteScanStartInput {
        node_id: registered.node.id,
        scan_session_id,
        incarnation_id: incarnation,
        token: token.clone(),
        idempotency_key: "scan-envelope-driver-start".to_owned(),
        request_hash: DRIVER_REQUEST_HASH.to_owned(),
    })
    .await
    .unwrap();
    let outcome = cp
        .complete_scan_session(RemoteScanCompleteInput {
            node_id: registered.node.id,
            scan_session_id,
            incarnation_id: incarnation,
            token: token.clone(),
            idempotency_key: "scan-envelope-driver-complete".to_owned(),
            request_hash: DRIVER_REQUEST_HASH.to_owned(),
            last_sequence: None,
            observation_count: 0,
        })
        .await
        .unwrap();
    assert_eq!(
        outcome.status,
        voom_core::ScanSessionStatus::Succeeded,
        "the driven session must complete successfully"
    );

    let output = child.wait_with_output().unwrap();
    assert_status(&output, Some(0));
    let json = envelope(output.stdout);
    assert_eq!(json["command"], "scan");
    assert_eq!(json["status"], "ok");
    assert_eq!(json["data"]["scan_session_id"], scan_session_id.0);
    assert_eq!(json["data"]["status"], "succeeded");
    assert_eq!(json["data"]["observation_count"], 0);
    assert_eq!(json["data"]["retired_location_count"], 0);
}

#[tokio::test]
async fn policy_input_create_from_scan_outputs_ids_for_scanned_file() {
    let seeded = seed().await;
    let source = seed_one_media_file(&seeded).await;
    let file_version_id = source.file_version_id.0.to_string();
    let media_snapshot_id = source.media_snapshot_id.0.to_string();

    let output = policy_input_from_scan_command(
        &seeded.url,
        "scan-h264",
        &file_version_id,
        &media_snapshot_id,
        "mp4",
        "h264",
    )
    .output()
    .unwrap();

    assert_status(&output, Some(0));
    let json = envelope(output.stdout);
    assert_eq!(json["command"], "policy");
    assert_eq!(json["status"], "ok");
    assert!(json["data"]["input_set"]["input_set_id"].as_u64().unwrap() > 0);
    assert_eq!(json["data"]["input_set"]["slug"], "scan-h264");
    assert_eq!(json["data"]["input_set"]["source_kind"], "imported");
    assert_eq!(
        json["data"]["input_set"]["file_version_id"],
        source.file_version_id.0
    );
    assert_eq!(
        json["data"]["input_set"]["media_snapshot_id"],
        source.media_snapshot_id.0
    );
}

#[tokio::test]
async fn policy_input_create_from_scan_can_feed_plan_show() {
    let seeded = seed().await;
    let source = seed_one_media_file(&seeded).await;
    let file_version_id = source.file_version_id.0.to_string();
    let media_snapshot_id = source.media_snapshot_id.0.to_string();
    let cp = voom_control_plane::ControlPlane::open(&seeded.url)
        .await
        .unwrap();
    let policy = cp
        .create_policy_document(
            "video-transcode-hevc",
            &load_policy_fixture("fixtures/policies/video-transcode-hevc.voom").unwrap(),
        )
        .await
        .unwrap();
    let create = policy_input_from_scan_command(
        &seeded.url,
        "scan-h264-plan",
        &file_version_id,
        &media_snapshot_id,
        "mp4",
        "h264",
    )
    .output()
    .unwrap();
    assert_status(&create, Some(0));
    let create_json = envelope(create.stdout);
    let input_set_id = create_json["data"]["input_set"]["input_set_id"]
        .as_u64()
        .unwrap()
        .to_string();

    let output = Command::new(env!("CARGO_BIN_EXE_voom"))
        .args([
            "--database-url",
            &seeded.url,
            "plan",
            "show",
            "--policy-version-id",
            &policy.version.id.0.to_string(),
            "--input-set-id",
            &input_set_id,
        ])
        .output()
        .unwrap();

    assert_status(&output, Some(0));
    let json = envelope(output.stdout);
    assert_eq!(json["command"], "plan");
    assert_eq!(json["status"], "ok");
    assert_eq!(
        json["data"]["plan"]["input"]["input_set_id"],
        input_set_id.parse::<u64>().unwrap()
    );
}

#[tokio::test]
async fn policy_input_create_from_scan_all_builds_whole_library() {
    let seeded = seed().await;
    seed_one_media_file(&seeded).await;

    let output = policy_input_whole_scan_command(&seeded.url, "whole")
        .output()
        .unwrap();

    assert_status(&output, Some(0));
    let json = envelope(output.stdout);
    assert_eq!(json["command"], "policy");
    assert_eq!(json["status"], "ok");
    assert!(json["data"]["input_set"]["input_set_id"].as_u64().unwrap() > 0);
    assert_eq!(json["data"]["input_set"]["slug"], "whole");
    assert_eq!(json["data"]["input_set"]["included_count"], 1);
    assert_eq!(json["data"]["input_set"]["skipped_count"], 0);
}

#[tokio::test]
async fn policy_input_create_from_scan_all_conflicts_with_single_file_args() {
    let seeded = seed().await;

    let output = Command::new(env!("CARGO_BIN_EXE_voom"))
        .args([
            "--database-url",
            &seeded.url,
            "policy",
            "input",
            "create-from-scan",
            "--slug",
            "whole",
            "--all",
            "--file-version-id",
            "1",
            "--media-snapshot-id",
            "1",
            "--container",
            "mp4",
            "--video-codec",
            "h264",
        ])
        .output()
        .unwrap();

    assert_status(&output, Some(1));
    let json = envelope(output.stdout);
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "BAD_ARGS");
}

#[tokio::test]
async fn policy_input_create_from_scan_without_a_mode_is_bad_args() {
    let seeded = seed().await;

    let output = Command::new(env!("CARGO_BIN_EXE_voom"))
        .args([
            "--database-url",
            &seeded.url,
            "policy",
            "input",
            "create-from-scan",
            "--slug",
            "whole",
        ])
        .output()
        .unwrap();

    assert_status(&output, Some(1));
    let json = envelope(output.stdout);
    assert_eq!(json["command"], "policy");
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "BAD_ARGS");
}

#[tokio::test]
async fn policy_input_create_from_scan_missing_rows_is_not_found() {
    let seeded = seed().await;

    let output = policy_input_from_scan_command(
        &seeded.url,
        "missing-scan",
        "999998",
        "999999",
        "mp4",
        "h264",
    )
    .output()
    .unwrap();

    assert_status(&output, Some(2));
    let json = envelope(output.stdout);
    assert_eq!(json["command"], "policy");
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "NOT_FOUND");
}

struct Seeded {
    _tmp: TempDatabase,
    root: TempDir,
    url: String,
}

async fn seed() -> Seeded {
    let tmp = TempDatabase::new().unwrap();
    let root = TempDir::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    voom_store::test_support::seed_test_storage_root(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE library_roots SET provider_locator = ?, display_locator = ? WHERE id = ?")
        .bind(root.path().display().to_string())
        .bind(root.path().display().to_string())
        .bind(i64::try_from(voom_store::test_support::TEST_STORAGE_ROOT_ID.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    Seeded {
        _tmp: tmp,
        root,
        url,
    }
}

/// Seed one tiny media fixture through the real scan-session chain and return
/// its published identity ids.
async fn seed_one_media_file(seeded: &Seeded) -> voom_test_support::scan_seed::SeededSource {
    let media = seeded.root.path().join("tiny.mp4");
    std::fs::copy(tiny_media_fixture(), &media).unwrap();
    let cp = ControlPlane::open(&seeded.url).await.unwrap();
    let seeded_sources = seed_scanned_files(
        &cp,
        &seeded.url,
        voom_store::test_support::TEST_STORAGE_ROOT_ID,
        &[SeedFile {
            locator: "tiny.mp4",
            path: &media,
            probe_snapshot: basic_mp4_probe_snapshot(),
        }],
    )
    .await
    .unwrap();
    seeded_sources[0]
}

/// Canned normalized probe snapshot matching what the real ffprobe worker
/// reports for the tiny fixture (`basic-mp4.json` once normalized).
fn basic_mp4_probe_snapshot() -> Value {
    serde_json::json!({
        "format": "sprint10-v1",
        "container": {
            "format_name": "mov,mp4,m4a,3gp,3g2,mj2",
            "format_long_name": "QuickTime / MOV",
        },
        "streams": [
            {
                "index": 0,
                "kind": "video",
                "codec_name": "h264",
                "width": 320,
                "height": 180,
            },
            {
                "index": 1,
                "kind": "audio",
                "codec_name": "aac",
                "channels": 2,
            },
        ],
    })
}

fn scan_command(seeded: &Seeded) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
    command
        .args([
            "--database-url",
            &seeded.url,
            "scan",
            "--root",
            &voom_store::test_support::TEST_STORAGE_ROOT_ID.0.to_string(),
        ])
        .env(
            "VOOM_LOCAL_NODE_ID",
            voom_store::test_support::TEST_STORAGE_ROOT_ID.0.to_string(),
        );
    command
}

/// Poll for the scan session the waited CLI just requested (the newest row in
/// the `requested` state).
async fn wait_for_requested_session(pool: &sqlx::SqlitePool) -> voom_core::ScanSessionId {
    let started = Instant::now();
    loop {
        let row: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM scan_sessions WHERE status = 'requested' \
             ORDER BY id DESC LIMIT 1",
        )
        .fetch_optional(pool)
        .await
        .unwrap();
        if let Some(id) = row {
            return voom_core::ScanSessionId(u64::try_from(id).unwrap());
        }
        assert!(
            started.elapsed() <= Duration::from_secs(10),
            "the waited scan never requested a session"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn policy_input_from_scan_command(
    url: &str,
    slug: &str,
    file_version_id: &str,
    media_snapshot_id: &str,
    container: &str,
    video_codec: &str,
) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
    command.args([
        "--database-url",
        url,
        "policy",
        "input",
        "create-from-scan",
        "--slug",
        slug,
        "--file-version-id",
        file_version_id,
        "--media-snapshot-id",
        media_snapshot_id,
        "--container",
        container,
        "--video-codec",
        video_codec,
    ]);
    command
}

fn policy_input_whole_scan_command(url: &str, slug: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
    command.args([
        "--database-url",
        url,
        "policy",
        "input",
        "create-from-scan",
        "--slug",
        slug,
        "--all",
    ]);
    command
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn tiny_media_fixture() -> PathBuf {
    workspace_root()
        .join("crates/voom-ffprobe-worker/fixtures/media/tiny.mp4")
        .canonicalize()
        .unwrap()
}

fn assert_status(output: &Output, expected: Option<i32>) {
    assert_eq!(
        output.status.code(),
        expected,
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn envelope(stdout: Vec<u8>) -> Value {
    let stdout = String::from_utf8(stdout).unwrap();
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("stdout must be one JSON envelope; got {stdout:?}: {err}"))
}

fn redact_common(json: &mut Value) {
    json["local"]["db_url"] = Value::String("[db-url]".to_owned());
    json["local"]["config_path"] = Value::String("[config-path]".to_owned());
}
