# Sprint 2 Conformance Fill-in Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the empty `voom-conformance` harness suites with manifest-driven active-worker checks plus conformance-owned malformed response fixtures.

**Architecture:** Keep `voom-conformance` independent of `voom-fake-support` and `voom-fakes`. Add small focused modules: `manifest.rs` for admission, `typed_suite.rs` for `HttpClient` semantic checks, `raw_wire_suite.rs` for byte-level request checks, and `negative_fixture.rs` for malformed response-stream fixtures that are not emitted by `echo-worker`.

**Tech Stack:** Rust 2024, Tokio, Hyper/Hyper-util, `voom-worker-protocol`, `serde`, `serde_json`, and `toml` for manifest parsing.

---

## File Structure

- Modify `Cargo.toml`: add workspace dependency `toml = "0.8"`.
- Modify `crates/voom-conformance/Cargo.toml`: add `toml.workspace = true`.
- Modify `crates/voom-conformance/src/lib.rs`: export new modules and public types needed by integration tests.
- Modify `crates/voom-conformance/src/harness.rs`: make suite runners async, call suite modules, add empty-suite failure helper.
- Create `crates/voom-conformance/src/manifest.rs`: parse/validate `voom-fakes-manifest.toml`, resolve active binaries, and report scaffold skips.
- Create `crates/voom-conformance/src/manifest_test.rs`: sibling tests for manifest validation and resolution.
- Create `crates/voom-conformance/src/typed_suite.rs`: active-worker typed protocol assertions.
- Create `crates/voom-conformance/src/typed_suite_test.rs`: sibling tests for result aggregation helpers where no worker process is needed.
- Create `crates/voom-conformance/src/raw_wire_suite.rs`: active-worker raw HTTP assertions plus protocol-negative fixture runner.
- Create `crates/voom-conformance/src/raw_wire_suite_test.rs`: sibling tests for raw request construction helpers.
- Create `crates/voom-conformance/src/negative_fixture.rs`: malformed response body fixtures consumed by raw-wire suite.
- Create `crates/voom-conformance/src/negative_fixture_test.rs`: sibling tests for fixture byte streams.
- Create `crates/voom-conformance/tests/conformance_all.rs`: integration test that drives the manifest, `echo-worker`, and negative fixture checks.
- Modify `crates/voom-conformance/voom-fakes-manifest.toml`: add `target` and `required` fields to `echo-worker` active entry, leave scaffold list explicit.

## Task 1: Manifest Admission

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/voom-conformance/Cargo.toml`
- Modify: `crates/voom-conformance/src/lib.rs`
- Create: `crates/voom-conformance/src/manifest.rs`
- Create: `crates/voom-conformance/src/manifest_test.rs`
- Modify: `crates/voom-conformance/voom-fakes-manifest.toml`

- [ ] **Step 1: Write manifest tests**

Add `#[cfg(test)] #[path = "manifest_test.rs"] mod tests;` at the bottom of `manifest.rs` when creating it. The tests should cover:

```rust
use super::*;

const VALID: &str = r#"
[[binaries]]
name = "echo-worker"
target = "echo-worker"
status = "active"
required = true

[scaffold]
binaries = ["chaos-worker", "benchmark-worker"]
"#;

#[test]
fn parses_active_and_scaffold_entries() {
    let manifest = Manifest::parse_str(VALID).unwrap();
    assert_eq!(manifest.active[0].name, "echo-worker");
    assert_eq!(manifest.active[0].target, "echo-worker");
    assert_eq!(manifest.scaffold, vec!["chaos-worker", "benchmark-worker"]);
}

#[test]
fn rejects_active_entry_without_required_true() {
    let raw = VALID.replace("required = true", "required = false");
    let err = Manifest::parse_str(&raw).unwrap_err();
    assert!(err.to_string().contains("required=true"));
}

#[test]
fn rejects_active_entry_with_non_active_status() {
    let raw = VALID.replace("status = \"active\"", "status = \"scaffold\"");
    let err = Manifest::parse_str(&raw).unwrap_err();
    assert!(err.to_string().contains("status=active"));
}

#[test]
fn rejects_binary_listed_as_active_and_scaffold() {
    let raw = VALID.replace(
        "binaries = [\"chaos-worker\", \"benchmark-worker\"]",
        "binaries = [\"echo-worker\"]",
    );
    let err = Manifest::parse_str(&raw).unwrap_err();
    assert!(err.to_string().contains("active and scaffold"));
}

#[test]
fn resolves_active_from_cargo_bin_env() {
    let entry = ActiveBinary {
        name: "echo-worker".to_owned(),
        target: "echo-worker".to_owned(),
        status: "active".to_owned(),
        required: true,
        path: None,
    };
    let path = resolve_active_with(&entry, |k| {
        (k == "CARGO_BIN_EXE_echo-worker").then(|| "/tmp/echo-worker".into())
    })
    .unwrap();
    assert_eq!(path, std::path::PathBuf::from("/tmp/echo-worker"));
}

#[test]
fn missing_active_binary_is_error() {
    let entry = ActiveBinary {
        name: "echo-worker".to_owned(),
        target: "echo-worker".to_owned(),
        status: "active".to_owned(),
        required: true,
        path: None,
    };
    let err = resolve_active_with(&entry, |_| None).unwrap_err();
    assert!(err.to_string().contains("CARGO_BIN_EXE_echo-worker"));
}
```

