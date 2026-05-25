# Chaos Librarian E2E Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a VOOM-owned Chaos Librarian E2E harness with deterministic CI-eligible real-media tests, observed-state comparison for the static baseline, and local-only wall-clock churn recipes.

**Architecture:** Keep Chaos Librarian in `third_party/chaos-librarian` and invoke it through `uv run` from that directory. Put deterministic orchestration in `crates/voom-cli/tests/` so tests can exercise the real `voom` binary and still seed durable policy rows through control-plane APIs where no public CLI exists yet. Put local wall-clock churn in `scripts/` and `just` recipes because it is an operator workflow, not default CI.

**Tech Stack:** Rust integration tests, `serde_json`, `tempfile`, `sqlx`, `uv`, Chaos Librarian CLI, VOOM CLI JSON envelopes, `just`, bash, `jq`.

---

## File Structure

- Create `crates/voom-cli/tests/support/mod.rs`: shared integration-test support module.
- Create `crates/voom-cli/tests/support/chaos_librarian.rs`: submodule validation, `uv run chaos-librarian ...` wrappers, materialize/step/compare helpers.
- Create `crates/voom-cli/tests/support/voom_cli.rs`: `voom` command runner, JSON envelope parsing, worker binary helpers, FFmpeg worker launch helper.
- Create `crates/voom-cli/tests/support/observed_state.rs`: narrow test-harness observed-state exporter that reads VOOM SQLite rows and writes Chaos Librarian `observed-state.json`.
- Create `crates/voom-cli/tests/support/policy_seed.rs`: helper that turns scan results into durable policy documents and input sets for compliance commands.
- Create `crates/voom-cli/tests/chaos_librarian_e2e.rs`: ignored deterministic E2E tests run only by `just chaos-e2e-ci`.
- Create `crates/voom-cli/tests/fixtures/chaos/video-transcode-required.yaml`: VOOM-owned H.264 scenario.
- Create `crates/voom-cli/tests/fixtures/chaos/video-transcode-noop.yaml`: VOOM-owned HEVC MKV scenario.
- Create `scripts/chaos-e2e-local.sh`: local wall-clock churn runner with checkpoint loop and allowlisted diagnostics.
- Modify `justfile`: add `chaos-e2e-ci`, `chaos-e2e-local`, and `chaos-e2e-soak`.

No production CLI command is added in this first implementation. The observed-state exporter is test harness code because the design only requires a VOOM-owned E2E export, and adding a public CLI command would expand the agent-facing JSON contract before the export shape is stable.

---

## Task 1: Chaos Librarian Command Support

**Files:**
- Create: `crates/voom-cli/tests/support/mod.rs`
- Create: `crates/voom-cli/tests/support/chaos_librarian.rs`
- Test: `crates/voom-cli/tests/chaos_librarian_e2e.rs`

- [ ] **Step 1: Create support module shell**

Create `crates/voom-cli/tests/support/mod.rs` with only the module that this
task implements. Add later support modules in the task that creates each file;
otherwise Task 1 will keep failing on unrelated missing modules after
`chaos_librarian.rs` exists.

```rust
pub mod chaos_librarian;
```

- [ ] **Step 2: Add a failing submodule validation test**

Create `crates/voom-cli/tests/chaos_librarian_e2e.rs` with this first test:

```rust
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "E2E tests fail loudly and preserve paths for diagnosis"
)]

mod support;

use support::chaos_librarian::ChaosLibrarian;

#[test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
fn chaos_librarian_submodule_is_pinned_and_ready() {
    let chaos = ChaosLibrarian::discover().unwrap();
    let readiness = chaos.validate_ready().unwrap();

    assert_eq!(
        readiness.revision,
        "057a4033a3a9ae14fef664ab82f2c31e1a223544"
    );
    assert!(readiness.capabilities["ok"].as_bool().unwrap_or(false));
}
```

- [ ] **Step 3: Run the failing test**

Run:

```bash
cargo test -p voom-cli --test chaos_librarian_e2e chaos_librarian_submodule_is_pinned_and_ready -- --ignored --nocapture
```

Expected: compile failure because `support::chaos_librarian` does not exist yet.

- [ ] **Step 4: Implement Chaos Librarian support**

Create `crates/voom-cli/tests/support/chaos_librarian.rs`:

