# VOOM Sprint 13 Container Remux And Track Selection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement policy-driven MKV remux and V1 track selection from typed policy intent through grouped planning, durable workflow tickets, an out-of-process MKVToolNix worker, staged artifact verification, add-only commit, committed-result probing, and stable CLI reporting.

**Architecture:** Keep remux as the same durable workflow shape established by Sprint 12 transcode: the worker writes only a staged MKV file, while the control plane owns source revalidation, artifact rows, verification, add-only commit, result snapshot persistence, lineage, and events. The planner groups same-target same-phase container and track operations into one typed `remux` node and blocks unsupported or under-factored shapes visibly.

**Tech Stack:** Rust workspace, tokio, sqlx/SQLite, serde JSON, MKVToolNix `mkvmerge`, existing worker protocol transport, ffprobe-backed scan/probe, insta snapshots, `just` command runner.

---

## Assumptions And Success Criteria

Assumptions:

- The policy compiler already accepts the Sprint 13 container and track-selection DSL as typed `CompiledOperation` variants; implementation should preserve those types instead of lowering to provider command strings.
- Durable media snapshot payloads already contain normalized `payload["streams"]` from ffprobe, but current snapshots do not guarantee a stored per-stream durable ID. Sprint 13 must persist a stable `id` for every stream before planning/execution can use it; missing stream IDs are blocked for remux rather than derived silently at dispatch time.
- `OperationKind::Remux` already exists and its wire name is `remux`; Sprint 13 adds typed remux payload structs and real routing.
- A new bundled crate named `voom-mkvtoolnix-worker` is appropriate because existing worker crates use one process per provider.
- Required MKVToolNix tests fail clearly when `mkvmerge` is missing. Do not mark required conformance tests ignored or silently skipped.
- The first implementation uses add-only target paths named `<source-stem>.remux.mkv` under the configured remux target directory.

Success criteria:

- Supported policy text containing `container mkv` plus V1 track operations compiles and plans to one grouped `remux` node per target and phase.
- Already-compliant fixture media produces no-op nodes with clear reasons.
- Missing stream/container facts, `keep/remove video`, unsupported filters, and `defaults ... best` produce blocked nodes and diagnostics.
- Compliance execution submits `remux` workflow tickets with policy targets and typed operation payloads.
- The MKVToolNix worker writes only to a canonical staging path, rejects overwrite/path escape/content drift, and returns selected/default stream identities.
- The control plane verifies the staged artifact, commits it add-only, records the result `MediaSnapshot`, and reports stable IDs.
- Representative failures surface stable public error codes and one JSON envelope on stdout.
- `just ci` passes.

## File Map

Policy and planner:

- Verify `crates/voom-policy/src/compiled.rs`, `validate.rs`, and existing sibling tests; modify them in Task 3 when policy acceptance tests show any Sprint 13 DSL shape is rejected or lowered incorrectly.
- Modify `crates/voom-plan/src/planner.rs`: group remux-capable operations, evaluate selectors, and emit typed payloads.
- Create `crates/voom-plan/src/remux.rs`: focused helpers for normalized stream fact parsing, selector evaluation, no-op/planned/blocked decisions, and payload building; expose only the selector helpers needed by `voom-control-plane` so planning and execution revalidation use the same semantics.
- Create `crates/voom-plan/src/remux_test.rs`; modify `crates/voom-plan/src/lib.rs` and `planner_test.rs`.
- Modify `crates/voom-plan/src/compliance_report.rs` and tests so `remux` planned nodes are executable and blocked nodes report reasons.
- Add fixtures under `crates/voom-plan/fixtures/plans/` and `crates/voom-plan/fixtures/reports/`.

Worker protocol and worker:

- Create `crates/voom-worker-protocol/src/remux.rs`; modify `crates/voom-worker-protocol/src/lib.rs`.
- Create tests in `crates/voom-worker-protocol/src/remux_test.rs`.
- Create crate `crates/voom-mkvtoolnix-worker/` with `src/lib.rs`, `src/main.rs`, `src/preflight.rs`, `src/observe.rs`, `src/mkvmerge.rs`, `src/handler.rs`, and sibling tests.
- Modify root `Cargo.toml` workspace members and `[workspace.dependencies]`.

Control plane and workflow:

- Create `crates/voom-control-plane/src/remux/mod.rs`, `source.rs`, `selection.rs`, `stage.rs`, `dispatch.rs`, `commit.rs`, `events.rs`, and sibling tests.
- Modify `crates/voom-control-plane/src/lib.rs`.
- Modify `crates/voom-control-plane/src/workflow/policy_bridge.rs`, `binding.rs`, and `executor.rs` for real `OperationKind::Remux` tickets; modify `runtime.rs` only to register the bundled MKVToolNix dispatcher if the current runtime registry cannot locate it.
- Modify `crates/voom-control-plane/src/workflow/*_test.rs`.

Persistence, events, CLI:

- Modify `crates/voom-events/src/kind.rs`, `payload.rs`, and tests for `artifact.remux_*`.
- Modify `crates/voom-control-plane/src/scan/persist.rs` and tests so stored media snapshots include `streams[*].id` before policy input extraction.
- Modify `crates/voom-control-plane/src/cases/policy_inputs.rs` and tests so `create_policy_input_set_from_scan` copies durable stream facts from the stored `MediaSnapshot` into `MediaSnapshotInput.stream_summary`.
- Verify `crates/voom-store/src/repo/artifacts.rs`; add a focused artifact helper there when Task 6 would otherwise need raw SQL in `crates/voom-control-plane/src/remux/commit.rs`.
- Modify `crates/voom-cli/src/commands/compliance.rs` and `crates/voom-cli/tests/compliance_envelope.rs`.
- Add or update insta snapshots under `crates/voom-cli/tests/snapshots/`.
- Add closeout evidence document `docs/superpowers/specs/2026-05-25-voom-sprint-13-closeout.md`.

---

### Task 1: Worker Protocol Remux Types

**Files:**

- Create: `crates/voom-worker-protocol/src/remux.rs`
- Create: `crates/voom-worker-protocol/src/remux_test.rs`
- Modify: `crates/voom-worker-protocol/src/lib.rs`

- [ ] **Step 1: Write failing serialization tests**

Add tests that lock the request/result wire shape:

```rust
#[test]
fn remux_request_serializes_wire_shape() {
    let request = RemuxRequest {
        input: RemuxInput {
            path: "/library/input.mp4".to_owned(),
            expected: RemuxExpectedFacts {
                size_bytes: 1234,
                content_hash: "blake3:abc".to_owned(),
                modified_at: Some("2026-05-25T00:00:00Z".to_owned()),
                local_file_key: None,
            },
        },
        output: RemuxOutput {
            staging_root: "/tmp/voom-stage".to_owned(),
            path: "/tmp/voom-stage/ticket-1/lease-1/input.remux.mkv".to_owned(),
            container: "mkv".to_owned(),
            overwrite: false,
        },
        selection: RemuxSelection {
            keep_streams: vec![RemuxStreamRef {
                snapshot_stream_id: "stream-0".to_owned(),
                provider_stream_index: 0,
            }],
            default_streams: vec![RemuxStreamRef {
                snapshot_stream_id: "stream-1".to_owned(),
                provider_stream_index: 1,
            }],
            clear_default_streams: vec![RemuxStreamRef {
                snapshot_stream_id: "stream-2".to_owned(),
                provider_stream_index: 2,
            }],
            track_order: vec![
                RemuxTrackGroup::Video,
                RemuxTrackGroup::Audio,
                RemuxTrackGroup::Subtitle,
            ],
        },
    };

    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["selection"]["track_order"], serde_json::json!(["video", "audio", "subtitle"]));
    assert_eq!(json["output"]["overwrite"], false);
}

#[test]
fn remux_result_rejects_unknown_fields() {
    let err = serde_json::from_value::<RemuxResult>(serde_json::json!({
        "status": "remuxed",
        "provider": "mkvtoolnix",
        "provider_version": "mkvmerge v80",
        "input_pre": { "size_bytes": 1, "content_hash": "blake3:a" },
        "input_post": { "size_bytes": 1, "content_hash": "blake3:a" },
        "output": { "size_bytes": 2, "content_hash": "blake3:b" },
        "output_container": "mkv",
        "kept_snapshot_stream_ids": ["stream-0"],
        "default_snapshot_stream_ids": [],
        "extra": true
    }))
    .unwrap_err();
    assert!(err.to_string().contains("unknown field"));
}
```