- [ ] **Step 2: Run manifest tests and verify they fail**

Run:

```bash
cargo test -p voom-conformance manifest --all-features
```

Expected: fail to compile because `manifest.rs`, `Manifest`, `ActiveBinary`, and `resolve_active_with` do not exist.

- [ ] **Step 3: Implement manifest parsing**

Add workspace dependencies:

```toml
# Cargo.toml [workspace.dependencies]
toml = "0.8"
```

```toml
# crates/voom-conformance/Cargo.toml [dependencies]
toml.workspace = true
```

Add `pub mod manifest;` to `crates/voom-conformance/src/lib.rs`.

Implement `manifest.rs` with these public shapes:

```rust
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("manifest decode: {0}")]
    Decode(String),
    #[error("active binary {name} must have status=active")]
    NonActiveStatus { name: String },
    #[error("active binary {name} must set required=true")]
    NotRequired { name: String },
    #[error("binary {name} listed as both active and scaffold")]
    ActiveAndScaffold { name: String },
    #[error("active binary {name} missing {env_key}")]
    MissingActiveBinary { name: String, env_key: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ActiveBinary {
    pub name: String,
    pub target: String,
    pub status: String,
    pub required: bool,
    #[serde(default)]
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub active: Vec<ActiveBinary>,
    pub scaffold: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawManifest {
    #[serde(default, rename = "binaries")]
    active: Vec<ActiveBinary>,
    #[serde(default)]
    scaffold: RawScaffold,
}

#[derive(Debug, Default, Deserialize)]
struct RawScaffold {
    #[serde(default)]
    binaries: Vec<String>,
}

impl Manifest {
    pub fn parse_str(raw: &str) -> Result<Self, ManifestError> {
        let decoded: RawManifest =
            toml::from_str(raw).map_err(|e| ManifestError::Decode(e.to_string()))?;
        validate(decoded)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, ManifestError> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| ManifestError::Decode(e.to_string()))?;
        Self::parse_str(&raw)
    }
}

pub fn resolve_active(entry: &ActiveBinary) -> Result<PathBuf, ManifestError> {
    resolve_active_with(entry, std::env::var_os)
}

pub fn resolve_active_with<F>(entry: &ActiveBinary, env: F) -> Result<PathBuf, ManifestError>
where
    F: Fn(&str) -> Option<std::ffi::OsString>,
{
    if let Some(path) = &entry.path {
        return Ok(path.clone());
    }
    let env_key = format!("CARGO_BIN_EXE_{}", entry.target);
    env(&env_key)
        .map(PathBuf::from)
        .ok_or_else(|| ManifestError::MissingActiveBinary {
            name: entry.name.clone(),
            env_key,
        })
}

fn validate(raw: RawManifest) -> Result<Manifest, ManifestError> {
    let scaffold: HashSet<&str> = raw.scaffold.binaries.iter().map(String::as_str).collect();
    for entry in &raw.active {
        if entry.status != "active" {
            return Err(ManifestError::NonActiveStatus {
                name: entry.name.clone(),
            });
        }
        if !entry.required {
            return Err(ManifestError::NotRequired {
                name: entry.name.clone(),
            });
        }
        if scaffold.contains(entry.name.as_str()) || scaffold.contains(entry.target.as_str()) {
            return Err(ManifestError::ActiveAndScaffold {
                name: entry.name.clone(),
            });
        }
    }
    Ok(Manifest {
        active: raw.active,
        scaffold: raw.scaffold.binaries,
    })
}

#[cfg(test)]
#[path = "manifest_test.rs"]
mod tests;
```

