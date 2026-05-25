# VOOM Sprint 12 FFmpeg Transcode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement policy-driven `transcode video to hevc {}` from DSL compile through planning, durable ticket execution, bundled FFmpeg worker staging, verification, add-only commit, committed-result probing, and CLI reporting.

**Architecture:** Add one typed operation, `transcode_video`, and keep FFmpeg as an out-of-process worker that writes only to a staging path. The control plane owns source selection, path hardening, artifact rows, verification, add-only commit, result scan/probe persistence, lineage, and events by reusing Sprint 11 artifact commit primitives.

**Tech Stack:** Rust workspace, tokio, sqlx/SQLite, serde JSON, axum-style worker protocol client/server helpers, FFmpeg/ffprobe binaries, insta snapshots, `just` command runner.

---

## Assumptions And Success Criteria

Assumptions:

- Sprint 12 can add a new crate named `voom-ffmpeg-worker` because the existing `voom-ffprobe-worker` and `voom-verify-artifact-worker` establish the bundled-worker pattern.
- The existing `OperationKind::TranscodeVideo` wire name is already correct (`transcode_video`), so the worker protocol should add payload structs rather than a new operation variant.
- Planning media-shape checks must use the current `MediaSnapshotInput` plus `stream_summary`; if `stream_summary` lacks stream cardinality, add helper parsing in `voom-plan` and block instead of guessing.
- Control-plane execution needs one new use-case module instead of expanding Sprint 11 stage-copy, because transcode staging dispatch has different source, worker, verification, commit, and result-probe phases.
- If a fixture cannot be represented with committed binary media in git, generate tiny media fixtures during tests with FFmpeg and fail clearly when FFmpeg is absent.

Success criteria:

- `transcode video to hevc {}` compiles into a typed `CompiledOperation::TranscodeVideo`.
- Planner produces planned, no-op, and blocked `transcode_video` nodes with stable diagnostics.
- Compliance execution submits `transcode_video` tickets instead of returning unsupported execution.
- A real FFmpeg worker produces staged HEVC MKV bytes and validates output facts with ffprobe.
- Control plane verifies, commits, scans/probes, and reports the result IDs.
- Representative failures are visible through stable error codes and JSON envelopes.
- `just ci` passes.

## File Map

Policy:

- Modify `crates/voom-policy/src/compiled.rs`: add `CompiledOperation::TranscodeVideo { target_codec, container, profile }` and lower accepted statements.
- Modify `crates/voom-policy/src/validate.rs`: accept only `transcode video to hevc` with no profile or arguments; reject unsupported shapes with existing policy diagnostics or a new stable diagnostic where needed.
- Modify tests in `crates/voom-policy/src/validate_test.rs`, `compiled_test.rs`, `pipeline_test.rs`, `fixtures_test.rs`, and affected policy fixtures/snapshots.

Planner and report:

- Modify `crates/voom-plan/src/planner.rs`: expand `CompiledOperation::TranscodeVideo`.
- Modify `crates/voom-plan/src/compliance_report.rs`: mark planned `transcode_video` supported, blocked `transcode_video` blocked.
- Modify tests and fixtures under `crates/voom-plan/src/*_test.rs` and `crates/voom-plan/fixtures/`.

Protocol and worker:

- Create `crates/voom-worker-protocol/src/transcode_video.rs`; modify `crates/voom-worker-protocol/src/lib.rs`.
- Create crate `crates/voom-ffmpeg-worker/` with `src/lib.rs`, `src/main.rs`, `src/preflight.rs`, `src/observe.rs`, `src/ffmpeg.rs`, `src/handler.rs`, and sibling tests.
- Modify root `Cargo.toml` members and workspace dependencies.

Control plane:

- Create `crates/voom-control-plane/src/transcode/mod.rs`, `source.rs`, `stage.rs`, `dispatch.rs`, `commit.rs`, `events.rs` and sibling tests.
- Modify `crates/voom-control-plane/src/lib.rs` to expose the module.
- Modify `crates/voom-control-plane/src/workflow/policy_bridge.rs` and tests.
- Modify `crates/voom-control-plane/src/workflow/model.rs` and `binding.rs` so workflow operation nodes can carry the policy target and operation payload needed by real transcode execution.
- Modify `crates/voom-control-plane/src/workflow/executor.rs` only enough to route `OperationKind::TranscodeVideo` tickets to the transcode use-case.
- Add the bundled FFmpeg process dispatcher in `crates/voom-control-plane/src/transcode/dispatch.rs`; modify `crates/voom-control-plane/src/workflow/runtime.rs` only if the existing runtime registry needs a small registration hook for the bundled worker.

