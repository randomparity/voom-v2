# Sprint 2 Phase 6 Fake Providers And Conformance Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement all eleven fake-provider workers and promote them into manifest-driven conformance as the foundation for Phase 7's simulated scheduler workflow.

**Architecture:** `voom-fake-support` wraps `voom_worker_protocol::HttpServer` and owns fake-provider bootstrap, stdin EOF shutdown, provider dispatch, payload validation, and frame construction. The eleven `fake-*` binaries become thin adapters over a shared provider catalog; `voom-conformance` becomes manifest-operation-aware and mechanically verifies `OperationKind` and `FailureClass` coverage.

**Tech Stack:** Rust 2024, Tokio, `voom-worker-protocol`, `voom-core`, `voom-fake-support`, `voom-conformance`, `serde`, `serde_json`, `toml`, `secrecy`, and existing process-backed integration-test patterns.

---

## File Structure

- Modify `crates/voom-core/src/failure.rs`: add `FailureClass::ALL`.
- Modify `crates/voom-core/src/failure_test.rs`: assert `ALL` covers retry and error-code mappings.
- Modify `crates/voom-worker-protocol/src/operation_kind.rs`: add `OperationKind::ALL`.
- Modify `crates/voom-worker-protocol/src/operation_kind_test.rs`: assert `ALL` covers the fixed operation vocabulary.
- Modify `crates/voom-fake-support/Cargo.toml`: add Tokio + `secrecy` dependencies already used by worker bootstrap.
- Replace `crates/voom-fake-support/src/lib.rs`: shared runtime, provider catalog types, payload helpers, frame builders, and `run_provider`.
- Replace `crates/voom-fake-support/src/lib_test.rs`: sibling tests for runtime-independent support logic.
- Modify all eleven `crates/voom-fakes/src/bin/fake_*.rs`: replace placeholders with thin provider launches.
- Create `crates/voom-fakes/tests/fake_providers.rs`: process-backed tests for all fake providers.
- Modify `crates/voom-conformance/src/manifest.rs`: add operation-case schema and coverage helpers.
- Modify `crates/voom-conformance/src/manifest_test.rs`: cover operation schema, all-operation coverage, and scaffold rejection.
- Modify `crates/voom-conformance/src/typed_suite.rs`: build positive/negative requests from manifest operation cases.
- Modify `crates/voom-conformance/src/raw_wire_suite.rs`: build raw operation bodies from manifest operation cases.
- Create `crates/voom-conformance/src/failure_taxonomy.rs`: registry and coverage checks for `FailureClass`.
- Create `crates/voom-conformance/src/failure_taxonomy_test.rs`: sibling tests for the registry.
- Modify `crates/voom-conformance/src/lib.rs`: export `failure_taxonomy`.
- Modify `crates/voom-conformance/tests/conformance_all.rs`: enforce active fake providers, operation coverage, and failure taxonomy coverage.
- Modify `crates/voom-conformance/voom-fakes-manifest.toml`: promote eleven fake providers and add operation cases.

## Provider Contract Decisions

Every fake response uses exactly one progress frame at `seq = 0` and one terminal result frame at `seq = 1`. All success payloads include `provider`, `operation`, `scenario`, and provider-specific result fields. All invalid payloads return `ProtocolError::InvalidPayload`; unsupported operations return `ProtocolError::UnknownOperation`; idempotency is delegated to `HttpServer`.

| Binary | Primary operation | Secondary operations | Required valid payload | Invalid payload |
|---|---|---|---|---|
| `fake-scanner` | `ScanLibrary` | none | `{ "path": "/library", "scenario": "default" }` | `{ "scenario": "missing_path" }` |
| `fake-prober` | `ProbeFile` | `HashFile` | `{ "path": "/library/movie.mkv", "scenario": "default" }` | `{ "scenario": "missing_path" }` |
| `fake-transcoder` | `TranscodeVideo` | `ExtractAudio`, `TranscribeAudio` | `{ "path": "/library/movie.mkv", "target_codec": "h265", "scenario": "default" }` | `{ "path": "/library/movie.mkv", "target_codec": "bad_codec" }` |
| `fake-remuxer` | `Remux` | none | `{ "path": "/library/movie.mkv", "container": "mkv", "scenario": "default" }` | `{ "path": "/library/movie.mkv", "container": "bad_container" }` |
| `fake-backup-store` | `BackUpFile` | `DeleteArtifact` | `{ "path": "/library/movie.mkv", "scenario": "default" }` | `{ "scenario": "missing_path" }` |
| `fake-health-checker` | `VerifyArtifact` | none | `{ "path": "/library/movie.mkv", "scenario": "default" }` | `{ "scenario": "missing_path" }` |
| `fake-identity-provider` | `IdentifyMedia` | none | `{ "path": "/library/movie.mkv", "scenario": "default" }` | `{ "scenario": "missing_path" }` |
| `fake-external-system` | `SyncExternalSystem` | none | `{ "path": "/library/movie.mkv", "system": "plex", "action": "refresh", "scenario": "default" }` | `{ "path": "/library/movie.mkv", "system": "unknown", "action": "refresh" }` |
| `fake-quality-scorer` | `ScoreQuality` | none | `{ "path": "/library/movie.mkv", "profile": "default", "scenario": "default" }` | `{ "path": "/library/movie.mkv", "profile": "unknown" }` |
| `fake-issue-provider` | `CommitArtifact` | none | `{ "path": "/library/movie.mkv", "reason": "quality_regression", "scenario": "default" }` | `{ "path": "/library/movie.mkv", "reason": "unknown" }` |
| `fake-use-lease-provider` | `EditTracks` | none | `{ "path": "/library/movie.mkv", "holder": "manual", "reason": "playback", "scenario": "default" }` | `{ "path": "/library/movie.mkv", "holder": "manual", "reason": "unknown" }` |

