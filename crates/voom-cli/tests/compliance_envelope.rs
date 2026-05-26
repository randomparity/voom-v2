#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::process::Command;

use serde_json::{Value, json};
use tempfile::NamedTempFile;
use voom_policy::{FixtureName, load_fixture, load_policy_fixture};
use voom_store::test_support::sqlite_url_for;
use voom_test_support::worker::{TestWorkerConfig, TestWorkerLaunch, cargo_bin_or_build};

#[tokio::test]
async fn report_outputs_compliance_report_envelope() {
    let seeded = seed(FixtureName::SyntheticNoncompliantTranscodeNeeded).await;

    let output = compliance_command(&seeded.url, "report", seeded.version_id, seeded.input_id);

    assert_eq!(output.status.code(), Some(0));
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
        url,
        version_id: created.version.id.0,
        input_id: input.id.0,
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
