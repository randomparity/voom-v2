# VOOM Sprint 10 Real Ingest FFprobe Worker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement explicit-path media scanning that hashes real files, probes them through an out-of-process `ffprobe` worker, and atomically records file identity rows plus media snapshots.

**Architecture:** Keep CLI parsing and JSON envelopes in `voom-cli`, scan orchestration and transaction boundaries in `voom-control-plane`, identity persistence in existing `voom-store` repositories, typed wire payloads in `voom-worker-protocol`, and real `ffprobe` execution in a new out-of-process worker crate. The control plane may read files for deterministic stat/hash collection, but only the worker process may invoke `ffprobe`.

**Tech Stack:** Rust, Tokio, sqlx/SQLite, serde/serde_json, BLAKE3, existing HTTP/NDJSON worker protocol, sibling unit tests, insta CLI snapshots, checked-in ffprobe JSON fixtures, small checked-in fixture media, `just` verification commands.

---

## Success Criteria

- `voom scan --path <file-or-dir>` emits exactly one JSON envelope.
- Explicit files and recursively discovered directory media are canonicalized, filtered by the Sprint 10 allowlist, hashed with `blake3:<hex>`, and scanned in deterministic path order.
- `voom scan` launches a separate `voom-ffprobe-worker` process over the existing worker protocol and dispatches `probe_file`; no in-process `ffprobe` shortcut exists.
- A live durable worker row named `builtin.ffprobe` is reused across scans, has `probe_file` capability and grant, and its id is recorded in `media_snapshots.probed_by`.
- Successful files commit `file_assets`, `file_versions`, `file_locations`, and `media_snapshots` in one transaction after the worker's pre/post observations match the candidate hash and size.
- Failed selected media files cause the command to fail with a per-file failure payload that includes any earlier committed successes.
- Unsupported directory entries are reported as skipped; an unsupported explicit file is `BAD_ARGS`.
- Unit, integration, CLI snapshot, and real-`ffprobe` fixture tests cover the acceptance matrix.
- `docs/specs/voom-control-plane-design.md` and the Sprint 10 closeout document record the explicit-path scope and verification evidence.
- `just ci` passes.

## Assumptions And Decisions

- The new worker crate is named `voom-ffprobe-worker` and builds a binary with the same name. This keeps real media probing separate from fake-provider support and from the `voom` CLI binary.
- Worker binary resolution is deterministic: `VOOM_FFPROBE_WORKER_BIN` wins; otherwise the control plane looks for `voom-ffprobe-worker` beside the current executable. Tests pass an explicit binary path.
- The `ffprobe` executable path is also injectable with `VOOM_FFPROBE_BIN`, defaulting to `ffprobe`. This makes unavailable/non-zero/invalid-output tests deterministic without changing production behavior.
- If a worker row named `builtin.ffprobe` exists and is retired, scan fails with `CONFLICT`. Creating a new name would violate the stable identity requirement and hide operator state.
- Sprint 10 records best-effort local file proof only when a portable value is already available from the standard library. The plan leaves `proof` as `None` for `local_path` ingest because rename reconciliation is explicitly out of scope.
- Real fixture-media tests require `ffprobe` and fail loudly when it is absent. Pure normalization tests use checked-in JSON fixtures and do not require the binary.
- `voom scan` dispatches directly to the bundled worker rather than creating durable execution tickets. The existing worker protocol still requires a `LeaseId`, so scan creates a per-file nonzero ephemeral protocol id from a monotonic counter seeded above the durable id range used in tests. That id is only a stream correlation token for this direct dispatch path and is never written to the `leases` table.
- Worker launch and dispatch waits are bounded. Startup must fail if the worker does not print its bound address within 5 seconds, and dispatch must fail if no terminal result/error frame arrives within the request's progress idle deadline. Failure paths must kill and reap the child process.
- `ensure_builtin_ffprobe_worker_in_tx` runs before the worker process is launched or any `probe_file` request is dispatched. Persistence receives that preselected `WorkerId`; it must not create or switch worker rows after probing, because doing so would make `media_snapshots.probed_by` diverge from the actual worker process that produced the result.
- Worker-domain failures such as missing `ffprobe`, non-zero `ffprobe`, invalid ffprobe JSON, and content drift must be returned as NDJSON `ProgressFrame::Error` terminal frames in a successful `/v1/operations` response. The worker handler should reserve `Err(ProtocolError)` for protocol violations such as unsupported operation or invalid request payload, because the existing HTTP transport converts handler errors into non-stream HTTP errors.

## File Structure