- [ ] **Step 2: Run protocol tests to verify failure**

Run:

```bash
cargo test -p voom-worker-protocol remux_
```

Expected: compile failure because the remux module and structs do not exist.

- [ ] **Step 3: Add typed structs**

Create `remux.rs` with the following public surface:

```rust
use serde::{Deserialize, Serialize};

pub const REMUX_CONTAINER_MKV: &str = "mkv";

#[must_use]
pub fn is_supported_remux_container(container: &str) -> bool {
    container.eq_ignore_ascii_case(REMUX_CONTAINER_MKV)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemuxExpectedFacts {
    pub size_bytes: u64,
    pub content_hash: String,
    pub modified_at: Option<String>,
    pub local_file_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemuxObservedFacts {
    pub size_bytes: u64,
    pub content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_file_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemuxInput {
    pub path: String,
    pub expected: RemuxExpectedFacts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemuxOutput {
    pub staging_root: String,
    pub path: String,
    pub container: String,
    pub overwrite: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemuxTrackGroup {
    Video,
    Audio,
    Subtitle,
    Attachment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemuxStreamRef {
    pub snapshot_stream_id: String,
    pub provider_stream_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemuxSelection {
    pub keep_streams: Vec<RemuxStreamRef>,
    pub default_streams: Vec<RemuxStreamRef>,
    pub clear_default_streams: Vec<RemuxStreamRef>,
    pub track_order: Vec<RemuxTrackGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemuxRequest {
    pub input: RemuxInput,
    pub output: RemuxOutput,
    pub selection: RemuxSelection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemuxStatus {
    Remuxed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemuxResult {
    pub status: RemuxStatus,
    pub provider: String,
    pub provider_version: String,
    pub input_pre: RemuxObservedFacts,
    pub input_post: RemuxObservedFacts,
    pub output: RemuxObservedFacts,
    pub output_container: String,
    pub kept_snapshot_stream_ids: Vec<String>,
    pub default_snapshot_stream_ids: Vec<String>,
}
```

Export the module from `lib.rs`:

```rust
mod remux;
pub use remux::*;
```

- [ ] **Step 4: Run and commit**

Run:

```bash
cargo test -p voom-worker-protocol remux_
```

Expected: remux protocol tests pass.

Commit:

```bash
git add crates/voom-worker-protocol
git commit -m "feat(protocol): add typed remux payloads"
```

### Task 2: Durable Stream IDs And Selector Evaluation

**Files:**

- Modify: `crates/voom-control-plane/src/scan/persist.rs`
- Test: `crates/voom-control-plane/src/scan/persist_test.rs`
- Modify: `crates/voom-control-plane/src/cases/policy_inputs.rs`
- Test: `crates/voom-control-plane/src/cases/policy_inputs_test.rs`
- Create: `crates/voom-plan/src/remux.rs`
- Create: `crates/voom-plan/src/remux_test.rs`
- Modify: `crates/voom-plan/src/lib.rs`

- [ ] **Step 1: Write failing durable stream ID persistence test**

Add a scan persistence test that proves every stored stream has an ID even when the worker snapshot omitted one:

```rust
#[tokio::test]
async fn persist_scan_assigns_stable_stream_ids() {
    let fixture = scan_persist_fixture().await;
    let result = probe_result_with_snapshot(serde_json::json!({
        "format": "sprint10-v1",
        "container": {"format_name": "mov,mp4"},
        "streams": [
            {"index": 0, "kind": "video", "codec_name": "h264"},
            {"index": 1, "kind": "audio", "codec_name": "aac", "language": "eng"}
        ]
    }));

    let persisted = persist_scanned_media_snapshot(
        &fixture.control_plane,
        fixture.worker_id,
        &fixture.path,
        &[],
        &fixture.candidate,
        &result,
    )
    .await
    .unwrap();

    let snapshot = fixture
        .control_plane
        .identity
        .get_media_snapshot(persisted.media_snapshot_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.payload["streams"][0]["id"], "stream-0");
    assert_eq!(snapshot.payload["streams"][1]["id"], "stream-1");
}
```

- [ ] **Step 2: Run scan test to verify failure**

Run:

```bash
cargo test -p voom-control-plane persist_scan_assigns_stable_stream_ids
```

Expected: failure because persisted snapshots currently preserve stream objects without adding `id`.

- [ ] **Step 3: Persist stable stream IDs**

In `persist_scanned_media_snapshot`, normalize the probe snapshot before `record_media_snapshot_in_tx`. This helper returns an error instead of silently storing a stream without an ID:

```rust
fn snapshot_with_stream_ids(
    mut payload: serde_json::Value,
) -> Result<serde_json::Value, ScanPersistError> {
    if let Some(streams) = payload.get_mut("streams").and_then(serde_json::Value::as_array_mut) {
        for stream in streams {
            let object = stream.as_object_mut().ok_or_else(|| {
                VoomError::Config("media snapshot stream must be an object".to_owned())
            })?;
            if object.get("id").is_none() {
                let index = object
                    .get("index")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| VoomError::Config("media snapshot stream missing index".to_owned()))?;
                object.insert("id".to_owned(), serde_json::json!(format!("stream-{index}")));
            }
        }
    }
    Ok(payload)
}
```

Use the normalized payload for the stored `NewMediaSnapshot`. Do not mutate the worker result before content-drift checks.

- [ ] **Step 4: Write failing policy-input-from-scan test**

Add a test proving policy input creation from a scanned snapshot preserves the stream facts required by the planner:

```rust
#[tokio::test]
async fn input_from_scan_copies_snapshot_stream_facts() {
    let cp = test_control_plane().await;
    let (file_version_id, media_snapshot_id) = scanned_snapshot_with_payload(
        &cp,
        serde_json::json!({
            "format": "sprint10-v1",
            "container": {"format_name": "mov,mp4"},
            "streams": [
                {"id": "stream-0", "index": 0, "kind": "video", "codec_name": "h264"},
                {"id": "stream-1", "index": 1, "kind": "audio", "codec_name": "aac", "language": "eng"}
            ]
        }),
    )
    .await;

    let created = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "scan-remux".to_owned(),
            file_version_id,
            media_snapshot_id,
            container: "mp4".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap();
    let input = cp.get_policy_input_set(created.input_set_id).await.unwrap().unwrap();

    assert_eq!(input.media_snapshots[0].stream_summary["streams"][0]["id"], "stream-0");
    assert_eq!(input.media_snapshots[0].stream_summary["streams"][1]["language"], "eng");
}
```

- [ ] **Step 5: Run policy input test to verify failure**

Run:

```bash
cargo test -p voom-control-plane input_from_scan_copies_snapshot_stream_facts
```

Expected: failure because `create_policy_input_set_from_scan` currently stores only `{"video_stream_count": 1}`.

- [ ] **Step 6: Copy stream facts into policy input**

In `create_policy_input_set_from_scan`, build `stream_summary` from the stored media snapshot payload:

```rust
fn stream_summary_from_snapshot_payload(payload: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "video_stream_count": payload
            .get("streams")
            .and_then(serde_json::Value::as_array)
            .map(|streams| streams.iter().filter(|stream| stream.get("kind").and_then(serde_json::Value::as_str) == Some("video")).count())
            .unwrap_or(0),
        "streams": payload.get("streams").cloned().unwrap_or_else(|| serde_json::json!([])),
    })
}
```

Use this value for `MediaSnapshotInput.stream_summary`. Keep the CLI-provided `container` and `video_codec` fields unchanged in Sprint 13 so existing command contracts do not change.