Persistence and events:

- Modify `crates/voom-store/src/repo/artifacts.rs` only if artifact handle/location read models need a focused method.
- Modify `crates/voom-events/src/kind.rs`, `payload.rs`, and tests for `artifact.transcode_*`.

CLI and docs:

- Modify `crates/voom-cli/src/commands/compliance.rs` and `crates/voom-cli/tests/compliance_envelope.rs` so the compliance execute envelope exposes all Sprint 12 IDs.
- Add or update insta snapshots under `crates/voom-cli/tests/snapshots/`.
- Add closeout evidence document `docs/superpowers/specs/2026-05-25-voom-sprint-12-closeout.md`.

---

### Task 1: Policy DSL Acceptance And Compilation

**Files:**

- Modify: `crates/voom-policy/src/compiled.rs`
- Modify: `crates/voom-policy/src/validate.rs`
- Modify: `crates/voom-policy/src/diagnostic.rs`
- Test: `crates/voom-policy/src/validate_test.rs`
- Test: `crates/voom-policy/src/compiled_test.rs`
- Test: `crates/voom-policy/src/pipeline_test.rs`

- [ ] **Step 1: Write failing validator tests**

Add tests that lock the exact supported and rejected shapes:

```rust
#[test]
fn accepts_sprint12_video_hevc_transcode() {
    assert!(compile_policy("policy \"p\" { phase a { transcode video to hevc {} } }").is_ok());
    assert!(compile_policy("policy \"p\" { phase a { transcode video to hevc } }").is_ok());
}

#[test]
fn rejects_sprint12_unsupported_transcode_shapes() {
    assert!(codes("policy \"p\" { phase a { transcode audio to opus {} } }")
        .contains(&"unsupported_transcode_shape".to_owned()));
    assert!(codes("policy \"p\" { phase a { transcode video to av1 {} } }")
        .contains(&"unsupported_transcode_shape".to_owned()));
    assert!(codes("policy \"p\" { phase a { transcode video to hevc using profile \"small\" {} } }")
        .contains(&"unsupported_transcode_shape".to_owned()));
}
```

- [ ] **Step 2: Run validator tests to verify failure**

Run:

```bash
cargo test -p voom-policy accepts_sprint12_video_hevc_transcode
cargo test -p voom-policy rejects_sprint12_unsupported_transcode_shapes
```

Expected: the accepted tests fail because `transcode` still produces `deferred_execution_operation`; unsupported-shape tests fail until the new diagnostic code exists.

- [ ] **Step 3: Add the typed compiled operation**

Add this enum variant in `CompiledOperation`:

```rust
TranscodeVideo {
    target_codec: String,
    container: String,
    profile: String,
},
```

Lower the accepted statement text to:

```rust
CompiledOperation::TranscodeVideo {
    target_codec: "hevc".to_owned(),
    container: "mkv".to_owned(),
    profile: "default-hevc".to_owned(),
}
```

- [ ] **Step 4: Implement strict validation**

In `validate.rs`, replace the blanket `transcode` deferred branch with a helper. The helper must accept both the braced block and no-body statement forms after parser normalization, and it must reject any trailing profile, argument, overwrite, stream-selection, or replace tokens:

```rust
fn validate_transcode_statement(&mut self, statement: &StatementAst) {
    let text = statement_text(statement);
    let tokens = words(text.as_ref());
    let accepted = tokens.as_slice() == ["transcode", "video", "to", "hevc"];
    if accepted {
        return;
    }
    self.error(
        DiagnosticCode::UnsupportedTranscodeShape,
        statement.span(),
        "only `transcode video to hevc {}` is supported in Sprint 12",
    );
}
```

Apply the same helper inside rule/conditional validation so nested supported transcode operations compile and nested unsupported transcodes fail the same way.

- [ ] **Step 5: Add compiler and pipeline tests**

Add assertions:

```rust
let policy = compile_policy("policy \"p\" { phase a { transcode video to hevc {} } }").unwrap();
assert_eq!(
    policy.phases[0].operations[0],
    CompiledOperation::TranscodeVideo {
        target_codec: "hevc".to_owned(),
        container: "mkv".to_owned(),
        profile: "default-hevc".to_owned(),
    },
);
```

- [ ] **Step 6: Run and commit**

Run: `cargo test -p voom-policy`

Expected: all `voom-policy` tests pass.

Commit:

```bash
git add crates/voom-policy
git commit -m "feat(policy): compile sprint 12 video transcode"
```