## Task 1: FailureClass Authoritative Variant List

**Files:**
- Modify: `crates/voom-core/src/failure.rs`
- Modify: `crates/voom-core/src/failure_test.rs`

- [ ] **Step 1: Add failing tests for `FailureClass::ALL`**

Add these tests to `crates/voom-core/src/failure_test.rs`:

```rust
#[test]
fn all_contains_every_failure_class_once() {
    use std::collections::HashSet;

    let all = FailureClass::ALL;
    assert_eq!(all.len(), 22);
    let unique = all.iter().copied().collect::<HashSet<_>>();
    assert_eq!(unique.len(), all.len());
    assert!(unique.contains(&FailureClass::WorkerTimeout));
    assert!(unique.contains(&FailureClass::WorkerCrash));
    assert!(unique.contains(&FailureClass::NoEligibleWorker));
    assert!(unique.contains(&FailureClass::ArtifactUnavailable));
    assert!(unique.contains(&FailureClass::ArtifactChecksumMismatch));
    assert!(unique.contains(&FailureClass::ExternalSystemUnavailable));
    assert!(unique.contains(&FailureClass::ExternalSystemRateLimited));
    assert!(unique.contains(&FailureClass::VerificationFailure));
    assert!(unique.contains(&FailureClass::BackupFailure));
    assert!(unique.contains(&FailureClass::CommitFailure));
    assert!(unique.contains(&FailureClass::PolicyParseError));
    assert!(unique.contains(&FailureClass::PolicyValidationError));
    assert!(unique.contains(&FailureClass::MissingCapability));
    assert!(unique.contains(&FailureClass::MalformedWorkerResult));
    assert!(unique.contains(&FailureClass::UserCancellation));
    assert!(unique.contains(&FailureClass::StaleIdentityEvidence));
    assert!(unique.contains(&FailureClass::ClosureResolutionIncomplete));
    assert!(unique.contains(&FailureClass::BlockedByActiveUseLease));
    assert!(unique.contains(&FailureClass::ApprovalRequired));
    assert!(unique.contains(&FailureClass::PriorityPolicyConflict));
    assert!(unique.contains(&FailureClass::ProgressTimeout));
    assert!(unique.contains(&FailureClass::AmbiguousWorkerSelection));
}

#[test]
fn all_variants_have_retry_and_error_code_mappings() {
    for class in FailureClass::ALL {
        let _ = class.retry_class();
        let _ = class.into_error_code();
    }
}
```

- [ ] **Step 2: Run the focused failing test**

Run:

```bash
cargo test -p voom-core all_contains_every_failure_class_once --all-features
```

Expected: compile failure because `FailureClass::ALL` does not exist.

- [ ] **Step 3: Implement `FailureClass::ALL`**

Add this associated constant inside `impl FailureClass` in `crates/voom-core/src/failure.rs` before `retry_class`:

```rust
pub const ALL: &'static [Self] = &[
    Self::WorkerTimeout,
    Self::WorkerCrash,
    Self::NoEligibleWorker,
    Self::ArtifactUnavailable,
    Self::ArtifactChecksumMismatch,
    Self::ExternalSystemUnavailable,
    Self::ExternalSystemRateLimited,
    Self::VerificationFailure,
    Self::BackupFailure,
    Self::CommitFailure,
    Self::PolicyParseError,
    Self::PolicyValidationError,
    Self::MissingCapability,
    Self::MalformedWorkerResult,
    Self::UserCancellation,
    Self::StaleIdentityEvidence,
    Self::ClosureResolutionIncomplete,
    Self::BlockedByActiveUseLease,
    Self::ApprovalRequired,
    Self::PriorityPolicyConflict,
    Self::ProgressTimeout,
    Self::AmbiguousWorkerSelection,
];
```