- Modify `Cargo.toml`: add workspace member `crates/voom-ffprobe-worker`; do not add `voom-ffprobe-worker` as a dependency of `voom-control-plane` or `voom-cli`, because production must launch it out of process rather than link it in process.
- Create `crates/voom-ffprobe-worker/Cargo.toml`: worker crate manifest, one library, one binary.
- Create `crates/voom-ffprobe-worker/src/lib.rs`: exports worker handler, file observation, ffprobe invocation, and normalization modules.
- Create `crates/voom-ffprobe-worker/src/main.rs`: reads worker credentials/env, starts HTTP worker server, prints bound loopback address on stdout, exits on stdin close.
- Create `crates/voom-ffprobe-worker/src/observe.rs` and `observe_test.rs`: regular-file stat/hash observation used before and after `ffprobe`.
- Create `crates/voom-ffprobe-worker/src/ffprobe.rs` and `ffprobe_test.rs`: subprocess execution and error mapping.
- Create `crates/voom-ffprobe-worker/src/normalize.rs` and `normalize_test.rs`: parse raw ffprobe JSON into the Sprint 10 snapshot shape.
- Create `crates/voom-ffprobe-worker/tests/probe_worker.rs`: protocol-level worker tests for success and failure frames.
- Create `crates/voom-ffprobe-worker/fixtures/ffprobe/*.json`: raw JSON fixtures for normalization tests.
- Create `crates/voom-ffprobe-worker/fixtures/media/tiny.mp4`: small real media fixture for release verification.
- Modify `crates/voom-worker-protocol/src/lib.rs`: export typed Sprint 10 probe payloads.
- Create `crates/voom-worker-protocol/src/probe_file.rs` and `probe_file_test.rs`: request/result/fact/snapshot wire types.
- Modify `crates/voom-control-plane/Cargo.toml`: add `tokio` features `fs` and `process` to the existing dependency; no dependency on `voom-ffprobe-worker`.
- Create `crates/voom-control-plane/src/scan/mod.rs`: public scan input/output models and orchestration.
- Create `crates/voom-control-plane/src/scan/discovery.rs` and `discovery_test.rs`: explicit path validation, allowlist, canonicalization, recursion, skip/failure records.
- Create `crates/voom-control-plane/src/scan/hash.rs` and `hash_test.rs`: BLAKE3 hash and observed file facts.
- Create `crates/voom-control-plane/src/scan/bootstrap.rs` and `bootstrap_test.rs`: idempotent durable `builtin.ffprobe` worker row/capability/grant.
- Create `crates/voom-control-plane/src/scan/worker.rs` and `worker_test.rs`: child-process lifecycle, handshake, `probe_file` dispatch, terminal-frame handling.
- Create `crates/voom-control-plane/src/scan/persist.rs` and `persist_test.rs`: one-transaction identity plus snapshot persistence.
- Modify `crates/voom-control-plane/src/lib.rs`: export `scan_path` and include the `scan` module.
- Modify `crates/voom-store/src/repo/workers.rs`: add narrow `get_by_name_in_tx`, `get_by_name`, and capability/grant existence helpers if direct SQL would otherwise leak into the control plane.
- Modify `crates/voom-store/src/repo/workers_test.rs`: repository coverage for the new worker lookup/existence helpers.
- Modify `crates/voom-cli/src/cli.rs`: add top-level `scan --path <path>`.
- Create `crates/voom-cli/src/commands/scan.rs` and `scan_test.rs`: command mapping and envelope serialization.
- Modify `crates/voom-cli/src/commands/mod.rs`: export scan command.
- Modify `crates/voom-cli/src/main.rs`: dispatch scan and preserve `BAD_ARGS` vs runtime exit codes.
- Create `crates/voom-cli/tests/scan_envelope.rs`: CLI integration tests and insta snapshots for success, unsupported explicit file, unsupported directory skip, content drift, and worker failure.
- Create snapshot files under `crates/voom-cli/tests/snapshots/` with `cargo insta review` after intentional output review.
- Modify `docs/specs/voom-control-plane-design.md`: name Sprint 10 explicit-path scan, defer durable library roots, and document the bundled out-of-process ffprobe worker.
- Create `docs/superpowers/specs/2026-05-24-voom-sprint-10-closeout.md`: evidence matrix after implementation and verification.

## Task 1: Typed Probe Protocol Payloads

**Files:**
- Create: `crates/voom-worker-protocol/src/probe_file.rs`
- Create: `crates/voom-worker-protocol/src/probe_file_test.rs`
- Modify: `crates/voom-worker-protocol/src/lib.rs`

- [ ] **Step 1: Write failing protocol tests**

Add `crates/voom-worker-protocol/src/probe_file_test.rs`:

```rust
use super::*;

#[test]
fn probe_request_serializes_stable_snake_case_shape() {
    let req = ProbeFileRequest {
        path: "/media/movie.mkv".to_owned(),
        expected: ExpectedFileFacts {
            size_bytes: 12,
            content_hash: "blake3:012345".to_owned(),
            modified_at: Some("2026-05-24T00:00:00Z".to_owned()),
            local_file_key: Some("dev=1,ino=2".to_owned()),
        },
    };

    let json = serde_json::to_value(&req).unwrap();

    assert_eq!(json["path"], "/media/movie.mkv");
    assert_eq!(json["expected"]["size_bytes"], 12);
    assert_eq!(json["expected"]["content_hash"], "blake3:012345");
    assert_eq!(json["expected"]["modified_at"], "2026-05-24T00:00:00Z");
    assert_eq!(json["expected"]["local_file_key"], "dev=1,ino=2");
}

#[test]
fn probe_result_requires_known_status() {
    let err = serde_json::from_value::<ProbeFileResult>(serde_json::json!({
        "status": "made_up",
        "provider": "ffprobe",
        "provider_version": "7.0",
        "pre_probe": { "size_bytes": 1, "content_hash": "blake3:aa" },
        "post_probe": { "size_bytes": 1, "content_hash": "blake3:aa" },
        "snapshot": { "format": "sprint10-v1" }
    }))
    .unwrap_err();

    assert!(err.to_string().contains("unknown variant"));
}
```

- [ ] **Step 2: Run protocol tests and verify failure**

Run:

```bash
cargo test -p voom-worker-protocol probe_request_serializes_stable_snake_case_shape
cargo test -p voom-worker-protocol probe_result_requires_known_status
```

