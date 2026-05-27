# VOOM Sprint 14 Audio Transcode And Extract Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement policy-driven audio transcode and exactly-one audio extraction through durable workflow tickets, FFmpeg workers, staged artifact verification, add-only commits, sidecar bundle registration, and stable CLI reports.

**Architecture:** Keep workers out of the database: the FFmpeg worker writes one staged output and returns typed facts, while the control plane owns source revalidation, selector re-evaluation, artifact rows, verification, file commits, bundle membership, lineage, events, and reports. Add a focused `audio` control-plane module for Sprint 14 rather than generalizing remux/transcode prematurely; extract shared helpers only when a task needs the same behavior in two production paths.

**Tech Stack:** Rust workspace, tokio, sqlx/SQLite, serde JSON, FFmpeg/ffprobe, existing worker protocol transport, existing durable workflow executor, insta snapshots, `just`.

---

## Assumptions And Success Criteria

Assumptions:

- The approved design is `docs/superpowers/specs/2026-05-26-voom-sprint-14-design.md`.
- `OperationKind::ExtractAudio` already exists. Sprint 14 adds `OperationKind::TranscodeAudio`.
- The existing `voom-ffmpeg-worker` remains the FFmpeg provider crate; add audio handlers there instead of creating a second FFmpeg worker.
- The policy parser stores phase operations as raw statement text. Sprint 14 can add audio operations by extending validation and lowering in `voom-policy/src/compiled.rs`.
- Existing media snapshots store normalized stream facts under `payload["streams"]`. Planning and execution must block when language, codec, channel count, title, commentary, default/disposition, stream ID, or provider index facts needed by the selected operation are absent.
- Extraction V1 emits exactly one Opus/Ogg sidecar. Broad selectors that match zero or multiple audio streams fail visibly.
- Audio extraction requires the source asset to already be an `asset_bundle_members.role = 'primary_video'` member.

Success criteria:

- Supported policy text compiles to typed `transcode_audio` and `extract_audio` operations.
- The planner emits executable `transcode_audio` nodes for selected non-compliant audio streams, no-op nodes for already-compliant selected streams, and blocked nodes for missing facts or zero matches.
- The planner emits executable `extract_audio` nodes only when exactly one audio stream matches and commentary state is known.
- Compliance execution submits real durable workflow tickets for both audio operations.
- The FFmpeg worker rejects malformed requests, overwrite, path escape, source drift, unsupported codec/container facts, and preservation mismatches before reporting success.
- Audio transcode commits a same-file successor, probes the committed result, and validates selected stream preservation.
- Audio extraction commits a new sidecar asset/version/location, records cross-asset lineage via artifact commit records and reports, and adds `commentary_audio` or `external_audio` bundle membership.
- Post-commit extraction bundle-registration failure returns a recovery report with sidecar and bundle IDs.
- CLI envelopes remain single JSON stdout documents.
- `just ci` passes.

## File Map

Policy and planner:

- Modify `crates/voom-policy/src/compiled.rs`, `validate.rs`, and sibling tests for `CompiledOperation::TranscodeAudio` and `CompiledOperation::ExtractAudio`.
- Add policy fixtures under `crates/voom-policy/fixtures/policies/` and `crates/voom-policy/fixtures/compiled/`.
- Create `crates/voom-plan/src/audio.rs` and `audio_test.rs` for audio operation payload parsing, stream fact extraction, selector evaluation, role derivation, and planning shape decisions.
- Modify `crates/voom-plan/src/lib.rs`, `planner.rs`, `planner_test.rs`, `compliance_report.rs`, and `compliance_report_test.rs`.

Worker protocol and FFmpeg worker:

- Create `crates/voom-worker-protocol/src/audio.rs` and `audio_test.rs`; modify `lib.rs` and `operation_kind.rs`.
- Modify `crates/voom-ffmpeg-worker/src/ffmpeg.rs`, `handler.rs`, `preflight.rs`, `main.rs`, and sibling tests.

Persistence and events:

- Add migration `migrations/0013_audio_sidecar_support.sql`.
- Modify `crates/voom-store/src/repo/bundles.rs`, `bundles_test.rs`, `identity.rs`, `identity_test.rs`, `artifacts.rs`, and `artifacts_test.rs`.
- Keep filesystem promotion in `voom-control-plane`; store repository helpers may record rows inside a transaction but must not move, copy, or delete staged media files.
- Modify `crates/voom-events/src/kind.rs`, `payload.rs`, and sibling tests for audio events.

Control plane and workflow:

- Create `crates/voom-control-plane/src/audio/mod.rs`, `source.rs`, `selection.rs`, `stage.rs`, `dispatch.rs`, `commit.rs`, `events.rs`, and sibling tests.
- Modify `crates/voom-control-plane/src/lib.rs`.
- Modify `crates/voom-control-plane/src/workflow/binding.rs`, `policy_bridge.rs`, `executor.rs`, `ticket_payload.rs`, and sibling tests.
- Modify `crates/voom-control-plane/src/cases/compliance.rs` and `compliance_test.rs`.

