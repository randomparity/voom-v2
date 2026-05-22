#![expect(
    clippy::unwrap_used,
    reason = "integration tests use direct process assertions"
)]
#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests fail fast on unexpected stream shapes"
)]

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use secrecy::SecretString;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin};
use voom_worker_protocol::{
    ClientHandle, HttpClient, NdjsonOutcome, OperationKind, OperationRequest, ProgressFrame,
    ProtocolError, WorkerCredentials,
};

struct ProviderCase {
    bin_env: &'static str,
    name: &'static str,
    primary: OperationKind,
    secondary: &'static [OperationKind],
    valid_payload: serde_json::Value,
    invalid_payload: serde_json::Value,
    expected_field: &'static str,
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "table-driven integration test keeps each protocol assertion in one worker lifecycle"
)]
async fn fake_providers_follow_worker_protocol() {
    for case in provider_cases() {
        let mut launch = spawn_provider(&case).await;
        let client = HttpClient::new(launch.bound);

        let req = operation_request(101, case.primary, case.valid_payload.clone());
        let frames = collect_body(
            client
                .dispatch(&launch.credentials, &format!("{}-primary", case.name), req)
                .await
                .unwrap(),
        )
        .await;
        assert_two_frame_success(&case, case.primary, &frames);

        let invalid = operation_request(102, case.primary, case.invalid_payload.clone());
        let err = client
            .dispatch(
                &launch.credentials,
                &format!("{}-invalid", case.name),
                invalid,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ProtocolError::InvalidPayload { .. }));

        let unsupported = if case.name == "fake-backup-store" {
            OperationKind::ProbeFile
        } else {
            OperationKind::DeleteArtifact
        };
        let unsupported_req = operation_request(103, unsupported, case.valid_payload.clone());
        let err = client
            .dispatch(
                &launch.credentials,
                &format!("{}-unsupported", case.name),
                unsupported_req,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ProtocolError::UnknownOperation { .. }));

        for (index, secondary) in case.secondary.iter().copied().enumerate() {
            let req = operation_request(200 + index as u64, secondary, case.valid_payload.clone());
            let frames = collect_body(
                client
                    .dispatch(
                        &launch.credentials,
                        &format!("{}-secondary-{index}", case.name),
                        req,
                    )
                    .await
                    .unwrap(),
            )
            .await;
            assert_two_frame_success(&case, secondary, &frames);

            let invalid =
                operation_request(250 + index as u64, secondary, case.invalid_payload.clone());
            let err = client
                .dispatch(
                    &launch.credentials,
                    &format!("{}-secondary-invalid-{index}", case.name),
                    invalid,
                )
                .await
                .unwrap_err();
            assert!(matches!(err, ProtocolError::InvalidPayload { .. }));
        }

        let replay = operation_request(301, case.primary, case.valid_payload.clone());
        let first = collect_body(
            client
                .dispatch(
                    &launch.credentials,
                    &format!("{}-replay", case.name),
                    replay.clone(),
                )
                .await
                .unwrap(),
        )
        .await;
        let second = collect_body(
            client
                .dispatch(
                    &launch.credentials,
                    &format!("{}-replay", case.name),
                    replay,
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(first, second);

        let conflict = operation_request(301, case.primary, case.valid_payload.clone());
        let different = operation_request(301, case.primary, case.invalid_payload.clone());
        let _ = client
            .dispatch(
                &launch.credentials,
                &format!("{}-conflict", case.name),
                conflict,
            )
            .await
            .unwrap();
        let err = client
            .dispatch(
                &launch.credentials,
                &format!("{}-conflict", case.name),
                different,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ProtocolError::DuplicateIdempotencyKey { .. }));

        launch.shutdown().await;
    }
}