### Task 2: Policy Fixtures

**Files:**

- Create: `crates/voom-policy/fixtures/policies/video-transcode-hevc.voom`
- Create: `crates/voom-policy/fixtures/compiled/video-transcode-hevc.json`
- Modify: `crates/voom-policy/src/policy_fixtures.rs`
- Test: `crates/voom-policy/src/policy_fixtures_test.rs`

- [ ] **Step 1: Add the accepted fixture**

Create policy source:

```text
policy "video transcode hevc" {
  phase normalize {
    transcode video to hevc {}
  }
}
```

Expected compiled operation shape:

```json
{
  "type": "transcode_video",
  "target_codec": "hevc",
  "container": "mkv",
  "profile": "default-hevc"
}
```

- [ ] **Step 2: Register fixture and run fixture tests**

Run:

```bash
cargo test -p voom-policy policy_fixtures_test
cargo test -p voom-policy fixtures_test
```

Expected: fixture inventory and deterministic compiled JSON tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/voom-policy/fixtures crates/voom-policy/src/policy_fixtures.rs
git commit -m "test(policy): add video transcode fixture"
```

### Task 3: Planner Media-Shape Semantics

**Files:**

- Modify: `crates/voom-plan/src/planner.rs`
- Modify: `crates/voom-plan/src/diagnostic.rs`
- Test: `crates/voom-plan/src/planner_test.rs`

- [ ] **Step 1: Write failing planner tests**

Add tests for:

```rust
#[test]
fn transcode_video_plans_non_hevc_or_non_mkv_single_video_snapshot() {
    let plan = generate_plan(request_with_transcode(snapshot_with("mp4", "h264", 1))).unwrap();
    assert_eq!(plan.nodes[0].operation_kind, "transcode_video");
    assert_eq!(plan.nodes[0].status, NodeStatus::Planned);
    assert_eq!(plan.nodes[0].operation_payload["target_codec"], "hevc");
    assert_eq!(plan.nodes[0].operation_payload["container"], "mkv");
}

#[test]
fn transcode_video_no_ops_hevc_mkv_single_video_snapshot() {
    let plan = generate_plan(request_with_transcode(snapshot_with("mkv", "hevc", 1))).unwrap();
    assert_eq!(plan.nodes[0].status, NodeStatus::NoOp);
}