Expected: compile failure because `probe_file` types are not defined.

- [ ] **Step 3: Add typed request/result module**

Create `crates/voom-worker-protocol/src/probe_file.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedFileFacts {
    pub size_bytes: u64,
    pub content_hash: String,
    pub modified_at: Option<String>,
    pub local_file_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedFileFacts {
    pub size_bytes: u64,
    pub content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_file_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeFileRequest {
    pub path: String,
    pub expected: ExpectedFileFacts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeFileStatus {
    Probed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeFileResult {
    pub status: ProbeFileStatus,
    pub provider: String,
    pub provider_version: String,
    pub pre_probe: ObservedFileFacts,
    pub post_probe: ObservedFileFacts,
    pub snapshot: serde_json::Value,
}
```

Modify `crates/voom-worker-protocol/src/lib.rs`:

```rust
pub mod probe_file;

pub use probe_file::{
    ExpectedFileFacts, ObservedFileFacts, ProbeFileRequest, ProbeFileResult, ProbeFileStatus,
};
```

Append to `crates/voom-worker-protocol/src/probe_file.rs`:

```rust
#[cfg(test)]
#[path = "probe_file_test.rs"]
mod tests;
```

- [ ] **Step 4: Run protocol tests and commit**

Run:

```bash
cargo test -p voom-worker-protocol probe_file
cargo fmt --all
git add crates/voom-worker-protocol/src/lib.rs crates/voom-worker-protocol/src/probe_file.rs crates/voom-worker-protocol/src/probe_file_test.rs
git commit -m "feat(protocol): add probe file payloads"
```

Expected: protocol tests pass and formatting produces no diff after commit.

## Task 2: Control-Plane Discovery And Hashing

**Files:**
- Create: `crates/voom-control-plane/src/scan/mod.rs`
- Create: `crates/voom-control-plane/src/scan/discovery.rs`
- Create: `crates/voom-control-plane/src/scan/discovery_test.rs`
- Create: `crates/voom-control-plane/src/scan/hash.rs`
- Create: `crates/voom-control-plane/src/scan/hash_test.rs`
- Modify: `crates/voom-control-plane/src/lib.rs`

- [ ] **Step 1: Write failing discovery tests**

Add these tests to `crates/voom-control-plane/src/scan/discovery_test.rs`:

- `explicit_supported_file_is_single_candidate`: create `clip.MP4`, call `discover_path`, assert `ScanMode::File`, one candidate, no skipped files, and canonical absolute path.
- `directory_discovery_returns_supported_media_in_lexicographic_order`: use the code below.
- `unsupported_file_inside_directory_is_skipped`: create `notes.txt` next to `clip.mp4`, assert one candidate and one `SkippedUnsupportedExtension`.
- `unsupported_explicit_file_is_bad_args`: call `discover_path` on `notes.txt`, assert the returned error code is `ErrorCode::BadArgs`.
- `explicit_symlink_is_rejected_before_canonicalization`: create a symlink pointing at a supported file, call `discover_path` on the symlink, assert `ErrorCode::BadArgs`.
- `directory_walk_does_not_traverse_symlinked_directory`: create `root/link` pointing to an outside directory containing `outside.mp4`, scan `root`, assert no candidate path contains `outside.mp4` and one skipped entry records symlink rejection.

Use this core assertion shape for the ordering test:

```rust
use super::*;

#[tokio::test]
async fn directory_discovery_returns_supported_media_in_lexicographic_order() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("b")).unwrap();
    std::fs::create_dir(dir.path().join("a")).unwrap();
    std::fs::write(dir.path().join("b").join("z.mkv"), b"z").unwrap();
    std::fs::write(dir.path().join("a").join("a.mp4"), b"a").unwrap();
    std::fs::write(dir.path().join("a").join("notes.txt"), b"skip").unwrap();

    let discovered = discover_path(dir.path()).await.unwrap();

    assert_eq!(discovered.mode, ScanMode::Directory);
    let names: Vec<_> = discovered
        .candidates
        .iter()
        .map(|candidate| candidate.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["a.mp4", "z.mkv"]);
    assert_eq!(discovered.skipped.len(), 1);
    assert_eq!(
        discovered.skipped[0].status,
        FileScanStatus::SkippedUnsupportedExtension
    );
}
```

- [ ] **Step 2: Write failing hash tests**

Add `crates/voom-control-plane/src/scan/hash_test.rs`:

```rust
use super::*;

#[tokio::test]
async fn observed_file_facts_use_blake3_prefix_and_size() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clip.mp4");
    std::fs::write(&path, b"voom").unwrap();

    let observed = observe_candidate_file(&path).await.unwrap();

    assert_eq!(observed.size_bytes, 4);
    assert!(observed.content_hash.starts_with("blake3:"));
    assert_eq!(observed.content_hash.len(), "blake3:".len() + 64);
    assert!(observed.modified_at.is_some());
}
```

- [ ] **Step 3: Run tests and verify failure**

Run:

```bash
cargo test -p voom-control-plane scan::discovery
cargo test -p voom-control-plane scan::hash
```

Expected: compile failure because the scan modules do not exist.

- [ ] **Step 4: Implement discovery and hashing modules**

Implement `ScanMode`, `FileScanStatus`, `DiscoveredScan`, `ScanCandidate`, `SkippedFile`, `discover_path`, `is_supported_media_path`, and `observe_candidate_file`. Key rules:

```rust
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "avi", "m2ts", "m4v", "mkv", "mov", "mp4", "mpeg", "mpg", "ts", "webm",
];

fn extension_key(path: &std::path::Path) -> Option<String> {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase)
}

pub fn is_supported_media_path(path: &std::path::Path) -> bool {
    extension_key(path)
        .as_deref()
        .is_some_and(|ext| SUPPORTED_EXTENSIONS.contains(&ext))
}
```

Use `tokio::fs::symlink_metadata` before canonicalizing the explicit path so explicit symlinks are rejected. During directory traversal, use `symlink_metadata` for each entry and skip symlinked files/directories rather than following them. Sort paths by their normalized string form before returning candidates.

For hashing, use async file reads with `tokio::fs::File` and `tokio::io::AsyncReadExt`, feed a `blake3::Hasher`, and return `content_hash = format!("blake3:{hex}")`.

- [ ] **Step 5: Export scan module and run tests**

Modify `crates/voom-control-plane/src/lib.rs`:

```rust
pub mod scan;
```

Run:

```bash
cargo test -p voom-control-plane scan::discovery
cargo test -p voom-control-plane scan::hash
cargo fmt --all
git add crates/voom-control-plane/src/lib.rs crates/voom-control-plane/src/scan
git commit -m "feat(control-plane): discover and hash scan inputs"
```

Expected: discovery and hashing tests pass.

## Task 3: FFprobe Worker Normalization And Observation

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/voom-ffprobe-worker/Cargo.toml`
- Create: `crates/voom-ffprobe-worker/src/lib.rs`
- Create: `crates/voom-ffprobe-worker/src/observe.rs`
- Create: `crates/voom-ffprobe-worker/src/observe_test.rs`
- Create: `crates/voom-ffprobe-worker/src/normalize.rs`
- Create: `crates/voom-ffprobe-worker/src/normalize_test.rs`
- Create: `crates/voom-ffprobe-worker/fixtures/ffprobe/basic-mp4.json`

- [ ] **Step 1: Scaffold crate and fixture**

Add workspace member `crates/voom-ffprobe-worker` and crate manifest with dependencies on `voom-core`, `voom-worker-protocol`, `blake3`, `chrono`, `secrecy`, `serde`, `serde_json`, `thiserror`, and `tokio` with `fs`, `io-util`, `process`, `rt-multi-thread`, `macros`, `net`, and `sync` features. Add dev-dependency `tempfile`. Do not add this crate to `[workspace.dependencies]` unless another crate needs it for tests only; production crates must not import it.

Create `basic-mp4.json` with a compact ffprobe-like object:

```json
{
  "format": {
    "format_name": "mov,mp4,m4a,3gp,3g2,mj2",
    "format_long_name": "QuickTime / MOV",
    "duration": "1.000000",
    "bit_rate": "128000"
  },
  "streams": [
    {
      "index": 0,
      "codec_type": "video",
      "codec_name": "h264",
      "width": 320,
      "height": 180,
      "duration": "1.000000",
      "avg_frame_rate": "30/1"
    },
    {
      "index": 1,
      "codec_type": "audio",
      "codec_name": "aac",
      "duration": "1.000000",
      "sample_rate": "48000",
      "channels": 2
    }
  ]
}
```

- [ ] **Step 2: Write failing normalization tests**

Add `normalize_test.rs`:

```rust
use super::*;

#[test]
fn normalizes_ffprobe_json_into_sprint10_snapshot() {
    let raw = serde_json::from_str(include_str!("../fixtures/ffprobe/basic-mp4.json")).unwrap();

    let snapshot = normalize_ffprobe_json(&raw, "7.0", "2026-05-24T00:00:00Z").unwrap();

    assert_eq!(snapshot["format"], "sprint10-v1");
    assert_eq!(snapshot["probe"]["provider"], "ffprobe");
    assert_eq!(snapshot["probe"]["provider_version"], "7.0");
    assert_eq!(snapshot["container"]["duration_seconds"], 1.0);
    assert_eq!(snapshot["container"]["bit_rate"], 128000);
    assert_eq!(snapshot["streams"][0]["kind"], "video");
    assert_eq!(snapshot["streams"][0]["avg_frame_rate"], "30/1");
    assert_eq!(snapshot["streams"][1]["kind"], "audio");
    assert!(snapshot["raw"]["ffprobe_json"].is_object());
}

#[test]
fn rejects_non_numeric_duration() {
    let raw = serde_json::json!({
        "format": { "duration": "not-a-number" },
        "streams": []
    });

    let err = normalize_ffprobe_json(&raw, "7.0", "2026-05-24T00:00:00Z").unwrap_err();

    assert_eq!(err.failure_class(), voom_core::FailureClass::MalformedWorkerResult);
}
```

- [ ] **Step 3: Write failing observation tests**

Add `observe_test.rs`:

```rust
use super::*;

#[tokio::test]
async fn observe_file_facts_returns_regular_file_hash_and_size() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clip.mp4");
    std::fs::write(&path, b"voom").unwrap();

    let observed = observe_file_facts(&path).await.unwrap();

    assert_eq!(observed.size_bytes, 4);
    assert!(observed.content_hash.starts_with("blake3:"));
    assert_eq!(observed.content_hash.len(), "blake3:".len() + 64);
    assert!(observed.modified_at.is_some());
}