- [ ] **Step 4: Verify and commit**

Run:

```bash
cargo test -p voom-core all_ --all-features
```

Expected: both new `all_` tests pass.

Commit:

```bash
git add crates/voom-core/src/failure.rs crates/voom-core/src/failure_test.rs
git commit -m "feat(core): expose failure class coverage list"
```

## Task 2: OperationKind Authoritative Variant List

**Files:**
- Modify: `crates/voom-worker-protocol/src/operation_kind.rs`
- Modify: `crates/voom-worker-protocol/src/operation_kind_test.rs`

- [ ] **Step 1: Add failing tests for `OperationKind::ALL`**

Add these tests to `crates/voom-worker-protocol/src/operation_kind_test.rs`:

```rust
#[test]
fn all_contains_every_operation_kind_once() {
    use std::collections::HashSet;

    let all = OperationKind::ALL;
    assert_eq!(all.len(), 15);
    let unique = all.iter().copied().collect::<HashSet<_>>();
    assert_eq!(unique.len(), all.len());
    assert!(unique.contains(&OperationKind::ScanLibrary));
    assert!(unique.contains(&OperationKind::ProbeFile));
    assert!(unique.contains(&OperationKind::HashFile));
    assert!(unique.contains(&OperationKind::IdentifyMedia));
    assert!(unique.contains(&OperationKind::ScoreQuality));
    assert!(unique.contains(&OperationKind::SyncExternalSystem));
    assert!(unique.contains(&OperationKind::BackUpFile));
    assert!(unique.contains(&OperationKind::Remux));
    assert!(unique.contains(&OperationKind::TranscodeVideo));
    assert!(unique.contains(&OperationKind::EditTracks));
    assert!(unique.contains(&OperationKind::ExtractAudio));
    assert!(unique.contains(&OperationKind::TranscribeAudio));
    assert!(unique.contains(&OperationKind::VerifyArtifact));
    assert!(unique.contains(&OperationKind::CommitArtifact));
    assert!(unique.contains(&OperationKind::DeleteArtifact));
}

#[test]
fn all_operation_kinds_round_trip_through_wire_names() {
    for operation in OperationKind::ALL {
        let encoded = serde_json::to_string(operation).unwrap();
        let decoded: OperationKind = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, *operation);
    }
}
```

- [ ] **Step 2: Run the focused failing test**

Run:

```bash
cargo test -p voom-worker-protocol all_operation --all-features
```

Expected: compile failure because `OperationKind::ALL` does not exist.

- [ ] **Step 3: Implement `OperationKind::ALL`**

Add this associated constant inside a new `impl OperationKind` block in `crates/voom-worker-protocol/src/operation_kind.rs`:

```rust
impl OperationKind {
    pub const ALL: &'static [Self] = &[
        Self::ScanLibrary,
        Self::ProbeFile,
        Self::HashFile,
        Self::IdentifyMedia,
        Self::ScoreQuality,
        Self::SyncExternalSystem,
        Self::BackUpFile,
        Self::Remux,
        Self::TranscodeVideo,
        Self::EditTracks,
        Self::ExtractAudio,
        Self::TranscribeAudio,
        Self::VerifyArtifact,
        Self::CommitArtifact,
        Self::DeleteArtifact,
    ];
}
```

- [ ] **Step 4: Verify and commit**

Run:

```bash
cargo test -p voom-worker-protocol all_operation --all-features
```

Expected: both new `all_operation` tests pass.

Commit:

```bash
git add crates/voom-worker-protocol/src/operation_kind.rs crates/voom-worker-protocol/src/operation_kind_test.rs
git commit -m "feat(protocol): expose operation kind coverage list"
```

## Task 3: Fake Support Runtime

**Files:**
- Modify: `crates/voom-fake-support/Cargo.toml`
- Modify: `crates/voom-fake-support/src/lib.rs`
- Modify: `crates/voom-fake-support/src/lib_test.rs`

- [ ] **Step 1: Write failing support tests**

Replace `crates/voom-fake-support/src/lib_test.rs` with tests covering the new helper API:

```rust
use super::*;
use voom_worker_protocol::OperationKind;

#[test]
fn provider_definition_rejects_unsupported_operation() {
    let provider = provider_definition("fake-prober").unwrap();
    let req = request(OperationKind::Remux, serde_json::json!({"path": "/library/movie.mkv"}));
    let err = dispatch_provider(&provider, req).unwrap_err();
    assert!(matches!(err, voom_worker_protocol::ProtocolError::UnknownOperation { .. }));
}

#[test]
fn provider_definition_accepts_secondary_operation() {
    let provider = provider_definition("fake-prober").unwrap();
    let req = request(OperationKind::HashFile, serde_json::json!({"path": "/library/movie.mkv"}));
    let dispatch = dispatch_provider(&provider, req).unwrap();
    assert_eq!(dispatch.response.lease_id, voom_core::LeaseId(42));
    assert!(String::from_utf8(dispatch.body).unwrap().contains("\"operation\":\"hash_file\""));
}

#[test]
fn missing_path_is_invalid_payload() {
    let provider = provider_definition("fake-scanner").unwrap();
    let req = request(OperationKind::ScanLibrary, serde_json::json!({"scenario": "default"}));
    let err = dispatch_provider(&provider, req).unwrap_err();
    assert!(matches!(err, voom_worker_protocol::ProtocolError::InvalidPayload { .. }));
}

fn request(
    operation: OperationKind,
    payload: serde_json::Value,
) -> voom_worker_protocol::OperationRequest {
    voom_worker_protocol::OperationRequest {
        operation,
        lease_id: voom_core::LeaseId(42),
        payload,
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    }
}
```

- [ ] **Step 2: Run failing support tests**

Run:

```bash
cargo test -p voom-fake-support provider_definition --all-features
```

Expected: compile failure because `provider_definition` and `dispatch_provider` do not exist.

- [ ] **Step 3: Add runtime dependencies**

Update `crates/voom-fake-support/Cargo.toml` dependencies:

```toml
secrecy.workspace = true
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "io-std", "io-util", "net", "sync", "time"] }
```

- [ ] **Step 4: Implement provider catalog and dispatch**

Replace `crates/voom-fake-support/src/lib.rs` with a module that keeps the existing `Scenario*` types and adds these public items:

```rust
#[derive(Debug, Clone)]
pub struct ProviderDefinition {
    pub binary_name: &'static str,
    pub provider: &'static str,
    pub primary: OperationKind,
    pub secondary: &'static [OperationKind],
}

pub fn provider_definition(binary_name: &str) -> Option<ProviderDefinition>;

pub fn dispatch_provider(
    provider: &ProviderDefinition,
    req: OperationRequest,
) -> Result<OperationDispatch, ProtocolError>;

pub async fn run_provider(binary_name: &'static str) -> Result<(), Box<dyn std::error::Error>>;
```

Implementation rules:

- `provider_definition` returns the exact provider mapping from the spec.
- `dispatch_provider` rejects unsupported operations with `ProtocolError::UnknownOperation`.
- `dispatch_provider` validates provider payloads exactly as the table in this plan defines.
- `dispatch_provider` creates `OperationResponse { lease_id, accepted_at: Utc::now() }`.
- `dispatch_provider` serializes exactly two `ProgressFrame`s into `OperationDispatch.body`: one `Progress` with `seq = 0`, one `Result` with `seq = 1`.
- Success payloads include at least `provider`, `operation`, `scenario`, and provider-specific fields from the spec.
- `run_provider` loads credentials from `VOOM_WORKER_SECRET`, `VOOM_WORKER_ID`, `VOOM_WORKER_EPOCH`, binds `VOOM_WORKER_BIND` defaulting to `127.0.0.1:0`, starts `HttpServer`, prints `BOUND addr={bound}`, and shuts down when stdin reaches EOF.

- [ ] **Step 5: Verify and commit**

Run:

```bash
cargo test -p voom-fake-support --all-features
```

Expected: all `voom-fake-support` tests pass.

Commit:

```bash
git add crates/voom-fake-support/Cargo.toml crates/voom-fake-support/src/lib.rs crates/voom-fake-support/src/lib_test.rs Cargo.lock
git commit -m "feat(fake-support): add provider runtime"
```

## Task 4: Fake Provider Binaries

**Files:**
- Modify: all eleven `crates/voom-fakes/src/bin/fake_*.rs`
- Create: `crates/voom-fakes/tests/fake_providers.rs`

- [ ] **Step 1: Write process-backed fake-provider tests**

Create `crates/voom-fakes/tests/fake_providers.rs` with a table-driven async test. The table must include:

```rust
struct ProviderCase {
    bin_env: &'static str,
    name: &'static str,
    primary: OperationKind,
    secondary: &'static [OperationKind],
    valid_payload: serde_json::Value,
    invalid_payload: serde_json::Value,
    expected_field: &'static str,
}
```

