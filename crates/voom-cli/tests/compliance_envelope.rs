#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use serde_json::{Value, json};
use std::process::Command;

use tempfile::TempDir;
use time::OffsetDateTime;
use voom_control_plane::policy::PolicyInputFromScanInput;
use voom_policy::{FixtureName, load_fixture, load_policy_fixture};
use voom_store::repo::media::identity::{DiscoveredFile, IngestOutcome};
use voom_store::test_support::sqlite_url_for;
use voom_test_support::TempDatabase;
use voom_test_support::worker::cargo_bin_or_build;

const TEST_LOCAL_NODE_ID: &str = "9000001";

#[tokio::test]
async fn report_outputs_compliance_report_envelope() {
    let seeded = seed(FixtureName::SyntheticNoncompliantTranscodeNeeded).await;

    let output = compliance_command(&seeded.url, "report", seeded.version_id, seeded.input_id);

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut json = envelope(output.stdout);
    assert_eq!(json["command"], "compliance");
    assert_eq!(json["status"], "ok");
    assert_eq!(json["data"]["report"]["summary"]["status"], "mixed");
    redact_local(&mut json);
    insta::assert_json_snapshot!("report_outputs_compliance_report_envelope", json);
}

#[tokio::test]
async fn apply_outputs_report_and_issue_summary() {
    let seeded = seed(FixtureName::SyntheticNoncompliantTranscodeNeeded).await;

    let output = compliance_command(&seeded.url, "apply", seeded.version_id, seeded.input_id);

    assert_eq!(output.status.code(), Some(0));
    let mut json = envelope(output.stdout);
    assert_eq!(json["data"]["issues"]["created_count"], 1);
    redact_local(&mut json);
    insta::assert_json_snapshot!("apply_outputs_report_and_issue_summary", json);
}

#[tokio::test]
async fn execute_and_report_expose_policy_artifact_verification() {
    let _verify_worker =
        cargo_bin_or_build("voom-verify-artifact-worker", "voom-verify-artifact-worker").unwrap();
    let seeded = seed_scanned_verify().await;

    let execute = compliance_command(&seeded.url, "execute", seeded.version_id, seeded.input_id);
    assert_eq!(
        execute.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&execute.stdout),
        String::from_utf8_lossy(&execute.stderr)
    );
    let execute = envelope(execute.stdout);
    assert_eq!(execute["status"], "ok");
    assert_eq!(execute["data"]["file_phases"][0]["outcome"], "verified");
    assert_eq!(
        execute["data"]["artifact_verifications"][0]["status"],
        "succeeded"
    );
    let job_id = execute["data"]["summary"]["job_id"].as_u64().unwrap();
    let verification_id = execute["data"]["artifact_verifications"][0]["verification_id"].clone();

    let report = Command::new(env!("CARGO_BIN_EXE_voom"))
        .args([
            "--database-url",
            &seeded.url,
            "compliance",
            "report",
            "--job-id",
            &job_id.to_string(),
        ])
        .output()
        .unwrap();
    assert_eq!(report.status.code(), Some(0));
    let report = envelope(report.stdout);
    assert_eq!(report["status"], "ok");
    assert_eq!(
        report["data"]["artifact_verifications"][0]["verification_id"],
        verification_id
    );
    assert_eq!(report["data"]["file_phases"][0]["outcome"], "verified");
}

#[tokio::test]
async fn report_unknown_job_id_uses_not_found() {
    let seeded = seed(FixtureName::SyntheticNoncompliantTranscodeNeeded).await;

    let output = Command::new(env!("CARGO_BIN_EXE_voom"))
        .args([
            "--database-url",
            &seeded.url,
            "compliance",
            "report",
            "--job-id",
            "999999",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let mut json = envelope(output.stdout);
    assert_eq!(json["command"], "compliance");
    assert_eq!(json["error"]["code"], "NOT_FOUND");
    redact_local(&mut json);
    insta::assert_json_snapshot!("report_unknown_job_id_uses_not_found", json);
}

#[test]
fn report_with_no_selector_args_is_bad_args() {
    // The argument combination is rejected before any DB open, so an in-memory
    // url is enough.
    let output = Command::new(env!("CARGO_BIN_EXE_voom"))
        .args(["--database-url", "sqlite::memory:", "compliance", "report"])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(1),
        "no selector => BAD_ARGS exit 1"
    );
    let json = envelope(output.stdout);
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "BAD_ARGS");
}

