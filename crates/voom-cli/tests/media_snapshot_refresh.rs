//! Explicit rescans refresh provider facts through the authenticated owner path.

#![expect(
    clippy::unwrap_used,
    reason = "integration fixtures fail loudly on setup or evidence errors"
)]

#[path = "support/process.rs"]
mod process;

use std::process::Command;
use std::time::Duration;

use process::{build_worker_package, run_bounded};
use voom_test_support::worker::{target_debug_dir, workspace_root};

#[test]
fn explicit_rescan_refreshes_legacy_cover_and_remuxes() {
    for package in [
        "voom-api",
        "voom-node-agent",
        "voom-scan-worker",
        "voom-hash-worker",
        "voom-ffprobe-worker",
        "voom-ffmpeg-worker",
        "voom-mkvtoolnix-worker",
        "voom-verify-artifact-worker",
    ] {
        build_worker_package(package, Duration::from_mins(2)).unwrap();
    }
    let temporary = tempfile::tempdir().unwrap();
    let library = temporary.path().join("library");
    std::fs::create_dir(&library).unwrap();
    let output = run_bounded(
        Command::new("python3")
            .arg(workspace_root().join("crates/voom-cli/tests/support/real_owner_refresh.py"))
            .arg("serve")
            .arg("--workdir")
            .arg(temporary.path().join("stack"))
            .arg("--input")
            .arg(&library)
            .arg("--bin-dir")
            .arg(target_debug_dir())
            .env("VOOM_ACCEPTANCE_PROCESS_GROUP", "inherit"),
        Duration::from_mins(3),
    )
    .unwrap();
    assert!(
        !output.timed_out && output.status.success(),
        "{}",
        output.diagnostics("snapshot refresh")
    );
    let evidence: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(evidence["rescans"], 2);
    assert_eq!(evidence["source_version_preserved"], true);
    assert_eq!(evidence["history_preserved"], true);
    assert_eq!(evidence["input_pin_preserved"], true);
    assert_eq!(evidence["new_plan_uses_refresh"], true);
    assert_eq!(evidence["root_and_whole_input_video_count"], 1);
    assert_eq!(evidence["remux_committed"], true);
    assert_eq!(evidence["attachments"], 1);
}