Update `voom-fakes-manifest.toml` active entry:

```toml
[[binaries]]
name = "echo-worker"
target = "echo-worker"
purpose = "bootstrap conformance - validates the harness against itself"
status = "active"
required = true
```

- [ ] **Step 4: Run manifest tests and verify they pass**

Run:

```bash
cargo test -p voom-conformance manifest --all-features
```

Expected: manifest tests pass.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/voom-conformance/Cargo.toml crates/voom-conformance/src/lib.rs crates/voom-conformance/src/manifest.rs crates/voom-conformance/src/manifest_test.rs crates/voom-conformance/voom-fakes-manifest.toml
git commit -m "feat(conformance): parse worker manifest"
```

## Task 2: Suite Result Aggregation and Async Harness

**Files:**
- Modify: `crates/voom-conformance/src/harness.rs`
- Test: add sibling tests in `crates/voom-conformance/src/harness_test.rs`

- [ ] **Step 1: Write harness tests**

Add `#[cfg(test)] #[path = "harness_test.rs"] mod tests;` at the bottom of `harness.rs`. Add tests:

```rust
use super::*;

#[test]
fn suite_result_merges_passes_and_failures() {
    let mut a = SuiteResult::default();
    a.pass("a");
    a.fail("b", "bad");
    let mut b = SuiteResult::default();
    b.pass("c");
    a.extend(b);
    assert_eq!(a.passed, vec!["a", "c"]);
    assert_eq!(a.failed, vec![("b".to_owned(), "bad".to_owned())]);
}

#[test]
fn empty_active_suite_becomes_failure() {
    let mut result = SuiteResult::default();
    result.fail_if_empty_for("echo-worker");
    assert!(!result.all_passed());
    assert_eq!(result.failed[0].0, "echo-worker::empty_suite");
}
```

- [ ] **Step 2: Run harness tests and verify they fail**

Run:

```bash
cargo test -p voom-conformance harness --all-features
```

Expected: fail because helper methods do not exist.

- [ ] **Step 3: Implement aggregation helpers and async suite signatures**

Add methods:

```rust
impl SuiteResult {
    pub fn pass(&mut self, name: impl Into<String>) {
        self.passed.push(name.into());
    }

    pub fn fail(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.failed.push((name.into(), detail.into()));
    }

    pub fn extend(&mut self, other: Self) {
        self.passed.extend(other.passed);
        self.failed.extend(other.failed);
    }

    pub fn fail_if_empty_for(&mut self, binary_name: &str) {
        if self.is_empty() {
            self.fail(
                format!("{binary_name}::empty_suite"),
                "active binary executed zero conformance checks",
            );
        }
    }
}
```

Change harness suite methods to async:

```rust
pub async fn run_typed_suite(&self, launch: &mut WorkerLaunch) -> SuiteResult {
    crate::typed_suite::run(launch).await
}

pub async fn run_raw_wire_suite(&self, launch: &mut WorkerLaunch) -> SuiteResult {
    crate::raw_wire_suite::run_active_worker(launch).await
}

pub async fn run_all(&self, launch: &mut WorkerLaunch) -> SuiteResult {
    let mut combined = self.run_typed_suite(launch).await;
    combined.extend(self.run_raw_wire_suite(launch).await);
    combined.fail_if_empty_for(
        self.worker_binary
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("worker"),
    );
    combined
}
```

Add suite modules to `lib.rs` in this task so `harness.rs` compiles:

```rust
pub mod raw_wire_suite;
pub mod typed_suite;
```

Create first-pass compiling modules with `run` returning a named failure that forces Tasks 3 and 4 to replace them before integration can pass:

```rust
pub async fn run(_launch: &mut crate::WorkerLaunch) -> crate::SuiteResult {
    let mut result = crate::SuiteResult::default();
    result.fail("typed_suite::pending_task_3", "typed suite pending Task 3");
    result
}
```