#[test]
fn transcode_video_blocks_unknown_or_multi_video_snapshots() {
    assert_blocked(snapshot_with_unknown_container());
    assert_blocked(snapshot_with_unknown_video_codec());
    assert_blocked(snapshot_with("mkv", "h264", 0));
    assert_blocked(snapshot_with("mkv", "h264", 2));
}
```

Implement helper fixtures in the test file using `stream_summary` values such as `json!({"video_stream_count": 1})` and the existing `MediaSnapshotInput` constructor style.

- [ ] **Step 2: Run planner tests to verify failure**

Run: `cargo test -p voom-plan planner_test::transcode_video`

Expected: tests fail because `TranscodeVideo` is treated as unsupported or does not exist yet.

- [ ] **Step 3: Implement planner expansion**

Add an `expand_transcode_video_for_snapshot` method with these branches:

```rust
match transcode_video_shape(snapshot) {
    TranscodeVideoShape::Compliant => NodeStatus::NoOp,
    TranscodeVideoShape::NeedsTranscode => NodeStatus::Planned,
    TranscodeVideoShape::InsufficientFacts(message)
    | TranscodeVideoShape::UnsupportedShape(message) => NodeStatus::Blocked,
}
```

Use normalized codec aliases so `h265` and `hevc` both satisfy the target. Treat unknown container, unknown codec, zero video streams, and multiple video streams as blocked.

- [ ] **Step 4: Add payload and observed state**

Planned and no-op nodes carry:

```json
{
  "type": "transcode_video",
  "target_codec": "hevc",
  "container": "mkv",
  "profile": "default-hevc"
}
```

Observed state carries known facts:

```json
{
  "container": "mp4",
  "video_codec": "h264",
  "video_stream_count": 1
}
```

- [ ] **Step 5: Run and commit**

Run: `cargo test -p voom-plan`

Expected: all planner/report/hash tests pass or snapshots are deliberately updated.

Commit:

```bash
git add crates/voom-plan
git commit -m "feat(plan): plan sprint 12 video transcode"
```

### Task 4: Compliance Report And Workflow Bridge

**Files:**

- Modify: `crates/voom-plan/src/compliance_report.rs`
- Test: `crates/voom-plan/src/compliance_report_test.rs`
- Modify: `crates/voom-control-plane/src/workflow/policy_bridge.rs`
- Modify: `crates/voom-control-plane/src/workflow/model.rs`
- Modify: `crates/voom-control-plane/src/workflow/binding.rs`
- Test: `crates/voom-control-plane/src/workflow/policy_bridge_test.rs`

- [ ] **Step 1: Write failing report and bridge tests**

Add assertions that planned `transcode_video` is execution-supported:

```rust
assert_eq!(
    report.nodes[0].execution_eligibility,
    ExecutionEligibility::Supported,
);
```

Add bridge test:

```rust
let plan = plan(vec![node("transcode_video", NodeStatus::Planned)]);
let bridged = workflow_plan_from_compliance(&plan, &report(&plan)).unwrap();
let workflow = bridged.workflow.unwrap();
assert_eq!(
    workflow.nodes[0].operation(),
    OperationKind::TranscodeVideo,
);
assert_eq!(bridged.summary.per_operation["transcode_video"], 1);
assert_eq!(
    workflow.nodes[0].policy_target(),
    Some(&TargetRef::FileVersion { id: FileVersionId(42) }),
);
assert_eq!(
    workflow.nodes[0].operation_payload()["target_codec"],
    "hevc",
);
```

- [ ] **Step 2: Run focused tests**

Run:

```bash
cargo test -p voom-plan compliance_report_test::transcode_video
cargo test -p voom-control-plane policy_bridge_test::bridge_maps_planned_transcode_video
```

Expected: tests fail until report/bridge recognize `transcode_video`.

- [ ] **Step 3: Implement report and bridge mapping**

Map report eligibility:

```rust
(NodeStatus::Planned, "transcode_video") => ExecutionEligibility::Supported,
(NodeStatus::Blocked, "transcode_video") => ExecutionEligibility::Blocked,
```

Extend `OperationNode` with optional `policy_target: Option<TargetRef>` and `operation_payload: serde_json::Value`, plus `policy_target()` and `operation_payload()` accessors used by tests. Existing synthetic workflow nodes set `policy_target = None` and `operation_payload = serde_json::Value::Null` so previous workflow tests keep their current behavior.

Bridge planned transcode nodes into workflow nodes that preserve the plan target and operation payload rather than reconstructing source identity later from synthetic branch defaults:

```rust
NodeStatus::Planned if node.operation_kind == "transcode_video" => {
    nodes.push(WorkflowNode::Operation(OperationNode {
        id: format!("policy-node_{}", node.node_id),
        operation: OperationKind::TranscodeVideo,
        policy_target: Some(node.target.clone()),
        operation_payload: node.operation_payload.clone(),
        depends_on: Vec::new(),
        depends_on_selected: Vec::new(),
        provides_selected: None,
    }));
}
```

- [ ] **Step 4: Run and commit**

Run:

```bash
cargo test -p voom-plan compliance_report_test::transcode_video
cargo test -p voom-control-plane policy_bridge_test::bridge_maps_planned_transcode_video
```

Expected: report and policy bridge tests pass.

Commit:

```bash
git add crates/voom-plan crates/voom-control-plane/src/workflow
git commit -m "feat(control-plane): bridge video transcode policy nodes"
```

### Task 5: Worker Protocol Payloads

**Files:**

- Create: `crates/voom-worker-protocol/src/transcode_video.rs`
- Modify: `crates/voom-worker-protocol/src/lib.rs`
- Test: `crates/voom-worker-protocol/src/transcode_video_test.rs`

- [ ] **Step 1: Add serialization tests**

Test request JSON includes input facts, output path, and default profile:

```rust
let request = TranscodeVideoRequest {
    input: TranscodeVideoInput {
        path: "/library/input.mkv".to_owned(),
        expected: TranscodeVideoExpectedFacts {
            size_bytes: 1234,
            content_hash: "blake3:abc".to_owned(),
            modified_at: Some("2026-05-25T00:00:00Z".to_owned()),
            local_file_key: None,
        },
    },
    output: TranscodeVideoOutput {
        staging_root: "/tmp/voom-stage".to_owned(),
        path: "/tmp/voom-stage/ticket-1/lease-1/input.hevc.mkv".to_owned(),
        container: "mkv".to_owned(),
        video_codec: "hevc".to_owned(),
        overwrite: false,
    },
    profile: TranscodeVideoProfile::default_hevc(),
};
```

Expected JSON keys use snake_case exactly as the Sprint 12 spec shows.

- [ ] **Step 2: Implement protocol structs**

Use `#[serde(deny_unknown_fields)]` on all request/result structs. Define:

```rust
pub struct TranscodeVideoRequest { pub input: TranscodeVideoInput, pub output: TranscodeVideoOutput, pub profile: TranscodeVideoProfile }
pub struct TranscodeVideoResult { pub status: TranscodeVideoStatus, pub provider: String, pub provider_version: String, pub input_pre: TranscodeVideoObservedFacts, pub input_post: TranscodeVideoObservedFacts, pub output: TranscodeVideoObservedFacts, pub output_container: String, pub output_video_codec: String }
```

- [ ] **Step 3: Run and commit**

Run: `cargo test -p voom-worker-protocol transcode_video`

Expected: transcode protocol serialization tests pass.

Commit:

```bash
git add crates/voom-worker-protocol
git commit -m "feat(protocol): add transcode video payloads"
```

### Task 6: Bundled FFmpeg Worker Preflight

**Files:**

- Create: `crates/voom-ffmpeg-worker/Cargo.toml`
- Create: `crates/voom-ffmpeg-worker/src/lib.rs`
- Create: `crates/voom-ffmpeg-worker/src/main.rs`
- Create: `crates/voom-ffmpeg-worker/src/preflight.rs`
- Test: `crates/voom-ffmpeg-worker/src/preflight_test.rs`
- Modify: root `Cargo.toml`

- [ ] **Step 1: Add crate and failing preflight tests**

Tests cover:

```rust
assert!(preflight_with_paths(missing_ffmpeg, valid_ffprobe).is_err());
assert!(preflight_with_paths(non_executable_ffmpeg, valid_ffprobe).is_err());
assert!(preflight_with_stub_encoder_list_without_libx265().is_err());
assert!(preflight_with_stub_encoder_list_containing_libx265().is_ok());
```

- [ ] **Step 2: Implement binary discovery and preflight**

Preflight resolves either environment overrides or `PATH`:

```rust
pub struct FfmpegPreflight {
    pub ffmpeg_path: PathBuf,
    pub ffprobe_path: PathBuf,
    pub ffmpeg_version: String,
    pub ffprobe_version: String,
    pub hevc_encoder: String,
}
```

Invoke:

```bash
ffmpeg -hide_banner -version
ffmpeg -hide_banner -encoders
ffprobe -hide_banner -version
```

Require executable files and an encoder line containing `libx265`.

- [ ] **Step 3: Run and commit**

Run: `cargo test -p voom-ffmpeg-worker preflight`

Expected: preflight tests pass and fail loudly for missing binaries.

Commit:

```bash
git add Cargo.toml crates/voom-ffmpeg-worker
git commit -m "feat(ffmpeg-worker): add preflight"
```

### Task 7: FFmpeg Worker Handler And Process Invocation

**Files:**

- Create: `crates/voom-ffmpeg-worker/src/observe.rs`
- Create: `crates/voom-ffmpeg-worker/src/ffmpeg.rs`
- Create: `crates/voom-ffmpeg-worker/src/handler.rs`
- Test: `crates/voom-ffmpeg-worker/src/observe_test.rs`
- Test: `crates/voom-ffmpeg-worker/src/ffmpeg_test.rs`
- Test: `crates/voom-ffmpeg-worker/src/handler_test.rs`
- Test: `crates/voom-ffmpeg-worker/tests/transcode_worker.rs`

- [ ] **Step 1: Write path and drift tests**

Cover missing input, input pre/post drift, existing output, non-canonical staging root, output path escape, malformed request, FFmpeg non-zero exit, timeout, and output facts mismatch.

Use assertions like:

```rust
let err = handle_transcode_video(request_with_output_escape()).await.unwrap_err();
assert_eq!(err.failure_class(), FailureClass::ConfigInvalid);
```

- [ ] **Step 2: Implement byte observation**

Reuse BLAKE3 observation style from `voom-verify-artifact-worker/src/observe.rs`. The worker compares:

```rust
input_pre.size_bytes == request.input.expected.size_bytes
input_pre.content_hash == request.input.expected.content_hash
input_post == input_pre
```

- [ ] **Step 3: Implement canonical staging checks**

Accept only:

```rust
let root = canonical_existing_dir_no_symlink(request.output.staging_root)?;
let parent = canonical_existing_dir_no_symlink(output_path.parent())?;
parent.starts_with(&root)
```

Reject any existing output leaf before spawning FFmpeg.

The worker must not create missing parent directories. The control plane creates the per-ticket/per-lease staging directory before dispatch after canonicalizing the root and verifying every path component is not a symlink. The worker only accepts an already-existing canonical parent under the canonical staging root.