```rust
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

#[derive(Debug, Clone)]
pub struct ChaosLibrarian {
    pub workspace_root: PathBuf,
    pub submodule_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ChaosReadiness {
    pub revision: String,
    pub capabilities: Value,
}

#[derive(Debug)]
pub struct ChaosRun {
    pub _tmp: TempDir,
    pub run_dir: PathBuf,
    pub report: Value,
}

impl ChaosLibrarian {
    pub fn discover() -> Result<Self, Box<dyn std::error::Error>> {
        let workspace_root = workspace_root();
        let submodule_dir = workspace_root.join("third_party/chaos-librarian");
        if !submodule_dir.join("pyproject.toml").is_file() {
            return Err(format!(
                "Chaos Librarian submodule is not initialized at {}",
                submodule_dir.display()
            )
            .into());
        }
        Ok(Self {
            workspace_root,
            submodule_dir,
        })
    }

    pub fn validate_ready(&self) -> Result<ChaosReadiness, Box<dyn std::error::Error>> {
        let status = command_output(
            Command::new("git")
                .current_dir(&self.workspace_root)
                .args(["submodule", "status", "third_party/chaos-librarian"]),
        )?;
        let line = String::from_utf8(status.stdout)?;
        let trimmed = line.trim_end();
        if !trimmed.starts_with(' ') {
            return Err(format!("submodule must be clean and initialized: {trimmed}").into());
        }
        let revision = trimmed
            .split_whitespace()
            .next()
            .ok_or("missing submodule revision")?
            .to_owned();

        command_output(
            Command::new("uv")
                .current_dir(&self.submodule_dir)
                .args(["sync", "--locked"]),
        )?;
        let capabilities = self.uv_json(["run", "chaos-librarian", "capabilities", "--json"])?;
        Ok(ChaosReadiness {
            revision,
            capabilities,
        })
    }

    pub fn materialize(&self, scenario: &Path) -> Result<ChaosRun, Box<dyn std::error::Error>> {
        let tmp = TempDir::new()?;
        let run_dir = tmp.path().join("run");
        let report = self.uv_json_with_args([
            "run",
            "chaos-librarian",
            "materialize",
            scenario.to_str().ok_or("scenario path is not UTF-8")?,
            "--out",
            run_dir.to_str().ok_or("run dir path is not UTF-8")?,
            "--json",
        ])?;
        Ok(ChaosRun {
            _tmp: tmp,
            run_dir,
            report,
        })
    }

    pub fn step_next(&self, run_dir: &Path) -> Result<Value, Box<dyn std::error::Error>> {
        self.uv_json_with_args([
            "run",
            "chaos-librarian",
            "step",
            run_dir.to_str().ok_or("run dir path is not UTF-8")?,
            "--next",
            "1",
            "--json",
        ])
    }

    pub fn compare_final_state(
        &self,
        run_dir: &Path,
        observed_state: &Path,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let output = Command::new("uv")
            .current_dir(&self.submodule_dir)
            .args([
                "run",
                "chaos-librarian",
                "compare",
                run_dir.to_str().ok_or("run dir path is not UTF-8")?,
                observed_state
                    .to_str()
                    .ok_or("observed-state path is not UTF-8")?,
                "--mode",
                "final-state",
                "--json",
            ])
            .output()?;
        if output.status.code() != Some(0) {
            return Err(format!(
                "chaos-librarian compare failed with {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        Ok(serde_json::from_slice(&output.stdout)?)
    }

    pub fn upstream_scenario(&self, name: &str) -> PathBuf {
        self.submodule_dir
            .join("tests/fixtures/scenarios")
            .join(name)
    }

    pub fn voom_scenario(&self, name: &str) -> PathBuf {
        self.workspace_root
            .join("crates/voom-cli/tests/fixtures/chaos")
            .join(name)
    }

    fn uv_json<const N: usize>(&self, args: [&str; N]) -> Result<Value, Box<dyn std::error::Error>> {
        self.uv_json_with_args(args)
    }

    fn uv_json_with_args<I, S>(&self, args: I) -> Result<Value, Box<dyn std::error::Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let output = command_output(Command::new("uv").current_dir(&self.submodule_dir).args(args))?;
        Ok(serde_json::from_slice(&output.stdout)?)
    }
}

fn command_output(command: &mut Command) -> Result<Output, Box<dyn std::error::Error>> {
    let output = command.output()?;
    if output.status.success() {
        return Ok(output);
    }
    Err(format!(
        "command failed with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .into())
}

pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("voom-cli manifest lives under crates/voom-cli")
        .to_path_buf()
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run:

```bash
cargo test -p voom-cli --test chaos_librarian_e2e chaos_librarian_submodule_is_pinned_and_ready -- --ignored --nocapture
```

Expected: PASS when `uv`, Python 3.13, and Chaos Librarian media capabilities are available. If it fails, the failure must name the missing submodule/tool.

- [ ] **Step 6: Commit**

```bash
git add crates/voom-cli/tests/support/mod.rs crates/voom-cli/tests/support/chaos_librarian.rs crates/voom-cli/tests/chaos_librarian_e2e.rs
git commit -m "test: validate chaos librarian submodule"
```

---

## Task 2: VOOM CLI And Worker Test Support

**Files:**
- Create: `crates/voom-cli/tests/support/voom_cli.rs`
- Modify: `crates/voom-cli/tests/chaos_librarian_e2e.rs`

- [ ] **Step 1: Add a failing VOOM CLI smoke test to the E2E file**

First extend `crates/voom-cli/tests/support/mod.rs`:

```rust
pub mod voom_cli;
```

Append this test to `crates/voom-cli/tests/chaos_librarian_e2e.rs`:

```rust
use support::voom_cli::{VoomTestDb, run_voom};

#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
async fn voom_e2e_support_runs_version_envelope() {
    let db = VoomTestDb::init().await.unwrap();
    let version = run_voom(&db.url, ["version"]).unwrap();

    assert_eq!(version.status_code, Some(0));
    assert_eq!(version.json["command"], "version");
    assert_eq!(version.json["status"], "ok");
}
```

- [ ] **Step 2: Run the failing test**

Run:

```bash
cargo test -p voom-cli --test chaos_librarian_e2e voom_e2e_support_runs_version_envelope -- --ignored --nocapture
```

Expected: compile failure because `support::voom_cli` is not implemented.

- [ ] **Step 3: Implement VOOM CLI support**

Create `crates/voom-cli/tests/support/voom_cli.rs`:

```rust
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::time::Duration;

use serde_json::Value;
use tempfile::NamedTempFile;
use voom_control_plane::ControlPlane;
use voom_store::repo::workers::{NewCapability, NewGrant, NewWorker, WorkerKind};

use super::chaos_librarian::workspace_root;

pub struct VoomTestDb {
    _file: NamedTempFile,
    pub url: String,
}

pub struct VoomOutput {
    pub status_code: Option<i32>,
    pub json: Value,
    pub stderr: String,
}

pub struct TranscodeWorkerLaunch {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl VoomTestDb {
    pub async fn init() -> Result<Self, Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;
        let url = voom_store::test_support::sqlite_url_for(file.path());
        voom_store::init(&url).await?;
        Ok(Self { _file: file, url })
    }

    pub async fn control_plane(&self) -> Result<ControlPlane, Box<dyn std::error::Error>> {
        Ok(ControlPlane::open(&self.url).await?)
    }
}

pub fn run_voom<I, S>(database_url: &str, args: I) -> Result<VoomOutput, Box<dyn std::error::Error>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = Command::new(env!("CARGO_BIN_EXE_voom"))
        .args(["--database-url", database_url])
        .args(args)
        .env("VOOM_FFPROBE_WORKER_BIN", worker_binary("voom-ffprobe-worker"))
        .env("VOOM_FFMPEG_WORKER_BIN", worker_binary("voom-ffmpeg-worker"))
        .env("VOOM_VERIFY_ARTIFACT_WORKER_BIN", worker_binary("voom-verify-artifact-worker"))
        .output()?;
    output_to_envelope(output)
}

pub fn output_to_envelope(output: Output) -> Result<VoomOutput, Box<dyn std::error::Error>> {
    let stdout = String::from_utf8(output.stdout)?;
    let json = serde_json::from_str(stdout.trim()).map_err(|err| {
        format!(
            "stdout must contain exactly one JSON envelope; got {stdout:?}: {err}"
        )
    })?;
    Ok(VoomOutput {
        status_code: output.status.code(),
        json,
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

pub fn build_worker_binary(package: &str) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new("cargo")
        .current_dir(workspace_root())
        .args(["build", "-p", package])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("failed to build {package}: {status}").into())
    }
}

