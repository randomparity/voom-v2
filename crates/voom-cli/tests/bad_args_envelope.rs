#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::process::Command;

use serde_json::Value;

/// Invoking the compiled binary with a bogus flag must produce a parseable
/// JSON envelope on stdout (the agent-facing contract), not clap's default
/// stderr message.
#[test]
fn unknown_flag_produces_bad_args_envelope_on_stdout() {
    let bin = env!("CARGO_BIN_EXE_voom");
    let output = Command::new(bin)
        .args(["--nonsense-flag"])
        .output()
        .expect("failed to invoke binary");

    assert_eq!(output.status.code(), Some(1), "BAD_ARGS exit code is 1");

    let stdout = String::from_utf8(output.stdout).expect("stdout must be UTF-8");
    let json: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be a JSON envelope; got {stdout:?}: {e}"));

    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "BAD_ARGS");
    assert_eq!(json["command"], "cli");
}
