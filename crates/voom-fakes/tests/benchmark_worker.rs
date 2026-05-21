#![expect(
    clippy::unwrap_used,
    reason = "integration tests use direct process assertions"
)]
#![expect(
    clippy::panic,
    reason = "integration tests fail fast on unexpected stream shapes"
)]

use std::process::Stdio;
use std::time::Duration;

use secrecy::SecretString;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin};
use voom_worker_protocol::{
    ClientHandle, HttpClient, NdjsonOutcome, OperationKind, OperationRequest, ProgressFrame,
    ProtocolError, WorkerCredentials,
};

#[tokio::test]
async fn baseline_launches_and_returns_ordered_frames() {
    let mut launch = spawn_benchmark().await;
    let client = HttpClient::new(launch.bound);
    let req = operation_request(101, serde_json::json!({"path": "/library/example.mkv"}));
    let mut stream = client
        .dispatch(&launch.credentials, "baseline-launch", req)
        .await
        .unwrap();
    let first = stream.frames.next_frame().await.unwrap();
    assert!(matches!(first, NdjsonOutcome::Frame(_)));
    let terminal = stream.frames.next_frame().await.unwrap();
    assert!(matches!(terminal, NdjsonOutcome::Terminated(_)));
    launch.shutdown().await;
}

#[tokio::test]
async fn invalid_payload_returns_protocol_error_and_worker_stays_alive() {
    let mut launch = spawn_benchmark().await;
    let client = HttpClient::new(launch.bound);
    let req = operation_request(102, serde_json::json!({}));
    let err = client
        .dispatch(&launch.credentials, "invalid-payload", req)
        .await
        .unwrap_err();
    assert!(matches!(err, ProtocolError::InvalidPayload { .. }));
    assert!(launch.child.try_wait().unwrap().is_none());
    launch.shutdown().await;
}

#[tokio::test]
async fn oversized_baseline_response_is_rejected_without_crashing() {
    let mut launch = spawn_benchmark().await;
    let client = HttpClient::new(launch.bound);
    let req = operation_request(103, serde_json::json!({"path": "x".repeat(64 * 1024)}));
    let err = client
        .dispatch(&launch.credentials, "oversized-baseline", req)
        .await
        .unwrap_err();
    assert!(matches!(err, ProtocolError::InvalidPayload { .. }));
    assert!(launch.child.try_wait().unwrap().is_none());
    launch.shutdown().await;
}

#[tokio::test]
async fn benchmark_mode_returns_cadence_progress_and_summary() {
    let mut launch = spawn_benchmark().await;
    let client = HttpClient::new(launch.bound);
    let req = operation_request(
        201,
        serde_json::json!({
            "path": "/library/example.mkv",
            "mode": "benchmark",
            "operations": 25,
            "emit_every": 10
        }),
    );
    let mut stream = client
        .dispatch(&launch.credentials, "benchmark-mode", req)
        .await
        .unwrap();
    let mut progress_cadence = Vec::new();
    let mut last_progress_elapsed = None;
    loop {
        match stream.frames.next_frame().await.unwrap() {
            NdjsonOutcome::Frame(frame) => {
                let ProgressFrame::Progress { payload, .. } = frame else {
                    panic!("expected progress frame");
                };
                let payload = payload.unwrap();
                assert_eq!(payload["mode"], "benchmark");
                assert_eq!(payload["operations_total"], 25);
                progress_cadence.push((
                    payload["sample_index"].as_u64().unwrap(),
                    payload["operations_completed"].as_u64().unwrap(),
                ));
                let elapsed = payload["elapsed_worker_ns"].as_u64().unwrap();
                assert!(elapsed > 0);
                if let Some(previous) = last_progress_elapsed {
                    assert!(elapsed >= previous);
                }
                last_progress_elapsed = Some(elapsed);
            }
            NdjsonOutcome::Terminated(frame) => {
                let ProgressFrame::Result { payload, .. } = frame else {
                    panic!("expected result frame");
                };
                assert_eq!(progress_cadence, vec![(0, 10), (1, 20), (2, 25)]);
                assert_eq!(payload["mode"], "benchmark");
                assert_eq!(payload["operations_total"], 25);
                assert_eq!(payload["progress_frames"], 3);
                let result_elapsed = payload["elapsed_worker_ns"].as_u64().unwrap();
                assert!(result_elapsed > 0);
                assert!(result_elapsed >= last_progress_elapsed.unwrap());
                assert!(payload["worker_ops_per_second_milli"].as_u64().unwrap() > 0);
                break;
            }
            other => panic!("unexpected outcome {other:?}"),
        }
    }
    launch.shutdown().await;
}

