use std::time::Duration;

use voom_conformance::manifest::{Manifest, resolve_active, validate_operation_coverage};
use voom_conformance::{Harness, SuiteResult};

#[tokio::test]
async fn echo_worker_and_negative_fixtures_pass_conformance() {
    let manifest_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("voom-fakes-manifest.toml");
    let manifest = match Manifest::load(manifest_path) {
        Ok(manifest) => manifest,
        Err(e) => {
            let mut combined = SuiteResult::default();
            combined.fail("manifest_loads", e.to_string());
            assert_all_passed(&combined);
            return;
        }
    };
    const REQUIRED_ACTIVE: &[&str] = &[
        "echo-worker",
        "chaos-worker",
        "benchmark-worker",
        "fake-scanner",
        "fake-prober",
        "fake-transcoder",
        "fake-remuxer",
        "fake-backup-store",
        "fake-health-checker",
        "fake-identity-provider",
        "fake-external-system",
        "fake-quality-scorer",
        "fake-issue-provider",
        "fake-use-lease-provider",
    ];
    for name in REQUIRED_ACTIVE {
        assert!(manifest.active.iter().any(|entry| entry.name == *name));
    }
    for name in &REQUIRED_ACTIVE[3..] {
        assert!(!manifest.scaffold.iter().any(|scaffold| scaffold == name));
    }
    validate_operation_coverage(&manifest).unwrap();

    let mut combined = SuiteResult::default();
    for entry in &manifest.active {
        let path = match resolve_active(entry) {
            Ok(path) => path,
            Err(e) => {
                combined.fail(format!("{}::resolve_active", entry.name), e.to_string());
                continue;
            }
        };
        let harness = Harness::new(path);
        let mut launch = match harness.launch().await {
            Ok(launch) => launch,
            Err(e) => {
                combined.fail(format!("{}::launch", entry.name), e.to_string());
                continue;
            }
        };
        let result = harness.run_all(&mut launch, entry).await;
        let shutdown_name = format!("{}::shutdown_after_suites", entry.name);
        record_shutdown(
            &mut combined,
            shutdown_name,
            launch.shutdown(Duration::from_secs(5)).await,
        );
        combined.extend(result);
    }

    combined.extend(voom_conformance::raw_wire_suite::run_protocol_negative_fixture().await);
    combined.extend(voom_conformance::failure_taxonomy::run().await);

    let stdin_result = stdin_eof_terminates_worker().await;
    combined.extend(stdin_result);

    assert_all_passed(&combined);
}

fn assert_all_passed(combined: &SuiteResult) {
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
    let mut result = SuiteResult::default();
    for entry in &manifest.active {
        let name = format!("{}::stdin_eof_terminates_worker", entry.name);
        let path = match resolve_active(entry) {
            Ok(path) => path,
            Err(e) => {
                result.fail(name, e.to_string());
                continue;
            }
        };
        let harness = Harness::new(path);
        match harness.launch().await {
            Ok(launch) => match launch.shutdown(Duration::from_secs(5)).await {
                Ok(status) if status.success() => result.pass(name),
                Ok(status) => result.fail(name, format!("exit status {status}")),
                Err(e) => result.fail(name, e.to_string()),
            },
            Err(e) => result.fail(name, e.to_string()),
        }
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