#[test]
fn report_with_job_id_and_preview_arg_is_bad_args() {
    let output = Command::new(env!("CARGO_BIN_EXE_voom"))
        .args([
            "--database-url",
            "sqlite::memory:",
            "compliance",
            "report",
            "--job-id",
            "1",
            "--policy-version-id",
            "1",
        ])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(1),
        "clap conflict => BAD_ARGS exit 1"
    );
    let json = envelope(output.stdout);
    assert_eq!(json["error"]["code"], "BAD_ARGS");
}

#[tokio::test]
async fn report_missing_input_set_uses_not_found() {
    let seeded = seed(FixtureName::SyntheticNoncompliantTranscodeNeeded).await;

    let output = compliance_command(&seeded.url, "report", seeded.version_id, 999_999);

    assert_eq!(output.status.code(), Some(2));
    let mut json = envelope(output.stdout);
    assert_eq!(json["error"]["code"], "NOT_FOUND");
    redact_local(&mut json);
    insta::assert_json_snapshot!("report_missing_input_set_uses_not_found", json);
}

#[tokio::test]
async fn report_stale_policy_version_uses_policy_validation_error() {
    let seeded = seed_with_stale_policy().await;

    let output = compliance_command(&seeded.url, "report", seeded.version_id, seeded.input_id);

    assert_eq!(output.status.code(), Some(2));
    let mut json = envelope(output.stdout);
    assert_eq!(json["error"]["code"], "POLICY_VALIDATION_ERROR");
    redact_local(&mut json);
    insta::assert_json_snapshot!(
        "report_stale_policy_version_uses_policy_validation_error",
        json
    );
}

#[test]
fn execute_unsupported_operation_uses_policy_execution_error() {
    let json = json!({
        "schema_version": "0",
        "command": "compliance",
        "status": "error",
        "data": {
            "report": {"report_id": "report_test"},
            "issues": {"created_count": 1, "updated_count": 0, "resolved_count": 0, "skipped_count": 0},
            "execution": {"submitted_node_count": 0},
            "execution_diagnostic": {"code": "unsupported_execution_operation"}
        },
        "warnings": [],
        "error": {
            "code": "POLICY_EXECUTION_ERROR",
            "message": "policy execution error: unsupported execution operation unsupported_operation"
        }
    });
    insta::assert_json_snapshot!(
        "execute_unsupported_operation_uses_policy_execution_error",
        json
    );
}

struct Seeded {
    _tmp: TempDatabase,
    /// Keeps the seeded media bytes alive for the test's duration.
    _dir: tempfile::TempDir,
    url: String,
    version_id: u64,
    input_id: u64,
}

async fn seed(fixture: FixtureName) -> Seeded {
    let tmp = TempDatabase::new().unwrap();
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
        .create_policy_input_set(load_fixture(fixture).unwrap())
        .await
        .unwrap();
    Seeded {
        _dir: tempfile::TempDir::new().unwrap(),
        _tmp: tmp,
        url,
        version_id: created.version.id.0,
        input_id: input.id.0,
    }
}

/// Background stand-in for the storage-owner agent (ADR 0074): drives every
/// pending commit intent to convergence so CLI commits complete.
fn spawn_commit_driver(url: &str) {
    let url = url.to_owned();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let pool = voom_store::connect(&url).await.unwrap();
            let node = voom_test_support::commit_node::SimulatedOwnerNode::new().unwrap();
            node.install(&pool).await.unwrap();
            let cp = voom_control_plane::ControlPlane::open(&url).await.unwrap();
            loop {
                let pending: Option<(i64, i64)> = sqlx::query_as(
                    "SELECT id, artifact_handle_id FROM artifact_commit_intents \
                     WHERE state = 'pending' ORDER BY id ASC LIMIT 1",
                )
                .fetch_optional(&pool)
                .await
                .unwrap();
                if let Some((_, handle)) = pending {
                    let _ = node
                        .drive_pending_commit(
                            &cp,
                            &pool,
                            voom_core::ArtifactHandleId(u64::try_from(handle).unwrap()),
                        )
                        .await;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        });
    });
}

