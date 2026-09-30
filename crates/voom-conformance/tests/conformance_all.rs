use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use voom_conformance::manifest::{
    ActiveBinary, Manifest, resolve_active, validate_operation_coverage,
};
use voom_conformance::{Harness, SuiteResult};

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
    for name in REQUIRED_ACTIVE {
        assert!(manifest.active.iter().any(|entry| entry.name == *name));
    }
    for name in &REQUIRED_ACTIVE[3..] {
        assert!(!manifest.scaffold.iter().any(|scaffold| scaffold == name));
    }
    if let Err(e) = validate_operation_coverage(&manifest) {
        let mut combined = SuiteResult::default();
        combined.fail("manifest_operation_coverage", e.to_string());
        assert_all_passed(&combined);
        return;
    }
    if let Err(e) = ensure_fake_worker_bins_built(&manifest.active) {
        let mut combined = SuiteResult::default();
        combined.fail("fake_worker_bins_build", e);
        assert_all_passed(&combined);
        return;
    }

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
    combined.extend(voom_conformance::failure_taxonomy::run());

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

fn ensure_fake_worker_bins_built(active: &[ActiveBinary]) -> Result<(), String> {
    static BUILD: OnceLock<Result<(), String>> = OnceLock::new();
    if std::env::var_os("VOOM_TEST_PREBUILT_WORKERS").is_some() {
        for entry in active {
            let path = resolve_active(entry).map_err(|error| error.to_string())?;
            if !path.is_file() {
                return Err(format!(
                    "prebuilt worker binary missing at {}; run `cargo build --workspace \
                     --all-features --all-targets` or unset VOOM_TEST_PREBUILT_WORKERS",
                    path.display()
                ));
            }
        }
        return Ok(());
    }

    BUILD
        .get_or_init(|| {
            let mut command = std::process::Command::new("cargo");
            command.args(["build", "-p", "voom-fakes", "--bins"]);
            if let Some(target_dir) = std::env::var_os("CARGO_TARGET_DIR") {
                command.arg("--target-dir").arg(target_dir);
            }
            if let Some(target) =
                std::env::var_os("CARGO_BUILD_TARGET").filter(|target| !target.is_empty())
            {
                command.arg("--target").arg(target);
            }
            let status = command
                .current_dir(workspace_root())
                .status()
                .map_err(|e| format!("fake worker build failed to start: {e}"))?;
            if status.success() {
                Ok(())
            } else {
                Err(format!("fake worker build exited with {status}"))
            }
        })
        .clone()
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

#[test]
fn prebuilt_fake_workers_do_not_invoke_cargo() -> Result<(), Box<dyn std::error::Error>> {
    const PROBE: &str = "VOOM_PREBUILT_FAKE_PROBE";
    let executable = std::env::current_exe()?;
    if let Ok(mode) = std::env::var(PROBE) {
        let path = if mode == "present" {
            executable
        } else {
            executable.join("missing-worker")
        };
        let entry = ActiveBinary {
            name: "prebuilt-probe".to_owned(),
            target: "fake-scanner".to_owned(),
            status: "active".to_owned(),
            required: true,
            operations: Vec::new(),
            path: Some(path),
        };
        let result = ensure_fake_worker_bins_built(&[entry]);
        match mode.as_str() {
            "present" => result.map_err(std::io::Error::other)?,
            "missing" => {
                let error = result.err().ok_or("missing worker unexpectedly accepted")?;
                if !error.contains("prebuilt worker binary missing")
                    || !error.contains("cargo build --workspace")
                {
                    return Err(error.into());
                }
            }
            "fallback" => {
                if !matches!(&result, Err(error) if error.contains("build failed to start")) {
                    return Err(format!("fallback did not invoke Cargo: {result:?}").into());
                }
            }
            _ => return Err("unknown probe mode".into()),
        }
        return Ok(());
    }
    for mode in ["present", "missing", "fallback"] {
        let mut command = std::process::Command::new(&executable);
        command
            .args([
                "--exact",
                "prebuilt_fake_workers_do_not_invoke_cargo",
                "--nocapture",
            ])
            .env(PROBE, mode)
            .env("PATH", "");
        if mode == "fallback" {
            command.env_remove("VOOM_TEST_PREBUILT_WORKERS");
        } else {
            command.env("VOOM_TEST_PREBUILT_WORKERS", "1");
        }
        let output = command.output()?;
        if !output.status.success()
            || !String::from_utf8_lossy(&output.stdout).contains("running 1 test")
        {
            return Err(format!("{mode}: {output:?}").into());
        }
    }
    Ok(())
}
