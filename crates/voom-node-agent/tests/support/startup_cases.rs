#![expect(
    clippy::unwrap_used,
    reason = "tests favor unwrap for fixture assertions"
)]

use super::*;

#[tokio::test]
async fn local_fence_startup_probe() {
    if std::env::var_os("VOOM_NODE_STARTUP_PROBE").is_none() {
        return;
    }
    startup(async {
        tracing::warn!(
            event = "lease_local_fence",
            lease_id = %voom_core::LeaseId(11),
            ticket_id = %voom_core::TicketId(13),
            recovery_pending = true,
            "lease fenced locally; remote recovery pending"
        );
    })
    .await;
}

#[tokio::test]
async fn local_fence_startup_emits_json_to_stderr() {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::local_fence_startup_probe", "--nocapture"])
            .env("VOOM_NODE_STARTUP_PROBE", "1")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    let event: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap();
    assert_eq!(event["level"], "WARN");
    assert_eq!(event["fields"]["event"], "lease_local_fence");
    assert_eq!(event["fields"]["lease_id"], "11");
    assert_eq!(event["fields"]["ticket_id"], "13");
    assert_eq!(event["fields"]["recovery_pending"], true);
    assert!(
        !String::from_utf8(output.stdout)
            .unwrap()
            .contains("lease_local_fence")
    );
}