```rust
pub async fn run_active_worker(_launch: &mut crate::WorkerLaunch) -> crate::SuiteResult {
    let mut result = crate::SuiteResult::default();
    result.fail("raw_wire_suite::pending_task_4", "raw-wire suite pending Task 4");
    result
}
```

- [ ] **Step 4: Run harness tests and verify they pass**

Run:

```bash
cargo test -p voom-conformance harness --all-features
```

Expected: harness tests pass. Full conformance tests still fail until Tasks 3 and 4 replace the pending-suite failures.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-conformance/src/harness.rs crates/voom-conformance/src/harness_test.rs crates/voom-conformance/src/lib.rs crates/voom-conformance/src/typed_suite.rs crates/voom-conformance/src/raw_wire_suite.rs
git commit -m "feat(conformance): make harness suites executable"
```

## Task 3: Typed Active-Worker Suite

**Files:**
- Modify: `crates/voom-conformance/src/typed_suite.rs`
- Create: `crates/voom-conformance/src/typed_suite_test.rs`

- [ ] **Step 1: Write typed-suite helper tests**

Add sibling tests for request builders:

```rust
use super::*;

#[test]
fn probe_request_uses_probe_file_and_deadlines() {
    let req = probe_request(voom_core::LeaseId(7), "/library/example.mkv");
    assert_eq!(req.operation, voom_worker_protocol::OperationKind::ProbeFile);
    assert_eq!(req.lease_id, voom_core::LeaseId(7));
    assert_eq!(req.heartbeat_deadline_ms, 1_000);
    assert_eq!(req.progress_idle_deadline_ms, 1_000);
}

#[test]
fn invalid_probe_request_omits_path() {
    let req = missing_path_request(voom_core::LeaseId(8));
    assert_eq!(req.operation, voom_worker_protocol::OperationKind::ProbeFile);
    assert!(req.payload.get("path").is_none());
}
```

- [ ] **Step 2: Run typed-suite tests and verify they fail**

Run:

```bash
cargo test -p voom-conformance typed_suite --all-features
```

Expected: fail because request helpers are absent.

- [ ] **Step 3: Implement typed suite**

Implement helpers:

```rust
use voom_core::LeaseId;
use voom_worker_protocol::{
    ClientHandle, HttpClient, NdjsonOutcome, OperationKind, OperationRequest, ProtocolError,
    WorkerCredentials,
};

pub(crate) fn probe_request(lease_id: LeaseId, path: &str) -> OperationRequest {
    OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id,
        payload: serde_json::json!({ "path": path }),
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    }
}