- [ ] **Step 4: Implement deterministic FFmpeg command**

Command shape:

```bash
ffmpeg -hide_banner -nostdin -n \
  -i <input> \
  -map 0:v:0 -map 0:a? -map 0:s? -map 0:t? \
  -c:v libx265 -crf 23 -preset medium \
  -c:a copy -c:s copy -c:t copy \
  -map_metadata 0 \
  -f matroska \
  -progress pipe:2 \
  <output>
```

Before running, reject `overwrite = true` and any existing output path. Use `-n`, not `-y`, so FFmpeg also refuses to overwrite a path that appears between the preflight check and process start. After running, verify ffprobe-reported `format_name` contains `matroska` and the first video codec is `hevc` or `h265`.

- [ ] **Step 5: Implement protocol route policy**

The worker accepts only `OperationKind::TranscodeVideo`; unknown operation requests return the same protocol error style as existing workers.

- [ ] **Step 6: Run and commit**

Run: `cargo test -p voom-ffmpeg-worker`

Expected: unit and integration tests pass with FFmpeg/ffprobe installed; absence fails with explicit setup diagnostics.

Commit:

```bash
git add crates/voom-ffmpeg-worker
git commit -m "feat(ffmpeg-worker): transcode hevc mkv"
```

### Task 8: Transcode Events

**Files:**

- Modify: `crates/voom-events/src/kind.rs`
- Modify: `crates/voom-events/src/payload.rs`
- Test: `crates/voom-events/src/kind_test.rs`
- Test: `crates/voom-events/src/payload_test.rs`

- [ ] **Step 1: Add event tests**

Assert string round trips:

```rust
assert_eq!(EventKind::ArtifactTranscodeStarted.as_str(), "artifact.transcode_started");
assert_eq!(EventKind::from_str("artifact.transcode_succeeded").unwrap(), EventKind::ArtifactTranscodeSucceeded);
```

Assert payload serializes correlation fields:

```rust
ArtifactTranscodeStartedPayload {
    job_id: 1,
    ticket_id: 2,
    lease_id: Some(3),
    source_file_version_id: 4,
    source_file_location_id: 5,
    staging_path: "/tmp/voom-stage/2/3/out.mkv".to_owned(),
    provider: Some("ffmpeg".to_owned()),
    provider_version: None,
}
```

- [ ] **Step 2: Implement event variants and payload structs**

Add started, progress, succeeded, and failed payloads. Include job ID, ticket ID, lease/attempt identity, source IDs, staging path, artifact IDs when known, provider facts when known, and failure class/error code on failure.

- [ ] **Step 3: Run and commit**

Run: `cargo test -p voom-events`

Expected: event kind and payload tests pass.

Commit:

```bash
git add crates/voom-events
git commit -m "feat(events): add transcode artifact events"
```

### Task 9: Control-Plane Transcode Use-Case

**Files:**

- Create: `crates/voom-control-plane/src/transcode/mod.rs`
- Create: `crates/voom-control-plane/src/transcode/source.rs`
- Create: `crates/voom-control-plane/src/transcode/stage.rs`
- Create: `crates/voom-control-plane/src/transcode/dispatch.rs`
- Create: `crates/voom-control-plane/src/transcode/commit.rs`
- Create: `crates/voom-control-plane/src/transcode/events.rs`
- Modify: `crates/voom-control-plane/src/lib.rs`
- Test: sibling `*_test.rs` files under `src/transcode/`

- [ ] **Step 1: Write source selection tests**

Cover missing source version, retired source version, explicit missing location, ambiguous local locations, non-local location, and one valid live local location.

Expected errors:

```rust
missing version => ErrorCode::NotFound
ambiguous location => ErrorCode::ConfigInvalid
```

- [ ] **Step 2: Write staging path tests**

Use deterministic selector:

```rust
let path = staging_path(root, TicketId(7), LeaseId(9), "Movie.mkv")?;
assert!(path.ends_with("ticket-7/lease-9/Movie.hevc.mkv"));
```

The selector creates the `ticket-7/lease-9` parent directory with exclusive directory creation, canonicalizes the created parent, and rejects it if any component is a symlink or if the canonical parent is outside the canonical staging root.

Assert retry-safe uniqueness:

```rust
assert_ne!(
    staging_path(root, TicketId(7), LeaseId(9), "Movie.mkv")?,
    staging_path(root, TicketId(7), LeaseId(10), "Movie.mkv")?,
);
```

