#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::process::Command;

use serde_json::Value;

#[test]
fn global_help_produces_single_json_envelope_on_stdout() {
    let output = Command::new(env!("CARGO_BIN_EXE_voom"))
        .arg("--help")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0), "--help must exit 0");
    assert!(
        output.stderr.is_empty(),
        "--help must not write clap help to stderr"
    );

    let json = parse_single_envelope(output.stdout);
    assert_eq!(json["command"], "help");
    assert_eq!(json["status"], "ok");
    assert!(
        json["data"]["text"]
            .as_str()
            .is_some_and(|text| text.contains("VOOM control plane CLI")),
        "help envelope must include the clap-rendered help text: {json}",
    );
}

#[test]
fn global_version_produces_single_json_envelope_on_stdout() {
    let output = Command::new(env!("CARGO_BIN_EXE_voom"))
        .arg("--version")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0), "--version must exit 0");
    assert!(
        output.stderr.is_empty(),
        "--version must not write clap version text to stderr"
    );

    let json = parse_single_envelope(output.stdout);
    assert_eq!(json["command"], "version");
    assert_eq!(json["status"], "ok");
    assert!(json["data"]["semver"].as_str().is_some());
}

fn parse_single_envelope(stdout: Vec<u8>) -> Value {
    let stdout = String::from_utf8(stdout).unwrap();
    let lines = stdout.lines().count();
    assert_eq!(lines, 1, "stdout must contain exactly one line: {stdout:?}");
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("stdout must be a JSON envelope; got {stdout:?}: {err}"))
}