- [ ] **Step 7: Write failing stream parser and selector tests**

Add tests for normalized snapshot payloads:

```rust
#[test]
fn parses_normalized_stream_facts() {
    let snapshot = media_snapshot_with_streams(serde_json::json!([
        {"id": "stream-0", "index": 0, "kind": "video", "codec_name": "h264"},
        {"id": "stream-1", "index": 1, "kind": "audio", "codec_name": "aac", "language": "eng", "channels": 2, "disposition": {"default": true}},
        {"id": "stream-2", "index": 2, "kind": "subtitle", "codec_name": "subrip", "language": "spa", "disposition": {"forced": true}}
    ]));

    let facts = stream_facts(&snapshot).unwrap();
    assert_eq!(facts[0].snapshot_stream_id, "stream-0");
    assert_eq!(facts[1].language.as_deref(), Some("eng"));
    assert!(facts[1].is_default);
    assert!(facts[2].is_forced);
}

#[test]
fn stream_parser_blocks_when_stream_id_is_missing() {
    let snapshot = media_snapshot_with_streams(serde_json::json!([
        {"index": 0, "kind": "video", "codec_name": "h264"}
    ]));

    let err = stream_facts(&snapshot).unwrap_err();
    assert_eq!(err, RemuxPlanningBlock::InsufficientSnapshotFacts);
}

#[test]
fn language_selector_blocks_when_language_fact_is_missing() {
    let snapshot = media_snapshot_with_streams(serde_json::json!([
        {"id": "stream-0", "index": 0, "kind": "video", "codec_name": "h264"},
        {"id": "stream-1", "index": 1, "kind": "audio", "codec_name": "aac"}
    ]));

    let err = evaluate_filter(&TrackFilter::LanguageIn { values: vec!["eng".to_owned()] }, &stream_facts(&snapshot).unwrap()[1]).unwrap_err();
    assert_eq!(err, RemuxPlanningBlock::InsufficientSnapshotFacts);
}
```

- [ ] **Step 8: Run selector tests to verify failure**

Run:

```bash
cargo test -p voom-plan remux_
```

Expected: compile failure because `voom-plan::remux` helpers do not exist.

- [ ] **Step 9: Add focused stream fact types**

Implement a public helper surface in `remux.rs` for the types and functions that control-plane execution must reuse, and keep lower-level helper functions private:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotStreamFact {
    pub snapshot_stream_id: String,
    pub provider_stream_index: u32,
    pub kind: voom_policy::TrackTarget,
    pub codec_name: Option<String>,
    pub language: Option<String>,
    pub channels: Option<u32>,
    pub title: Option<String>,
    pub mime_type: Option<String>,
    pub filename: Option<String>,
    pub is_default: bool,
    pub is_forced: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemuxPlanningBlock {
    InsufficientSnapshotFacts,
    UnsupportedMediaShape,
}
```

Require stream IDs from the durable snapshot payload. Missing `id`, missing `index`, missing `kind`, or duplicate `id` values return `RemuxPlanningBlock::InsufficientSnapshotFacts`:

```rust
fn stream_id(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<String, RemuxPlanningBlock> {
    object
        .get("id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or(RemuxPlanningBlock::InsufficientSnapshotFacts)
}
```

- [ ] **Step 10: Implement filter evaluation**

Support V1 filters only:

```rust
pub fn evaluate_filter(
    filter: &voom_policy::TrackFilter,
    stream: &SnapshotStreamFact,
) -> Result<bool, RemuxPlanningBlock> {
    use voom_policy::TrackFilter;
    match filter {
        TrackFilter::LanguageIn { values } => stream
            .language
            .as_ref()
            .map(|language| values.iter().any(|value| value == language))
            .ok_or(RemuxPlanningBlock::InsufficientSnapshotFacts),
        TrackFilter::CodecIn { values } => stream
            .codec_name
            .as_ref()
            .map(|codec| values.iter().any(|value| value == codec))
            .ok_or(RemuxPlanningBlock::InsufficientSnapshotFacts),
        TrackFilter::Channels { op, value } => stream
            .channels
            .map(|channels| compare_u32(channels, *op, *value))
            .ok_or(RemuxPlanningBlock::InsufficientSnapshotFacts),
        TrackFilter::Default => Ok(stream.is_default),
        TrackFilter::Forced => Ok(stream.is_forced),
        TrackFilter::Font => Ok(stream.kind == voom_policy::TrackTarget::Attachment
            && stream
                .mime_type
                .as_deref()
                .is_some_and(|mime| mime.contains("font"))),
        TrackFilter::TitleContains { value } => stream
            .title
            .as_ref()
            .map(|title| title.contains(value))
            .ok_or(RemuxPlanningBlock::InsufficientSnapshotFacts),
        TrackFilter::Not { inner } => evaluate_filter(inner, stream).map(|matched| !matched),
        TrackFilter::And { filters } => filters.iter().try_fold(true, |acc, filter| {
            evaluate_filter(filter, stream).map(|matched| acc && matched)
        }),
        TrackFilter::Or { filters } => {
            let mut saw_unknown = false;
            for filter in filters {
                match evaluate_filter(filter, stream) {
                    Ok(true) => return Ok(true),
                    Ok(false) => {}
                    Err(RemuxPlanningBlock::InsufficientSnapshotFacts) => saw_unknown = true,
                    Err(err) => return Err(err),
                }
            }
            if saw_unknown {
                Err(RemuxPlanningBlock::InsufficientSnapshotFacts)
            } else {
                Ok(false)
            }
        }
        TrackFilter::Commentary | TrackFilter::TitleMatches { .. } => {
            Err(RemuxPlanningBlock::UnsupportedMediaShape)
        }
    }
}
```

- [ ] **Step 11: Run and commit**

Run:

```bash
cargo test -p voom-control-plane persist_scan_assigns_stable_stream_ids
cargo test -p voom-control-plane input_from_scan_copies_snapshot_stream_facts
cargo test -p voom-plan remux_
```

Expected: scan persistence and remux helper tests pass.

Commit:

```bash
git add crates/voom-control-plane/src/scan/persist.rs crates/voom-control-plane/src/scan/persist_test.rs crates/voom-control-plane/src/cases/policy_inputs.rs crates/voom-control-plane/src/cases/policy_inputs_test.rs crates/voom-plan/src/remux.rs crates/voom-plan/src/remux_test.rs crates/voom-plan/src/lib.rs
git commit -m "feat(plan): parse durable remux stream facts"
```

### Task 3: Planner Remux Grouping

**Files:**

- Modify: `crates/voom-plan/src/planner.rs`
- Modify: `crates/voom-plan/src/remux.rs`
- Test: `crates/voom-plan/src/planner_test.rs`
- Test: `crates/voom-plan/src/remux_test.rs`

- [ ] **Step 1: Write failing grouping tests**

Add tests proving same-phase operations group into one node and unsupported operations remain visible:

```rust
#[test]
fn groups_container_and_track_operations_into_one_remux_node() {
    let policy = compiled_policy_with_ops(vec![
        CompiledOperation::SetContainer { container: "mkv".to_owned() },
        CompiledOperation::KeepTracks {
            target: TrackTarget::Audio,
            filter: Some(TrackFilter::LanguageIn { values: vec!["eng".to_owned(), "und".to_owned()] }),
        },
        CompiledOperation::SetDefaults {
            target: TrackTarget::Audio,
            strategy: DefaultStrategy::First,
        },
    ]);

    let plan = generate_plan(request(policy, snapshot_mp4_with_video_audio_subtitle())).unwrap();

    assert_eq!(plan.nodes.len(), 1);
    assert_eq!(plan.nodes[0].operation_kind, "remux");
    assert_eq!(plan.nodes[0].status, NodeStatus::Planned);
    assert_eq!(plan.nodes[0].operation_payload["type"], "remux");
    assert_eq!(plan.nodes[0].operation_payload["container"], "mkv");
    assert_eq!(plan.nodes[0].operation_payload["track_actions"][0]["type"], "keep_tracks");
}

#[test]
fn container_mkv_alone_is_no_op_when_snapshot_is_already_mkv() {
    let policy = compiled_policy_with_ops(vec![
        CompiledOperation::SetContainer { container: "mkv".to_owned() },
    ]);

    let plan = generate_plan(request(policy, snapshot_mkv_with_video_audio_subtitle())).unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "remux");
    assert_eq!(plan.nodes[0].status, NodeStatus::NoOp);
    assert_eq!(plan.nodes[0].status_reason, "container is already mkv and track selection is unchanged");
}

#[test]
fn container_mkv_alone_plans_when_snapshot_container_is_not_mkv() {
    let policy = compiled_policy_with_ops(vec![
        CompiledOperation::SetContainer { container: "mkv".to_owned() },
    ]);

    let plan = generate_plan(request(policy, snapshot_mp4_with_video_audio_subtitle())).unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "remux");
    assert_eq!(plan.nodes[0].status, NodeStatus::Planned);
    assert_eq!(plan.nodes[0].status_reason, "container mp4 will be changed to mkv");
}

#[test]
fn container_mkv_alone_blocks_when_snapshot_container_is_unknown() {
    let policy = compiled_policy_with_ops(vec![
        CompiledOperation::SetContainer { container: "mkv".to_owned() },
    ]);
    let mut snapshot = snapshot_mp4_with_video_audio_subtitle();
    snapshot.container = None;

    let plan = generate_plan(request(policy, snapshot)).unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "remux");
    assert_eq!(plan.nodes[0].status, NodeStatus::Blocked);
    assert_eq!(plan.diagnostics[0].code, PlanningDiagnosticCode::InsufficientSnapshotFacts);
}