- [ ] **Step 3: Write orchestration tests with fake dispatcher**

Test the successful path records:

- staged artifact handle;
- one live staging artifact location;
- successful artifact verification;
- committed artifact record;
- result `FileVersion`;
- result `FileLocation`;
- result `MediaSnapshot`;
- started/succeeded events.

- [ ] **Step 4: Implement `TranscodeVideoInput` and report**

Use:

```rust
pub struct ExecuteTranscodeVideoInput {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: LeaseId,
    pub source_file_version_id: FileVersionId,
    pub source_location_id: Option<FileLocationId>,
    pub staging_root: PathBuf,
    pub target_dir: PathBuf,
}
```

Report includes every CLI-required stable ID from Sprint 12 section 7.

- [ ] **Step 5: Implement source revalidation and worker dispatch**

Before dispatch:

- require exactly one live local source unless explicit location ID exists;
- canonicalize source path, staging parent, and target parent;
- create the per-ticket/per-lease staging parent with no symlink traversal before worker dispatch;
- observe source bytes and compare `FileVersion` size/hash;
- re-read latest media snapshot and require known container, exactly one video stream, and known codec.

- [ ] **Step 6: Implement artifact record, verification, commit, and result probe**

After worker success:

- reject input drift and non-HEVC/non-MKV worker facts;
- create `artifact_handles` and one `artifact_locations.kind = 'staging'`;
- call `verify_artifact`;
- call `commit_artifact`;
- launch the bundled ffprobe scan/probe path for the committed result path, persist a `MediaSnapshot` for the result `FileVersion`, and include result IDs plus commit record ID in the error data if this post-commit probe fails;
- return IDs even when post-commit probing fails.

- [ ] **Step 7: Run and commit**

Run: `cargo test -p voom-control-plane transcode`

Expected: transcode use-case unit tests pass.

Commit:

```bash
git add crates/voom-control-plane/src/transcode crates/voom-control-plane/src/lib.rs
git commit -m "feat(control-plane): orchestrate video transcode"
```

### Task 10: Workflow Execution Routing

**Files:**

- Modify: `crates/voom-control-plane/src/workflow/executor.rs`
- Modify: `crates/voom-control-plane/src/workflow/model.rs`
- Modify: `crates/voom-control-plane/src/workflow/binding.rs`
- Modify: `crates/voom-control-plane/src/workflow/ticket_payload.rs`
- Modify: `crates/voom-control-plane/src/transcode/dispatch.rs`
- Modify: `crates/voom-control-plane/src/workflow/runtime.rs` only for a small bundled-worker registration hook if the dispatcher cannot reuse the Sprint 11 process-launch pattern directly.
- Test: `crates/voom-control-plane/src/workflow/executor_test.rs`
- Test: `crates/voom-control-plane/tests/compliance_execute.rs`

- [ ] **Step 1: Write failing workflow tests**

Test a planned `transcode_video` node creates a durable ticket with:

```json
{
  "operation": "transcode_video",
  "source_file_version_id": 1,
  "source_location_id": 2
}
```

Test execution calls the transcode use-case rather than generic worker dispatch when the ticket operation is `TranscodeVideo`.

- [ ] **Step 2: Implement payload binding**

Extend workflow ticket payload rendering for policy nodes so `transcode_video` carries source IDs from the plan target, the original plan node payload, and command-scoped staging/target roots. Do not use the existing synthetic branch defaults for policy transcode tickets.

Target extraction rules:

```rust
match policy_target {
    TargetRef::FileVersion { id } => source_file_version_id = id,
    TargetRef::FileLocation { id } => {
        source_location_id = Some(id);
        source_file_version_id = lookup_location(id)?.file_version_id;
    }
    other => return Err(VoomError::Config(format!(
        "transcode_video requires file_version or file_location target, got {other:?}"
    ))),
}
```

- [ ] **Step 3: Route transcode tickets**

At dispatch time:

```rust
if workflow_payload.operation == OperationKind::TranscodeVideo {
    return dispatch_control_plane_transcode(control, ticket, workflow_payload, lease_id).await;
}
```

The dispatch must mark ticket success/failure through existing ticket lease APIs and emit progress events when the worker stream reports progress.

- [ ] **Step 4: Run and commit**

Run:

```bash
cargo test -p voom-control-plane workflow::executor_test
cargo test -p voom-control-plane --test compliance_execute
```

Expected: workflow tests pass and unsupported-execution errors are gone for `transcode_video`.

Commit:

```bash
git add crates/voom-control-plane/src/workflow crates/voom-control-plane/tests/compliance_execute.rs
git commit -m "feat(control-plane): execute transcode workflow tickets"
```