pub fn worker_binary(name: &str) -> PathBuf {
    workspace_root()
        .join("target")
        .join("debug")
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

impl TranscodeWorkerLaunch {
    pub async fn start(cp: &ControlPlane) -> Result<Self, Box<dyn std::error::Error>> {
        build_worker_binary("voom-ffmpeg-worker")?;
        let secret = "chaos-librarian-transcode-e2e-secret";
        let worker = cp
            .register_worker(NewWorker {
                name: "chaos-librarian-ffmpeg".to_owned(),
                kind: WorkerKind::Synthetic,
                registered_at: cp.clock().now(),
                node_id: None,
            })
            .await?;
        let mut child = Command::new(worker_binary("voom-ffmpeg-worker"))
            .env("VOOM_WORKER_SECRET", secret)
            .env("VOOM_WORKER_ID", worker.id.0.to_string())
            .env("VOOM_WORKER_EPOCH", "0")
            .env("VOOM_WORKER_BIND", "127.0.0.1:0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child.stdin.take();
        let bound = read_bound_addr(&mut child)?;
        cp.record_capability(NewCapability {
            worker_id: worker.id,
            operation: "transcode_video".to_owned(),
            codecs: Vec::new(),
            hardware: Vec::new(),
            artifact_access: Vec::new(),
            extra: serde_json::json!({
                "endpoint": bound.to_string(),
                "secret": secret,
            }),
        })
        .await?;
        cp.record_grant(NewGrant {
            worker_id: worker.id,
            can_execute: vec!["transcode_video".to_owned()],
            can_access_read: Vec::new(),
            can_access_write: Vec::new(),
            denies: Vec::new(),
            max_parallel: serde_json::json!({ "transcode_video": 1 }),
        })
        .await?;
        Ok(Self { child, stdin })
    }

    pub fn shutdown(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        drop(self.stdin.take());
        let started = std::time::Instant::now();
        loop {
            if let Some(status) = self.child.try_wait()? {
                if status.success() {
                    return Ok(());
                }
                return Err(format!("voom-ffmpeg-worker exited with {status}").into());
            }
            if started.elapsed() > Duration::from_secs(5) {
                let _ = self.child.kill();
                return Err("voom-ffmpeg-worker cleanup timed out".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn read_bound_addr(child: &mut Child) -> Result<std::net::SocketAddr, Box<dyn std::error::Error>> {
    let stdout = child
        .stdout
        .take()
        .ok_or("worker stdout missing")?;
    let mut lines = std::io::BufReader::new(stdout).lines();
    let line = lines
        .next()
        .transpose()?
        .ok_or("worker exited before bind line")?;
    Ok(line
        .strip_prefix("BOUND addr=")
        .ok_or_else(|| format!("malformed bind line: {line}"))?
        .parse()?)
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run:

```bash
cargo test -p voom-cli --test chaos_librarian_e2e voom_e2e_support_runs_version_envelope -- --ignored --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-cli/tests/support/mod.rs crates/voom-cli/tests/support/voom_cli.rs crates/voom-cli/tests/chaos_librarian_e2e.rs
git commit -m "test: add voom e2e command support"
```

---

## Task 3: Observed-State Exporter

**Files:**
- Create: `crates/voom-cli/tests/support/observed_state.rs`
- Modify: `crates/voom-cli/tests/chaos_librarian_e2e.rs`

- [ ] **Step 1: Add failing exporter unit tests inside the integration test**

First extend `crates/voom-cli/tests/support/mod.rs`:

```rust
pub mod observed_state;
```

Append these tests to `crates/voom-cli/tests/chaos_librarian_e2e.rs`:

```rust
use support::observed_state::{library_relative_path, sha256_to_observed_hash};

#[test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
fn observed_state_rejects_paths_outside_library() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("chaos-run/library");
    let outside_dir = tmp.path().join("other");
    std::fs::create_dir_all(&library).unwrap();
    std::fs::create_dir_all(&outside_dir).unwrap();
    let outside = outside_dir.join("Movie.mkv");
    std::fs::write(&outside, b"not real media").unwrap();

    let err = library_relative_path(&library.canonicalize().unwrap(), &outside).unwrap_err();

    assert!(err.to_string().contains("outside library root"));
}

#[test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
fn observed_state_hash_uses_chaos_librarian_prefix() {
    let hash = sha256_to_observed_hash(
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    )
    .unwrap();

    assert_eq!(
        hash,
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p voom-cli --test chaos_librarian_e2e observed_state_ -- --ignored --nocapture
```

Expected: compile failure because `support::observed_state` is not implemented.

- [ ] **Step 3: Implement exporter**

Create `crates/voom-cli/tests/support/observed_state.rs`:

```rust
use std::path::{Component, Path, PathBuf};

use serde_json::{Value, json};
use time::format_description::well_known::Rfc3339;

pub async fn export_observed_state(
    database_url: &str,
    run_dir: &Path,
    output_path: &Path,
    consumer_version: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    let pool = voom_store::connect(database_url).await?;
    let library_root = run_dir.join("library").canonicalize()?;
    let run_id = fixture_run_id(run_dir)?;
    let rows = sqlx::query_as::<_, ObservedRow>(
        "SELECT fa.id AS file_asset_id, fv.id AS file_version_id, fv.content_hash, \
                fv.size_bytes, fl.value AS location_value, ms.payload AS snapshot_payload \
         FROM file_assets fa \
         JOIN file_versions fv ON fv.file_asset_id = fa.id AND fv.retired_at IS NULL \
         JOIN file_locations fl ON fl.file_version_id = fv.id \
              AND fl.retired_at IS NULL AND fl.kind = 'local_path' \
         LEFT JOIN media_snapshots ms ON ms.id = ( \
             SELECT max(ms2.id) FROM media_snapshots ms2 WHERE ms2.file_version_id = fv.id \
         ) \
         WHERE fa.retired_at IS NULL \
         ORDER BY fa.id ASC, fv.id ASC, fl.id ASC",
    )
    .fetch_all(&pool)
    .await?;

    let mut assets = Vec::with_capacity(rows.len());
    for row in rows {
        let current_path = library_relative_path(&library_root, Path::new(&row.location_value))?;
        let mut asset = serde_json::Map::new();
        asset.insert(
            "observed_ref".to_owned(),
            Value::String(format!("file_asset_{}", row.file_asset_id)),
        );
        asset.insert("current_path".to_owned(), Value::String(current_path));
        asset.insert(
            "content_hash".to_owned(),
            Value::String(sha256_to_observed_hash(&row.content_hash)?),
        );
        if let Some(probed) = probed_media(&row.snapshot_payload, row.size_bytes)? {
            asset.insert("probed".to_owned(), probed);
        }
        assets.push(Value::Object(asset));
    }

    let observed_at = time::OffsetDateTime::now_utc().format(&Rfc3339)?;
    let observed = json!({
        "schema_version": 1,
        "consumer": {
            "name": "voom",
            "version": consumer_version,
        },
        "run_id": run_id,
        "observed_at": observed_at,
        "assets": assets,
    });
    std::fs::write(output_path, serde_json::to_vec_pretty(&observed)?)?;
    Ok(observed)
}

#[derive(sqlx::FromRow)]
struct ObservedRow {
    file_asset_id: i64,
    #[allow(dead_code)]
    file_version_id: i64,
    content_hash: String,
    size_bytes: i64,
    location_value: String,
    snapshot_payload: Option<String>,
}

pub fn library_relative_path(
    library_root: &Path,
    absolute_path: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    let canonical = absolute_path.canonicalize()?;
    let relative = canonical.strip_prefix(library_root).map_err(|_| {
        format!(
            "path {} is outside library root {}",
            canonical.display(),
            library_root.display()
        )
    })?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => {
                let text = part
                    .to_str()
                    .ok_or("library-relative path contains non-UTF-8 segment")?;
                if text.is_empty() || text == "." || text == ".." || text.contains('\\') {
                    return Err(format!("invalid observed-state path segment: {text:?}").into());
                }
                parts.push(text.to_owned());
            }
            Component::CurDir | Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("invalid observed-state path: {}", relative.display()).into());
            }
        }
    }
    if parts.is_empty() {
        return Err("observed-state path must not be empty".into());
    }
    Ok(parts.join("/"))
}

pub fn sha256_to_observed_hash(hash: &str) -> Result<String, Box<dyn std::error::Error>> {
    let Some(hex) = hash.strip_prefix("sha256:") else {
        return Err(format!("observed-state export requires sha256 hash, got {hash}").into());
    };
    if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(format!("sha256:{}", hex.to_ascii_lowercase()))
    } else {
        Err(format!("invalid sha256 hash for observed-state export: {hash}").into())
    }
}

fn fixture_run_id(run_dir: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let replay_path = run_dir.join("replay.json");
    let replay: Value = serde_json::from_slice(&std::fs::read(&replay_path)?)?;
    replay
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{} does not contain run_id", replay_path.display()).into())
}

fn probed_media(
    snapshot_payload: &Option<String>,
    size_bytes: i64,
) -> Result<Option<Value>, Box<dyn std::error::Error>> {
    let Some(payload) = snapshot_payload else {
        return Ok(None);
    };
    let snapshot: Value = serde_json::from_str(payload)?;
    let Some(container) = snapshot.pointer("/container/format_name").and_then(Value::as_str) else {
        return Ok(None);
    };
    let Some(duration_seconds) = snapshot
        .pointer("/container/duration_seconds")
        .and_then(Value::as_f64)
    else {
        return Ok(None);
    };
    let streams = snapshot
        .get("streams")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(probed_stream)
                .collect::<Vec<Value>>()
        })
        .unwrap_or_default();
    Ok(Some(json!({
        "container": container,
        "duration_seconds": duration_seconds,
        "size_bytes": size_bytes,
        "streams": streams,
    })))
}

fn probed_stream(stream: &Value) -> Option<Value> {
    let kind = stream.get("kind").and_then(Value::as_str)?;
    let codec = stream.get("codec_name").and_then(Value::as_str)?;
    if !matches!(kind, "video" | "audio" | "subtitle") {
        return None;
    }
    let mut out = serde_json::Map::new();
    out.insert("kind".to_owned(), Value::String(kind.to_owned()));
    out.insert("codec".to_owned(), Value::String(codec.to_owned()));
    for (source, target) in [
        ("width", "width"),
        ("height", "height"),
        ("channels", "channels"),
        ("sample_rate", "sample_rate"),
    ] {
        if let Some(value) = stream.get(source).and_then(Value::as_u64) {
            out.insert(target.to_owned(), Value::Number(value.into()));
        }
    }
    if let Some(fps) = stream
        .get("avg_frame_rate")
        .and_then(Value::as_str)
        .and_then(parse_ratio)
    {
        out.insert("fps".to_owned(), serde_json::Number::from_f64(fps)?.into());
    }
    Some(Value::Object(out))
}

fn parse_ratio(text: &str) -> Option<f64> {
    let (num, den) = text.split_once('/')?;
    let num = num.parse::<f64>().ok()?;
    let den = den.parse::<f64>().ok()?;
    if den == 0.0 {
        None
    } else {
        Some(num / den)
    }
}
```

- [ ] **Step 4: Run exporter tests**

Run:

```bash
cargo test -p voom-cli --test chaos_librarian_e2e observed_state_ -- --ignored --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-cli/tests/support/mod.rs crates/voom-cli/tests/support/observed_state.rs crates/voom-cli/tests/chaos_librarian_e2e.rs
git commit -m "test: export chaos observed state"
```

---

## Task 4: Static Library Baseline With Compare

**Files:**
- Modify: `crates/voom-cli/tests/chaos_librarian_e2e.rs`

- [ ] **Step 1: Write the failing static baseline E2E test**

Append:

```rust
use support::observed_state::export_observed_state;

#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
async fn static_library_baseline_scans_exports_and_compares() {
    let chaos = ChaosLibrarian::discover().unwrap();
    chaos.validate_ready().unwrap();
    support::voom_cli::build_worker_binary("voom-ffprobe-worker").unwrap();

    let run = chaos
        .materialize(&chaos.upstream_scenario("static-library.yaml"))
        .unwrap();
    let db = VoomTestDb::init().await.unwrap();
    let library_path = run.run_dir.join("library");
    let library_arg = library_path.to_str().unwrap().to_owned();

    let scan = run_voom(
        &db.url,
        ["scan", "--path", library_arg.as_str()],
    )
    .unwrap();
    assert_eq!(scan.status_code, Some(0), "stderr: {}", scan.stderr);
    assert_eq!(scan.json["status"], "ok");
    assert!(scan.json["data"]["summary"]["ingested"].as_u64().unwrap() > 0);
    assert_eq!(scan.json["data"]["summary"]["failed"], 0);

    let observed_path = run.run_dir.join("observed-state.json");
    export_observed_state(&db.url, &run.run_dir, &observed_path, env!("CARGO_PKG_VERSION"))
        .await
        .unwrap();
    let compare = chaos
        .compare_final_state(&run.run_dir, &observed_path)
        .unwrap();

    assert_eq!(compare["status"], "ok");
}
```

- [ ] **Step 2: Run the failing test**

Run:

```bash
cargo test -p voom-cli --test chaos_librarian_e2e static_library_baseline_scans_exports_and_compares -- --ignored --nocapture
```

Expected: initial failure is acceptable if the exporter mapping does not yet satisfy Chaos Librarian compare. The failure must be a concrete compare/export mismatch, not a compile error.

- [ ] **Step 3: Fix exporter mapping if compare rejects fields**

Use the compare divergence JSON to adjust only `crates/voom-cli/tests/support/observed_state.rs`. Keep the exporter narrow:

```rust
// Acceptable adjustments in this step:
// - map VOOM container aliases to Chaos Librarian container names;
// - omit optional probed fields that VOOM did not persist;
// - normalize stream kind/codec values already present in media_snapshots.payload.
//
// Unacceptable adjustments:
// - reading Chaos Librarian manifests to manufacture VOOM observations;
// - hardcoding static-library file names;
// - adding path history fields that VOOM did not observe.
```

- [ ] **Step 4: Run static baseline again**

Run:

```bash
cargo test -p voom-cli --test chaos_librarian_e2e static_library_baseline_scans_exports_and_compares -- --ignored --nocapture
```

Expected: PASS, including `chaos-librarian compare --mode final-state` exit `0`.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-cli/tests/support/observed_state.rs crates/voom-cli/tests/chaos_librarian_e2e.rs
git commit -m "test: compare static chaos library"
```

---

## Task 5: Policy Seeding From Scan Results

**Files:**
- Create: `crates/voom-cli/tests/support/policy_seed.rs`
- Modify: `crates/voom-cli/tests/chaos_librarian_e2e.rs`

- [ ] **Step 1: Add a failing policy seed test**

First extend `crates/voom-cli/tests/support/mod.rs`:

```rust
pub mod policy_seed;
```

Append:

```rust
use support::policy_seed::seed_transcode_policy_from_scan;

#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
async fn policy_seed_creates_durable_ids_from_scan_envelope() {
    let db = VoomTestDb::init().await.unwrap();
    let cp = db.control_plane().await.unwrap();
    let scan = serde_json::json!({
        "data": {
            "files": [{
                "status": "scanned",
                "file_version_id": 7,
                "media_snapshot_id": 9
            }]
        }
    });

    let ids = seed_transcode_policy_from_scan(&cp, &scan, "seed-test", "mp4", "h264")
        .await
        .unwrap();

    assert!(ids.policy_version_id > 0);
    assert!(ids.input_set_id > 0);
}
```

- [ ] **Step 2: Run the failing test**

Run:

```bash
cargo test -p voom-cli --test chaos_librarian_e2e policy_seed_creates_durable_ids_from_scan_envelope -- --ignored --nocapture
```

Expected: compile failure because `support::policy_seed` is empty.

- [ ] **Step 3: Implement policy seed helper**

Create `crates/voom-cli/tests/support/policy_seed.rs`:

```rust
use serde_json::Value;
use voom_control_plane::ControlPlane;
use voom_core::{FileVersionId, MediaSnapshotId};
use voom_policy::{
    MediaSnapshotInput, PolicyInputSetDraft, PolicyInputSourceKind, TargetRef, load_policy_fixture,
};

pub struct SeededPolicyIds {
    pub policy_version_id: u64,
    pub input_set_id: u64,
}

pub async fn seed_transcode_policy_from_scan(
    cp: &ControlPlane,
    scan_envelope: &Value,
    slug: &str,
    container: &str,
    video_codec: &str,
) -> Result<SeededPolicyIds, Box<dyn std::error::Error>> {
    let file = scan_envelope["data"]["files"]
        .as_array()
        .ok_or("scan envelope missing data.files")?
        .iter()
        .find(|file| file["status"] == "scanned")
        .ok_or("scan envelope has no scanned file")?;
    let file_version_id = file["file_version_id"]
        .as_u64()
        .map(FileVersionId)
        .ok_or("scanned file missing file_version_id")?;
    let media_snapshot_id = file["media_snapshot_id"].as_u64().map(MediaSnapshotId);

    let policy = cp
        .create_policy_document(
            "chaos-video-transcode-hevc",
            &load_policy_fixture("fixtures/policies/video-transcode-hevc.voom")?,
        )
        .await?;
    let input = cp
        .create_policy_input_set(PolicyInputSetDraft {
            slug: slug.to_owned(),
            display_name: slug.to_owned(),
            schema_version: 1,
            source_kind: PolicyInputSourceKind::Test,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            description: None,
            fixture_labels: vec![format!("chaos-librarian-{slug}")],
            synthetic_targets: Vec::new(),
            media_snapshots: vec![MediaSnapshotInput {
                ordinal: 1,
                target: TargetRef::FileVersion {
                    id: file_version_id,
                },
                container: Some(container.to_owned()),
                stream_summary: serde_json::json!({"video_stream_count": 1}),
                video_codec: Some(video_codec.to_owned()),
                width: Some(32),
                height: Some(32),
                hdr: None,
                bitrate: None,
                duration_millis: Some(1000),
                audio_languages: Vec::new(),
                subtitle_languages: Vec::new(),
                health_flags: Vec::new(),
                existing_media_snapshot_id: media_snapshot_id,
            }],
            identity_evidence: Vec::new(),
            bundle_targets: Vec::new(),
            quality_profiles: Vec::new(),
            issues: Vec::new(),
        })
        .await?;

    Ok(SeededPolicyIds {
        policy_version_id: policy.version.id.0,
        input_set_id: input.id.0,
    })
}
```

- [ ] **Step 4: Adjust the seed test to use a real scanned row**

The synthetic IDs in Step 1 are invalid for `create_policy_input_set`. Replace the test body with a real materialize/scan flow:

```rust
#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
async fn policy_seed_creates_durable_ids_from_scan_envelope() {
    let chaos = ChaosLibrarian::discover().unwrap();
    chaos.validate_ready().unwrap();
    support::voom_cli::build_worker_binary("voom-ffprobe-worker").unwrap();
    let run = chaos
        .materialize(&chaos.voom_scenario("video-transcode-required.yaml"))
        .unwrap();
    let db = VoomTestDb::init().await.unwrap();
    let library_path = run.run_dir.join("library");
    let library_arg = library_path.to_str().unwrap().to_owned();
    let scan = run_voom(
        &db.url,
        ["scan", "--path", library_arg.as_str()],
    )
    .unwrap();
    assert_eq!(scan.status_code, Some(0), "stderr: {}", scan.stderr);

    let cp = db.control_plane().await.unwrap();
    let ids = seed_transcode_policy_from_scan(&cp, &scan.json, "seed-test", "mp4", "h264")
        .await
        .unwrap();

    assert!(ids.policy_version_id > 0);
    assert!(ids.input_set_id > 0);
}
```

- [ ] **Step 5: Run policy seed test**

Run:

```bash
cargo test -p voom-cli --test chaos_librarian_e2e policy_seed_creates_durable_ids_from_scan_envelope -- --ignored --nocapture
```

Expected: FAIL until the VOOM-owned scenario file is added in Task 6.

- [ ] **Step 6: Commit after Task 6 scenario creation**

Do not commit this task until Task 6 adds `video-transcode-required.yaml`; then commit both together if this task is otherwise passing.

---

## Task 6: VOOM-Owned Transcode Scenarios

**Files:**
- Create: `crates/voom-cli/tests/fixtures/chaos/video-transcode-required.yaml`
- Create: `crates/voom-cli/tests/fixtures/chaos/video-transcode-noop.yaml`
- Modify: `crates/voom-cli/tests/chaos_librarian_e2e.rs`

- [ ] **Step 1: Create H.264 transcode-required scenario**

Create `crates/voom-cli/tests/fixtures/chaos/video-transcode-required.yaml`:

```yaml
schema_version: 10
scenario_id: voom-video-transcode-required
seed: 1201
duration_scale: short
library:
  roots:
    - id: root_main
      path: library
works:
  - id: w_movie
    title: VOOM Transcode Required
    variants:
      - id: va_main
        label: main
        bundle:
          id: b_main
          assets:
            - id: a_main
              role: main
              container: mp4
              duration_seconds: 1.0
              video:
                source: color_bars
                codec: h264
                resolution: small
              audio:
                - source: sine
                  codec: aac
                  channels: stereo
                  language: eng
timeline: []
```

- [ ] **Step 2: Create HEVC no-op scenario**

Create `crates/voom-cli/tests/fixtures/chaos/video-transcode-noop.yaml`:

```yaml
schema_version: 10
scenario_id: voom-video-transcode-noop
seed: 1202
duration_scale: short
library:
  roots:
    - id: root_main
      path: library
works:
  - id: w_movie
    title: VOOM Transcode Noop
    variants:
      - id: va_main
        label: main
        bundle:
          id: b_main
          assets:
            - id: a_main
              role: main
              container: mkv
              duration_seconds: 1.0
              video:
                source: color_bars
                codec: hevc
                resolution: small
              audio:
                - source: sine
                  codec: aac
                  channels: stereo
                  language: eng
timeline: []
```

- [ ] **Step 3: Validate scenarios**

Run:

```bash
cd third_party/chaos-librarian
uv run chaos-librarian validate ../../crates/voom-cli/tests/fixtures/chaos/video-transcode-required.yaml --json
uv run chaos-librarian validate ../../crates/voom-cli/tests/fixtures/chaos/video-transcode-noop.yaml --json
```

Expected: both commands exit `0`.

- [ ] **Step 4: Run policy seed test from Task 5**

Run:

```bash
cargo test -p voom-cli --test chaos_librarian_e2e policy_seed_creates_durable_ids_from_scan_envelope -- --ignored --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit Task 5 and Task 6 files**

```bash
git add crates/voom-cli/tests/support/mod.rs crates/voom-cli/tests/support/policy_seed.rs crates/voom-cli/tests/chaos_librarian_e2e.rs crates/voom-cli/tests/fixtures/chaos
git commit -m "test: seed chaos policy inputs"
```

---

## Task 7: Deterministic Policy E2E Cases

**Files:**
- Modify: `crates/voom-cli/tests/chaos_librarian_e2e.rs`

- [ ] **Step 1: Add transcode-required E2E test**

Append:

```rust
#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
async fn transcode_required_executes_real_worker_and_commits_hevc_mkv() {
    let chaos = ChaosLibrarian::discover().unwrap();
    chaos.validate_ready().unwrap();
    support::voom_cli::build_worker_binary("voom-ffprobe-worker").unwrap();
    support::voom_cli::build_worker_binary("voom-verify-artifact-worker").unwrap();
    support::voom_cli::build_worker_binary("voom-ffmpeg-worker").unwrap();

    let run = chaos
        .materialize(&chaos.voom_scenario("video-transcode-required.yaml"))
        .unwrap();
    let db = VoomTestDb::init().await.unwrap();
    let library_path = run.run_dir.join("library");
    let library_arg = library_path.to_str().unwrap().to_owned();
    let scan = run_voom(
        &db.url,
        ["scan", "--path", library_arg.as_str()],
    )
    .unwrap();
    assert_eq!(scan.status_code, Some(0), "stderr: {}", scan.stderr);

    let cp = db.control_plane().await.unwrap();
    let ids = seed_transcode_policy_from_scan(&cp, &scan.json, "chaos-h264", "mp4", "h264")
        .await
        .unwrap();
    let plan = run_voom(
        &db.url,
        [
            "plan",
            "show",
            "--policy-version-id",
            &ids.policy_version_id.to_string(),
            "--input-set-id",
            &ids.input_set_id.to_string(),
        ],
    )
    .unwrap();
    assert_eq!(plan.status_code, Some(0), "stderr: {}", plan.stderr);
    assert_eq!(plan.json["data"]["nodes"][0]["operation_kind"], "transcode_video");

    let mut worker = support::voom_cli::TranscodeWorkerLaunch::start(&cp)
        .await
        .unwrap();
    let stage = run.run_dir.join("voom-stage");
    let out = run.run_dir.join("voom-output");
    let execute = run_voom(
        &db.url,
        [
            "compliance",
            "execute",
            "--policy-version-id",
            &ids.policy_version_id.to_string(),
            "--input-set-id",
            &ids.input_set_id.to_string(),
            "--staging-root",
            stage.to_str().unwrap(),
            "--output-dir",
            out.to_str().unwrap(),
        ],
    )
    .unwrap();
    worker.shutdown().unwrap();

    assert_eq!(execute.status_code, Some(0), "stderr: {}", execute.stderr);
    let ticket = execute.json["data"]["tickets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|ticket| ticket["operation"] == "transcode_video")
        .unwrap();
    assert_eq!(ticket["status"], "completed");
    assert!(ticket["result"]["staged_artifact_handle_id"].as_u64().unwrap() > 0);
    assert!(ticket["result"]["verification_id"].as_u64().unwrap() > 0);
    assert!(ticket["result"]["commit_record_id"].as_u64().unwrap() > 0);
    assert!(out.join("VOOM Transcode Required.hevc.mkv").is_file());
}
```

- [ ] **Step 2: Run transcode-required test**

Run:

```bash
cargo test -p voom-cli --test chaos_librarian_e2e transcode_required_executes_real_worker_and_commits_hevc_mkv -- --ignored --nocapture
```

Expected: PASS. If the output filename differs, update the assertion to derive the committed path from the execute envelope or artifact inspect output instead of hardcoding a guessed title.

- [ ] **Step 3: Add no-op E2E test**

Append:

```rust
#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
async fn transcode_noop_does_not_schedule_worker_mutation() {
    let chaos = ChaosLibrarian::discover().unwrap();
    chaos.validate_ready().unwrap();
    support::voom_cli::build_worker_binary("voom-ffprobe-worker").unwrap();

    let run = chaos
        .materialize(&chaos.voom_scenario("video-transcode-noop.yaml"))
        .unwrap();
    let db = VoomTestDb::init().await.unwrap();
    let library_path = run.run_dir.join("library");
    let library_arg = library_path.to_str().unwrap().to_owned();
    let scan = run_voom(
        &db.url,
        ["scan", "--path", library_arg.as_str()],
    )
    .unwrap();
    assert_eq!(scan.status_code, Some(0), "stderr: {}", scan.stderr);

    let cp = db.control_plane().await.unwrap();
    let ids = seed_transcode_policy_from_scan(&cp, &scan.json, "chaos-hevc", "mkv", "hevc")
        .await
        .unwrap();
    let report = run_voom(
        &db.url,
        [
            "compliance",
            "report",
            "--policy-version-id",
            &ids.policy_version_id.to_string(),
            "--input-set-id",
            &ids.input_set_id.to_string(),
        ],
    )
    .unwrap();

    assert_eq!(report.status_code, Some(0), "stderr: {}", report.stderr);
    assert_eq!(report.json["data"]["plan"]["nodes"][0]["status"], "no_op");
    assert_eq!(report.json["data"]["summary"]["planned"], 0);
}
```

- [ ] **Step 4: Run no-op test**

Run:

```bash
cargo test -p voom-cli --test chaos_librarian_e2e transcode_noop_does_not_schedule_worker_mutation -- --ignored --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-cli/tests/chaos_librarian_e2e.rs
git commit -m "test: exercise chaos transcode policy"
```

---

## Task 8: Step Mutation And Malformed Deterministic Cases

**Files:**
- Modify: `crates/voom-cli/tests/chaos_librarian_e2e.rs`

- [ ] **Step 1: Add step mutation rescan test**

Append:

```rust
#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
async fn step_mutation_rescan_observes_changed_media_facts() {
    let chaos = ChaosLibrarian::discover().unwrap();
    chaos.validate_ready().unwrap();
    support::voom_cli::build_worker_binary("voom-ffprobe-worker").unwrap();

    let run = chaos
        .materialize(&chaos.upstream_scenario("reencode-video.yaml"))
        .unwrap();
    let db = VoomTestDb::init().await.unwrap();
    let library_path = run.run_dir.join("library");
    let library_arg = library_path.to_str().unwrap().to_owned();
    let first = run_voom(
        &db.url,
        ["scan", "--path", library_arg.as_str()],
    )
    .unwrap();
    assert_eq!(first.status_code, Some(0), "stderr: {}", first.stderr);

    chaos.step_next(&run.run_dir).unwrap();
    let second = run_voom(
        &db.url,
        ["scan", "--path", library_arg.as_str()],
    )
    .unwrap();
    assert_eq!(second.status_code, Some(0), "stderr: {}", second.stderr);

    assert!(
        second.json["data"]["summary"]["snapshots_recorded"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_ne!(
        first.json["data"]["files"][0]["content_hash"],
        second.json["data"]["files"][0]["content_hash"]
    );
}
```

- [ ] **Step 2: Run step mutation test**

Run:

```bash
cargo test -p voom-cli --test chaos_librarian_e2e step_mutation_rescan_observes_changed_media_facts -- --ignored --nocapture
```

Expected: PASS. If Chaos Librarian requires a different step fixture, use `remux-container.yaml` and assert changed container facts through the media snapshot query.

- [ ] **Step 3: Add malformed media test**

Append:

```rust
#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
async fn malformed_media_fails_loudly_without_execution_ticket() {
    let chaos = ChaosLibrarian::discover().unwrap();
    chaos.validate_ready().unwrap();
    support::voom_cli::build_worker_binary("voom-ffprobe-worker").unwrap();

    let run = chaos
        .materialize(&chaos.upstream_scenario("malformed-container-header.yaml"))
        .unwrap();
    let db = VoomTestDb::init().await.unwrap();
    let library_path = run.run_dir.join("library");
    let library_arg = library_path.to_str().unwrap().to_owned();
    let scan = run_voom(
        &db.url,
        ["scan", "--path", library_arg.as_str()],
    )
    .unwrap();

    assert_eq!(scan.status_code, Some(2), "stderr: {}", scan.stderr);
    assert_eq!(scan.json["status"], "error");
    assert_ne!(scan.json["error"]["code"], "INTERNAL");

    let pool = voom_store::connect(&db.url).await.unwrap();
    let ticket_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tickets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(ticket_count, 0);
}
```

- [ ] **Step 4: Run malformed test**

Run:

```bash
cargo test -p voom-cli --test chaos_librarian_e2e malformed_media_fails_loudly_without_execution_ticket -- --ignored --nocapture
```

Expected: PASS. If scan partially succeeds because one asset remains parseable, assert that failed files have stable non-`INTERNAL` error codes and the ticket count remains `0`.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-cli/tests/chaos_librarian_e2e.rs
git commit -m "test: cover chaos mutations and malformed media"
```

---

## Task 9: Local Wall-Clock Churn Script

**Files:**
- Create: `scripts/chaos-e2e-local.sh`

This first local script is scan-only by default. It records policy execution as
explicitly skipped because the shell runner creates its own ephemeral database
and has no public CLI path to seed policy/input rows against each checkpoint's
latest scan. Keep `CHAOS_EXECUTE_POLICY=1` rejected until a Rust local harness
or public seeding command can create same-database policy inputs for the current
checkpoint.

- [ ] **Step 1: Create the script**

Create `scripts/chaos-e2e-local.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
chaos_dir="$repo_root/third_party/chaos-librarian"

scenario="${CHAOS_SCENARIO:-active-library-churn.yaml}"
duration="${CHAOS_DURATION:-10m}"
speed="${CHAOS_SPEED:-5x}"
checkpoint_interval="${CHAOS_CHECKPOINT_INTERVAL:-30s}"
execute_policy="${CHAOS_EXECUTE_POLICY:-0}"
preserve="${CHAOS_PRESERVE_OUTPUT:-1}"
cleanup="${CHAOS_CLEANUP:-0}"

for tool in git uv jq cargo; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "required tool not found: $tool" >&2
    exit 1
  fi
done

if [[ "$execute_policy" != "0" ]]; then
  echo "CHAOS_EXECUTE_POLICY=1 is intentionally unsupported in the first local churn script" >&2
  echo "Execution-enabled churn needs a Rust harness path that seeds policy/input rows in the same ephemeral database after each scan." >&2
  exit 1
fi

case "$scenario" in
  */*) scenario_path="$scenario" ;;
  video-transcode-*.yaml) scenario_path="$repo_root/crates/voom-cli/tests/fixtures/chaos/$scenario" ;;
  *) scenario_path="$chaos_dir/tests/fixtures/scenarios/$scenario" ;;
esac

if [[ ! -f "$scenario_path" ]]; then
  echo "scenario not found: $scenario_path" >&2
  exit 1
fi

workdir="${CHAOS_WORKDIR:-$(mktemp -d -t voom-chaos-local.XXXXXX)}"
run_dir="$workdir/run"
db="$workdir/voom.db"
url="sqlite://$db"
summary="$workdir/summary.jsonl"

chaos_pid=""
cleanup_run() {
  if [[ -n "$chaos_pid" ]] && kill -0 "$chaos_pid" 2>/dev/null; then
    kill "$chaos_pid" 2>/dev/null || true
    wait "$chaos_pid" 2>/dev/null || true
  fi
  if [[ "$preserve" = "1" ]]; then
    echo "preserved chaos E2E workdir: $workdir" >&2
  elif [[ "$cleanup" = "1" ]]; then
    rm -rf "$workdir"
  else
    echo "workdir left in place: $workdir" >&2
  fi
}
trap cleanup_run EXIT INT TERM

git -C "$repo_root" submodule status third_party/chaos-librarian | grep -E '^ 057a4033a3a9ae14fef664ab82f2c31e1a223544 ' >/dev/null
if [[ -n "$(git -C "$chaos_dir" status --short --untracked-files=no)" ]]; then
  echo "Chaos Librarian submodule has tracked modifications" >&2
  exit 1
fi
cd "$chaos_dir"
uv sync --locked
uv run chaos-librarian capabilities --json | jq -e '.ok == true' >/dev/null

cd "$repo_root"
cargo build -p voom-cli -p voom-ffprobe-worker -p voom-verify-artifact-worker -p voom-ffmpeg-worker
cargo run -q -p voom-cli -- --database-url "$url" init >/dev/null

cd "$chaos_dir"
uv run chaos-librarian run "$scenario_path" --out "$run_dir" --duration "$duration" --speed "$speed" --json > "$workdir/chaos-run.json" &
chaos_pid=$!

checkpoint=0
while kill -0 "$chaos_pid" 2>/dev/null; do
  checkpoint=$((checkpoint + 1))
  scan_out="$workdir/scan-$checkpoint.json"
  set +e
  "$repo_root/target/debug/voom" --database-url "$url" scan --path "$run_dir/library" > "$scan_out"
  scan_rc=$?
  set -e
  error_code="$(jq -r '.error.code // empty' "$scan_out")"
  status="$(jq -r '.status' "$scan_out")"
  if [[ "$scan_rc" -ne 0 && "$error_code" != "ARTIFACT_UNAVAILABLE" && "$error_code" != "MALFORMED_WORKER_RESULT" && "$error_code" != "ARTIFACT_CHECKSUM_MISMATCH" ]]; then
    echo "non-allowlisted scan failure at checkpoint $checkpoint: $error_code" >&2
    exit 1
  fi
  jq -n \
    --argjson checkpoint "$checkpoint" \
    --arg status "$status" \
    --arg error_code "$error_code" \
    --arg scan_out "$scan_out" \
    --arg policy_status "skipped" \
    --arg policy_reason "first local churn script is scan-only; execution-enabled churn requires same-database policy seeding" \
    '{checkpoint:$checkpoint,status:$status,error_code:$error_code,scan_out:$scan_out,policy_status:$policy_status,policy_reason:$policy_reason}' >> "$summary"
  sleep "$checkpoint_interval"
done

wait "$chaos_pid"
echo "chaos local summary: $summary"
```

- [ ] **Step 2: Make the script executable**

Run:

```bash
chmod +x scripts/chaos-e2e-local.sh
```

- [ ] **Step 3: Run shell syntax check**

Run:

```bash
bash -n scripts/chaos-e2e-local.sh
```

Expected: exit `0`.

- [ ] **Step 4: Commit**

```bash
git add scripts/chaos-e2e-local.sh
git commit -m "test: add local chaos churn runner"
```

---

## Task 10: Just Recipes

**Files:**
- Modify: `justfile`

- [ ] **Step 1: Add recipes**

Append to `justfile`:

```just
# Run deterministic Chaos Librarian E2E tests. Not part of default `just ci`.
chaos-e2e-ci:
    cargo test -p voom-cli --test chaos_librarian_e2e -- --ignored --nocapture

# Run a short local-only Chaos Librarian wall-clock churn scenario.
chaos-e2e-local:
    ./scripts/chaos-e2e-local.sh

# Run an extended local-only Chaos Librarian wall-clock soak.
chaos-e2e-soak:
    CHAOS_DURATION=${CHAOS_DURATION:-2h} CHAOS_SPEED=${CHAOS_SPEED:-10x} CHAOS_PRESERVE_OUTPUT=1 ./scripts/chaos-e2e-local.sh
```

- [ ] **Step 2: Verify just lists the recipes**

Run:

```bash
just --list | rg 'chaos-e2e-(ci|local|soak)'
```

Expected: all three recipe names are printed.

- [ ] **Step 3: Run deterministic suite**

Run:

```bash
just chaos-e2e-ci
```

Expected: all ignored Chaos Librarian deterministic tests pass. Missing `uv`, Python 3.13, ffmpeg/ffprobe, or MKVToolNix is a setup failure with an explicit message.

- [ ] **Step 4: Commit**

```bash
git add justfile
git commit -m "test: wire chaos e2e recipes"
```

---

## Task 11: Final Verification And Closeout

**Files:**
- Modify: `docs/superpowers/specs/2026-05-25-chaos-librarian-e2e-design.md` only if implementation reveals a real design correction.
- Create: `docs/superpowers/specs/2026-05-25-chaos-librarian-e2e-closeout.md`

- [ ] **Step 1: Run focused E2E verification**

Run:

```bash
just chaos-e2e-ci
```

Expected: PASS.

- [ ] **Step 2: Run standard repository checks**

Run:

```bash
just fmt-check
just check-test-layout
cargo test -p voom-cli --test chaos_librarian_e2e -- --ignored --nocapture
```

Expected: all pass. `just ci` is not required in this plan because the new E2E suite is explicitly outside default CI and depends on external media tools.

- [ ] **Step 3: Write closeout evidence**

Create `docs/superpowers/specs/2026-05-25-chaos-librarian-e2e-closeout.md`:

```markdown
# Chaos Librarian E2E Closeout

| Requirement | Evidence |
| --- | --- |
| Submodule validation | `cargo test -p voom-cli --test chaos_librarian_e2e chaos_librarian_submodule_is_pinned_and_ready -- --ignored --nocapture` |
| Static baseline compare | `cargo test -p voom-cli --test chaos_librarian_e2e static_library_baseline_scans_exports_and_compares -- --ignored --nocapture` |
| Transcode required | `cargo test -p voom-cli --test chaos_librarian_e2e transcode_required_executes_real_worker_and_commits_hevc_mkv -- --ignored --nocapture` |
| Transcode no-op | `cargo test -p voom-cli --test chaos_librarian_e2e transcode_noop_does_not_schedule_worker_mutation -- --ignored --nocapture` |
| Step mutation | `cargo test -p voom-cli --test chaos_librarian_e2e step_mutation_rescan_observes_changed_media_facts -- --ignored --nocapture` |
| Malformed media | `cargo test -p voom-cli --test chaos_librarian_e2e malformed_media_fails_loudly_without_execution_ticket -- --ignored --nocapture` |
| Local churn recipe syntax | `bash -n scripts/chaos-e2e-local.sh` |
| Just recipes | `just --list | rg 'chaos-e2e-(ci|local|soak)'` |

The Chaos Librarian E2E suite is not part of `just ci`. It is invoked explicitly
with `just chaos-e2e-ci` for deterministic media tests and `just chaos-e2e-local`
or `just chaos-e2e-soak` for local wall-clock churn.
```

- [ ] **Step 4: Commit closeout**

```bash
git add docs/superpowers/specs/2026-05-25-chaos-librarian-e2e-closeout.md
git commit -m "docs: close out chaos librarian e2e"
```

- [ ] **Step 5: Final status**

Run:

```bash
git status --short --branch
git log --oneline -8
```

Expected: clean worktree on `feat/chaos-librarian` with the implementation commits listed above.

---

## Self-Review

Spec coverage:

- Submodule location and validation: Tasks 1 and 10.
- Deterministic CI-safe E2E tests: Tasks 4, 7, 8, and 10.
- Real workers and policy execution: Tasks 2, 5, 7.
- Observed-state export and compare: Tasks 3 and 4.
- Local-only wall-clock churn and soak: Tasks 9 and 10.
- Failure preservation and non-CI separation: Tasks 9, 10, and 11.

No placeholder terms are intentionally left in this plan. The implementation steps avoid broad production abstractions and keep the first export in integration-test support, matching the approved design's narrow first implementation.