#[tokio::test]
async fn idempotent_replay_returns_same_benchmark_response() {
    let mut launch = spawn_benchmark().await;
    let client = HttpClient::new(launch.bound);
    let req = operation_request(
        202,
        serde_json::json!({
            "path": "/library/example.mkv",
            "mode": "benchmark",
            "operations": 10,
            "emit_every": 5
        }),
    );
    let first = collect_body(
        client
            .dispatch(&launch.credentials, "benchmark-replay", req.clone())
            .await
            .unwrap(),
    )
    .await;
    let second = collect_body(
        client
            .dispatch(&launch.credentials, "benchmark-replay", req)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(first, second);
    launch.shutdown().await;
}

#[tokio::test]
async fn same_idempotency_key_with_different_benchmark_body_is_rejected() {
    let mut launch = spawn_benchmark().await;
    let client = HttpClient::new(launch.bound);
    let first = operation_request(
        203,
        serde_json::json!({
            "path": "/library/example.mkv",
            "mode": "benchmark",
            "operations": 10,
            "emit_every": 5
        }),
    );
    let second = operation_request(
        203,
        serde_json::json!({
            "path": "/library/example.mkv",
            "mode": "benchmark",
            "operations": 10,
            "emit_every": 10
        }),
    );
    let _ = client
        .dispatch(&launch.credentials, "benchmark-conflict", first)
        .await
        .unwrap();
    let err = client
        .dispatch(&launch.credentials, "benchmark-conflict", second)
        .await
        .unwrap_err();
    assert!(matches!(err, ProtocolError::DuplicateIdempotencyKey { .. }));
    launch.shutdown().await;
}

#[tokio::test]
async fn too_many_progress_frames_is_rejected_without_crashing() {
    let mut launch = spawn_benchmark().await;
    let client = HttpClient::new(launch.bound);
    let req = operation_request(
        204,
        serde_json::json!({
            "path": "/library/example.mkv",
            "mode": "benchmark",
            "operations": 10_000,
            "emit_every": 1
        }),
    );
    let err = client
        .dispatch(&launch.credentials, "benchmark-too-many-frames", req)
        .await
        .unwrap_err();
    assert!(matches!(err, ProtocolError::InvalidPayload { .. }));
    assert!(launch.child.try_wait().unwrap().is_none());
    launch.shutdown().await;
}

async fn collect_body(mut stream: voom_worker_protocol::DispatchStream) -> Vec<ProgressFrame> {
    let mut frames = Vec::new();
    loop {
        match stream.frames.next_frame().await.unwrap() {
            NdjsonOutcome::Frame(frame) => frames.push(frame),
            NdjsonOutcome::Terminated(frame) => {
                frames.push(frame);
                return frames;
            }
            other => panic!("unexpected outcome {other:?}"),
        }
    }
}

fn operation_request(lease_id: u64, payload: serde_json::Value) -> OperationRequest {
    OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id: voom_core::LeaseId(lease_id),
        payload,
        heartbeat_deadline_ms: 1000,
        progress_idle_deadline_ms: 1000,
    }
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

async fn spawn_benchmark() -> TestLaunch {
    let worker_id = voom_core::WorkerId(1);
    let worker_epoch = 0;
    let secret = "phase1-bootstrap-secret";
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_benchmark-worker"))
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