#[test]
fn intervening_non_remux_operation_does_not_split_same_phase_remux_group() {
    let policy = compiled_policy_with_ops(vec![
        CompiledOperation::SetContainer { container: "mkv".to_owned() },
        CompiledOperation::SetTag {
            key: "title".to_owned(),
            value: "Movie".to_owned(),
        },
        CompiledOperation::KeepTracks {
            target: TrackTarget::Audio,
            filter: Some(TrackFilter::LanguageIn { values: vec!["eng".to_owned()] }),
        },
    ]);

    let plan = generate_plan(request(policy, snapshot_mp4_with_video_audio_subtitle())).unwrap();

    let remux_nodes = plan
        .nodes
        .iter()
        .filter(|node| node.operation_kind == "remux")
        .collect::<Vec<_>>();
    assert_eq!(remux_nodes.len(), 1);
    assert_eq!(remux_nodes[0].operation_payload["track_actions"][0]["type"], "keep_tracks");
}

#[test]
fn remux_operations_in_different_phases_remain_separate_nodes() {
    let policy = compiled_policy_with_phases(vec![
        ("normalize", vec![CompiledOperation::SetContainer { container: "mkv".to_owned() }]),
        (
            "tracks",
            vec![CompiledOperation::KeepTracks {
                target: TrackTarget::Audio,
                filter: Some(TrackFilter::LanguageIn { values: vec!["eng".to_owned()] }),
            }],
        ),
    ]);

    let plan = generate_plan(request(policy, snapshot_mp4_with_video_audio_subtitle())).unwrap();

    let remux_nodes = plan
        .nodes
        .iter()
        .filter(|node| node.operation_kind == "remux")
        .collect::<Vec<_>>();
    assert_eq!(remux_nodes.len(), 2);
    assert_eq!(remux_nodes[0].phase_name, "normalize");
    assert_eq!(remux_nodes[1].phase_name, "tracks");
    assert!(plan.edges.iter().any(|edge| edge.from_node_id == remux_nodes[0].node_id && edge.to_node_id == remux_nodes[1].node_id));
}

#[test]
fn defaults_best_blocks_instead_of_joining_executable_group() {
    let policy = compiled_policy_with_ops(vec![
        CompiledOperation::SetContainer { container: "mkv".to_owned() },
        CompiledOperation::SetDefaults {
            target: TrackTarget::Audio,
            strategy: DefaultStrategy::Best,
        },
    ]);

    let plan = generate_plan(request(policy, snapshot_mp4_with_video_audio_subtitle())).unwrap();

    assert!(plan.nodes.iter().any(|node| node.operation_kind == "remux" && node.status == NodeStatus::Planned));
    assert!(plan.nodes.iter().any(|node| node.operation_kind == "set_defaults" && node.status == NodeStatus::Blocked));
}
```

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
cargo test -p voom-plan groups_container_and_track_operations_into_one_remux_node
cargo test -p voom-plan container_mkv_alone_is_no_op_when_snapshot_is_already_mkv
cargo test -p voom-plan container_mkv_alone_plans_when_snapshot_container_is_not_mkv
cargo test -p voom-plan container_mkv_alone_blocks_when_snapshot_container_is_unknown
cargo test -p voom-plan intervening_non_remux_operation_does_not_split_same_phase_remux_group
cargo test -p voom-plan remux_operations_in_different_phases_remain_separate_nodes
cargo test -p voom-plan defaults_best_blocks_instead_of_joining_executable_group
```

Expected: failure because `SetContainer` still expands as `set_container` and track operations are blocked as unsupported Sprint 5 operations.

- [ ] **Step 3: Add phase-level remux grouping**

Change `expand_operations_for_snapshot` so it scans all operations in a phase, collects remux-capable operations into one group per snapshot and phase, emits one grouped `remux` node, and emits separate blocked nodes for unsupported remux shapes. Keep conditional/rules expansion behavior by recursively flattening matched operation lists before grouping.

Use this grouping predicate:

```rust
fn remux_candidate_kind(operation: &CompiledOperation) -> Option<&'static str> {
    match operation {
        CompiledOperation::SetContainer { .. } => Some("set_container"),
        CompiledOperation::KeepTracks { .. } => Some("keep_tracks"),
        CompiledOperation::RemoveTracks { .. } => Some("remove_tracks"),
        CompiledOperation::ReorderTracks { .. } => Some("reorder_tracks"),
        CompiledOperation::SetDefaults { .. } => Some("set_defaults"),
        _ => None,
    }
}
```

Do not flush the remux group when a non-remux operation appears between remux operations in the same phase. Instead, collect the phase's supported remux operations by target, emit the grouped `remux` node at the ordinal of the first grouped operation, and expand non-remux operations as their own nodes with ordinals that preserve deterministic phase order. This preserves the Sprint 13 requirement that tag edits or other non-remux operations between track operations do not split same-phase same-target remux work.

- [ ] **Step 4: Build remux payload and status**

Emit payloads shaped like:

```json
{
  "type": "remux",
  "container": "mkv",
  "track_actions": [
    {
      "type": "keep_tracks",
      "target": "audio",
      "filter": {
        "type": "language_in",
        "values": ["eng", "und"]
      }
    }
  ],
  "track_order": ["video", "audio", "subtitle"],
  "defaults": [
    {
      "target": "audio",
      "strategy": "first"
    }
  ]
}
```

Set node status rules:

- `NoOp`: container already MKV and selector/default/order output equals snapshot.
- `Planned`: at least one supported remux operation changes the media shape.
- `Blocked`: the whole group cannot be evaluated because required facts are missing.

Keep unsupported shapes out of the executable group and emit blocked nodes for the original operation.

- [ ] **Step 5: Run and commit**

Run:

```bash
cargo test -p voom-plan planner_test
cargo test -p voom-plan remux_test
```

Expected: planner tests pass, including existing transcode tests unchanged and existing container tests updated from `set_container` expectations to grouped `remux` expectations.

Commit:

```bash
git add crates/voom-plan/src/planner.rs crates/voom-plan/src/planner_test.rs crates/voom-plan/src/remux.rs crates/voom-plan/src/remux_test.rs
git commit -m "feat(plan): group executable remux operations"
```

