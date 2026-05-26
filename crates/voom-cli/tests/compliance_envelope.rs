#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use tempfile::{NamedTempFile, TempDir};
use time::OffsetDateTime;
use voom_control_plane::cases::policy_inputs::PolicyInputFromScanInput;
use voom_policy::{FixtureName, load_fixture, load_policy_fixture};
use voom_store::repo::identity::{DiscoveredFile, FileLocationKind, IngestOutcome};
use voom_store::test_support::sqlite_url_for;
use voom_test_support::worker::{TestWorkerConfig, TestWorkerLaunch, cargo_bin_or_build};

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
async fn execute_outputs_report_and_execution_summary() {
    let seeded = seed(FixtureName::SyntheticNoncompliantTranscodeNeeded).await;
    let mut provider = RemuxProviderLaunch::start(&seeded.url).await.unwrap();

    let output = compliance_command(&seeded.url, "execute", seeded.version_id, seeded.input_id);
    provider.shutdown().unwrap();

    assert_eq!(output.status.code(), Some(2));
    let mut json = envelope(output.stdout);
    assert_eq!(json["error"]["code"], "CONFIG_INVALID");
    assert!(
        json["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("workflow root payload binding"))
    );
    assert!(json["error"]["message"].as_str().is_some_and(|message| {
        message.contains("remux requires file_version or file_location target")
    }));
    assert_eq!(json["data"]["execution"]["submitted_node_count"], 1);
    assert_eq!(json["data"]["execution"]["dispatch_count"], 0);
    redact_local(&mut json);
    insta::assert_json_snapshot!("execute_outputs_report_and_execution_summary", json);
}

#[tokio::test]
async fn execute_scanned_remux_outputs_ticket_result_ids() {
    let seeded = seed_scanned_remux().await;
    let mut provider = RemuxProviderLaunch::start(&seeded.url).await.unwrap();

    let remux_root = seeded.dir.path().canonicalize().unwrap();
    let staging_root = remux_root.join("stage");
    let output_dir = remux_root.join("out");
    let ffprobe_bin = fake_ffprobe_bin(&remux_root);
    let output = compliance_execute_command_with_dirs(
        &seeded.url,
        seeded.version_id,
        seeded.input_id,
        &staging_root,
        &output_dir,
        &ffprobe_bin,
    );
    provider.shutdown().unwrap();

    assert_eq!(output.status.code(), Some(0));
    let mut json = envelope(output.stdout);
    assert_eq!(json["command"], "compliance");
    assert_eq!(json["status"], "ok");
    assert_eq!(json["data"]["tickets"].as_array().unwrap().len(), 1);
    let result = &json["data"]["tickets"][0]["result"];
    for field in [
        "job_id",
        "ticket_id",
        "lease_id",
        "source_file_version_id",
        "source_file_location_id",
        "staged_artifact_handle_id",
        "staged_artifact_location_id",
        "verification_id",
        "commit_record_id",
        "result_file_version_id",
        "result_file_location_id",
        "result_media_snapshot_id",
    ] {
        assert!(
            result[field].is_number(),
            "{field} should be a stable numeric id"
        );
    }
    redact_local(&mut json);
    redact_remux_ticket_paths(&mut json);
    insta::assert_json_snapshot!("execute_scanned_remux_outputs_ticket_result_ids", json);
}

#[tokio::test]
async fn execute_scanned_remux_existing_target_outputs_failure_envelope() {
    let seeded = seed_scanned_remux().await;
    let mut provider = RemuxProviderLaunch::start(&seeded.url).await.unwrap();

    let remux_root = seeded.dir.path().canonicalize().unwrap();
    let staging_root = remux_root.join("stage");
    let output_dir = remux_root.join("out");
    let ffprobe_bin = fake_ffprobe_bin(&remux_root);
    std::fs::create_dir_all(&output_dir).unwrap();
    std::fs::write(output_dir.join("Movie.remux.mkv"), b"existing").unwrap();

    let output = compliance_execute_command_with_dirs(
        &seeded.url,
        seeded.version_id,
        seeded.input_id,
        &staging_root,
        &output_dir,
        &ffprobe_bin,
    );
    provider.shutdown().unwrap();

    assert_eq!(output.status.code(), Some(2));
    let mut json = envelope(output.stdout);
    assert_eq!(json["command"], "compliance");
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "CONFIG_INVALID");
    assert!(
        json["error"]["message"]
            .as_str()
            .is_some_and(|message| { message.contains("remux target path already exists") }),
        "stdout={} stderr={}",
        serde_json::to_string_pretty(&json).unwrap(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(json["data"]["execution"]["dispatch_count"], 1);
    assert_eq!(json["data"]["execution"]["failure_count"], 1);
    redact_local(&mut json);
    redact_temp_path_values(&mut json, &remux_root);
    insta::assert_json_snapshot!(
        "execute_scanned_remux_existing_target_outputs_failure_envelope",
        json
    );
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
    _tmp: NamedTempFile,
    dir: TempDir,
    url: String,
    version_id: u64,
    input_id: u64,
}

async fn seed(fixture: FixtureName) -> Seeded {
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
        .create_policy_input_set(load_fixture(fixture).unwrap())
        .await
        .unwrap();
    Seeded {
        _tmp: tmp,
        dir: TempDir::new().unwrap(),
        url,
        version_id: created.version.id.0,
        input_id: input.id.0,
    }
}

async fn seed_scanned_remux() -> Seeded {
    let tmp = NamedTempFile::new().unwrap();
    let dir = TempDir::new().unwrap();
    let root = dir.path().canonicalize().unwrap();
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
    let source = root.join("Movie.mp4");
    let source_bytes = b"source bytes";
    std::fs::write(&source, source_bytes).unwrap();
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: source.display().to_string(),
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
        panic!("seed_scanned_remux should create a new file asset");
    };
    let snapshot = cp
        .record_media_snapshot(
            file_version_id,
            None,
            json!({
                "streams": [
                    {
                        "id": "stream-0",
                        "index": 0,
                        "kind": "video",
                        "codec_name": "h264",
                        "disposition": {"default": true}
                    },
                    {
                        "id": "stream-1",
                        "index": 1,
                        "kind": "audio",
                        "codec_name": "aac",
                        "language": "eng",
                        "channels": 2,
                        "disposition": {"default": false}
                    }
                ]
            }),
            OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "cli-scan-remux".to_owned(),
            file_version_id,
            media_snapshot_id: snapshot.id,
            container: "mp4".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap();
    Seeded {
        _tmp: tmp,
        dir,
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

fn compliance_execute_command_with_dirs(
    url: &str,
    version_id: u64,
    input_id: u64,
    staging_root: &Path,
    output_dir: &Path,
    ffprobe_bin: &Path,
) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_voom"))
        .env("VOOM_FFPROBE_BIN", ffprobe_bin)
        .args([
            "--database-url",
            url,
            "compliance",
            "execute",
            "--policy-version-id",
            &version_id.to_string(),
            "--input-set-id",
            &input_id.to_string(),
            "--staging-root",
            &staging_root.display().to_string(),
            "--output-dir",
            &output_dir.display().to_string(),
        ])
        .output()
        .unwrap()
}

fn fake_ffprobe_bin(dir: &Path) -> PathBuf {
    let path = dir.join(format!("ffprobe-test{}", script_suffix()));
    std::fs::write(
        dir.join("basic-mp4.json"),
        include_str!("../../voom-ffprobe-worker/fixtures/ffprobe/basic-mp4.json"),
    )
    .unwrap();
    std::fs::write(&path, fake_ffprobe_script()).unwrap();
    make_executable(&path);
    path
}

#[cfg(unix)]
fn script_suffix() -> &'static str {
    ""
}

#[cfg(windows)]
fn script_suffix() -> &'static str {
    ".cmd"
}

#[cfg(unix)]
fn fake_ffprobe_script() -> String {
    "#!/bin/sh\n\
     if [ \"${1:-}\" = '-version' ]; then printf 'ffprobe version test-helper\\n'; exit 0; fi\n\
     script_dir=${0%/*}\n\
     if [ \"$script_dir\" = \"$0\" ]; then script_dir=.; fi\n\
     cat \"$script_dir/basic-mp4.json\"\n"
        .to_owned()
}

#[cfg(windows)]
fn fake_ffprobe_script() -> String {
    "@echo off\r\n\
     if \"%1\"==\"-version\" echo ffprobe version test-helper& exit /B 0\r\n\
     type \"%~dp0basic-mp4.json\"\r\n"
        .to_owned()
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[cfg(windows)]
fn make_executable(_path: &Path) {}

fn envelope(stdout: Vec<u8>) -> Value {
    let stdout = String::from_utf8(stdout).unwrap();
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be one JSON envelope; got {stdout:?}: {e}"))
}

fn redact_local(json: &mut Value) {
    json["local"]["db_url"] = Value::String("[db-url]".to_owned());
    json["local"]["config_path"] = Value::String("[config-path]".to_owned());
    if json["data"]["execution"]["job_id"].is_number() {
        json["data"]["execution"]["job_id"] = Value::String("[job-id]".to_owned());
    }
}

fn redact_remux_ticket_paths(json: &mut Value) {
    let Some(result) = json["data"]["tickets"]
        .as_array_mut()
        .and_then(|tickets| tickets.first_mut())
        .and_then(|ticket| ticket.get_mut("result"))
    else {
        return;
    };
    if result["staging_path"].is_string() {
        result["staging_path"] = Value::String("[staging-path]".to_owned());
    }
    if result["target_path"].is_string() {
        result["target_path"] = Value::String("[target-path]".to_owned());
    }
}

fn redact_temp_path_values(json: &mut Value, temp_dir: &Path) {
    match json {
        Value::String(value) => {
            *value = value.replace(&temp_dir.display().to_string(), "[tmp-dir]");
        }
        Value::Array(values) => {
            for value in values {
                redact_temp_path_values(value, temp_dir);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                redact_temp_path_values(value, temp_dir);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn blake3_checksum(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

struct RemuxProviderLaunch {
    inner: TestWorkerLaunch,
}

impl RemuxProviderLaunch {
    async fn start(url: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let pool = voom_store::connect(url).await?;
        let cp = voom_control_plane::ControlPlane::open_with_pool(
            pool,
            std::sync::Arc::new(voom_core::SystemClock),
        )
        .await?;
        Ok(Self {
            inner: TestWorkerLaunch::start(
                &cp,
                TestWorkerConfig::synthetic(
                    cargo_bin_or_build("voom-fakes", "fake-remuxer")?,
                    "cli-compliance-remux",
                    "cli-compliance-remux-secret",
                    "remux",
                ),
            )
            .await?,
        })
    }

    fn shutdown(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.inner.shutdown()
    }
}
