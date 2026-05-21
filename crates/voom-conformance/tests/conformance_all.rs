use std::time::Duration;

use voom_conformance::manifest::{Manifest, resolve_active};
use voom_conformance::{Harness, SuiteResult};

#[tokio::test]
async fn echo_worker_and_negative_fixtures_pass_conformance() {
    let manifest_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("voom-fakes-manifest.toml");
    let manifest = Manifest::load(manifest_path).unwrap();
    assert_eq!(manifest.active.len(), 1);
    assert!(manifest.scaffold.iter().any(|s| s == "chaos-worker"));

    let mut combined = SuiteResult::default();
    for entry in &manifest.active {
        let path = resolve_active(entry).unwrap();
        let harness = Harness::new(path);
        let mut launch = harness.launch().await.unwrap();
        let result = harness.run_all(&mut launch).await;
        let shutdown_name = format!("{}::shutdown_after_suites", entry.name);
        record_shutdown(
            &mut combined,
            shutdown_name,
            launch.shutdown(Duration::from_secs(5)).await,
        );
        combined.extend(result);
    }

    combined.extend(voom_conformance::raw_wire_suite::run_protocol_negative_fixture().await);

    let stdin_result = stdin_eof_terminates_worker().await;
    combined.extend(stdin_result);

    assert!(
        combined.all_passed(),
        "conformance failures: {:?}",
        combined.failed
    );
    assert!(!combined.is_empty());
}

async fn stdin_eof_terminates_worker() -> SuiteResult {
    let manifest_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("voom-fakes-manifest.toml");
    let manifest = match Manifest::load(manifest_path) {
        Ok(m) => m,
        Err(e) => {
            let mut result = SuiteResult::default();
            result.fail("stdin_eof_terminates_worker", e.to_string());
            return result;
        }
    };
    let Some(entry) = manifest
        .active
        .iter()
        .find(|entry| entry.name == "echo-worker")
    else {
        let mut result = SuiteResult::default();
        result.fail(
            "stdin_eof_terminates_worker",
            "echo-worker active entry missing",
        );
        return result;
    };
    let mut result = SuiteResult::default();
    let path = match resolve_active(entry) {
        Ok(path) => path,
        Err(e) => {
            result.fail("stdin_eof_terminates_worker", e.to_string());
            return result;
        }
    };
    let harness = Harness::new(path);
    match harness.launch().await {
        Ok(launch) => match launch.shutdown(Duration::from_secs(5)).await {
            Ok(status) if status.success() => result.pass("stdin_eof_terminates_worker"),
            Ok(status) => result.fail(
                "stdin_eof_terminates_worker",
                format!("exit status {status}"),
            ),
            Err(e) => result.fail("stdin_eof_terminates_worker", e.to_string()),
        },
        Err(e) => result.fail("stdin_eof_terminates_worker", e.to_string()),
    }
    result
}

fn record_shutdown(
    result: &mut SuiteResult,
    name: String,
    shutdown: std::io::Result<std::process::ExitStatus>,
) {
    match shutdown {
        Ok(status) if status.success() => result.pass(name),
        Ok(status) => result.fail(name, format!("exit status {status}")),
        Err(e) => result.fail(name, e.to_string()),
    }
}