### Task 4: Compliance Bridge Remux Tickets

**Files:**

- Modify: `crates/voom-control-plane/src/workflow/policy_bridge.rs`
- Modify: `crates/voom-control-plane/src/workflow/binding.rs`
- Modify: `crates/voom-control-plane/src/workflow/ticket_payload.rs`
- Test: `crates/voom-control-plane/src/workflow/policy_bridge_test.rs`
- Test: `crates/voom-control-plane/src/workflow/binding_test.rs`
- Test: `crates/voom-control-plane/src/workflow/executor_test.rs`

- [ ] **Step 1: Write failing bridge tests**

Add tests equivalent to transcode bridge tests:

```rust
#[test]
fn bridge_maps_planned_remux_with_policy_target_and_payload() {
    let plan = plan(vec![node_with_payload(
        "remux",
        NodeStatus::Planned,
        serde_json::json!({
            "type": "remux",
            "container": "mkv",
            "track_actions": [],
            "track_order": ["video", "audio", "subtitle"],
            "defaults": []
        }),
    )]);
    let report = compliance_report_for(&plan);

    let bridged = workflow_plan_from_compliance(&plan, &report).unwrap();
    let workflow = bridged.workflow.unwrap();

    assert_eq!(workflow.nodes[0].operation(), OperationKind::Remux);
    assert!(workflow.nodes[0].policy_target().is_some());
    assert_eq!(workflow.nodes[0].operation_payload()["type"], "remux");
    assert_eq!(bridged.summary.per_operation["remux"], 1);
}
```

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
cargo test -p voom-control-plane bridge_maps_planned_remux_with_policy_target_and_payload
```

Expected: bridge either treats planned `remux` as unsupported or drops target/payload.

- [ ] **Step 3: Route `remux` plan nodes to `OperationKind::Remux`**

In `policy_bridge.rs`, replace the old `set_container` remux bridge with:

```rust
NodeStatus::Planned if node.operation_kind == "remux" => {
    nodes.push(WorkflowNode::Operation(OperationNode {
        id: format!("policy-node_{}", node.node_id),
        operation: OperationKind::Remux,
        policy_target: Some(node.target.clone()),
        operation_payload: node.operation_payload.clone(),
        depends_on: Vec::new(),
        depends_on_selected: Vec::new(),
        provides_selected: None,
    }));
    summary.submitted_node_count += 1;
    *summary.per_operation.entry("remux".to_owned()).or_insert(0) += 1;
}
```

- [ ] **Step 4: Add remux payload rendering**

Add a `PolicyRemuxSource` and renderer next to transcode:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyRemuxSource {
    pub file_version_id: FileVersionId,
    pub location_id: Option<FileLocationId>,
}

pub fn render_policy_remux_payload(
    source: PolicyRemuxSource,
    operation_payload: &Value,
    staging_root: &Path,
    target_dir: &Path,
    timing: EffectiveTiming,
) -> Result<Value, BindingError> {
    if operation_payload.get("type").and_then(Value::as_str) != Some("remux") {
        return Err(BindingError::new("remux payload missing `type: remux`"));
    }
    let mut payload = serde_json::json!({
        "operation": "remux",
        "remux": operation_payload,
        "staging_root": staging_root,
        "target_dir": target_dir,
        "duration_ms": timing.duration_ms,
        "progress_interval_ms": timing.progress_interval_ms,
    });
    let object = payload.as_object_mut().expect("payload literal is object");
    object.insert("source_file_version_id".to_owned(), serde_json::json!(source.file_version_id));
    if let Some(location_id) = source.location_id {
        object.insert("source_location_id".to_owned(), serde_json::json!(location_id));
    }
    Ok(payload)
}
```

- [ ] **Step 5: Run and commit**

Run:

```bash
cargo test -p voom-control-plane workflow::policy_bridge
cargo test -p voom-control-plane workflow::binding
```

Expected: bridge and binding tests pass.

Commit:

```bash
git add crates/voom-control-plane/src/workflow
git commit -m "feat(control-plane): bridge remux policy tickets"
```

### Task 5: MKVToolNix Worker Crate

**Files:**

- Create: `crates/voom-mkvtoolnix-worker/Cargo.toml`
- Create: `crates/voom-mkvtoolnix-worker/src/lib.rs`
- Create: `crates/voom-mkvtoolnix-worker/src/main.rs`
- Create: `crates/voom-mkvtoolnix-worker/src/preflight.rs`
- Create: `crates/voom-mkvtoolnix-worker/src/observe.rs`
- Create: `crates/voom-mkvtoolnix-worker/src/mkvmerge.rs`
- Create: `crates/voom-mkvtoolnix-worker/src/handler.rs`
- Create sibling tests for each module.
- Modify: `Cargo.toml`

- [ ] **Step 1: Add crate skeleton tests**

Write tests first for preflight parsing, provider-index mapping, and handler rejection:

```rust
#[test]
fn parses_supported_mkvmerge_version() {
    let version = parse_mkvmerge_version("mkvmerge v80.0 ('Roundabout') 64-bit").unwrap();
    assert_eq!(version.major, 80);
}

#[test]
fn rejects_unsupported_mkvmerge_version() {
    let err = parse_mkvmerge_version("mkvmerge v40.0").unwrap_err();
    assert!(err.to_string().contains("unsupported mkvmerge version"));
}

#[test]
fn maps_snapshot_provider_indexes_to_mkvmerge_track_ids() {
    let identify = serde_json::json!({
        "tracks": [
            {"id": 7, "type": "video", "properties": {"number": 1}},
            {"id": 12, "type": "audio", "properties": {"number": 2}},
            {"id": 14, "type": "subtitles", "properties": {"number": 3}}
        ]
    });

    let mapping = track_mapping_from_identify(&identify).unwrap();

    assert_eq!(mapping.mkvmerge_track_id_for_provider_index(0), Some(7));
    assert_eq!(mapping.mkvmerge_track_id_for_provider_index(1), Some(12));
    assert_eq!(mapping.mkvmerge_track_id_for_provider_index(2), Some(14));
}
```

```rust
#[tokio::test]
async fn handler_rejects_existing_output() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.mp4");
    let output = temp.path().join("stage").join("out.mkv");
    tokio::fs::create_dir_all(output.parent().unwrap()).await.unwrap();
    tokio::fs::write(&input, b"not real media").await.unwrap();
    tokio::fs::write(&output, b"stale").await.unwrap();

    let request = request_for_paths(&input, temp.path().join("stage"), &output);
    let err = handle_remux(&request, &MkvmergeConfig::for_tests()).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("output path already exists"));
}
```

Add the rest of the worker conformance tests before implementation:

```rust
#[tokio::test]
async fn handler_rejects_missing_input_with_artifact_unavailable() {
    let temp = tempfile::tempdir().unwrap();
    let request = request_for_paths(
        &temp.path().join("missing.mp4"),
        temp.path().join("stage"),
        &temp.path().join("stage/out.mkv"),
    );

    let err = handle_remux(&request, &MkvmergeConfig::for_tests()).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ArtifactUnavailable);
}

#[tokio::test]
async fn handler_rejects_output_path_escape() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.mp4");
    tokio::fs::write(&input, b"input").await.unwrap();
    let request = request_for_paths(&input, temp.path().join("stage"), &temp.path().join("out.mkv"));

    let err = handle_remux(&request, &MkvmergeConfig::for_tests()).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("escapes staging root"));
}

#[tokio::test]
async fn handler_rejects_no_video_selection() {
    let request = request_with_selection(RemuxSelection {
        keep_streams: vec![audio_ref("stream-1", 1)],
        default_streams: vec![],
        clear_default_streams: vec![],
        track_order: vec![RemuxTrackGroup::Audio],
    });

    let err = handle_remux(&request, &MkvmergeConfig::for_tests()).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("at least one video stream"));
}

#[tokio::test]
async fn handler_rejects_input_drift_after_provider_run() {
    let fixture = remux_fixture_with_fake_mkvmerge_that_mutates_input().await;

    let err = handle_remux(&fixture.request, &fixture.config).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ArtifactChecksumMismatch);
}

#[tokio::test]
async fn handler_rejects_selected_stream_mismatch() {
    let fixture = remux_fixture_with_output_probe(vec!["stream-0"], vec![]);

    let err = handle_remux(&fixture.request, &fixture.config).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::MalformedWorkerResult);
    assert!(err.to_string().contains("selected stream mismatch"));
}

#[tokio::test]
async fn handler_rejects_default_track_mismatch() {
    let fixture = remux_fixture_with_output_probe(vec!["stream-0", "stream-1"], vec![]);

    let err = handle_remux(&fixture.request, &fixture.config).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::MalformedWorkerResult);
    assert!(err.to_string().contains("default stream mismatch"));
}

#[tokio::test]
async fn handler_rejects_non_mkv_output_facts() {
    let fixture = remux_fixture_with_output_container("mp4");

    let err = handle_remux(&fixture.request, &fixture.config).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::MalformedWorkerResult);
    assert!(err.to_string().contains("output container"));
}
```

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
cargo test -p voom-mkvtoolnix-worker
```

Expected: crate does not exist.

- [ ] **Step 3: Add crate and preflight**

Follow `crates/voom-ffmpeg-worker` structure. `preflight.rs` exposes:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MkvmergeConfig {
    pub command: PathBuf,
    pub provider_version: String,
    pub timeout: Duration,
}

pub fn preflight_mkvmerge(command: &Path) -> Result<MkvmergeConfig, MkvtoolnixError>;
pub fn parse_mkvmerge_version(output: &str) -> Result<MkvmergeVersion, MkvtoolnixError>;
```

Reject missing, non-executable, and versions below the fixed minimum chosen for Sprint 13 command support.

- [ ] **Step 4: Implement request validation and path hardening**

Mirror transcode handler checks:

- `request.output.overwrite` must be false.
- `request.output.container` must be supported MKV.
- staging root and output parent must be existing non-symlink canonical directories.
- output parent must be under staging root.
- output path must not already exist.
- selection must include at least one video stream.
- duplicate `snapshot_stream_id` or duplicate provider index in keep/default lists is `CONFIG_INVALID`.

- [ ] **Step 5: Implement deterministic mkvmerge command builder**

Build arguments from typed fields only:

```rust
pub fn build_mkvmerge_args(
    request: &RemuxRequest,
    mapping: &MkvmergeTrackMapping,
) -> Result<Vec<String>, MkvtoolnixError> {
    let mut args = vec![
        "--output".to_owned(),
        request.output.path.clone(),
        "--no-global-tags".to_owned(),
    ];
    args.extend(track_selection_args(&request.selection, mapping)?);
    args.push(request.input.path.clone());
    Ok(args)
}
```

Use MKVToolNix track selectors derived from the identify mapping, not from ffprobe stream indexes directly: keep-list arguments are built by mapping `RemuxSelection.keep_streams[*].provider_stream_index` to MKVToolNix track IDs, default-track flags are built from `default_streams` and `clear_default_streams` through the same mapping, and track order is built from `track_order` after mapping the selected streams into group order. Missing mappings are `CONFIG_INVALID`. Do not add any API surface that accepts raw provider arguments.

- [ ] **Step 6: Implement handler success path**

`handle_remux` sequence:

1. Validate request.
2. Observe input facts and compare to expected.
3. Run `mkvmerge --identify --identification-format json <input>` and build a mapping from durable request `provider_stream_index` values to MKVToolNix track IDs.
4. Build the `mkvmerge` command from that mapping and spawn it with timeout.
5. Observe input facts again and require no drift.
6. Observe output facts.
7. Run provider-local output validation.
8. Return `RemuxResult` echoing kept/default snapshot stream IDs.

- [ ] **Step 7: Run and commit**

Run:

```bash
cargo test -p voom-mkvtoolnix-worker
```

Expected: worker tests pass. Required binary-backed tests fail clearly if `mkvmerge` is unavailable.

Commit:

```bash
git add Cargo.toml crates/voom-mkvtoolnix-worker
git commit -m "feat(worker): add mkvtoolnix remux worker"
```

### Task 6: Control-Plane Remux Use Case

**Files:**

- Create: `crates/voom-control-plane/src/remux/mod.rs`
- Create: `crates/voom-control-plane/src/remux/source.rs`
- Create: `crates/voom-control-plane/src/remux/selection.rs`
- Create: `crates/voom-control-plane/src/remux/stage.rs`
- Create: `crates/voom-control-plane/src/remux/dispatch.rs`
- Create: `crates/voom-control-plane/src/remux/commit.rs`
- Create: `crates/voom-control-plane/src/remux/events.rs`
- Create sibling tests.
- Modify: `crates/voom-control-plane/src/lib.rs`

- [ ] **Step 1: Write failing unit tests for source, stage, and selection**

Start with these tests:

```rust
#[tokio::test]
async fn staging_path_includes_ticket_and_lease() {
    let root = tempfile::tempdir().unwrap();
    let source = PathBuf::from("/library/Movie.mp4");

    let path = staging_path(root.path(), TicketId(10), LeaseId(20), &source).await.unwrap();

    assert!(path.starts_with(root.path().canonicalize().unwrap()));
    assert!(path.to_string_lossy().contains("ticket-10"));
    assert!(path.to_string_lossy().contains("lease-20"));
    assert!(path.ends_with("Movie.remux.mkv"));
}

#[test]
fn selection_preserves_video_and_applies_audio_keep() {
    let payload = serde_json::json!({
        "type": "remux",
        "container": "mkv",
        "track_actions": [{"type": "keep_tracks", "target": "audio", "filter": {"type": "language_in", "values": ["eng"]}}],
        "track_order": ["video", "audio", "subtitle"],
        "defaults": [{"target": "audio", "strategy": "first"}]
    });
    let snapshot = snapshot_with_video_audio_languages(["eng", "spa"]);

    let selection = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap();

    assert_eq!(selection.keep_streams.iter().map(|s| s.snapshot_stream_id.as_str()).collect::<Vec<_>>(), vec!["stream-0", "stream-1"]);
    assert_eq!(selection.default_streams[0].snapshot_stream_id, "stream-1");
}
```

Add focused failure tests before implementation:

```rust
#[tokio::test]
async fn source_selection_rejects_ambiguous_live_locations() {
    let fixture = source_fixture_with_two_live_locations().await;

    let err = source::select_source(&fixture.cp, fixture.file_version_id, None)
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("multiple live local source locations"));
}

#[tokio::test]
async fn staging_path_rejects_existing_output() {
    let root = tempfile::tempdir().unwrap();
    let path = stage::staging_path(root.path(), TicketId(10), LeaseId(20), Path::new("/library/Movie.mp4"))
        .await
        .unwrap();
    tokio::fs::write(&path, b"stale").await.unwrap();

    let err = stage::staging_path(root.path(), TicketId(10), LeaseId(20), Path::new("/library/Movie.mp4"))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("staging path already exists"));
}

#[tokio::test]
async fn target_path_rejects_existing_output() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("Movie.remux.mkv");
    tokio::fs::write(&target, b"existing").await.unwrap();

    let err = stage::target_path(root.path(), Path::new("/library/Movie.mp4"))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("target path already exists"));
}

#[test]
fn selection_rejects_keep_remove_video_policy() {
    let payload = serde_json::json!({
        "type": "remux",
        "container": "mkv",
        "track_actions": [{"type": "remove_tracks", "target": "video", "filter": null}],
        "track_order": ["video", "audio"],
        "defaults": []
    });
    let snapshot = snapshot_with_video_audio_languages(["eng"]);

    let err = selection_from_payload_and_snapshot(&payload, &snapshot).unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("video track policy is unsupported"));
}

#[tokio::test]
async fn result_probe_failure_preserves_committed_result_ids_in_error() {
    let fixture = remux_fixture_with_successful_commit_and_failed_probe().await;

    let err = execute_remux_with_dispatchers(&fixture.cp, fixture.input, &fixture.remux, &fixture.verify)
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ExternalSystemUnavailable);
    assert!(err.to_string().contains("result_file_version_id"));
    assert!(err.to_string().contains("result_file_location_id"));
    assert!(err.to_string().contains("commit_record_id"));
}
```

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
cargo test -p voom-control-plane remux::
```

Expected: compile failure because remux module does not exist.

- [ ] **Step 3: Add public execution input/report**

Create the use-case API:

```rust
#[derive(Debug, Clone)]
pub struct ExecuteRemuxInput {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: LeaseId,
    pub source_file_version_id: FileVersionId,
    pub source_location_id: Option<FileLocationId>,
    pub operation_payload: serde_json::Value,
    pub staging_root: PathBuf,
    pub target_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecuteRemuxReport {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: LeaseId,
    pub source_file_version_id: FileVersionId,
    pub source_file_location_id: FileLocationId,
    pub staged_artifact_handle_id: ArtifactHandleId,
    pub staged_artifact_location_id: ArtifactLocationId,
    pub verification_id: ArtifactVerificationId,
    pub commit_record_id: ArtifactCommitRecordId,
    pub result_file_version_id: FileVersionId,
    pub result_file_location_id: FileLocationId,
    pub result_media_snapshot_id: MediaSnapshotId,
    pub staging_path: PathBuf,
    pub target_path: PathBuf,
}
```

- [ ] **Step 4: Implement use-case sequence**

Mirror `execute_transcode_video_with_dispatchers`. `selection::selection_from_payload_and_snapshot` must call the public selector helpers from `voom_plan::remux` instead of reimplementing filter/default/order semantics in control-plane code:

```rust
pub(crate) async fn execute_remux_with_dispatchers(
    cp: &ControlPlane,
    input: ExecuteRemuxInput,
    remux: &dyn RemuxDispatcher,
    verify: &dyn VerifyArtifactDispatcher,
) -> Result<ExecuteRemuxReport, VoomError> {
    let selected = source::select_source(cp, input.source_file_version_id, input.source_location_id).await?;
    let snapshot = source::read_media_snapshot(cp, input.source_file_version_id).await?;
    let selection = selection::selection_from_payload_and_snapshot(&input.operation_payload, &snapshot)?;
    let staging_path = stage::staging_path(&input.staging_root, input.ticket_id, input.lease_id, &selected.location.value).await?;
    let target_path = stage::target_path(&input.target_dir, &selected.location.value).await?;

    events::record_started(cp, &input, selected.location.id, &selection, &staging_path).await?;
    let request = dispatch::request_for(&selected, &selection, &input.staging_root, &staging_path)?;
    let result = remux.dispatch_remux(request).await?;
    dispatch::validate_result(&selection, &result)?;
    dispatch::require_output_file_matches_result(&staging_path, &result).await?;

    let staged = commit::record_staged_remux(cp, &input, selected.location.id, &staging_path, &result).await?;
    let verified = verify_artifact_with_dispatcher(cp, VerifyArtifactInput { artifact_handle_id: staged.artifact_handle_id }, verify, &NoVerifyArtifactHooks).await?;
    if verified.status != ArtifactVerificationStatus::Succeeded {
        return Err(VoomError::VerificationFailure(format!("remux artifact verification failed for {}", staged.artifact_handle_id)));
    }
    let committed = cp.commit_artifact(CommitArtifactInput { artifact_handle_id: staged.artifact_handle_id, target_path: target_path.clone() }).await.map_err(|err| VoomError::CommitFailure(err.to_string()))?;
    let result_file_version_id = committed.result_file_version_id.ok_or_else(|| VoomError::Internal("committed remux missing result_file_version_id".to_owned()))?;
    let result_file_location_id = committed.result_file_location_id.ok_or_else(|| VoomError::Internal("committed remux missing result_file_location_id".to_owned()))?;
    let snapshot = commit::record_result_snapshot(cp, result_file_version_id, &result).await?;
    events::record_succeeded(cp, &input, selected.location.id, staged.artifact_handle_id, staged.artifact_location_id, &result).await?;

    Ok(ExecuteRemuxReport {
        job_id: input.job_id,
        ticket_id: input.ticket_id,
        lease_id: input.lease_id,
        source_file_version_id: input.source_file_version_id,
        source_file_location_id: selected.location.id,
        staged_artifact_handle_id: staged.artifact_handle_id,
        staged_artifact_location_id: staged.artifact_location_id,
        verification_id: verified.verification_id,
        commit_record_id: committed.commit_record_id,
        result_file_version_id,
        result_file_location_id,
        result_media_snapshot_id: snapshot.id,
        staging_path,
        target_path,
    })
}
```

- [ ] **Step 5: Run and commit**

Run:

```bash
cargo test -p voom-control-plane remux::
```

Expected: remux module unit tests pass.

Commit:

```bash
git add crates/voom-control-plane/src/remux crates/voom-control-plane/src/lib.rs
git commit -m "feat(control-plane): execute remux artifacts"
```

### Task 7: Workflow Executor Remux Routing

**Files:**

- Modify: `crates/voom-control-plane/src/workflow/executor.rs`
- Modify: `crates/voom-control-plane/src/workflow/binding.rs`
- Modify: `crates/voom-control-plane/src/workflow/runtime.rs`
- Test: `crates/voom-control-plane/src/workflow/executor_test.rs`

- [ ] **Step 1: Write failing executor tests**

Add a test like the existing policy transcode routing test:

```rust
#[tokio::test]
async fn policy_remux_ticket_runs_real_remux_path() {
    let (cp, source) = seeded_control_plane_with_source_file_and_snapshot().await;
    let plan = workflow_plan_with_policy_remux(source.file_version_id);
    let runtimes = WorkerRuntimeRegistry::default();
    let executor = WorkflowExecutor::with_options(cp.clone(), SingleWorkerPerKindSelector::default(), runtimes, WorkflowExecutorOptions::for_tests());

    let err = executor.submit_and_run(plan).await.unwrap_err();

    assert!(err.source.to_string().contains("mkvtoolnix"));
}
```

Then add a fake-runtime test that succeeds without requiring the binary:

```rust
#[tokio::test]
async fn policy_remux_success_reports_operation_summary() {
    let fixture = remux_executor_fixture_with_fake_runtime().await;
    let summary = fixture.executor.submit_and_run(fixture.plan).await.unwrap();

    assert_eq!(summary.operation_count(OperationKind::Remux), 1);
    assert_eq!(summary.failure_count, 0);
}
```

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
cargo test -p voom-control-plane policy_remux_
```

Expected: `OperationKind::Remux` still dispatches synthetic runtime payloads or lacks source resolution.

- [ ] **Step 3: Add remux options and source resolution**

Extend `WorkflowExecutorOptions`:

```rust
pub remux_staging_root: PathBuf,
pub remux_target_dir: PathBuf,
```

Set defaults:

```rust
remux_staging_root: PathBuf::from("/tmp/voom/remux/staging"),
remux_target_dir: PathBuf::from("/tmp/voom/remux/output"),
```

Add `resolve_policy_remux_source` with the same target handling as transcode, but error messages must say `remux`.

- [ ] **Step 4: Route leased remux tickets to the real use case**

Add `RuntimeRemuxDispatcher` next to `RuntimeTranscodeDispatcher` so policy remux execution dispatches through the selected leased worker runtime:

```rust
struct RuntimeRemuxDispatcher<'a> {
    runtime: &'a super::runtime::WorkerRuntime,
    control: &'a ControlPlane,
    lease_id: LeaseId,
    options: &'a WorkflowExecutorOptions,
}

#[async_trait::async_trait]
impl RemuxDispatcher for RuntimeRemuxDispatcher<'_> {
    async fn dispatch_remux(
        &self,
        request: RemuxRequest,
    ) -> Result<RemuxResult, VoomError> {
        await_with_lease_heartbeats(
            self.control,
            self.lease_id,
            self.options,
            crate::remux::dispatch::dispatch_remux_with_client(
                self.runtime.client.as_ref(),
                &self.runtime.credentials,
                request,
            ),
        )
        .await
    }
}
```

In the leased-ticket dispatch path, when `workflow_payload.operation == OperationKind::Remux` and the rendered payload contains `source_file_version_id`, call:

```rust
execute_remux_with_dispatchers(
    &self.control_plane,
    ExecuteRemuxInput {
        job_id,
        ticket_id: ticket.id,
        lease_id: lease.id,
        source_file_version_id,
        source_location_id,
        operation_payload,
        staging_root,
        target_dir,
    },
    &RuntimeRemuxDispatcher {
        runtime,
        control: &self.control_plane,
        lease_id: lease.id,
        options: &self.options,
    },
    &crate::artifact::verify::BundledVerifyArtifactDispatcher,
)
.await
```

Keep synthetic `OperationKind::Remux` runtime dispatch for non-policy workflow nodes.

- [ ] **Step 5: Run and commit**

Run:

```bash
cargo test -p voom-control-plane workflow::executor
```

Expected: executor tests pass.

Commit:

```bash
git add crates/voom-control-plane/src/workflow
git commit -m "feat(workflow): route policy remux execution"
```

### Task 8: Events And CLI Reporting

**Files:**

- Modify: `crates/voom-events/src/kind.rs`
- Modify: `crates/voom-events/src/payload.rs`
- Test: `crates/voom-events/src/kind_test.rs`
- Test: `crates/voom-events/src/payload_test.rs`
- Modify: `crates/voom-control-plane/src/remux/events.rs`
- Modify: `crates/voom-cli/src/commands/compliance.rs`
- Test: `crates/voom-cli/tests/compliance_envelope.rs`
- Update: `crates/voom-cli/tests/snapshots/*.snap`

- [ ] **Step 1: Write failing event payload tests**

Add event kind and payload tests:

```rust
#[test]
fn remux_event_kinds_are_stable() {
    assert_eq!(EventKind::ArtifactRemuxStarted.as_str(), "artifact.remux_started");
    assert_eq!(EventKind::ArtifactRemuxProgress.as_str(), "artifact.remux_progress");
    assert_eq!(EventKind::ArtifactRemuxSucceeded.as_str(), "artifact.remux_succeeded");
    assert_eq!(EventKind::ArtifactRemuxFailed.as_str(), "artifact.remux_failed");
}
```

Payload must include job, ticket, lease, source IDs, selected/default stream IDs, staging/artifact IDs when known, provider facts when known, and public error code on failure.

- [ ] **Step 2: Run event tests to verify failure**

Run:

```bash
cargo test -p voom-events remux
```

Expected: event variants do not exist.

- [ ] **Step 3: Add typed event payloads and recorders**

Add payload structs with `serde(deny_unknown_fields)` and update `remux/events.rs` so start/success/failure events are appended through the existing event log path. `artifact.remux_progress` is emitted only when worker progress is available; lack of progress is not a failure.

- [ ] **Step 4: Add CLI envelope coverage**

Extend compliance execute output so successful remux execution includes:

- policy version and input set IDs
- report ID and job ID
- ticket ID
- source file version/location IDs
- staged artifact handle/location IDs
- verification ID
- commit record ID
- result file version/location IDs
- result media snapshot ID

Add one successful insta test with a fake dispatcher and one failure snapshot for existing target path or missing facts.

- [ ] **Step 5: Run and commit**

Run:

```bash
cargo test -p voom-events remux
cargo test -p voom-cli compliance_envelope
```

Review intentional snapshot changes:

```bash
cargo insta review
```

Commit:

```bash
git add crates/voom-events crates/voom-control-plane/src/remux/events.rs crates/voom-cli
git commit -m "feat(cli): report remux execution ids"
```

### Task 9: Integration Fixtures And Closeout Evidence

**Files:**

- Create: `crates/voom-control-plane/tests/remux_flow.rs`
- Modify: `crates/voom-plan/fixtures/plans/*.json`
- Modify: `crates/voom-plan/fixtures/reports/*.json`
- Create: `docs/superpowers/specs/2026-05-25-voom-sprint-13-closeout.md`

- [ ] **Step 1: Add end-to-end integration test**

Create an integration test that:

1. Initializes an ephemeral SQLite database.
2. Generates or copies a small media fixture with at least one video stream and two audio/subtitle facts.
3. Runs scan/probe persistence.
4. Stores a policy containing `container mkv`, audio keep, subtitle remove, order, and defaults.
5. Plans and executes compliance.
6. Asserts staged artifact, verification, commit, result location, and result media snapshot rows exist.

Use this policy text:

```text
policy "remux track selection" {
  phase normalize {
    container mkv
    keep audio where lang in [eng, und]
    remove subtitle where forced
    order tracks [video, audio, subtitle]
    defaults audio: first
    defaults subtitle: none
  }
}
```

- [ ] **Step 2: Run targeted integration test**

Run:

```bash
cargo test -p voom-control-plane --test remux_flow
```

Expected: pass when required MKVToolNix binaries are installed; fail with explicit setup diagnostics when they are absent.

- [ ] **Step 3: Update plan/report fixtures**

Run the fixture update path already used by `voom-plan` tests. Then run:

```bash
cargo test -p voom-plan fixtures_test
```

Expected: fixture inventory and deterministic JSON pass.

- [ ] **Step 4: Write closeout matrix**

Create `docs/superpowers/specs/2026-05-25-voom-sprint-13-closeout.md` with this structure and fill every row with the command and evidence file/test name:

```markdown
# VOOM Sprint 13 Closeout

| Requirement | Evidence |
|---|---|
| DSL accepts supported container and track policy shapes | `cargo test -p voom-policy` |
| Planner groups same-phase remux operations | `cargo test -p voom-plan groups_container_and_track_operations_into_one_remux_node` |
| Unsupported or missing facts block visibly | `cargo test -p voom-plan defaults_best_blocks_instead_of_joining_executable_group` |
| Worker preflight fails loudly | `cargo test -p voom-mkvtoolnix-worker preflight` |
| Worker writes staged MKV only | `cargo test -p voom-mkvtoolnix-worker handler` |
| Control plane verifies, commits, and probes result | `cargo test -p voom-control-plane --test remux_flow` |
| CLI emits stable JSON envelopes | `cargo test -p voom-cli compliance_envelope` |
| Full suite passes | `just ci` |
```

- [ ] **Step 5: Run full verification and commit**

Run:

```bash
just fmt
just ci
```

Expected: full CI suite passes. If `mkvmerge` is missing, install/setup the required binary and rerun; do not skip required tests.

Commit:

```bash
git add crates/voom-control-plane/tests/remux_flow.rs crates/voom-plan/fixtures docs/superpowers/specs/2026-05-25-voom-sprint-13-closeout.md
git commit -m "test: close out sprint 13 remux flow"
```

## Self-Review

Spec coverage:

- Policy/planning grouping is covered by Tasks 2 and 3.
- Typed worker protocol is covered by Task 1.
- MKVToolNix preflight and execution are covered by Task 5.
- Durable ticket bridge and executor routing are covered by Tasks 4 and 7.
- Control-plane source revalidation, staging, verification, commit, result snapshot, and events are covered by Tasks 6 and 8.
- CLI stable IDs and closeout evidence are covered by Tasks 8 and 9.

Conflict surfaced:

- The design requires selector evaluation against durable stream facts, while the current `MediaSnapshotInput` model exposes stream facts through normalized JSON payload rather than typed fields. This plan centralizes stream parsing in `voom-plan::remux` and requires control-plane remux selection to call those public helpers; a separate schema/model cleanup can promote streams to first-class typed policy input fields after Sprint 13 behavior is locked.

Verification commands:

- Use targeted `cargo test -p <crate> <test_name>` commands after each task.
- Use `just fmt` before final CI.
- Use `just ci` before declaring Sprint 13 complete.
