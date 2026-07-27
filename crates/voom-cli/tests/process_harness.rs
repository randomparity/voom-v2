#![expect(
    clippy::unwrap_used,
    reason = "integration tests fail loudly with captured process output"
)]

#[path = "support/process.rs"]
mod process;

use std::process::Command;
use std::time::{Duration, Instant};

use process::run_bounded;

#[test]
fn bounded_process_captures_success_output() {
    let mut command = shell("printf 'ready'; printf 'detail' >&2");

    let output = run_bounded(&mut command, Duration::from_secs(1)).unwrap();

    assert!(output.status.success());
    assert!(!output.timed_out);
    assert_eq!(output.stdout, b"ready");
    assert_eq!(output.stderr, b"detail");
}

#[test]
fn bounded_process_preserves_nonzero_status_and_output() {
    let mut command = shell("printf 'bad' >&2; exit 7");

    let output = run_bounded(&mut command, Duration::from_secs(1)).unwrap();

    assert_eq!(output.status.code(), Some(7));
    assert!(!output.timed_out);
    assert_eq!(output.stderr, b"bad");
}

#[test]
fn bounded_process_kills_and_reaps_at_deadline() {
    let mut command = shell("printf 'started'; exec sleep 2");
    let started = Instant::now();

    let output = run_bounded(&mut command, Duration::from_millis(100)).unwrap();

    assert!(output.timed_out);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(output.stdout, b"started");
}

fn shell(script: &str) -> Command {
    let mut command = Command::new("sh");
    command.args(["-c", script]);
    command
}
