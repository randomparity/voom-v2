#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::process::Command;

use serde_json::Value;

/// Clap's `value_enum + env` validates the env value before dispatch runs, so
/// an invalid `VOOM_LOG_FORMAT` surfaces as `BAD_ARGS` (exit 1) — a
/// user-correctable error — rather than collapsing through dispatch into a
/// generic `INTERNAL` envelope. This pins the perimeter so the catch-all in
/// `main.rs` doesn't have to do the work alone.
#[test]
fn invalid_log_format_env_returns_bad_args_envelope() {
    let bin = env!("CARGO_BIN_EXE_voom");
    let output = Command::new(bin)
        .env("VOOM_LOG_FORMAT", "xml-not-supported")
        .args(["--database-url", "sqlite::memory:", "init"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));

    let stdout = String::from_utf8(output.stdout).unwrap();
    let json: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be a JSON envelope; got {stdout:?}: {e}"));
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "BAD_ARGS");
}

/// With a valid `VOOM_LOG_FORMAT` env and a CLI override, the command runs
/// to completion — proving `resolve_cfg` correctly uses clap's resolved value
/// instead of re-reading env. If a future regression reintroduced the
/// `Config::resolve(..., None, None)` shape, this test would still pass (env
/// is valid), but the in-process integration confirms the override pipeline
/// is wired end-to-end.
#[test]
fn cli_log_format_override_runs_init_successfully() {
    let bin = env!("CARGO_BIN_EXE_voom");
    let output = Command::new(bin)
        .env("VOOM_LOG_FORMAT", "text")
        .args([
            "--log-format",
            "json",
            "--database-url",
            "sqlite::memory:",
            "init",
        ])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(0),
        "init should succeed; stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    let json: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be a JSON envelope; got {stdout:?}: {e}"));
    assert_eq!(json["status"], "ok");
    assert_eq!(json["command"], "init");
}