Use these cases:

- `CARGO_BIN_EXE_fake-scanner`, `fake-scanner`, `ScanLibrary`, `[]`, expected field `files`.
- `CARGO_BIN_EXE_fake-prober`, `fake-prober`, `ProbeFile`, `[HashFile]`, expected field `duration_ms`.
- `CARGO_BIN_EXE_fake-transcoder`, `fake-transcoder`, `TranscodeVideo`, `[ExtractAudio, TranscribeAudio]`, expected field `output_path`.
- `CARGO_BIN_EXE_fake-remuxer`, `fake-remuxer`, `Remux`, `[]`, expected field `container`.
- `CARGO_BIN_EXE_fake-backup-store`, `fake-backup-store`, `BackUpFile`, `[DeleteArtifact]`, expected field `local_backup_id`.
- `CARGO_BIN_EXE_fake-health-checker`, `fake-health-checker`, `VerifyArtifact`, `[]`, expected field `status`.
- `CARGO_BIN_EXE_fake-identity-provider`, `fake-identity-provider`, `IdentifyMedia`, `[]`, expected field `canonical_media_id`.
- `CARGO_BIN_EXE_fake-external-system`, `fake-external-system`, `SyncExternalSystem`, `[]`, expected field `refresh_status`.
- `CARGO_BIN_EXE_fake-quality-scorer`, `fake-quality-scorer`, `ScoreQuality`, `[]`, expected field `score`.
- `CARGO_BIN_EXE_fake-issue-provider`, `fake-issue-provider`, `CommitArtifact`, `[]`, expected field `issue_key`.
- `CARGO_BIN_EXE_fake-use-lease-provider`, `fake-use-lease-provider`, `EditTracks`, `[]`, expected field `decision`.

Each case must:

- spawn the binary with `VOOM_WORKER_SECRET`, `VOOM_WORKER_ID`, `VOOM_WORKER_EPOCH`, `VOOM_WORKER_BIND=127.0.0.1:0`;
- read `BOUND addr=...`;
- use `HttpClient::dispatch` with the primary operation and valid payload;
- assert one progress frame, one terminal result, terminal is last, and terminal payload contains `provider`, `operation`, `scenario`, and `expected_field`;
- dispatch the invalid payload and assert `ProtocolError::InvalidPayload`;
- dispatch `OperationKind::DeleteArtifact` as unsupported unless the case is `fake-backup-store`, otherwise use `OperationKind::ProbeFile` as unsupported;
- dispatch each secondary operation with the valid payload and assert success;
- dispatch the same idempotency key + same body twice and assert the second response succeeds;
- dispatch the same idempotency key + different body and assert `DuplicateIdempotencyKey`;
- close stdin and require clean shutdown within five seconds.

- [ ] **Step 2: Run failing fake-provider tests**

Run:

```bash
cargo test -p voom-fakes --test fake_providers --all-features
```

Expected: failures because placeholder binaries do not print `BOUND addr=...`.

- [ ] **Step 3: Replace fake binaries with thin launchers**

Each fake binary becomes:

```rust
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    voom_fake_support::run_provider("fake-scanner").await
}
```

Use the matching binary name string in each file:

- `fake_scanner.rs`: `"fake-scanner"`
- `fake_prober.rs`: `"fake-prober"`
- `fake_transcoder.rs`: `"fake-transcoder"`
- `fake_remuxer.rs`: `"fake-remuxer"`
- `fake_backup_store.rs`: `"fake-backup-store"`
- `fake_health_checker.rs`: `"fake-health-checker"`
- `fake_identity_provider.rs`: `"fake-identity-provider"`
- `fake_external_system.rs`: `"fake-external-system"`
- `fake_quality_scorer.rs`: `"fake-quality-scorer"`
- `fake_issue_provider.rs`: `"fake-issue-provider"`
- `fake_use_lease_provider.rs`: `"fake-use-lease-provider"`

- [ ] **Step 4: Verify and commit**

Run:

```bash
cargo test -p voom-fakes --test fake_providers --all-features
cargo test -p voom-fakes --all-features
```

Expected: all `voom-fakes` tests pass, including existing chaos and benchmark tests.

Commit:

```bash
git add crates/voom-fakes/src/bin/fake_*.rs crates/voom-fakes/tests/fake_providers.rs
git commit -m "feat(fakes): implement provider workers"
```

## Task 5: Manifest Operation Cases

**Files:**
- Modify: `crates/voom-conformance/src/manifest.rs`
- Modify: `crates/voom-conformance/src/manifest_test.rs`
- Modify: `crates/voom-conformance/voom-fakes-manifest.toml`

- [ ] **Step 1: Write failing manifest tests**