Integration, CLI, docs:

- Add integration tests under `crates/voom-control-plane/tests/audio_transcode_flow.rs` and `audio_extract_flow.rs`.
- Modify `crates/voom-cli/src/commands/compliance.rs` when its current serialization does not pass through the required audio report fields.
- Add/update insta snapshots under `crates/voom-cli/tests/snapshots/`.
- Add closeout evidence `docs/superpowers/specs/2026-05-26-voom-sprint-14-closeout.md`.

---

### Task 1: Policy Audio Operations

**Files:**

- Modify: `crates/voom-policy/src/compiled.rs`
- Modify: `crates/voom-policy/src/compiled_test.rs`
- Modify: `crates/voom-policy/src/validate.rs`
- Modify: `crates/voom-policy/src/validate_test.rs`
- Add: `crates/voom-policy/fixtures/policies/audio-transcode-extract.voom`
- Add: `crates/voom-policy/fixtures/compiled/audio-transcode-extract.json`
- Modify: `crates/voom-policy/src/policy_fixtures.rs`

- [ ] **Step 1: Write failing compiler tests**

Add tests that lock accepted audio lowering:

```rust
#[test]
fn compile_policy_lowers_audio_transcode_and_extract() {
    let out = compile_policy(
        "policy \"p\" { phase a {
            transcode audio to aac where lang in [eng, und]
            extract audio where commentary
        } }",
    )
    .unwrap();

    let operations = &out.phases[0].operations;
    assert_eq!(
        operations[0],
        CompiledOperation::TranscodeAudio {
            target_codec: "aac".to_owned(),
            container: "mkv".to_owned(),
            filter: Some(TrackFilter::LanguageIn {
                values: vec!["eng".to_owned(), "und".to_owned()],
            }),
        }
    );
    assert_eq!(
        operations[1],
        CompiledOperation::ExtractAudio {
            target_codec: "opus".to_owned(),
            container: "ogg".to_owned(),
            filter: Some(TrackFilter::Commentary),
        }
    );
}
```

- [ ] **Step 2: Write failing validation tests**

Add tests for supported and rejected shapes:

```rust
#[test]
fn accepts_sprint14_audio_transcode_and_extract() {
    assert!(compile_policy("policy \"p\" { phase a { transcode audio to aac where lang in [eng] } }").is_ok());
    assert!(compile_policy("policy \"p\" { phase a { transcode audio to opus where codec in [aac] } }").is_ok());
    assert!(compile_policy("policy \"p\" { phase a { extract audio where commentary } }").is_ok());
}

#[test]
fn rejects_sprint14_unsupported_audio_shapes() {
    assert!(codes("policy \"p\" { phase a { transcode audio to flac where lang in [eng] } }")
        .contains(&"unsupported_transcode_shape".to_owned()));
    assert!(codes("policy \"p\" { phase a { extract subtitles where forced } }")
        .contains(&"unknown_phase_statement_or_operation".to_owned()));
}
```

- [ ] **Step 3: Run policy tests to verify failure**

Run:

```bash
cargo test -p voom-policy audio
```

Expected: compile failure or assertion failure because audio operation variants do not exist or validation still rejects them.

- [ ] **Step 4: Add compiled operation variants**

Extend `CompiledOperation`:

```rust
TranscodeAudio {
    target_codec: String,
    container: String,
    filter: Option<TrackFilter>,
},
ExtractAudio {
    target_codec: String,
    container: String,
    filter: Option<TrackFilter>,
},
```

Update `lower_operation` so:

- `transcode audio to aac|opus where ...` lowers to `TranscodeAudio`.
- `extract audio where ...` lowers to `ExtractAudio` with `target_codec = "opus"` and `container = "ogg"`.
- Existing `transcode video to hevc` behavior remains unchanged.

- [ ] **Step 5: Update validation**

Change `validate_transcode_statement` to accept only:

```text
transcode video to hevc
transcode audio to aac where <valid-track-filter>
transcode audio to opus where <valid-track-filter>
```

Add an `extract` branch in operation validation that accepts exactly `extract audio where <valid-track-filter>`.

- [ ] **Step 6: Add fixture and run focused policy suite**

Run:

```bash
cargo test -p voom-policy
```

Expected: all `voom-policy` tests pass, including fixture golden tests.

- [ ] **Step 7: Commit**

```bash
git add crates/voom-policy
git commit -m "feat(policy): add sprint 14 audio operations"
```

### Task 2: Planner Audio Payloads And Selector Semantics

**Files:**

- Add: `crates/voom-plan/src/audio.rs`
- Add: `crates/voom-plan/src/audio_test.rs`
- Modify: `crates/voom-plan/src/lib.rs`
- Modify: `crates/voom-plan/src/planner.rs`
- Modify: `crates/voom-plan/src/planner_test.rs`
- Modify: `crates/voom-plan/src/compliance_report.rs`
- Modify: `crates/voom-plan/src/compliance_report_test.rs`

- [ ] **Step 1: Write failing audio helper tests**

Create `audio_test.rs` with tests for stream fact parsing and extraction role selection:

```rust
#[test]
fn extraction_role_requires_known_commentary_state() {
    let commentary = SnapshotAudioStreamFact {
        snapshot_stream_id: "stream-a".to_owned(),
        provider_stream_index: 1,
        codec: Some("aac".to_owned()),
        language: Some("eng".to_owned()),
        title: Some("Commentary".to_owned()),
        channels: Some(2),
        default: Some(false),
        disposition: Some(AudioDispositionFact {
            default: Some(false),
            forced: Some(false),
            commentary: Some(true),
        }),
        commentary: Some(true),
    };
    assert_eq!(extraction_role(&commentary).unwrap(), AudioBundleRole::CommentaryAudio);

    let mut unknown = commentary;
    unknown.commentary = None;
    assert_eq!(
        extraction_role(&unknown).unwrap_err(),
        AudioPlanningBlock::InsufficientSnapshotFacts
    );
}
```

- [ ] **Step 2: Write failing planner tests**

Add planner tests that assert:

- `transcode audio to opus where lang in [eng]` emits a planned `transcode_audio` node when selected audio codec is AAC.
- The same policy emits a no-op node when selected audio codec is already Opus and container is MKV.
- Zero selected audio streams blocks with `operation_kind = "transcode_audio"`.
- `extract audio where commentary` plans only for exactly one matched commentary stream.
- Extraction blocks on zero, multiple, or unknown commentary state.

- [ ] **Step 3: Run planner tests to verify failure**

Run:

```bash
cargo test -p voom-plan audio
cargo test -p voom-plan planner
```

Expected: failures because `voom_plan::audio` and planner branches do not exist.

- [ ] **Step 4: Add audio planning model**

Create `audio.rs` with focused types:

```rust
pub const AUDIO_TRANSCODE_CONTAINER: &str = "mkv";
pub const AUDIO_EXTRACT_CONTAINER: &str = "ogg";
pub const AUDIO_EXTRACT_CODEC: &str = "opus";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioOperationPayload {
    pub operation_type: AudioOperationType,
    pub target_codec: String,
    pub container: String,
    pub source_media_snapshot_id: Option<u64>,
    pub filter: Option<voom_policy::TrackFilter>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioOperationType {
    TranscodeAudio,
    ExtractAudio,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotAudioStreamFact {
    pub snapshot_stream_id: String,
    pub provider_stream_index: u32,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub channels: Option<u64>,
    pub default: Option<bool>,
    pub disposition: Option<AudioDispositionFact>,
    pub commentary: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDispositionFact {
    pub default: Option<bool>,
    pub forced: Option<bool>,
    pub commentary: Option<bool>,
}
```

Expose helpers for:

- `stream_facts(&MediaSnapshotInput) -> Result<Vec<SnapshotAudioStreamFact>, AudioPlanningBlock>`
- `evaluate_audio_filter(&TrackFilter, &SnapshotAudioStreamFact) -> Result<bool, AudioPlanningBlock>`
- `transcode_audio_shape(...)`
- `extract_audio_shape(...)`
- `AudioOperationPayload::into_value()`
- `AudioOperationPayload::try_from_execution_value(...)`

- [ ] **Step 5: Wire planner branches**

Update `planner.rs`:

- Add `CompiledOperation::TranscodeAudio` and `CompiledOperation::ExtractAudio` to `operation_kind`.
- Add payload generation with `source_media_snapshot_id`.
- Add `expand_transcode_audio_for_snapshot`.
- Add `expand_extract_audio_for_snapshot`.
- Use capability hints `transcode_audio` and `extract_audio`.

- [ ] **Step 6: Update compliance report executable operation handling**

Ensure planned `transcode_audio` and `extract_audio` checks use `IssueActionHint::CreateOrUpdatePlanned` rather than `UnsupportedExecutionOperation`.

- [ ] **Step 7: Run focused planner suite**

Run:

```bash
cargo test -p voom-plan audio
cargo test -p voom-plan planner
cargo test -p voom-plan compliance
```