#[tokio::test]
async fn observe_file_facts_rejects_directory() {
    let dir = tempfile::tempdir().unwrap();

    let err = observe_file_facts(dir.path()).await.unwrap_err();

    assert_eq!(err.failure_class(), voom_core::FailureClass::ArtifactUnavailable);
}
```

- [ ] **Step 4: Implement normalization and observation**

Implement `WorkerError` with a `failure_class()` method returning `FailureClass`. Implement `normalize_ffprobe_json` so unknown values are omitted, not replaced with sentinel strings. Reject numeric strings that fail parsing or overflow `u64`. Preserve `raw.ffprobe_json` exactly.

- [ ] **Step 5: Run tests and commit**

Run:

```bash
cargo test -p voom-ffprobe-worker normalize
cargo test -p voom-ffprobe-worker observe
cargo fmt --all
git add Cargo.toml crates/voom-ffprobe-worker
git commit -m "feat(worker): normalize ffprobe snapshots"
```

Expected: new worker crate unit tests pass.

## Task 4: Real FFprobe Worker Binary

**Files:**
- Create: `crates/voom-ffprobe-worker/src/ffprobe.rs`
- Create: `crates/voom-ffprobe-worker/src/ffprobe_test.rs`
- Create: `crates/voom-ffprobe-worker/src/main.rs`
- Modify: `crates/voom-ffprobe-worker/src/lib.rs`
- Create: `crates/voom-ffprobe-worker/tests/probe_worker.rs`

- [ ] **Step 1: Write failing subprocess tests**

In `ffprobe_test.rs`, test that an explicit nonexistent ffprobe path maps to `ExternalSystemUnavailable`, and that a helper process producing invalid JSON maps to `MalformedWorkerResult`. Use `VOOM_FFPROBE_BIN` in tests instead of modifying `PATH`.

- [ ] **Step 2: Write failing protocol worker tests**

In `tests/probe_worker.rs`, start the worker server in-process with a temporary media file and a fake ffprobe command only for protocol failure cases. Assert missing `ffprobe`, non-zero `ffprobe`, invalid JSON, and content-drift cases return HTTP-success NDJSON streams whose terminal frame is `ProgressFrame::Error` with the expected `FailureClass` and `ErrorCode`; do not accept transport-level HTTP errors for worker-domain failures. The success test may use real `ffprobe` and must return a clear failure when `ffprobe` is absent:

```rust
fn require_ffprobe() {
    let status = std::process::Command::new("ffprobe")
        .arg("-version")
        .status()
        .expect("release verification requires ffprobe on PATH");
    assert!(status.success(), "release verification requires working ffprobe");
}
```

- [ ] **Step 3: Implement ffprobe invocation**

Invoke:

```bash
ffprobe -v error -print_format json -show_format -show_streams <path>
```

Use `tokio::process::Command`. Map:

- spawn failure and non-zero exit to `FailureClass::ExternalSystemUnavailable` / `ErrorCode::ExternalSystemUnavailable`;
- invalid JSON and normalization failure to `FailureClass::MalformedWorkerResult` / `ErrorCode::MalformedWorkerResult`;
- pre/post observation mismatch to `FailureClass::ArtifactChecksumMismatch` / `ErrorCode::ArtifactChecksumMismatch`.

- [ ] **Step 4: Implement HTTP operation handler and binary**

The handler must:

1. Deserialize `ProbeFileRequest` from `OperationRequest.payload`.
2. Verify `OperationKind::ProbeFile`.
3. Observe expected size/hash before ffprobe.
4. Run ffprobe and normalize JSON.
5. Observe expected size/hash after ffprobe.
6. Return one progress frame and one `ProbeFileResult` terminal result on success.
7. Return a terminal error frame with failure class, error code, message, and payload on worker-domain failures by building an `OperationDispatch` whose body contains `ProgressFrame::Error`. Do not return `Err(ProtocolError)` for missing `ffprobe`, non-zero `ffprobe`, invalid ffprobe JSON, or content drift.

The binary must read `VOOM_WORKER_ID`, `VOOM_WORKER_EPOCH`, `VOOM_WORKER_SECRET`, and optional `VOOM_WORKER_BIND`, start `HttpServer`, print the bound address to stdout, and shut down when stdin closes.

- [ ] **Step 5: Run worker tests and commit**

Run:

```bash
cargo test -p voom-ffprobe-worker
cargo fmt --all
git add crates/voom-ffprobe-worker
git commit -m "feat(worker): add ffprobe protocol worker"
```

Expected: worker unit and protocol tests pass in an environment with `ffprobe`.

## Task 5: Durable Built-In Worker Bootstrap

**Files:**
- Modify: `crates/voom-store/src/repo/workers.rs`
- Create or modify: `crates/voom-store/src/repo/workers_test.rs`
- Create: `crates/voom-control-plane/src/scan/bootstrap.rs`
- Create: `crates/voom-control-plane/src/scan/bootstrap_test.rs`
- Modify: `crates/voom-control-plane/src/scan/mod.rs`

- [ ] **Step 1: Write failing store helper tests**

Test `get_by_name` returns the seeded worker and returns `None` for a missing name. Test capability/grant existence helpers for operation `probe_file`.

- [ ] **Step 2: Implement narrow worker repo helpers**

Add trait methods:

```rust
async fn get_by_name_in_tx<'tx>(
    &self,
    tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
    name: &str,
) -> Result<Option<Worker>, VoomError>;

async fn get_by_name(&self, name: &str) -> Result<Option<Worker>, VoomError>;