Add tests for:

- parsing `[[binaries.operations]]`;
- rejecting active entries with zero operation cases;
- rejecting non-object `valid_payload` or `invalid_payload`;
- `operation_coverage(&manifest)` returning every fixed `OperationKind`;
- missing `DeleteArtifact` returning a coverage error.

The public shapes must be:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OperationCase {
    pub operation: voom_worker_protocol::OperationKind,
    pub valid_payload: serde_json::Value,
    pub invalid_payload: serde_json::Value,
}

pub struct ActiveBinary {
    pub name: String,
    pub target: String,
    pub status: String,
    pub required: bool,
    pub operations: Vec<OperationCase>,
    pub path: Option<PathBuf>,
}

pub fn validate_operation_coverage(manifest: &Manifest) -> Result<(), ManifestError>;
```

- [ ] **Step 2: Run failing manifest tests**

Run:

```bash
cargo test -p voom-conformance manifest --all-features
```

Expected: compile failure because `OperationCase` and `validate_operation_coverage` do not exist.

- [ ] **Step 3: Implement manifest schema**

Update `ManifestError` with:

```rust
#[error("active binary {name} must declare at least one operation case")]
MissingOperationCases { name: String },
#[error("active binary {name} operation {operation:?} {field} must be a JSON object")]
PayloadNotObject {
    name: String,
    operation: voom_worker_protocol::OperationKind,
    field: &'static str,
},
#[error("operation coverage missing: {missing:?}")]
MissingOperationCoverage {
    missing: Vec<voom_worker_protocol::OperationKind>,
},
```

Validation rules:

- `echo-worker`, `chaos-worker`, and `benchmark-worker` must have a `ProbeFile` operation case in the manifest.
- Every active entry must have at least one operation case.
- Every `valid_payload` and `invalid_payload` must be an object.
- `validate_operation_coverage` checks all variants from `voom_worker_protocol::OperationKind::ALL`.
- Update `voom-fakes-manifest.toml` so currently active `echo-worker`, `chaos-worker`, and `benchmark-worker` each have this operation case:

```toml
[[binaries.operations]]
operation = "probe_file"
valid_payload = { path = "/library/example.mkv" }
invalid_payload = { }
```

- Keep all eleven fake providers under `[scaffold]` in this task.

- [ ] **Step 4: Verify and commit**

Run:

```bash
cargo test -p voom-conformance manifest --all-features
```

Expected: manifest tests pass.

Commit:

```bash
git add crates/voom-conformance/src/manifest.rs crates/voom-conformance/src/manifest_test.rs crates/voom-conformance/voom-fakes-manifest.toml
git commit -m "feat(conformance): add manifest operation cases"
```

## Task 6: Typed And Raw Suites Use Manifest Operation Cases

**Files:**
- Modify: `crates/voom-conformance/src/harness.rs`
- Modify: `crates/voom-conformance/src/typed_suite.rs`
- Modify: `crates/voom-conformance/src/raw_wire_suite.rs`
- Modify: `crates/voom-conformance/tests/conformance_all.rs`

- [ ] **Step 1: Write failing suite tests**

Update or add sibling tests so `typed_suite` and `raw_wire_suite` request builders accept an `OperationCase` and build `OperationRequest` with the case's operation/payload instead of hard-coded `ProbeFile`.

Required helper signatures:

```rust
pub async fn run(
    launch: &mut crate::WorkerLaunch,
    entry: &crate::manifest::ActiveBinary,
) -> crate::SuiteResult;

