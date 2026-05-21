#![expect(
    clippy::unwrap_used,
    reason = "integration tests use direct process assertions"
)]

use std::process::Stdio;
use std::time::Duration;

use secrecy::SecretString;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin};
use voom_worker_protocol::{
    ClientHandle, HttpClient, OperationKind, OperationRequest, WorkerCredentials,
};

#[tokio::test]
async fn baseline_launches_and_returns_ordered_frames() {
    let mut launch = spawn_chaos().await;
    let client = HttpClient::new(launch.bound);
    let req = OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id: voom_core::LeaseId(101),
        payload: serde_json::json!({"path": "/library/example.mkv"}),
        heartbeat_deadline_ms: 1000,
        progress_idle_deadline_ms: 1000,
    };
    let mut stream = client
        .dispatch(&launch.credentials, "baseline-launch", req)
        .await
        .unwrap();
    let first = stream.frames.next_frame().await.unwrap();
    assert!(matches!(
        first,
        voom_worker_protocol::NdjsonOutcome::Frame(_)
    ));
    let terminal = stream.frames.next_frame().await.unwrap();
    assert!(matches!(
        terminal,
        voom_worker_protocol::NdjsonOutcome::Terminated(_)
    ));
    launch.shutdown().await;
}

#[tokio::test]
async fn invalid_payload_returns_protocol_error_and_worker_stays_alive() {
    let mut launch = spawn_chaos().await;
    let client = HttpClient::new(launch.bound);
    let req = OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id: voom_core::LeaseId(102),
        payload: serde_json::json!({}),
        heartbeat_deadline_ms: 1000,
        progress_idle_deadline_ms: 1000,
    };
    let err = client
        .dispatch(&launch.credentials, "invalid-payload", req)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        voom_worker_protocol::ProtocolError::InvalidPayload { .. }
    ));
    assert!(launch.child.try_wait().unwrap().is_none());
    launch.shutdown().await;
}

struct TestLaunch {
    child: Child,
    stdin: Option<ChildStdin>,
    bound: std::net::SocketAddr,
    credentials: WorkerCredentials,
}

impl TestLaunch {
    async fn shutdown(&mut self) {
        drop(self.stdin.take());
        let status = tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success(), "status={status}");
    }
}

async fn spawn_chaos() -> TestLaunch {
    let worker_id = voom_core::WorkerId(1);
    let worker_epoch = 0;
    let secret = "phase1-bootstrap-secret";
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_chaos-worker"))
        .env("VOOM_WORKER_SECRET", secret)
        .env("VOOM_WORKER_ID", worker_id.0.to_string())
        .env("VOOM_WORKER_EPOCH", worker_epoch.to_string())
        .env("VOOM_WORKER_BIND", "127.0.0.1:0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let bound = line
        .strip_prefix("BOUND addr=")
        .unwrap()
        .parse::<std::net::SocketAddr>()
        .unwrap();
    TestLaunch {
        child,
        stdin,
        bound,
        credentials: WorkerCredentials {
            worker_id,
            worker_epoch,
            secret: SecretString::from(secret),
        },
    }
}
