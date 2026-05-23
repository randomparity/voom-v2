#![expect(
    clippy::panic_in_result_fn,
    reason = "integration test assertions should fail fast after setup errors use Result"
)]

use voom_control_plane::ControlPlane;
use voom_policy::{FixtureName, load_fixture, load_policy_fixture};
use voom_store::repo::workers::{NewCapability, NewGrant, NewWorker, WorkerKind};
use voom_worker_protocol::OperationKind;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin};

#[tokio::test]
async fn compliance_execute_runs_set_container_as_remux_through_workflow_executor()
-> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::NamedTempFile::new()?;
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::init(&url).await?;
    let pool = voom_store::connect(&url).await?;
    let cp = ControlPlane::open(&url).await?;
    let source = load_policy_fixture("fixtures/policies/container-metadata.voom")?;
    let created_policy = cp
        .create_policy_document("container-metadata", &source)
        .await
        .map_err(|err| std::io::Error::other(format!("{err:?}")))?;
    let input = cp
        .create_policy_input_set(load_fixture(
            FixtureName::SyntheticNoncompliantTranscodeNeeded,
        )?)
        .await?;
    let mut provider = RemuxProviderLaunch::start(&cp).await?;

    let result = async {
        cp.execute_compliance_policy(created_policy.version.id, input.id)
            .await
            .map_err(|err| std::io::Error::other(err.source.to_string()))
    }
    .await;
    let data = combine_result_and_cleanup(result, provider.shutdown().await)?;

    assert!(data.execution.job_id.is_some());
    assert_eq!(data.execution.submitted_node_count, 1);
    assert_eq!(data.execution.dispatch_count, 1);
    assert_eq!(data.execution.failure_count, 0);
    assert_eq!(data.execution.per_operation.get("remux"), Some(&1));

    let ticket_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tickets WHERE kind = 'synthetic.workflow.operation.remux'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(ticket_count, 1);

    let lease_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM leases WHERE worker_id IN (SELECT worker_id FROM worker_capabilities WHERE operation = ?)",
    )
    .bind(operation_name(OperationKind::Remux))
    .fetch_one(&pool)
    .await?;
    assert_eq!(lease_count, 1);
    Ok(())
}

struct RemuxProviderLaunch {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl RemuxProviderLaunch {
    async fn start(cp: &ControlPlane) -> Result<Self, Box<dyn std::error::Error>> {
        let secret = "compliance-remux-secret";
        let worker = cp
            .register_worker(NewWorker {
                name: "compliance-remux".to_owned(),
                kind: WorkerKind::Synthetic,
                registered_at: cp.clock().now(),
            })
            .await?;
        let mut child = tokio::process::Command::new(provider_binary("fake-remuxer")?)
            .env("VOOM_WORKER_SECRET", secret)
            .env("VOOM_WORKER_ID", worker.id.0.to_string())
            .env("VOOM_WORKER_EPOCH", "0")
            .env("VOOM_WORKER_BIND", "127.0.0.1:0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child.stdin.take();
        let bound = read_bound_addr(&mut child).await?;
        cp.record_capability(NewCapability {
            worker_id: worker.id,
            operation: operation_name(OperationKind::Remux).to_owned(),
            codecs: Vec::new(),
            hardware: Vec::new(),
            artifact_access: Vec::new(),
            extra: serde_json::json!({
                "endpoint": bound.to_string(),
                "secret": secret,
            }),
        })
        .await?;
        cp.record_grant(NewGrant {
            worker_id: worker.id,
            can_execute: vec![operation_name(OperationKind::Remux).to_owned()],
            can_access_read: Vec::new(),
            can_access_write: Vec::new(),
            denies: Vec::new(),
            max_parallel: serde_json::json!({ operation_name(OperationKind::Remux): 1 }),
        })
        .await?;
        Ok(Self { child, stdin })
    }

    async fn shutdown(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        drop(self.stdin.take());
        let status = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await??;
        if status.success() {
            Ok(())
        } else {
            Err(Box::new(std::io::Error::other(format!(
                "fake-remuxer exited with {status}"
            ))))
        }
    }
}

async fn read_bound_addr(
    child: &mut Child,
) -> Result<std::net::SocketAddr, Box<dyn std::error::Error>> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("fake-remuxer stdout missing"))?;
    let mut lines = BufReader::new(stdout).lines();
    let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
        .await??
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
    let status = std::process::Command::new("cargo")
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

fn combine_result_and_cleanup<T>(
    result: Result<T, std::io::Error>,
    cleanup: Result<(), Box<dyn std::error::Error>>,
) -> Result<T, Box<dyn std::error::Error>> {
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(err), Ok(())) => Err(Box::new(err)),
        (Ok(_), Err(err)) => Err(err),
        (Err(err), Err(cleanup_err)) => Err(Box::new(std::io::Error::other(format!(
            "{err}; provider cleanup failed: {cleanup_err}"
        )))),
    }
}

fn operation_name(operation: OperationKind) -> &'static str {
    match operation {
        OperationKind::Remux => "remux",
        _ => unreachable!("test only needs remux"),
    }
}