pub(crate) fn missing_path_request(lease_id: LeaseId) -> OperationRequest {
    OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id,
        payload: serde_json::json!({}),
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    }
}
```

Implement `run(launch)` as a sequence of named checks. Each check catches its own error and records `pass` or `fail`:

```rust
pub async fn run(launch: &mut crate::WorkerLaunch) -> crate::SuiteResult {
    let client = HttpClient::new(launch.bound);
    let mut result = crate::SuiteResult::default();

    record(&mut result, "handshake_returns_supported_version", async {
        let resp = client.handshake(voom_core::PROTOCOL_VERSION).await?;
        if resp.agreed == voom_core::PROTOCOL_VERSION {
            Ok(())
        } else {
            Err(ProtocolError::InvalidPayload {
                detail: format!("agreed={}", resp.agreed),
            })
        }
    }).await;

    record(&mut result, "handshake_rejects_below_supported_min", async {
        expect_protocol_err(
            client.handshake(voom_core::PROTOCOL_VERSION_SUPPORTED_MIN - 1).await,
            |e| matches!(e, ProtocolError::UnsupportedProtocolVersion { .. }),
        )
    }).await;

    result
}
```

Before running Step 4, the source must include one `record(...)` call
for every named typed assertion listed below.

The remaining checks must perform these exact assertions:

- `probe_file_accepts_valid_payload`: dispatch `probe_request(LeaseId(10), "/library/example.mkv")` with key `typed-valid`; read one `NdjsonOutcome::Frame` then one `NdjsonOutcome::Terminated`.
- `probe_file_rejects_missing_path`: dispatch `missing_path_request(LeaseId(11))` and require `ProtocolError::InvalidPayload`.
- `unknown_operation_rejected`: dispatch `OperationKind::HashFile` with valid JSON payload and require `ProtocolError::UnknownOperation`.
- `progress_seq_starts_at_zero`: assert the first progress frame seq is `0`.
- `progress_seq_is_monotonic`: assert result frame seq is first seq + 1.
- `terminal_frame_is_last`: after the terminal frame, call `next_frame` once and require `ProtocolError::UnexpectedFrameAfterTerminal`.
- `wrong_bearer_rejected`: clone launch credentials, replace secret with `"wrong"`, dispatch valid request, require `ProtocolError::UnauthorizedBearer`.
- `wrong_worker_id_rejected`: clone credentials with `WorkerId(999)`, require `ProtocolError::UnknownWorkerId`.
- `wrong_worker_epoch_rejected`: clone credentials with `worker_epoch + 1`, require `ProtocolError::StaleWorkerEpoch`.
- `idempotency_exact_byte_replay_returns_cached_response`: dispatch the same request object twice with key `typed-replay`; require both responses have same lease id and both streams terminate.
- `idempotency_same_key_different_body_rejected`: dispatch two requests with the same key and different `payload.path`; require `ProtocolError::DuplicateIdempotencyKey` on the second.
- `stdin_eof_terminates_worker`: do not consume `launch`; this remains covered by an integration test that owns a separate launch and calls `shutdown`.

Use these helper functions:

```rust
async fn record<F>(result: &mut crate::SuiteResult, name: &'static str, fut: F)
where
    F: std::future::Future<Output = Result<(), ProtocolError>>,
{
    match fut.await {
        Ok(()) => result.pass(name),
        Err(e) => result.fail(name, e.to_string()),
    }
}

fn expect_protocol_err(
    got: Result<impl std::fmt::Debug, ProtocolError>,
    predicate: impl FnOnce(&ProtocolError) -> bool,
) -> Result<(), ProtocolError> {
    match got {
        Ok(v) => Err(ProtocolError::InvalidPayload {
            detail: format!("expected error, got {v:?}"),
        }),
        Err(e) if predicate(&e) => Ok(()),
        Err(e) => Err(e),
    }
}
```

- [ ] **Step 4: Run typed-suite tests**

Run:

```bash
cargo test -p voom-conformance typed_suite --all-features
```

Expected: typed-suite unit tests pass. Process-level integration may still fail until Task 6.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-conformance/src/typed_suite.rs crates/voom-conformance/src/typed_suite_test.rs
git commit -m "feat(conformance): add typed worker suite"
```

## Task 4: Raw-Wire Active-Worker Suite

**Files:**
- Modify: `crates/voom-conformance/src/raw_wire_suite.rs`
- Create: `crates/voom-conformance/src/raw_wire_suite_test.rs`

- [ ] **Step 1: Write raw-wire helper tests**

Add tests for request bytes:

```rust
use super::*;

#[test]
fn auth_headers_include_protocol_worker_and_idempotency() {
    let creds = voom_worker_protocol::WorkerCredentials {
        worker_id: voom_core::WorkerId(1),
        worker_epoch: 0,
        secret: secrecy::SecretString::from("secret"),
    };
    let headers = auth_headers(&creds, "abc");
    assert!(headers.iter().any(|(k, _)| *k == "X-Voom-Protocol-Version"));
    assert!(headers.iter().any(|(k, _)| *k == "Authorization"));
    assert!(headers.iter().any(|(k, _)| *k == "X-Voom-Idempotency-Key"));
}

#[test]
fn malformed_json_body_is_not_valid_json() {
    assert!(serde_json::from_slice::<serde_json::Value>(malformed_json_body()).is_err());
}
```

- [ ] **Step 2: Run raw-wire tests and verify they fail**

Run:

```bash
cargo test -p voom-conformance raw_wire_suite --all-features
```

Expected: fail because raw-wire helpers are absent.

- [ ] **Step 3: Implement active raw-wire suite**

Use `tokio::net::TcpStream`, `AsyncWriteExt`, and `AsyncReadExt` to write `voom_worker_protocol::low_level::raw_post_request` bytes to `launch.bound`.

Implement checks:

- `golden_handshake_request_round_trips`: send raw `POST /v1/handshake` with `{"offered":1}`; assert response status starts with `HTTP/1.1 200`.
- `golden_operation_request_round_trips`: send raw valid `POST /v1/operations`; assert `HTTP/1.1 200` and response body contains `"lease_id":`.
- `missing_auth_headers_rejected`: omit auth/version/idempotency headers; assert non-2xx.
- `wrong_bearer_header_rejected`: use wrong bearer; assert response body contains `UNAUTHORIZED_BEARER`.
- `wrong_worker_epoch_header_rejected`: use stale epoch; assert response body contains `STALE_WORKER_EPOCH`.
- `malformed_json_rejected`: send body `b"{not-json"`; assert non-2xx.
- `wrong_content_length_rejected`: manually construct a request whose `Content-Length` is larger than the written body, close the socket, and assert non-2xx or connection close without `HTTP/1.1 200`.
- `unknown_route_returns_404`: send `POST /v1/unknown`; assert `HTTP/1.1 404`.
- `handshake_version_skew_returns_structured_error`: send unsupported offered version and assert body contains `UNSUPPORTED_PROTOCOL_VERSION`.

Implement this helper shape:

```rust
pub async fn run_active_worker(launch: &mut crate::WorkerLaunch) -> crate::SuiteResult {
    let mut result = crate::SuiteResult::default();
    record_raw(&mut result, "golden_handshake_request_round_trips", golden_handshake_request_round_trips(launch)).await;
    record_raw(&mut result, "golden_operation_request_round_trips", golden_operation_request_round_trips(launch)).await;
    record_raw(&mut result, "missing_auth_headers_rejected", missing_auth_headers_rejected(launch)).await;
    record_raw(&mut result, "wrong_bearer_header_rejected", wrong_bearer_header_rejected(launch)).await;
    record_raw(&mut result, "wrong_worker_epoch_header_rejected", wrong_worker_epoch_header_rejected(launch)).await;
    record_raw(&mut result, "malformed_json_rejected", malformed_json_rejected(launch)).await;
    record_raw(&mut result, "wrong_content_length_rejected", wrong_content_length_rejected(launch)).await;
    record_raw(&mut result, "unknown_route_returns_404", unknown_route_returns_404(launch)).await;
    record_raw(&mut result, "handshake_version_skew_returns_structured_error", handshake_version_skew_returns_structured_error(launch)).await;
    result
}

async fn send_raw(addr: std::net::SocketAddr, bytes: bytes::Bytes) -> std::io::Result<Vec<u8>> {
    let mut stream = tokio::net::TcpStream::connect(addr).await?;
    tokio::io::AsyncWriteExt::write_all(&mut stream, &bytes).await?;
    tokio::io::AsyncWriteExt::shutdown(&mut stream).await?;
    let mut out = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut out).await?;
    Ok(out)
}
```

Use string inspection for HTTP status/body in this task. Do not add an HTTP response parser unless a test proves string inspection is ambiguous.

- [ ] **Step 4: Run raw-wire helper tests**

Run:

```bash
cargo test -p voom-conformance raw_wire_suite --all-features
```

Expected: raw-wire unit tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-conformance/src/raw_wire_suite.rs crates/voom-conformance/src/raw_wire_suite_test.rs
git commit -m "feat(conformance): add raw wire worker suite"
```

## Task 5: Protocol Negative Fixture

**Files:**
- Create: `crates/voom-conformance/src/negative_fixture.rs`
- Create: `crates/voom-conformance/src/negative_fixture_test.rs`
- Modify: `crates/voom-conformance/src/lib.rs`
- Modify: `crates/voom-conformance/src/raw_wire_suite.rs`

- [ ] **Step 1: Write fixture tests**

Add tests:

```rust
use super::*;

#[tokio::test]
async fn wrong_lease_fixture_is_rejected() {
    let err = classify_fixture(FixtureMode::WrongLeaseId).await.unwrap_err();
    assert!(matches!(err, voom_worker_protocol::ProtocolError::WrongLeaseId { .. }));
}

#[tokio::test]
async fn frame_after_terminal_fixture_is_rejected() {
    let err = classify_fixture(FixtureMode::FrameAfterTerminal).await.unwrap_err();
    assert!(matches!(
        err,
        voom_worker_protocol::ProtocolError::UnexpectedFrameAfterTerminal
    ));
}

#[tokio::test]
async fn truncated_fixture_is_malformed() {
    let err = classify_fixture(FixtureMode::TruncatedBody).await.unwrap_err();
    assert!(matches!(
        err,
        voom_worker_protocol::ProtocolError::MalformedFrame { .. }
    ));
}
```

- [ ] **Step 2: Run fixture tests and verify they fail**

Run:

```bash
cargo test -p voom-conformance negative_fixture --all-features
```

Expected: fail because fixture module is missing.

- [ ] **Step 3: Implement fixture classification**

Add `pub mod negative_fixture;` to `lib.rs`.

Implement `negative_fixture.rs`:

```rust
use chrono::Utc;
use tokio::io::{AsyncRead, AsyncWriteExt};
use voom_core::LeaseId;
use voom_worker_protocol::{NdjsonReader, ProgressFrame, ProtocolError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureMode {
    WrongLeaseId,
    FrameAfterTerminal,
    TruncatedBody,
}

pub async fn classify_fixture(mode: FixtureMode) -> Result<(), ProtocolError> {
    let expected = LeaseId(1);
    let bytes = fixture_bytes(mode, expected)?;
    let (mut writer, reader) = tokio::io::duplex(bytes.len().saturating_add(1));
    writer
        .write_all(&bytes)
        .await
        .map_err(|e| ProtocolError::MalformedFrame {
            detail: format!("fixture write: {e}"),
        })?;
    drop(writer);
    classify_reader(reader, expected).await
}

pub async fn classify_reader<R>(reader: R, expected: LeaseId) -> Result<(), ProtocolError>
where
    R: AsyncRead + Unpin,
{
    let mut reader = NdjsonReader::new(reader, expected);
    loop {
        match reader.next_frame().await? {
            voom_worker_protocol::NdjsonOutcome::Frame(_) => {}
            voom_worker_protocol::NdjsonOutcome::Terminated(_) => {
                // Calling again is deliberate: frame-after-terminal must fail here.
                reader.next_frame().await?;
                return Ok(());
            }
            voom_worker_protocol::NdjsonOutcome::Closed => return Ok(()),
            voom_worker_protocol::NdjsonOutcome::StreamEnd { .. } => {
                return Err(ProtocolError::MalformedFrame {
                    detail: "stream ended before terminal".to_owned(),
                });
            }
        }
    }
}

fn fixture_bytes(mode: FixtureMode, expected: LeaseId) -> Result<Vec<u8>, ProtocolError> {
    let now = Utc::now();
    let mut bytes = Vec::new();
    let progress = ProgressFrame::Progress {
        lease_id: match mode {
            FixtureMode::WrongLeaseId => LeaseId(expected.0 + 1),
            _ => expected,
        },
        seq: 0,
        emitted_at: now,
        percent: None,
        message: Some("fixture".to_owned()),
        payload: None,
    };
    push_frame(&mut bytes, &progress)?;
    if mode == FixtureMode::WrongLeaseId {
        return Ok(bytes);
    }

    let result = ProgressFrame::Result {
        lease_id: expected,
        seq: 1,
        emitted_at: now,
        payload: serde_json::json!({"ok": true}),
    };
    push_frame(&mut bytes, &result)?;

    match mode {
        FixtureMode::FrameAfterTerminal => {
            let extra = ProgressFrame::Progress {
                lease_id: expected,
                seq: 2,
                emitted_at: now,
                percent: None,
                message: Some("after terminal".to_owned()),
                payload: None,
            };
            push_frame(&mut bytes, &extra)?;
        }
        FixtureMode::TruncatedBody => {
            bytes.pop();
        }
        FixtureMode::WrongLeaseId => {}
    }
    Ok(bytes)
}

fn push_frame(out: &mut Vec<u8>, frame: &ProgressFrame) -> Result<(), ProtocolError> {
    let mut bytes = serde_json::to_vec(frame).map_err(|e| ProtocolError::MalformedFrame {
        detail: format!("fixture encode: {e}"),
    })?;
    bytes.push(b'\n');
    out.extend(bytes);
    Ok(())
}

#[cfg(test)]
#[path = "negative_fixture_test.rs"]
mod tests;
```

Add a raw-wire suite runner:

```rust
pub async fn run_protocol_negative_fixture() -> crate::SuiteResult {
    let mut result = crate::SuiteResult::default();
    record_fixture(&mut result, "frame_with_wrong_lease_id_rejected", crate::negative_fixture::FixtureMode::WrongLeaseId).await;
    record_fixture(&mut result, "frame_after_terminal_rejected", crate::negative_fixture::FixtureMode::FrameAfterTerminal).await;
    record_fixture(&mut result, "partial_response_body_classified", crate::negative_fixture::FixtureMode::TruncatedBody).await;
    result
}
```

- [ ] **Step 4: Run fixture tests**

Run:

```bash
cargo test -p voom-conformance negative_fixture --all-features
```

Expected: fixture tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-conformance/src/lib.rs crates/voom-conformance/src/negative_fixture.rs crates/voom-conformance/src/negative_fixture_test.rs crates/voom-conformance/src/raw_wire_suite.rs
git commit -m "feat(conformance): add negative stream fixtures"
```

## Task 6: Manifest-Driven Integration Gate

**Files:**
- Create: `crates/voom-conformance/tests/conformance_all.rs`
- Modify: `crates/voom-conformance/src/harness.rs`
- Modify: `crates/voom-conformance/src/lib.rs`

- [ ] **Step 1: Write integration test**

Create `tests/conformance_all.rs`:

```rust
use std::time::Duration;

use voom_conformance::manifest::{resolve_active, Manifest};
use voom_conformance::{Harness, SuiteResult};

#[tokio::test]
async fn echo_worker_and_negative_fixtures_pass_conformance() {
    let manifest = Manifest::load("crates/voom-conformance/voom-fakes-manifest.toml").unwrap();
    assert_eq!(manifest.active.len(), 1);
    assert!(manifest.scaffold.iter().any(|s| s == "chaos-worker"));

    let mut combined = SuiteResult::default();
    for entry in &manifest.active {
        let path = resolve_active(entry).unwrap();
        let harness = Harness::new(path);
        let mut launch = harness.launch().await.unwrap();
        let result = harness.run_all(&mut launch).await;
        let _ = launch.shutdown(Duration::from_secs(5)).await.unwrap();
        combined.extend(result);
    }

    combined.extend(voom_conformance::raw_wire_suite::run_protocol_negative_fixture().await);

    assert!(
        combined.all_passed(),
        "conformance failures: {:?}",
        combined.failed
    );
    assert!(!combined.is_empty());
}
```

- [ ] **Step 2: Run integration test and verify it fails before final wiring**

Run:

```bash
cargo test -p voom-conformance --test conformance_all --all-features
```

Expected before final fixes: fail if any suite still returns `*_pending_task_*` or if the manifest cannot resolve `echo-worker`.

- [ ] **Step 3: Finish exports and manifest path handling**

Ensure `lib.rs` exports:

```rust
pub mod harness;
pub mod manifest;
pub mod negative_fixture;
pub mod raw_wire_suite;
pub mod typed_suite;

pub use harness::{Harness, SuiteResult, WorkerLaunch};
```

Use the crate-local manifest path so the test works whether Cargo runs from the workspace root or the crate directory:

```rust
let manifest_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("voom-fakes-manifest.toml");
let manifest = Manifest::load(manifest_path).unwrap();
```

- [ ] **Step 4: Run conformance crate tests**

Run:

```bash
cargo test -p voom-conformance --all-features
```

Expected: all `voom-conformance` tests pass, and `SuiteResult.failed` is empty in `conformance_all`.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-conformance/src/lib.rs crates/voom-conformance/src/harness.rs crates/voom-conformance/tests/conformance_all.rs
git commit -m "test(conformance): gate echo worker and fixtures"
```

## Task 7: Branch Verification

**Files:**
- No planned source changes.

- [ ] **Step 1: Run focused verification**

Run:

```bash
cargo test -p voom-conformance --all-features
```

Expected: all conformance tests pass.

- [ ] **Step 2: Run workspace gate**

Run:

```bash
just ci
```

Expected: format, lint, test, docs, deny, and audit checks pass.

- [ ] **Step 3: Confirm the worktree state**

Run:

```bash
git status --short
```

Expected: no uncommitted changes. If verification required a fix, make that fix in the task that owns the failing file, rerun that task's verification command, and commit with that task's commit message pattern.