async fn has_capability_in_tx<'tx>(
    &self,
    tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
    worker_id: WorkerId,
    operation: &str,
) -> Result<bool, VoomError>;

async fn has_execute_grant_in_tx<'tx>(
    &self,
    tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
    worker_id: WorkerId,
    operation: &str,
) -> Result<bool, VoomError>;
```

- [ ] **Step 3: Write failing bootstrap tests**

`ensure_builtin_ffprobe_worker_reuses_existing_live_row` should call the bootstrap twice and assert the same `WorkerId` and a single row named `builtin.ffprobe`. `retired_builtin_ffprobe_worker_fails_loudly` should retire the row and assert `VoomError::Conflict`.

- [ ] **Step 4: Implement bootstrap**

Implement `ensure_builtin_ffprobe_worker_in_tx` in control-plane scan bootstrap:

- create `NewWorker { name: "builtin.ffprobe", kind: WorkerKind::Local, node_id: None }` when missing;
- reject retired existing row with `VoomError::Conflict`;
- insert `probe_file` capability only if absent;
- insert execute grant only if absent;
- leave remote node registration untouched.

- [ ] **Step 5: Run tests and commit**

Run:

```bash
cargo test -p voom-store workers::get_by_name
cargo test -p voom-control-plane scan::bootstrap
cargo fmt --all
git add crates/voom-store/src/repo/workers.rs crates/voom-store/src/repo/workers_test.rs crates/voom-control-plane/src/scan/bootstrap.rs crates/voom-control-plane/src/scan/bootstrap_test.rs crates/voom-control-plane/src/scan/mod.rs
git commit -m "feat(control-plane): bootstrap builtin ffprobe worker"
```

Expected: bootstrap is idempotent and retired stable-name conflicts are loud.

## Task 6: Worker Launch And Dispatch Client

**Files:**
- Create: `crates/voom-control-plane/src/scan/worker.rs`
- Create: `crates/voom-control-plane/src/scan/worker_test.rs`
- Modify: `crates/voom-control-plane/src/scan/mod.rs`
- Modify: `crates/voom-control-plane/Cargo.toml`

- [ ] **Step 1: Write failing launch/dispatch tests**

Write tests that start a small protocol test worker process with a caller-supplied `WorkerId`, read the bound address from stdout, perform handshake, dispatch `ProbeFileRequest`, and assert a terminal `ProbeFileResult`. Also test that the presented worker id is the durable id passed by orchestration, that a worker terminal error becomes a scan worker error carrying `failure_class` and `error_code`, that two consecutive dispatches use distinct nonzero protocol lease ids, and that a child process that never prints its bound address fails within the startup timeout and is reaped.

- [ ] **Step 2: Implement child launch**

Implement:

```rust
pub struct BundledWorkerProcess {
    pub worker_id: voom_core::WorkerId,
    pub credentials: voom_worker_protocol::WorkerCredentials,
    pub client: voom_worker_protocol::HttpClient,
    child: tokio::process::Child,
}
```

Use the durable `WorkerId` returned by bootstrap and random test-safe credentials generated in control plane memory for the child process environment. Read exactly one stdout line for the bound address with `tokio::time::timeout(Duration::from_secs(5), ...)`; if the timeout fires or the line is not a socket address, kill and wait on the child before returning the launch error. Keep stdin piped so a normal drop requests worker shutdown, and implement `Drop`/explicit shutdown so abnormal paths do not leave worker processes running.

- [ ] **Step 3: Implement dispatch**

Build `OperationRequest { operation: OperationKind::ProbeFile, lease_id, payload, heartbeat_deadline_ms: 30_000, progress_idle_deadline_ms: 30_000 }` with a fresh nonzero ephemeral `LeaseId` per file. Use a fresh idempotency key from a random 128-bit hex string. Consume frames until terminal under a timeout equal to `progress_idle_deadline_ms`; reset the idle timer when a progress frame arrives. Decode result into `ProbeFileResult`; map `ProgressFrame::Error` to scan failure. If the stream ends before a terminal frame or the idle timeout fires, return a worker dispatch failure and shut down the child.

- [ ] **Step 4: Run tests and commit**

Run:

```bash
cargo test -p voom-control-plane scan::worker
cargo fmt --all
git add crates/voom-control-plane/Cargo.toml crates/voom-control-plane/src/scan/worker.rs crates/voom-control-plane/src/scan/worker_test.rs crates/voom-control-plane/src/scan/mod.rs
git commit -m "feat(control-plane): dispatch bundled ffprobe worker"
```

Expected: launch/dispatch tests pass and no worker process remains running after tests.

## Task 7: Atomic Scan Persistence

**Files:**
- Create: `crates/voom-control-plane/src/scan/persist.rs`
- Create: `crates/voom-control-plane/src/scan/persist_test.rs`
- Modify: `crates/voom-control-plane/src/scan/mod.rs`

- [ ] **Step 1: Write failing persistence tests**

Test successful persistence inserts one file asset, one file version, one local path location, and one media snapshot in a single transaction using the `WorkerId` passed into persistence. Test content drift skips persistence for the failing file and returns `failed_content_drift`. Test persistence rejects a missing or retired `WorkerId` instead of creating a replacement worker row after probing.

- [ ] **Step 2: Implement content consistency gate**

Implement a pure function:

```rust
fn verify_probe_facts(
    candidate: &ObservedCandidateFacts,
    result: &voom_worker_protocol::ProbeFileResult,
) -> Result<(), ScanFileError>
```

It must require `pre_probe.size_bytes`, `post_probe.size_bytes`, `pre_probe.content_hash`, and `post_probe.content_hash` to all match the candidate facts. On mismatch return `FileScanStatus::FailedContentDrift`, `ErrorCode::ArtifactChecksumMismatch`, `FailureClass::ArtifactChecksumMismatch`, and message `file changed between hashing and probing`.

- [ ] **Step 3: Implement transaction persistence**

Inside one `pool.begin()` transaction:

1. Re-read the provided `worker_id` in the same transaction and reject missing or retired rows with `VoomError::Conflict`.
2. Call `IdentityRepo::record_discovered_file_in_tx` with `FileLocationKind::LocalPath`, canonical path value, `content_hash`, `size_bytes`, current clock time, and `proof: None`.
3. Convert `IngestOutcome` to `file_asset_id`, `file_version_id`, and `file_location_id`.
4. Call `record_media_snapshot_in_tx` with `probed_by: Some(worker_id)` and the normalized snapshot payload.
5. Commit and return row ids.

- [ ] **Step 4: Run tests and commit**

Run:

```bash
cargo test -p voom-control-plane scan::persist
cargo fmt --all
git add crates/voom-control-plane/src/scan/persist.rs crates/voom-control-plane/src/scan/persist_test.rs crates/voom-control-plane/src/scan/mod.rs
git commit -m "feat(control-plane): persist scanned media snapshots"
```

Expected: persistence tests pass and content drift does not record a snapshot.

## Task 8: Scan Orchestration And CLI Envelope

**Files:**
- Modify: `crates/voom-control-plane/src/scan/mod.rs`
- Create: `crates/voom-control-plane/src/scan/mod_test.rs`
- Modify: `crates/voom-cli/src/cli.rs`
- Create: `crates/voom-cli/src/commands/scan.rs`
- Create: `crates/voom-cli/src/commands/scan_test.rs`
- Modify: `crates/voom-cli/src/commands/mod.rs`
- Modify: `crates/voom-cli/src/main.rs`

- [ ] **Step 1: Write failing orchestration tests**

Test directory scans summarize discovered, ingested, probed, snapshots recorded, skipped, and failed counts. Test failure after a prior committed file includes both committed success and failing file in the returned scan output. Test orchestration bootstraps `builtin.ffprobe` before launching the worker process and passes that same `WorkerId` through dispatch and persistence.

- [ ] **Step 2: Implement `ControlPlane::scan_path`**

Implement a public method:

```rust
pub async fn scan_path(&self, input: ScanPathInput) -> Result<ScanReport, ScanCommandError>
```

`ScanCommandError` should carry `ScanReport` so CLI error envelopes can include partial successes. Process candidates in deterministic order. At the start of scan, open a short transaction that calls `ensure_builtin_ffprobe_worker_in_tx`, commit it, then launch the worker process with that durable `WorkerId`. For each successful worker result, pass the same `WorkerId` into persistence. For selected media file failures, stop scanning and return the report with `status = error`.

- [ ] **Step 3: Write failing CLI command tests**

Add command-level tests that verify `ScanData` serializes to the spec shape, including `path`, `mode`, `summary`, `files`, `skipped`, row ids, `content_hash`, `size_bytes`, `probe_worker_id`, and failure `error` objects. Use these DTO field names:

```rust
#[derive(Debug, serde::Serialize)]
pub struct ScanData {
    pub path: String,
    pub mode: String,
    pub summary: ScanSummaryData,
    pub files: Vec<ScanFileData>,
    pub skipped: Vec<ScanFileData>,
}