### Task 11: CLI Envelope And Snapshots

**Files:**

- Modify: `crates/voom-cli/src/commands/compliance.rs`
- Test: `crates/voom-cli/tests/compliance_envelope.rs`
- Update: `crates/voom-cli/tests/snapshots/compliance_envelope__*.snap`

- [ ] **Step 1: Add CLI integration tests**

Add successful flow:

```rust
voom scan --path <tiny-h264-mp4>
voom compliance execute --policy video-transcode-hevc.voom --input-set <scan-derived-input> --staging-root <tmp/stage> --output-dir <tmp/out>
```

Assert one JSON envelope on stdout and data contains:

```json
{
  "job_id": 1,
  "tickets": [{"operation": "transcode_video"}],
  "staged_artifact_handle_id": 1,
  "verification_id": 1,
  "commit_record_id": 1,
  "result_file_version_id": 2,
  "result_file_location_id": 2,
  "result_media_snapshot_id": 2
}
```

- [ ] **Step 2: Add representative failure snapshots**

Cover existing staging path, unsupported media shape, source drift, and missing FFmpeg preflight. The expected envelope uses `status = "error"` and stable `error.code`.

- [ ] **Step 3: Implement envelope additions**

Keep stdout to exactly one JSON envelope. Put logs and FFmpeg diagnostics on stderr. Include partial IDs in `error.data` when durable state was recorded before failure.

- [ ] **Step 4: Run and review snapshots**

Run: `cargo test -p voom-cli compliance_envelope`

If snapshots changed intentionally, run: `cargo insta review`

Expected: accepted snapshots show stable IDs and no extra stdout.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-cli
git commit -m "feat(cli): report video transcode execution"
```

### Task 12: End-To-End Integration And Closeout

**Files:**

- Create: `crates/voom-control-plane/tests/video_transcode_flow.rs`
- Create: `docs/superpowers/specs/2026-05-25-voom-sprint-12-closeout.md`
- Modify: `justfile` only if a new fixture-generation command is required by tests.

- [ ] **Step 1: Add end-to-end test**

Test sequence:

```rust
scan_h264_fixture();
compile_policy("transcode video to hevc {}");
generate_plan();
execute_compliance();
assert_staged_artifact_verified_committed();
assert_result_snapshot_is_hevc_mkv();
generate_plan_for_result();
assert_result_plan_is_no_op();
```

- [ ] **Step 2: Add closeout matrix**

Create a closeout document with rows:

```markdown
| Requirement | Evidence |
|---|---|
| DSL compiles `transcode video to hevc {}` | `cargo test -p voom-policy` |
| Planner planned/no-op/blocked behavior | `cargo test -p voom-plan planner_test::transcode_video` |
| Worker preflight and transcode failures | `cargo test -p voom-ffmpeg-worker` |
| Verify and commit integration | `cargo test -p voom-control-plane video_transcode_flow` |
| CLI envelope | `cargo test -p voom-cli compliance_envelope` |
| Full suite | `just ci` |
```

- [ ] **Step 3: Run final verification**

Run:

```bash
just fmt
just ci
```

Expected: `just ci` passes without skipped Sprint 12 FFmpeg tests.

- [ ] **Step 4: Commit closeout**

```bash
git add crates docs justfile
git commit -m "test: close out sprint 12 video transcode"
```

---

## Self-Review Notes

Spec coverage:

- Policy/compiler support is covered by Tasks 1-2.
- Planner planned/no-op/blocked semantics are covered by Task 3.
- Compliance bridge and durable ticket submission are covered by Tasks 4 and 10.
- Typed worker protocol and bundled FFmpeg worker are covered by Tasks 5-7.
- Preflight is covered by Task 6.
- Control-plane orchestration, artifact recording, verification, commit, result snapshot, and events are covered by Tasks 8-10.
- CLI envelopes and golden fixtures are covered by Task 11.
- Integration evidence and closeout matrix are covered by Task 12.

Known risks to resolve during implementation:

- Current policy statements are parsed as generic statements; validation/lowering must inspect statement text without accepting free-form FFmpeg arguments.
- Current workflow bridge uses synthetic branch payloads for older workflow simulations; Sprint 12 needs real source IDs from policy input targets.
- Post-commit result probing must not hide a successful commit. The transcode report and CLI error data must include commit/result IDs if probing fails.
- FFmpeg/ffprobe availability is a CI prerequisite for Sprint 12; tests should fail with explicit setup messages instead of skipping.