async fn seed_scanned_verify() -> Seeded {
    let tmp = TempDatabase::new().unwrap();
    let dir = TempDir::new().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let url = sqlite_url_for(tmp.path());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    voom_store::test_support::seed_test_storage_root(&pool)
        .await
        .unwrap();

    spawn_commit_driver(&url);
    let cp = voom_control_plane::ControlPlane::open_with_pool(
        pool,
        std::sync::Arc::new(voom_core::SystemClock),
    )
    .await
    .unwrap();
    let created = cp
        .create_policy_document(
            "verify-artifact",
            "policy \"verify artifact\" { phase verify { verify artifact } }",
        )
        .await
        .unwrap();
    let source = root.join("Movie.mkv");
    let source_bytes = b"cli policy verify bytes";
    std::fs::write(&source, source_bytes).unwrap();
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                storage_root_id: voom_store::test_support::TEST_STORAGE_ROOT_ID,
                provider_relative_locator: voom_store::test_support::test_relative_locator(
                    &source.display().to_string(),
                ),
                content_hash: blake3_checksum(source_bytes),
                size_bytes: u64::try_from(source_bytes.len()).unwrap(),
                observed_at: OffsetDateTime::UNIX_EPOCH,
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_version_id, ..
    } = outcome
    else {
        panic!("seed_scanned_verify should create a new file asset");
    };
    let snapshot = cp
        .record_media_snapshot(
            file_version_id,
            None,
            json!({
                "container": { "format_name": "mkv" },
                "streams": [{
                    "id": "stream-0",
                    "index": 0,
                    "kind": "video",
                    "codec_name": "h264"
                }]
            }),
            OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "cli-scan-verify".to_owned(),
            file_version_id,
            media_snapshot_id: snapshot.id,
            container: "mkv".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap();
    Seeded {
        _tmp: tmp,
        _dir: dir,
        url,
        version_id: created.version.id.0,
        input_id: input.input_set_id.0,
    }
}

async fn seed_with_stale_policy() -> Seeded {
    let seeded = seed(FixtureName::SyntheticNoncompliantTranscodeNeeded).await;
    let pool = voom_store::connect(&seeded.url).await.unwrap();
    let cp = voom_control_plane::ControlPlane::open_with_pool(
        pool,
        std::sync::Arc::new(voom_core::SystemClock),
    )
    .await
    .unwrap();
    cp.add_policy_version(
        voom_core::PolicyDocumentId(1),
        "policy \"container-metadata\" { phase normalize {} }",
    )
    .await
    .unwrap();
    seeded
}

fn compliance_command(
    url: &str,
    subcommand: &str,
    version_id: u64,
    input_id: u64,
) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_voom"))
        .env("VOOM_LOCAL_NODE_ID", TEST_LOCAL_NODE_ID)
        .args([
            "--database-url",
            url,
            "compliance",
            subcommand,
            "--policy-version-id",
            &version_id.to_string(),
            "--input-set-id",
            &input_id.to_string(),
        ])
        .output()
        .unwrap()
}

fn envelope(stdout: Vec<u8>) -> Value {
    let stdout = String::from_utf8(stdout).unwrap();
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be one JSON envelope; got {stdout:?}: {e}"))
}

fn redact_local(json: &mut Value) {
    json["local"]["db_url"] = Value::String("[db-url]".to_owned());
    json["local"]["config_path"] = Value::String("[config-path]".to_owned());
    if json["data"]["summary"]["job_id"].is_number() {
        json["data"]["summary"]["job_id"] = Value::String("[job-id]".to_owned());
    }
}

/// Replace the volatile DB row ids a committed file-phase row carries (produced
/// version/location, reprobe snapshot, and ticket ids) with stable placeholders
/// so the golden does not pin autoincrement ids.
fn blake3_checksum(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}