pub async fn run_active_worker(
    launch: &mut crate::WorkerLaunch,
    entry: &crate::manifest::ActiveBinary,
) -> crate::SuiteResult;
```

- [ ] **Step 2: Run failing conformance compile**

Run:

```bash
cargo test -p voom-conformance typed_suite --all-features
cargo test -p voom-conformance raw_wire_suite --all-features
```

Expected: compile failure until suite signatures and callers are updated.

- [ ] **Step 3: Update typed suite**

Rules:

- `handshake_*`, auth, identity, progress, terminal, and idempotency checks use `entry.operations[0].valid_payload`.
- invalid-payload check uses `entry.operations[0].invalid_payload`.
- operation case loop runs the happy-path progress/terminal assertions for every `entry.operations` item.
- `unknown_operation_rejected` chooses the first operation from `OperationKind::ALL` that does not appear in `entry.operations`.
- assertion names include the binary and operation, for example `fake-transcoder::transcode_video::progress_seq_starts_at_zero`.

- [ ] **Step 4: Update raw-wire suite**

Rules:

- `operation_body` accepts `OperationKind` and `serde_json::Value`.
- golden operation, idempotency replay, and idempotency conflict use `entry.operations[0].valid_payload`.
- malformed JSON, wrong content length, auth, route, and handshake tests remain independent of operation case shape.
- assertion names include the binary where the caller records failures.

- [ ] **Step 5: Update harness and integration caller**

Rules:

- `Harness::run_typed_suite` and `run_raw_wire_suite` accept `&ActiveBinary`.
- `Harness::run_all` accepts `&ActiveBinary`.
- `conformance_all.rs` continues to launch every currently active manifest entry.
- Full `validate_operation_coverage(&manifest)` integration remains deferred until Task 8, after all fake-provider operation cases are active.
- `stdin_eof_terminates_worker` remains per active entry.

- [ ] **Step 6: Verify and commit**

Run:

```bash
cargo test -p voom-conformance typed_suite --all-features
cargo test -p voom-conformance raw_wire_suite --all-features
cargo test -p voom-conformance --test conformance_all --all-features
```

Expected: tests pass for currently active echo, chaos, and benchmark entries because Task 5 already added their `ProbeFile` operation cases.

Commit:

```bash
git add crates/voom-conformance/src/harness.rs crates/voom-conformance/src/typed_suite.rs crates/voom-conformance/src/raw_wire_suite.rs crates/voom-conformance/tests/conformance_all.rs
git commit -m "feat(conformance): drive suites from manifest operations"
```

## Task 7: Failure Taxonomy Registry

**Files:**
- Create: `crates/voom-conformance/src/failure_taxonomy.rs`
- Create: `crates/voom-conformance/src/failure_taxonomy_test.rs`
- Modify: `crates/voom-conformance/src/lib.rs`
- Modify: `crates/voom-conformance/tests/conformance_all.rs`

- [ ] **Step 1: Write failing registry tests**

Create `failure_taxonomy.rs` with a sibling test module declaration and tests expecting:

```rust
pub struct FailureFixture {
    pub name: &'static str,
    pub class: voom_core::FailureClass,
    pub code: voom_core::ErrorCode,
    pub retry: voom_core::FailureRetryClass,
    pub source: FixtureSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureSource {
    FakeProviderErrorFrame,
    ChaosWorkerScenario,
    SyntheticFrame,
}

pub fn registry() -> &'static [FailureFixture];
pub fn validate_registry() -> Result<(), FailureTaxonomyError>;
pub async fn run() -> crate::SuiteResult;
```

Tests must assert:

- `validate_registry()` succeeds with the full registry.
- removing one class in a test-only helper fails with missing coverage.
- duplicating one class in a test-only helper fails with duplicate coverage.
- every fixture's `code` equals `class.into_error_code()`.
- every fixture's `retry` equals `class.retry_class()`.

- [ ] **Step 2: Run failing registry tests**

Run:

```bash
cargo test -p voom-conformance failure_taxonomy --all-features
```

Expected: compile failure until the module exists and is exported.

- [ ] **Step 3: Implement registry**

Add one fixture per `FailureClass::ALL` variant. Use names:

- `failure_taxonomy_worker_timeout`
- `failure_taxonomy_worker_crash`
- `failure_taxonomy_no_eligible_worker`
- `failure_taxonomy_artifact_unavailable`
- `failure_taxonomy_artifact_checksum_mismatch`
- `failure_taxonomy_external_system_unavailable`
- `failure_taxonomy_external_system_rate_limited`
- `failure_taxonomy_verification_failure`
- `failure_taxonomy_backup_failure`
- `failure_taxonomy_commit_failure`
- `failure_taxonomy_policy_parse_error`
- `failure_taxonomy_policy_validation_error`
- `failure_taxonomy_missing_capability`
- `failure_taxonomy_malformed_worker_result`
- `failure_taxonomy_user_cancellation`
- `failure_taxonomy_stale_identity_evidence`
- `failure_taxonomy_closure_resolution_incomplete`
- `failure_taxonomy_blocked_by_active_use_lease`
- `failure_taxonomy_approval_required`
- `failure_taxonomy_priority_policy_conflict`
- `failure_taxonomy_progress_timeout`
- `failure_taxonomy_ambiguous_worker_selection`

Use `FixtureSource::ChaosWorkerScenario` for `WorkerTimeout`, `WorkerCrash`, `MalformedWorkerResult`, and `ProgressTimeout`; use `FixtureSource::FakeProviderErrorFrame` for provider-like classes; use `FixtureSource::SyntheticFrame` for policy/scheduler/operator classes.

`run()` calls `validate_registry()` and records one passing assertion per fixture after checking class/code/retry mapping.

- [ ] **Step 4: Wire registry into conformance integration**

Update `lib.rs` with `pub mod failure_taxonomy;`.

Update `conformance_all.rs` after protocol-negative fixtures:

```rust
combined.extend(voom_conformance::failure_taxonomy::run().await);
```

- [ ] **Step 5: Verify and commit**

Run:

```bash
cargo test -p voom-conformance failure_taxonomy --all-features
cargo test -p voom-conformance --test conformance_all --all-features
```

Expected: failure taxonomy tests pass and conformance integration includes registry assertions.

Commit:

```bash
git add crates/voom-conformance/src/lib.rs crates/voom-conformance/src/failure_taxonomy.rs crates/voom-conformance/src/failure_taxonomy_test.rs crates/voom-conformance/tests/conformance_all.rs
git commit -m "feat(conformance): enforce failure taxonomy coverage"
```

## Task 8: Promote Fake Providers In Manifest

**Files:**
- Modify: `crates/voom-conformance/voom-fakes-manifest.toml`
- Modify: `crates/voom-conformance/tests/conformance_all.rs`

- [ ] **Step 1: Update conformance integration expectations**

In `conformance_all.rs`, assert all active entries exist:

```rust
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
```

Also assert none of the eleven fake-provider names appear in `manifest.scaffold`.

After these required-active and no-scaffold assertions, call `validate_operation_coverage(&manifest)` before launching workers. This is the first integration point where full fixed-operation coverage is expected to pass because Task 8 promotes all fake-provider operation cases.

- [ ] **Step 2: Run failing integration**

Run:

```bash
cargo test -p voom-conformance --test conformance_all --all-features
```

Expected: failure because fake providers are still scaffolded and full operation coverage is incomplete.

- [ ] **Step 3: Promote manifest entries**

Replace the scaffolded fake-provider entries with active `[[binaries]]` entries using the exact operation cases from the Provider Contract Decisions table. Keep the existing `echo-worker`, `chaos-worker`, and `benchmark-worker` active entries and their `ProbeFile` operation cases unchanged.

For fake providers, each active entry has:

```toml
[[binaries]]
name = "fake-scanner"
target = "fake-scanner"
purpose = "phase 6 scanner fake - deterministic library discovery"
status = "active"
required = true

[[binaries.operations]]
operation = "scan_library"
valid_payload = { path = "/library", scenario = "default" }
invalid_payload = { scenario = "missing_path" }
```

Use one `[[binaries.operations]]` entry per secondary operation for `fake-prober`, `fake-transcoder`, and `fake-backup-store`.

Leave `[scaffold].binaries = []`.

- [ ] **Step 4: Verify and commit**

Run:

```bash
cargo test -p voom-fakes --all-features
cargo test -p voom-conformance --all-features
```

Expected: all fake-provider and conformance tests pass with all providers active.

Commit:

```bash
git add crates/voom-conformance/voom-fakes-manifest.toml crates/voom-conformance/tests/conformance_all.rs
git commit -m "test(conformance): promote fake providers"
```

## Task 9: Final Phase 6 Verification

**Files:**
- No planned source edits unless verification exposes a bug in the Phase 6 files touched above.

- [ ] **Step 1: Run focused verification**

Run:

```bash
cargo test -p voom-core --all-features
cargo test -p voom-fake-support --all-features
cargo test -p voom-fakes --all-features
cargo test -p voom-conformance --all-features
```

Expected: all pass.

- [ ] **Step 2: Run branch verification**

Run:

```bash
just ci
```

Expected: full CI passes.

- [ ] **Step 3: Check formatting and whitespace**

Run:

```bash
cargo fmt --all -- --check
git diff --check
```

Expected: both pass with no output from `git diff --check`.

- [ ] **Step 4: Handle verification fixes**

If verification failed and required code changes, return to the task that owns the failed subsystem, make the smallest fix there, commit it with that task's commit style, and rerun Task 9 from Step 1.

If no fixes were needed, do not create an empty commit.

## Self-Review Notes

- Spec coverage: Tasks 3 and 4 implement fake providers; Task 5 defines manifest operation coverage validation; Task 6 makes typed/raw suites consume per-entry operation cases; Task 7 implements `FailureClass` registry coverage; Task 8 activates all providers, enforces no scaffolds, and integrates full operation coverage; Task 9 verifies Phase 6.
- Phase 7 boundary: This plan intentionally does not add scanner-to-prober orchestration through the real scheduler, supervisor-side chaos recovery, or scheduler throughput reporting.
- Type consistency: `OperationKind::ALL`, `OperationCase`, `ActiveBinary.operations`, `FailureClass::ALL`, and `FailureFixture` names are used consistently across tasks.
