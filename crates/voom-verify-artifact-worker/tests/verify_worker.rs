#![expect(
    clippy::expect_used,
    reason = "integration tests use expect for process assertions"
)]

use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

#[tokio::test]
async fn binary_prints_bound_address_and_stops_on_stdin_close() {
    let binary = env!("CARGO_BIN_EXE_voom-verify-artifact-worker");
    let mut child = tokio::process::Command::new(binary)
        .env("VOOM_WORKER_ID", "7")
        .env("VOOM_WORKER_EPOCH", "3")
        .env("VOOM_WORKER_SECRET", "secret")
        .env("VOOM_WORKER_BIND", "127.0.0.1:0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("worker binary should spawn");

    let stdout = child.stdout.take().expect("worker stdout should be piped");
    let mut lines = BufReader::new(stdout).lines();
    let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
        .await
        .expect("worker should print bound address before timeout")
        .expect("worker stdout read should succeed")
        .expect("worker should print one stdout line");
    assert!(
        line.strip_prefix("BOUND addr=")
            .and_then(|addr| addr.parse::<std::net::SocketAddr>().ok())
            .is_some(),
        "unexpected worker bound line: {line}"
    );

    drop(child.stdin.take());
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("worker should stop after stdin closes")
        .expect("worker wait should succeed");

    if !status.success() {
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            let _read = pipe.read_to_string(&mut stderr).await;
        }
        assert!(status.success(), "worker exited {status}: {stderr}");
    }
}