#[tokio::test]
async fn timed_fake_provider_streams_progress_before_terminal() {
    let case = ProviderCase {
        bin_env: "CARGO_BIN_EXE_fake-scanner",
        name: "fake-scanner",
        primary: OperationKind::ScanLibrary,
        secondary: &[],
        valid_payload: serde_json::json!({"path": "/library", "scenario": "timed"}),
        invalid_payload: serde_json::json!({"scenario": "missing_path"}),
        expected_field: "files",
    };
    let mut launch = spawn_provider(&case).await;
    let client = HttpClient::new(launch.bound);

    let req = operation_request(
        401,
        OperationKind::ScanLibrary,
        serde_json::json!({
            "path": "/library",
            "duration_ms": 150_u64,
            "progress_interval_ms": 50_u64
        }),
    );
    let stream = client.dispatch(&launch.credentials, "fake-scanner-timed", req);
    let mut stream = tokio::time::timeout(Duration::from_secs(2), stream)
        .await
        .expect("timed dispatch should expose response before terminal")
        .unwrap();

    let mut progress_count = 0_u32;
    let mut first_progress_at = None;
    let terminal_at = loop {
        let outcome = tokio::time::timeout(Duration::from_secs(2), stream.frames.next_frame())
            .await
            .expect("timed provider frame read should not hang")
            .unwrap();
        match outcome {
            NdjsonOutcome::Frame(ProgressFrame::Progress { .. }) => {
                progress_count += 1;
                first_progress_at.get_or_insert_with(Instant::now);
            }
            NdjsonOutcome::Terminated(ProgressFrame::Result { .. }) => break Instant::now(),
            other => panic!("unexpected timed frame outcome {other:?}"),
        }
    };

    assert!(
        progress_count >= 2,
        "expected at least two progress frames before terminal, got {progress_count}"
    );
    let streamed_elapsed = terminal_at.duration_since(first_progress_at.unwrap());
    assert!(
        streamed_elapsed >= Duration::from_millis(90),
        "expected progress to stream across wall-clock time, got {streamed_elapsed:?}"
    );

    launch.shutdown().await;
}

#[expect(
    clippy::too_many_lines,
    reason = "table-driven integration cases keep each provider contract visible"
)]
fn provider_cases() -> Vec<ProviderCase> {
    vec![
        ProviderCase {
            bin_env: "CARGO_BIN_EXE_fake-scanner",
            name: "fake-scanner",
            primary: OperationKind::ScanLibrary,
            secondary: &[],
            valid_payload: serde_json::json!({"path": "/library", "scenario": "default"}),
            invalid_payload: serde_json::json!({"scenario": "missing_path"}),
            expected_field: "files",
        },
        ProviderCase {
            bin_env: "CARGO_BIN_EXE_fake-prober",
            name: "fake-prober",
            primary: OperationKind::ProbeFile,
            secondary: &[OperationKind::HashFile],
            valid_payload: serde_json::json!({"path": "/library/movie.mkv", "scenario": "default"}),
            invalid_payload: serde_json::json!({"scenario": "missing_path"}),
            expected_field: "duration_ms",
        },
        ProviderCase {
            bin_env: "CARGO_BIN_EXE_fake-transcoder",
            name: "fake-transcoder",
            primary: OperationKind::TranscodeVideo,
            secondary: &[OperationKind::ExtractAudio, OperationKind::TranscribeAudio],
            valid_payload: serde_json::json!({
                "path": "/library/movie.mkv",
                "target_codec": "h265",
                "scenario": "default"
            }),
            invalid_payload: serde_json::json!({
                "path": "/library/movie.mkv",
                "target_codec": "bad_codec"
            }),
            expected_field: "output_path",
        },
        ProviderCase {
            bin_env: "CARGO_BIN_EXE_fake-remuxer",
            name: "fake-remuxer",
            primary: OperationKind::Remux,
            secondary: &[],
            valid_payload: serde_json::json!({
                "path": "/library/movie.mkv",
                "container": "mkv",
                "scenario": "default"
            }),
            invalid_payload: serde_json::json!({
                "path": "/library/movie.mkv",
                "container": "bad_container"
            }),
            expected_field: "container",
        },
        ProviderCase {
            bin_env: "CARGO_BIN_EXE_fake-backup-store",
            name: "fake-backup-store",
            primary: OperationKind::BackUpFile,
            secondary: &[OperationKind::DeleteArtifact],
            valid_payload: serde_json::json!({"path": "/library/movie.mkv", "scenario": "default"}),
            invalid_payload: serde_json::json!({"scenario": "missing_path"}),
            expected_field: "local_backup_id",
        },
        ProviderCase {
            bin_env: "CARGO_BIN_EXE_fake-health-checker",
            name: "fake-health-checker",
            primary: OperationKind::VerifyArtifact,
            secondary: &[],
            valid_payload: serde_json::json!({"path": "/library/movie.mkv", "scenario": "default"}),
            invalid_payload: serde_json::json!({"scenario": "missing_path"}),
            expected_field: "status",
        },
        ProviderCase {
            bin_env: "CARGO_BIN_EXE_fake-identity-provider",
            name: "fake-identity-provider",
            primary: OperationKind::IdentifyMedia,
            secondary: &[],
            valid_payload: serde_json::json!({"path": "/library/movie.mkv", "scenario": "default"}),
            invalid_payload: serde_json::json!({"scenario": "missing_path"}),
            expected_field: "canonical_media_id",
        },
        ProviderCase {
            bin_env: "CARGO_BIN_EXE_fake-external-system",
            name: "fake-external-system",
            primary: OperationKind::SyncExternalSystem,
            secondary: &[],
            valid_payload: serde_json::json!({
                "path": "/library/movie.mkv",
                "system": "plex",
                "action": "refresh",
                "scenario": "default"
            }),
            invalid_payload: serde_json::json!({
                "path": "/library/movie.mkv",
                "system": "unknown",
                "action": "refresh"
            }),
            expected_field: "refresh_status",
        },
        ProviderCase {
            bin_env: "CARGO_BIN_EXE_fake-quality-scorer",
            name: "fake-quality-scorer",
            primary: OperationKind::ScoreQuality,
            secondary: &[],
            valid_payload: serde_json::json!({
                "path": "/library/movie.mkv",
                "profile": "default",
                "scenario": "default"
            }),
            invalid_payload: serde_json::json!({
                "path": "/library/movie.mkv",
                "profile": "unknown"
            }),
            expected_field: "score",
        },
        ProviderCase {
            bin_env: "CARGO_BIN_EXE_fake-issue-provider",
            name: "fake-issue-provider",
            primary: OperationKind::CommitArtifact,
            secondary: &[],
            valid_payload: serde_json::json!({
                "path": "/library/movie.mkv",
                "reason": "quality_regression",
                "scenario": "default"
            }),
            invalid_payload: serde_json::json!({
                "path": "/library/movie.mkv",
                "reason": "unknown"
            }),
            expected_field: "issue_key",
        },
        ProviderCase {
            bin_env: "CARGO_BIN_EXE_fake-use-lease-provider",
            name: "fake-use-lease-provider",
            primary: OperationKind::EditTracks,
            secondary: &[],
            valid_payload: serde_json::json!({
                "path": "/library/movie.mkv",
                "holder": "manual",
                "reason": "playback",
                "scenario": "default"
            }),
            invalid_payload: serde_json::json!({
                "path": "/library/movie.mkv",
                "holder": "manual",
                "reason": "unknown"
            }),
            expected_field: "decision",
        },
    ]
}

