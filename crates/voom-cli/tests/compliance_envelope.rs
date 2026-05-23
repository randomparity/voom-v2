#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::NamedTempFile;
use voom_policy::{FixtureName, load_fixture, load_policy_fixture};
use voom_store::repo::workers::{NewCapability, NewGrant, NewWorker, WorkerKind};
use voom_store::test_support::sqlite_url_for;

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

    assert_eq!(output.status.code(), Some(0));
    let mut json = envelope(output.stdout);
    assert_eq!(json["data"]["execution"]["submitted_node_count"], 1);
    assert_eq!(json["data"]["execution"]["dispatch_count"], 1);
    redact_local(&mut json);
    redact_job_id(&mut json);
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
}

fn redact_job_id(json: &mut Value) {
    json["data"]["execution"]["job_id"] = Value::String("[job-id]".to_owned());
}

struct RemuxProviderLaunch {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl RemuxProviderLaunch {
    async fn start(url: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let pool = voom_store::connect(url).await?;
        let cp = voom_control_plane::ControlPlane::open_with_pool(
            pool,
            std::sync::Arc::new(voom_core::SystemClock),
        )
        .await?;
        let secret = "cli-compliance-remux-secret";
        let worker = cp
            .register_worker(NewWorker {
                name: "cli-compliance-remux".to_owned(),
                kind: WorkerKind::Synthetic,
                registered_at: cp.clock().now(),
            })
            .await?;
        let mut child = Command::new(provider_binary("fake-remuxer")?)
            .env("VOOM_WORKER_SECRET", secret)
            .env("VOOM_WORKER_ID", worker.id.0.to_string())
            .env("VOOM_WORKER_EPOCH", "0")
            .env("VOOM_WORKER_BIND", "127.0.0.1:0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child.stdin.take();
        let bound = read_bound_addr(&mut child)?;
        cp.record_capability(NewCapability {
            worker_id: worker.id,
            operation: "remux".to_owned(),
            codecs: Vec::new(),
            hardware: Vec::new(),
            artifact_access: Vec::new(),
            extra: json!({
                "endpoint": bound.to_string(),
                "secret": secret,
            }),
        })
        .await?;
        cp.record_grant(NewGrant {
            worker_id: worker.id,
            can_execute: vec!["remux".to_owned()],
            can_access_read: Vec::new(),
            can_access_write: Vec::new(),
            denies: Vec::new(),
            max_parallel: json!({ "remux": 1 }),
        })
        .await?;
        Ok(Self { child, stdin })
    }

    fn shutdown(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        drop(self.stdin.take());
        let started = std::time::Instant::now();
        loop {
            if let Some(status) = self.child.try_wait()? {
                if status.success() {
                    return Ok(());
                }
                return Err(Box::new(std::io::Error::other(format!(
                    "fake-remuxer exited with {status}"
                ))));
            }
            if started.elapsed() > Duration::from_secs(5) {
                let _ = self.child.kill();
                return Err(Box::new(std::io::Error::other(
                    "fake-remuxer cleanup timed out",
                )));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn read_bound_addr(child: &mut Child) -> Result<std::net::SocketAddr, Box<dyn std::error::Error>> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("fake-remuxer stdout missing"))?;
    let mut lines = std::io::BufReader::new(stdout).lines();
    let line = lines
        .next()
        .transpose()?
        .ok_or_else(|| std::io::Error::other("fake-remuxer exited before bind line"))?;
    Ok(line
        .strip_prefix("BOUND addr=")
        .ok_or_else(|| std::io::Error::other(format!("malformed fake-remuxer bind line: {line}")))?
        .parse::<std::net::SocketAddr>()?)
}

fn provider_binary(name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let env_name = format!("CARGO_BIN_EXE_{name}");
    if let Some(path) = std::env::var_os(env_name) {
        return Ok(PathBuf::from(path));
    }
    let status = Command::new("cargo")
        .args(["build", "-p", "voom-fakes", "--bin", name])
        .current_dir(workspace_root())
        .status()?;
    if !status.success() {
        return Err(Box::new(std::io::Error::other(format!(
            "fake provider build exited with {status}"
        ))));
    }
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    Ok(workspace_root()
        .join("target")
        .join("debug")
        .join(format!("{name}{suffix}")))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}