#[derive(Debug, serde::Serialize)]
pub struct ScanSummaryData {
    pub discovered: u64,
    pub ingested: u64,
    pub probed: u64,
    pub snapshots_recorded: u64,
    pub skipped: u64,
    pub failed: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct ScanFileData {
    pub path: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_asset_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_version_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_location_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_snapshot_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe_worker_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ScanFileErrorData>,
}

#[derive(Debug, serde::Serialize)]
pub struct ScanFileErrorData {
    pub code: &'static str,
    pub failure_class: String,
    pub message: String,
}
```

Status strings must be exactly `scanned`, `skipped_unsupported_extension`, `failed_content_drift`, or `failed`. Populate `ScanFileErrorData.code` with `ErrorCode::as_str()`. Populate `ScanFileErrorData.failure_class` through serde, not `Debug`, so `FailureClass::ArtifactChecksumMismatch` becomes `artifact_checksum_mismatch`:

```rust
fn failure_class_wire(class: voom_core::FailureClass) -> String {
    serde_json::to_value(class)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "malformed_worker_result".to_owned())
}
```

- [ ] **Step 4: Implement CLI command and dispatch**

Add:

```rust
Scan {
    #[arg(long)]
    path: PathBuf,
}
```

Route `Command::Scan { path }` to `scan::run(&cfg.database_url, local, &path).await`. Return exit `1` for `BAD_ARGS` explicit-path validation and exit `2` for runtime scan failures. Use existing `emit_ok` for success and `emit_err_with_data` for scan failures so the error envelope's `data` field carries the partial `ScanData` with committed successes plus the failing file.

- [ ] **Step 5: Run tests and commit**

Run:

```bash
cargo test -p voom-control-plane scan::mod_test
cargo test -p voom-cli commands::scan
cargo fmt --all
git add crates/voom-control-plane/src/scan/mod.rs crates/voom-control-plane/src/scan/mod_test.rs crates/voom-cli/src/cli.rs crates/voom-cli/src/commands/scan.rs crates/voom-cli/src/commands/scan_test.rs crates/voom-cli/src/commands/mod.rs crates/voom-cli/src/main.rs
git commit -m "feat(cli): add explicit path scan command"
```

Expected: command tests pass and scan error paths still emit one JSON envelope.

## Task 9: CLI Integration Snapshots And Real Fixture Media

**Files:**
- Create: `crates/voom-cli/tests/scan_envelope.rs`
- Add snapshots under: `crates/voom-cli/tests/snapshots/`
- Create: `crates/voom-ffprobe-worker/fixtures/media/tiny.mp4`

- [ ] **Step 1: Add small fixture media**

Add a tiny valid media file under `crates/voom-ffprobe-worker/fixtures/media/tiny.mp4`. Verify locally:

```bash
ffprobe -v error -print_format json -show_format -show_streams crates/voom-ffprobe-worker/fixtures/media/tiny.mp4 >/tmp/voom-tiny-ffprobe.json
```

Expected: command exits `0` and writes valid JSON.

- [ ] **Step 2: Write failing integration tests**

Cover:

- `scan_file_success_outputs_envelope_and_persists_snapshot`;
- `scan_directory_reports_unsupported_entries_as_skipped`;
- `scan_unsupported_explicit_file_is_bad_args`;
- `scan_reuses_builtin_ffprobe_worker_row`;
- `scan_content_drift_fails_without_snapshot`.

Each test should initialize an on-disk SQLite DB with `voom_store::init`, run `env!("CARGO_BIN_EXE_voom")`, build the worker binary with `cargo build -p voom-ffprobe-worker --bin voom-ffprobe-worker` when the test starts, set `VOOM_FFPROBE_WORKER_BIN` to the built binary under Cargo's target directory, redact local paths/db URLs before snapshots, and assert row counts directly with sqlx when persistence is part of the contract. Do not use `env!("CARGO_BIN_EXE_voom-ffprobe-worker")` from `voom-cli` integration tests; Cargo only guarantees `CARGO_BIN_EXE_*` for binaries in the package currently being tested.

- [ ] **Step 3: Run integration tests and accept snapshots**

Run:

```bash
cargo test -p voom-cli --test scan_envelope
cargo insta review
```

Expected: tests pass after intentional snapshot review.

- [ ] **Step 4: Commit snapshots**

Run:

```bash
git add crates/voom-cli/tests/scan_envelope.rs crates/voom-cli/tests/snapshots crates/voom-ffprobe-worker/fixtures/media/tiny.mp4
git commit -m "test(cli): lock scan envelope shape"
```

Expected: scan snapshots are committed.

## Task 10: Architecture Docs And Closeout

**Files:**
- Modify: `docs/specs/voom-control-plane-design.md`
- Create: `docs/superpowers/specs/2026-05-24-voom-sprint-10-closeout.md`

- [ ] **Step 1: Update architecture spec**

Add a Sprint 10 note stating:

```markdown
Sprint 10 scan is explicit-path only: `voom scan --path <file-or-dir>`.
Durable library roots, scheduled scans, watch loops, and policy-driven scan
selection are deferred until after explicit ingest proves the identity and
provider boundary. Media probing for this path is performed by the bundled
out-of-process `builtin.ffprobe` worker; the control plane may hash local
bytes but must not invoke `ffprobe` in-process.
```

- [ ] **Step 2: Create closeout matrix**

Record a table with rows for scan command, discovery/hash, worker boundary, worker bootstrap reuse, snapshot persistence, skipped unsupported files, failure envelope, CLI snapshots, real ffprobe fixture, docs, and `just ci`. Include exact commands and observed results.

- [ ] **Step 3: Run doc checks**

Run:

```bash
rg -n "T[B]D|T[O]DO|place[Hh]older|INCOMPLETE" docs/specs/voom-control-plane-design.md docs/superpowers/specs/2026-05-24-voom-sprint-10-closeout.md
cargo test -p voom-cli --test scan_envelope
just ci
```

Expected: incomplete-marker scan returns no matches from Sprint 10 docs, CLI scan integration passes, and `just ci` passes.

- [ ] **Step 4: Commit docs**

Run:

```bash
git add docs/specs/voom-control-plane-design.md docs/superpowers/specs/2026-05-24-voom-sprint-10-closeout.md
git commit -m "docs: close out sprint 10 real ingest"
```

Expected: docs commit succeeds after verification.

## Final Verification

Run:

```bash
just ci
```

Expected: `fmt-check`, `lint`, `check-test-layout`, `test`, `doc`, `deny`, and `audit` all pass.

Run a manual smoke scan:

```bash
workdir=$(mktemp -d -t voom-s10.XXXXXX)
db="$workdir/voom.db"
cargo run -q -p voom-cli -- --database-url "sqlite://$db" init
VOOM_FFPROBE_WORKER_BIN="$(cargo metadata --format-version 1 --no-deps | jq -r '.target_directory')/debug/voom-ffprobe-worker" \
  cargo run -q -p voom-cli -- --database-url "sqlite://$db" scan --path crates/voom-ffprobe-worker/fixtures/media/tiny.mp4
```

Expected: one ok JSON envelope with `summary.ingested = 1`, `summary.probed = 1`, `summary.snapshots_recorded = 1`, and `files[0].probe_worker_id` set.