fn assert_two_frame_success(
    case: &ProviderCase,
    operation: OperationKind,
    frames: &[ProgressFrame],
) {
    assert_eq!(frames.len(), 2);
    assert!(matches!(frames[0], ProgressFrame::Progress { seq: 0, .. }));
    let ProgressFrame::Result { seq, payload, .. } = &frames[1] else {
        panic!("terminal frame must be result");
    };
    assert_eq!(*seq, 1);
    assert_eq!(payload["provider"], case.name);
    assert_eq!(payload["operation"], operation_name(operation));
    assert_eq!(payload["scenario"], "default");
    assert!(
        payload.get(case.expected_field).is_some(),
        "{} missing {}",
        case.name,
        case.expected_field
    );
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

fn operation_request(
    lease_id: u64,
    operation: OperationKind,
    payload: serde_json::Value,
) -> OperationRequest {
    OperationRequest {
        operation,
        lease_id: voom_core::LeaseId(lease_id),
        payload,
        heartbeat_deadline_ms: 1000,
        progress_idle_deadline_ms: 1000,
    }
}

fn operation_name(operation: OperationKind) -> String {
    serde_json::to_value(operation)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
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

async fn spawn_provider(case: &ProviderCase) -> TestLaunch {
    let worker_id = voom_core::WorkerId(1);
    let worker_epoch = 0;
    let secret = "phase6-provider-secret";
    let mut child = tokio::process::Command::new(binary_path(case))
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

fn binary_path(case: &ProviderCase) -> PathBuf {
    std::env::var_os(case.bin_env).map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(std::path::Path::parent)
                .unwrap()
                .join("target")
                .join("debug")
                .join(case.name)
        },
        PathBuf::from,
    )
}