Expected: all focused planner/report tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/voom-plan
git commit -m "feat(plan): plan sprint 14 audio operations"
```

### Task 3: Worker Protocol Audio Types

**Files:**

- Add: `crates/voom-worker-protocol/src/audio.rs`
- Add: `crates/voom-worker-protocol/src/audio_test.rs`
- Modify: `crates/voom-worker-protocol/src/lib.rs`
- Modify: `crates/voom-worker-protocol/src/operation_kind.rs`
- Modify: `crates/voom-worker-protocol/src/operation_kind_test.rs`

- [ ] **Step 1: Write failing operation-kind tests**

Add assertions:

```rust
#[test]
fn transcode_audio_operation_kind_is_stable() {
    assert_eq!(
        serde_json::to_value(OperationKind::TranscodeAudio).unwrap(),
        serde_json::json!("transcode_audio")
    );
    assert_eq!(
        serde_json::from_value::<OperationKind>(serde_json::json!("extract_audio")).unwrap(),
        OperationKind::ExtractAudio
    );
}
```

- [ ] **Step 2: Write failing request/result serialization tests**

Lock wire shapes for:

- `TranscodeAudioRequest` with selected streams array.
- `TranscodeAudioResult` with `selected_output_streams` in request order, including snapshot stream ID, output provider stream index, codec, language, title, default/disposition state, and channel count when present.
- `ExtractAudioRequest` with one selected stream.
- `ExtractAudioResult` with selected snapshot ID, output codec/container, language, and title.

Use `serde(deny_unknown_fields)` tests matching existing `remux_test.rs` and `transcode_video_test.rs`.

- [ ] **Step 3: Run protocol tests to verify failure**

Run:

```bash
cargo test -p voom-worker-protocol audio
cargo test -p voom-worker-protocol operation_kind
```

Expected: failures because `TranscodeAudio` and `audio` structs are missing.

- [ ] **Step 4: Add operation kind and audio protocol module**

Add `TranscodeAudio` to `OperationKind` and `OperationKind::ALL`.

Create `audio.rs` with:

```rust
pub const TRANSCODE_AUDIO_CONTAINER: &str = "mkv";
pub const TRANSCODE_AUDIO_CODEC_AAC: &str = "aac";
pub const TRANSCODE_AUDIO_CODEC_OPUS: &str = "opus";
pub const EXTRACT_AUDIO_CONTAINER: &str = "ogg";
pub const EXTRACT_AUDIO_CODEC: &str = "opus";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioStreamRef {
    pub snapshot_stream_id: String,
    pub provider_stream_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioOutputStreamFact {
    pub snapshot_stream_id: String,
    pub output_provider_stream_index: u32,
    pub codec: String,
    pub language: Option<String>,
    pub title: Option<String>,
    pub default: Option<bool>,
    pub disposition: Option<AudioDispositionFact>,
    pub channels: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioDispositionFact {
    pub default: Option<bool>,
    pub forced: Option<bool>,
    pub commentary: Option<bool>,
}
```

Add request/result structs using the approved design fields and re-export them from `lib.rs`.

- [ ] **Step 5: Run protocol suite**

Run:

```bash
cargo test -p voom-worker-protocol
```

Expected: all protocol tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/voom-worker-protocol
git commit -m "feat(protocol): add audio worker payloads"
```

### Task 4: Store Migration For Sidecar Support

**Files:**

- Add: `migrations/0013_audio_sidecar_support.sql`
- Modify: `crates/voom-store/src/repo/bundles.rs`
- Modify: `crates/voom-store/src/repo/bundles_test.rs`
- Modify: `crates/voom-store/src/repo/identity.rs`
- Modify: `crates/voom-store/src/repo/identity_test.rs`
- Modify: `crates/voom-store/src/repo/artifacts.rs`
- Modify: `crates/voom-store/src/repo/artifacts_test.rs`

- [ ] **Step 1: Write failing bundle role test**

Add a test proving `external_audio` round-trips through `BundleMemberRole`.

- [ ] **Step 2: Write failing identity enforcement tests**

Add tests that assert:

- Direct `create_file_version_in_tx` rejects `ProducedBy::StagedCommit` with `produced_from_version_id = None`.
- Same-file staged commit helper still creates `ProducedBy::StagedCommit` with a parent.
- New sidecar artifact row helper can create a sidecar `FileAsset` and null-parent staged commit version only when it also finalizes an existing pending `artifact_commit_records.source_file_version_id` row in the same transaction.
- Store helpers do not perform filesystem promotion; extraction promotion and recovery visibility are control-plane responsibilities.

- [ ] **Step 3: Run store tests to verify failure**

Run:

```bash
cargo test -p voom-store bundles
cargo test -p voom-store identity
cargo test -p voom-store artifacts
```

Expected: failures because `external_audio` and sidecar row helper do not exist, and the schema still rejects null-parent `staged_commit`.

- [ ] **Step 4: Add migration**

Create migration that rebuilds:

- `asset_bundle_members.role` CHECK to include `external_audio`.
- `file_versions` CHECK to admit `produced_by = 'staged_commit' AND produced_from_version_id IS NULL`.

Keep the repository boundary stricter than SQLite by rejecting direct null-parent staged commits in `create_file_version_in_tx`.

- [ ] **Step 5: Update repositories**

Add:

```rust
BundleMemberRole::ExternalAudio
```

Add an artifact repository method with a narrow name such as:

```rust
async fn record_verified_sidecar_commit_rows_in_tx(
    &self,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: NewSidecarArtifactCommit,
) -> Result<SidecarArtifactCommit, VoomError>;
```

The method must:

- Require a succeeded verification row.
- Require a target path supplied by the control-plane commit path.
- Create a new `FileAsset`.
- Create a `FileVersion` with `ProducedBy::StagedCommit` and null parent.
- Create one `FileLocation`.
- Require the pending `artifact_commit_records.source_file_version_id`.
- Update that pending commit record to `committed` with the sidecar result file version/location IDs.
- Return commit record, sidecar asset, version, and location IDs.
- Never move staged bytes, create temp files, rename paths, remove paths, or mark filesystem recovery state. Those steps belong to `crates/voom-control-plane/src/audio/commit.rs`.

- [ ] **Step 6: Run migration and store tests**

Run:

```bash
cargo test -p voom-store
```

Expected: all store tests pass.

- [ ] **Step 7: Commit**

```bash
git add migrations/0013_audio_sidecar_support.sql crates/voom-store
git commit -m "feat(store): support verified audio sidecar commits"
```

### Task 5: FFmpeg Worker Audio Execution

**Files:**

- Modify: `crates/voom-ffmpeg-worker/src/ffmpeg.rs`
- Modify: `crates/voom-ffmpeg-worker/src/ffmpeg_test.rs`
- Modify: `crates/voom-ffmpeg-worker/src/handler.rs`
- Modify: `crates/voom-ffmpeg-worker/src/handler_test.rs`
- Modify: `crates/voom-ffmpeg-worker/src/preflight.rs`
- Modify: `crates/voom-ffmpeg-worker/src/preflight_test.rs`
- Modify: `crates/voom-ffmpeg-worker/tests/transcode_worker.rs`

- [ ] **Step 1: Write failing handler tests**

Add tests that cover:

- `OperationKind::TranscodeAudio` routes to typed payload decode.
- `OperationKind::ExtractAudio` routes to typed payload decode.
- Unknown operation still fails with `ProtocolError::UnknownOperation`.
- Existing output path returns `CONFIG_INVALID`.
- Output path outside staging root returns `CONFIG_INVALID`.
- Worker result mismatch for selected stream IDs returns malformed result.
- Audio transcode result mismatch for selected output ordering returns malformed result.
- Audio transcode result mismatch for preserved language, title, default/disposition, or known channel count returns malformed result.
- Audio extraction result drops a source language or title that was present returns malformed result.

- [ ] **Step 2: Write failing FFmpeg command-shape tests**

Add tests using fake `ffmpeg`/`ffprobe` scripts that assert:

- Audio transcode maps all streams and encodes only selected audio indexes.
- Audio extraction maps exactly one selected audio stream.
- Opus extraction requests Ogg output.
- Audio transcode writes metadata/disposition options needed to preserve selected stream language, title, and default state.
- Audio extraction writes selected source language/title metadata when present.
- AAC and Opus encoder preflight checks inspect `ffmpeg -encoders`.
- Matroska and Ogg muxer preflight checks inspect `ffmpeg -muxers`.

- [ ] **Step 3: Run worker tests to verify failure**

Run:

```bash
cargo test -p voom-ffmpeg-worker audio
```

Expected: failures because audio handlers and FFmpeg functions are absent.

- [ ] **Step 4: Add FFmpeg audio command functions**

Add:

```rust
pub async fn run_ffmpeg_transcode_audio(
    config: &FfmpegConfig,
    input: &Path,
    output: &Path,
    request: &TranscodeAudioRequest,
) -> Result<AudioOutputProbe, FfmpegError>
```

and:

```rust
pub async fn run_ffmpeg_extract_audio(
    config: &FfmpegConfig,
    input: &Path,
    output: &Path,
    request: &ExtractAudioRequest,
) -> Result<AudioExtractProbe, FfmpegError>
```

Build commands from typed request fields only. Do not accept provider args from policy text. The command builder must carry enough selected-stream expectation data to validate request-order output stream facts after ffprobe returns.

- [ ] **Step 5: Add handler paths**

Refactor shared observation/path validation only as much as needed to avoid copying the entire video handler. Keep public errors operation-specific so failure messages say `transcode_audio` or `extract_audio`.

Worker success validation must reject output probes that lose requested selected-stream metadata: selected output count, selected output order, selected snapshot IDs, target codec, language, title, default/disposition state, and known channel count for transcode; selected snapshot ID, Opus/Ogg facts, and present language/title for extraction.

- [ ] **Step 6: Run worker tests**

Run:

```bash
cargo test -p voom-ffmpeg-worker
```

Expected: all worker tests pass. If FFmpeg is missing on the machine, tests that require real FFmpeg fail with an explicit setup diagnostic rather than being ignored.

- [ ] **Step 7: Commit**

```bash
git add crates/voom-ffmpeg-worker
git commit -m "feat(ffmpeg-worker): execute sprint 14 audio operations"
```

### Task 6: Control-Plane Audio Module

**Files:**

- Add: `crates/voom-control-plane/src/audio/mod.rs`
- Add: `crates/voom-control-plane/src/audio/source.rs`
- Add: `crates/voom-control-plane/src/audio/selection.rs`
- Add: `crates/voom-control-plane/src/audio/stage.rs`
- Add: `crates/voom-control-plane/src/audio/dispatch.rs`
- Add: `crates/voom-control-plane/src/audio/commit.rs`
- Add: `crates/voom-control-plane/src/audio/events.rs`
- Add sibling `*_test.rs` files for each new module.
- Modify: `crates/voom-control-plane/src/lib.rs`

- [ ] **Step 1: Write failing selection tests**

Add tests proving:

- Transcode selection returns one or more selected audio stream refs in request order.
- Transcode rejects zero matches and sources without video.
- Extraction selection returns exactly one stream and role.
- Extraction rejects zero, multiple, or unknown commentary state.
- Missing selected language/title/default facts block transcode preservation.

- [ ] **Step 2: Write failing stage tests**

Assert paths:

- Staging path includes ticket ID and lease ID under canonical root.
- Transcode target is `<source-stem>.audio-<codec>.mkv`.
- Extraction target is `<source-stem>.<sanitized-snapshot-stream-id>.<codec>.ogg`.
- Extraction target ignores title, language, and provider stream index.
- Existing staging/target paths fail with `CONFIG_INVALID`.

- [ ] **Step 3: Write failing dispatch validation tests**

Assert:

- Input pre/post drift returns `ARTIFACT_CHECKSUM_MISMATCH`.
- Selected stream ID mismatch returns malformed worker result.
- Selected output ordering mismatch returns malformed worker result.
- Transcode output codec mismatch returns malformed worker result.
- Transcode selected output language/title/default/disposition/channel facts that differ from the source snapshot return malformed worker result.
- Extraction output container/codec mismatch returns malformed worker result.
- Extraction output missing language/title that was present on the source stream returns malformed worker result.
- Output file facts must match result facts.

- [ ] **Step 4: Write failing commit tests**

Assert:

- `record_staged_audio_transcode` writes artifact source lineage with selected stream IDs.
- `record_staged_audio_extract` writes artifact source lineage with selected stream ID and intended role.
- Same-file transcode commit uses existing `commit_artifact`.
- Sidecar extraction commit creates a pending artifact commit record before any filesystem promotion begins.
- Sidecar extraction commit promotes staged bytes to a temp path and target path in the control plane after durable prepare succeeds.
- Sidecar extraction commit uses the store sidecar row helper during finalize, after promotion succeeds, to create sidecar identity rows and mark the commit record committed in one transaction.
- Filesystem promotion failures preserve Sprint 11-style `recovery_required` visibility with target path, temp path, staging path, and sidecar IDs when rows were already created.
- Bundle insertion failure after commit returns a recovery report containing commit record, sidecar asset/version/location, source bundle, role, and error code.

- [ ] **Step 5: Run control-plane audio unit tests to verify failure**

Run:

```bash
cargo test -p voom-control-plane audio
```

Expected: compile failure because the module does not exist.

- [ ] **Step 6: Implement module**

Mirror the linear shape of `remux/mod.rs`:

```rust
pub struct ExecuteTranscodeAudioInput {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: LeaseId,
    pub source_file_version_id: FileVersionId,
    pub source_location_id: Option<FileLocationId>,
    pub operation_payload: serde_json::Value,
    pub staging_root: PathBuf,
    pub target_dir: PathBuf,
}

pub struct ExecuteExtractAudioInput {
    pub job_id: JobId,
    pub ticket_id: TicketId,
    pub lease_id: LeaseId,
    pub source_file_version_id: FileVersionId,
    pub source_location_id: Option<FileLocationId>,
    pub operation_payload: serde_json::Value,
    pub staging_root: PathBuf,
    pub target_dir: PathBuf,
}
```

Expose `ControlPlane::execute_transcode_audio` and `ControlPlane::execute_extract_audio`.

- [ ] **Step 7: Add committed-result probe reconciliation for audio transcode**

Reuse the remux result probe dispatcher shape. After same-file commit, probe the target and reject a result snapshot that loses selected stream count, ordering, codec, language, title, default/disposition, or known channel count.

- [ ] **Step 8: Add sidecar filesystem promotion and recovery handling**

Implement extraction commit in `audio/commit.rs` with the same prepare/promote/finalize discipline as `artifact::commit`: canonicalize target parent, reject existing target/temp paths, create a pending `artifact_commit_records` row before filesystem mutation, promote from staging without overwrite, then call the store sidecar row helper from Task 4 during finalize. Promotion or finalize failures after durable prepare must transition the pending commit record to `recovery_required`; bundle-member insertion failure after a committed sidecar must return the post-commit bundle recovery report rather than rerunning FFmpeg.

- [ ] **Step 9: Run control-plane audio tests**

Run:

```bash
cargo test -p voom-control-plane audio
```

Expected: all audio unit tests pass.

- [ ] **Step 10: Commit**

```bash
git add crates/voom-control-plane/src/audio crates/voom-control-plane/src/lib.rs
git commit -m "feat(control-plane): execute audio artifact workflows"
```

### Task 7: Workflow And Compliance Bridge

**Files:**

- Modify: `crates/voom-control-plane/src/workflow/policy_bridge.rs`
- Modify: `crates/voom-control-plane/src/workflow/policy_bridge_test.rs`
- Modify: `crates/voom-control-plane/src/workflow/binding.rs`
- Modify: `crates/voom-control-plane/src/workflow/binding_test.rs`
- Modify: `crates/voom-control-plane/src/workflow/executor.rs`
- Modify: `crates/voom-control-plane/src/workflow/executor_test.rs`
- Modify: `crates/voom-control-plane/src/workflow/ticket_payload.rs`
- Modify: `crates/voom-control-plane/src/workflow/ticket_payload_test.rs`
- Modify: `crates/voom-control-plane/src/cases/compliance.rs`
- Modify: `crates/voom-control-plane/src/cases/compliance_test.rs`

- [ ] **Step 1: Write failing bridge tests**

Assert planned `transcode_audio` and `extract_audio` nodes become workflow nodes with `OperationKind::TranscodeAudio` and `OperationKind::ExtractAudio`.

- [ ] **Step 2: Write failing binding tests**

Assert policy payload rendering includes:

```json
{
  "operation": "transcode_audio",
  "source_file_version_id": 1,
  "audio": { "type": "transcode_audio" },
  "staging_root": "/custom/audio/staging",
  "target_dir": "/custom/audio/output"
}
```

and equivalent `extract_audio` payloads.

- [ ] **Step 3: Write failing executor tests**

Assert:

- Policy audio tickets dispatch through control-plane handlers when `source_file_version_id` is present.
- Runtime registry loads workers advertising `transcode_audio` and `extract_audio`.
- Compliance options can override audio staging and target roots.

- [ ] **Step 4: Run workflow/compliance tests to verify failure**

Run:

```bash
cargo test -p voom-control-plane workflow
cargo test -p voom-control-plane compliance
```

Expected: failures because bridge/executor do not route audio.

- [ ] **Step 5: Implement bridge and binding**

Add `render_policy_transcode_audio_payload` and `render_policy_extract_audio_payload`. Add audio roots to `WorkflowExecutorOptions` and `ComplianceExecutionOptions`.

- [ ] **Step 6: Implement executor dispatch paths**

Follow the remux pattern:

- `RuntimeTranscodeAudioDispatcher`
- `RuntimeExtractAudioDispatcher`
- `dispatch_control_plane_transcode_audio`
- `dispatch_control_plane_extract_audio`

Release leases with serialized execution reports or recovery reports.

- [ ] **Step 7: Run workflow/compliance tests**

Run:

```bash
cargo test -p voom-control-plane workflow
cargo test -p voom-control-plane compliance
```

Expected: all focused tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/voom-control-plane/src/workflow crates/voom-control-plane/src/cases/compliance.rs crates/voom-control-plane/src/cases/compliance_test.rs
git commit -m "feat(workflow): route policy audio operations"
```

### Task 8: Events And Reporting

**Files:**

- Modify: `crates/voom-events/src/kind.rs`
- Modify: `crates/voom-events/src/kind_test.rs`
- Modify: `crates/voom-events/src/payload.rs`
- Modify: `crates/voom-events/src/payload_test.rs`
- Modify: `crates/voom-control-plane/src/audio/events.rs`
- Modify: `crates/voom-control-plane/src/audio/events_test.rs`
- Modify: `crates/voom-cli/src/commands/compliance.rs`
- Modify: `crates/voom-cli/tests/compliance_envelope.rs`
- Modify snapshots under `crates/voom-cli/tests/snapshots/`.

- [ ] **Step 1: Write failing event tests**

Add event kind round-trip tests for:

- `artifact.audio_transcode_started`
- `artifact.audio_transcode_progress`
- `artifact.audio_transcode_succeeded`
- `artifact.audio_transcode_failed`
- `artifact.audio_extract_started`
- `artifact.audio_extract_progress`
- `artifact.audio_extract_succeeded`
- `artifact.audio_extract_failed`

- [ ] **Step 2: Write failing payload tests**

Payloads must include job ID, ticket ID, lease ID, source IDs, selected snapshot stream IDs, provider stream indexes, staging path, artifact IDs when known, provider/version when known, and public error code on failure.

- [ ] **Step 3: Run event tests to verify failure**

Run:

```bash
cargo test -p voom-events audio
cargo test -p voom-control-plane audio::events
```

Expected: failures because event kinds/payloads are absent.

- [ ] **Step 4: Implement events**

Add typed payloads and audio event recording functions. Keep events as audit facts only; do not use them for routing or recovery decisions.

- [ ] **Step 5: Update CLI snapshots**

Add or update CLI tests so successful audio transcode and successful audio extraction envelopes expose policy/input/plan/report IDs, job/ticket IDs, source version/location IDs, staged artifact IDs, verification ID, commit record ID, produced file/version/location IDs, and extraction bundle member role. Add a representative failure or recovery snapshot that exposes the public error code plus any durable IDs recorded before failure.

Run the relevant CLI snapshot tests:

```bash
cargo test -p voom-cli compliance
cargo insta review
```

Accept only snapshots that reflect intentional audio report fields. If `crates/voom-cli/src/commands/compliance.rs` already passes the control-plane JSON through without an allowlist, leave the command code unchanged and commit the tests/snapshots that prove the pass-through behavior.

- [ ] **Step 6: Commit**

```bash
git add crates/voom-events crates/voom-control-plane/src/audio/events.rs crates/voom-control-plane/src/audio/events_test.rs crates/voom-cli
git commit -m "feat(events): report audio workflow facts"
```

### Task 9: End-To-End Audio Integration

**Files:**

- Add: `crates/voom-control-plane/tests/audio_transcode_flow.rs`
- Add: `crates/voom-control-plane/tests/audio_extract_flow.rs`
- Add: `crates/voom-control-plane/tests/audio_fixture_support.rs`

- [ ] **Step 1: Write failing audio transcode integration test**

Create `audio_fixture_support.rs` with a helper that uses the configured FFmpeg binary to synthesize a tiny MKV containing one video stream plus two audio streams: one known commentary stream and one known non-commentary stream, with language/title/default metadata. The helper fails the test with an explicit setup diagnostic when FFmpeg/ffprobe are unavailable or cannot create the required fixture.

Test scan -> policy plan -> compliance execute -> `transcode_audio` -> verify -> commit -> result snapshot. Assert report IDs are populated and result snapshot selected stream facts satisfy codec and preservation rules.

- [ ] **Step 2: Write failing audio extract integration test**

Test scan -> pre-existing primary-video bundle membership -> policy plan -> compliance execute -> `extract_audio` -> verify -> sidecar commit -> bundle member registration. Assert sidecar asset/version/location IDs, source bundle ID, bundle role, artifact commit record, and selected stream ID are present.

- [ ] **Step 3: Write representative failure integration tests**

Cover:

- Extraction multi-match blocks before worker dispatch.
- Extraction source without primary bundle membership fails with `CONFIG_INVALID`.
- Existing target path fails before mutation.
- Worker selected stream mismatch fails without commit.

- [ ] **Step 4: Run integration tests to verify failure**

Run:

```bash
cargo test -p voom-control-plane --test audio_transcode_flow
cargo test -p voom-control-plane --test audio_extract_flow
```

Expected: failures until prior tasks are complete and fixtures exist.

- [ ] **Step 5: Make integration tests pass**

Use existing scan/probe and worker launch helpers. Do not bypass durable tickets with direct function calls in these integration tests; the point is to prove the workflow path.

- [ ] **Step 6: Commit**

```bash
git add crates/voom-control-plane/tests
git commit -m "test(control-plane): cover audio workflow integration"
```

### Task 10: Closeout And Full Verification

**Files:**

- Add: `docs/superpowers/specs/2026-05-26-voom-sprint-14-closeout.md`
- Modify snapshots only through the explicit CLI snapshot task above; `just ci` at closeout is a verification gate, not the point where new output shapes are accepted.

- [ ] **Step 1: Run formatting**

Run:

```bash
just fmt
```

Expected: workspace formatting completes successfully.

- [ ] **Step 2: Run full CI**

Run:

```bash
just ci
```

Expected: `fmt-check`, `lint`, `check-test-layout`, `test`, `doc`, `deny`, and `audit` all pass. If any command is skipped by `just`, record the exact skip reason in closeout.

- [ ] **Step 3: Run placeholder scan**

Run:

```bash
rg -n -e 'TB[D]' -e 'TO''DO' -e 'FIX''ME' -e 'implementation choo''ses' -e 'may''be' -e '\\?\\?' docs/superpowers/specs/2026-05-26-voom-sprint-14-design.md docs/superpowers/plans/2026-05-26-voom-sprint-14-audio-transcode-extract.md crates docs
```

Expected: no unresolved Sprint 14 placeholders. Existing unrelated historical references require explicit review before ignoring.

- [ ] **Step 4: Write closeout evidence**

Record:

- Policy parser/compiler evidence.
- Planner evidence.
- Worker protocol and FFmpeg preflight evidence.
- Control-plane unit evidence.
- Store migration and repository evidence.
- Workflow/compliance evidence.
- Integration and CLI snapshot evidence.
- `just ci` result.

- [ ] **Step 5: Commit closeout**

```bash
git add docs/superpowers/specs/2026-05-26-voom-sprint-14-closeout.md
git commit -m "docs: close out sprint 14 audio implementation"
```

## Self-Review Notes

Spec coverage:

- Policy DSL and compiler: Task 1.
- Planner planned/no-op/blocked behavior: Task 2.
- Worker protocol operation vocabulary and payloads: Task 3.
- Sidecar schema, `external_audio`, and null-parent staged commit enforcement: Task 4.
- FFmpeg execution, preflight, path hardening, and worker validation: Task 5.
- Control-plane source re-read, selector re-evaluation, staging, artifact recording, verification, commit, result snapshot, bundle registration, and recovery report: Task 6.
- Durable workflow and compliance bridge: Task 7.
- Events, reports, and CLI envelope stability: Task 8.
- Real workflow integration: Task 9.
- Closeout and `just ci`: Task 10.

Red-flag wording scan:

- The plan text avoids unresolved marker words and open-ended implementation placeholders.

Type consistency:

- Policy operation names are `transcode_audio` and `extract_audio`.
- Worker operation kind variants are `TranscodeAudio` and existing `ExtractAudio`.
- Control-plane entry points are `execute_transcode_audio` and `execute_extract_audio`.
- Bundle roles are `commentary_audio` and `external_audio`.
