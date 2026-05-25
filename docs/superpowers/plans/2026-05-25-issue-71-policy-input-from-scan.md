# Issue 71 Policy Input From Scan Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a public CLI path that creates a durable policy input set from existing scan-created file version and media snapshot rows.

**Architecture:** Keep persistence in `voom-store` and transaction orchestration in `voom-control-plane`. Add a narrow control-plane method that validates scan row IDs and creates the existing `PolicyInputSetDraft`, then expose it through a nested `voom policy input create-from-scan` CLI command with the standard JSON envelope.

**Tech Stack:** Rust, clap nested subcommands, serde JSON envelopes, sqlx-backed repositories, sibling unit tests, CLI integration tests.

---

### Task 1: Control-Plane API and Tests

**Files:**
- Modify: `crates/voom-control-plane/src/cases/policy_inputs.rs`
- Modify: `crates/voom-control-plane/src/cases/policy_inputs_test.rs`
- Modify: `crates/voom-store/src/repo/identity.rs`

- [x] Add public input/output structs near the top of `policy_inputs.rs`:

```rust
#[derive(Debug, Clone)]
pub struct PolicyInputFromScanInput {
    pub slug: String,
    pub file_version_id: voom_core::FileVersionId,
    pub media_snapshot_id: voom_core::MediaSnapshotId,
    pub container: String,
    pub video_codec: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyInputFromScanResult {
    pub input_set_id: voom_core::PolicyInputSetId,
    pub slug: String,
    pub source_kind: voom_policy::PolicyInputSourceKind,
    pub file_version_id: voom_core::FileVersionId,
    pub media_snapshot_id: voom_core::MediaSnapshotId,
}
```

- [x] Add failing tests in `policy_inputs_test.rs`:
  - `create_policy_input_set_from_scan_links_existing_rows`: seed a discovered file and media snapshot using existing test helpers, call `create_policy_input_set_from_scan`, assert the returned ids, then load the input set and assert it has one media snapshot with target `FileVersion`, container `mp4`, video codec `h264`, and `existing_media_snapshot_id`.
  - `create_policy_input_set_from_scan_rejects_missing_file_version`: call the method with `FileVersionId(999999)` and an existing media snapshot id, then assert `err.code() == ErrorCode::NotFound`.
  - `create_policy_input_set_from_scan_rejects_missing_snapshot`: call the method with an existing file version id and `MediaSnapshotId(999999)`, then assert `err.code() == ErrorCode::NotFound`.
  - `create_policy_input_set_from_scan_rejects_snapshot_for_other_file_version`: seed two file versions with one snapshot on the first, call the method with the second version and first snapshot, then assert `err.code() == ErrorCode::Conflict`.

- [x] Run:

```bash
cargo test -p voom-control-plane cases::policy_inputs::tests::create_policy_input_set_from_scan -- --nocapture
```

Expected: tests fail because the method and structs do not exist.

- [x] Add `get_media_snapshot_in_tx` to `IdentityRepo` and `SqliteIdentityRepo`, delegating to the existing private `get_media_snapshot_in_tx` helper.

- [x] Implement `ControlPlane::create_policy_input_set_from_scan` in `policy_inputs.rs`. The method must:
  - open one transaction with `begin_tx`;
  - read `file_version_id` using `self.identity.get_file_version_in_tx`;
  - return `VoomError::NotFound("file version <id> not found")` when absent;
  - return `VoomError::Conflict("file version <id> is retired")` when retired;
  - read `media_snapshot_id` using `self.identity.get_media_snapshot_in_tx`;
  - return `VoomError::NotFound("media snapshot <id> not found")` when absent;
  - return `VoomError::Conflict("media snapshot <id> does not belong to file version <id>")` when mismatched;
  - call `self.policy_inputs.create_input_set_in_tx` with a `PolicyInputSetDraft` matching the design;
  - commit and return `PolicyInputFromScanResult`.

- [x] Re-run the focused control-plane tests. Expected: pass.

### Task 2: CLI Command Surface

**Files:**
- Modify: `crates/voom-cli/src/cli.rs`
- Modify: `crates/voom-cli/src/main.rs`
- Modify: `crates/voom-cli/src/commands/mod.rs`
- Create: `crates/voom-cli/src/commands/policy.rs`
- Create: `crates/voom-cli/src/commands/policy_test.rs`

- [x] Add nested clap enums:

```rust
Policy(PolicyCommand),

#[derive(Subcommand, Debug, Clone)]
pub enum PolicyCommand {
    #[command(subcommand)]
    Input(PolicyInputCommand),
}

#[derive(Subcommand, Debug, Clone)]
pub enum PolicyInputCommand {
    CreateFromScan {
        #[arg(long)]
        slug: String,
        #[arg(long)]
        file_version_id: u64,
        #[arg(long)]
        media_snapshot_id: u64,
        #[arg(long)]
        container: String,
        #[arg(long)]
        video_codec: String,
    },
}
```

- [x] Add command dispatch in `main.rs` following the existing `dispatch_scan` pattern:

```rust
Command::Policy(ref command) => dispatch_policy(&cli, command.clone()).await,
```

- [x] Create `commands/policy.rs` with a `run` function that opens the control plane, calls `create_policy_input_set_from_scan`, and emits:

```rust
#[derive(Debug, Serialize)]
pub struct PolicyInputCreateFromScanData {
    pub input_set: PolicyInputCreateFromScanSummary,
}

#[derive(Debug, Serialize)]
pub struct PolicyInputCreateFromScanSummary {
    pub input_set_id: u64,
    pub slug: String,
    pub source_kind: String,
    pub file_version_id: u64,
    pub media_snapshot_id: u64,
}
```

- [x] Add `policy_test.rs` unit tests for clap parsing and success data serialization.

- [x] Run:

```bash
cargo test -p voom-cli commands::policy -- --nocapture
```

Expected: command unit tests pass.

### Task 3: CLI Integration Tests

**Files:**
- Modify: `crates/voom-cli/tests/scan_envelope.rs` or create `crates/voom-cli/tests/policy_input_envelope.rs`
- Add snapshots under `crates/voom-cli/tests/snapshots/`

- [x] Add an integration test that:
  - initializes a temp DB;
  - runs `voom scan --path crates/voom-ffprobe-worker/fixtures/media/tiny.mp4`;
  - extracts `file_version_id` and `media_snapshot_id` from the scan envelope;
  - runs `voom policy input create-from-scan --slug scan-h264 --file-version-id <id> --media-snapshot-id <id> --container mp4 --video-codec h264`;
  - asserts one JSON envelope with `status = "ok"` and positive `input_set.input_set_id`.

- [x] Add a second integration test that creates a durable policy document, runs `voom plan show --policy-version-id <id> --input-set-id <new id>`, and asserts the command succeeds.

- [x] Add a negative integration test with missing IDs and assert a runtime error envelope with code `NOT_FOUND`.

- [x] Run:

```bash
cargo test -p voom-cli --test scan_envelope policy_input_create_from_scan -- --nocapture
```

Expected: tests pass. No snapshots were added because the existing scan envelope integration suite asserts the public JSON shape directly.

### Task 4: Chaos Runner Hook

**Files:**
- Modify: `scripts/chaos-e2e-local.sh`

- [x] Add optional policy execution environment gates:

```bash
if [[ -n "${VOOM_CHAOS_POLICY_VERSION_ID:-}" ]]; then
  # after a successful scan, select the first scanned row with jq,
  # create the policy input set through the CLI, then run compliance report.
fi
```

- [x] Keep default behavior scan-only when the env var is unset.

- [x] Run:

```bash
bash -n scripts/chaos-e2e-local.sh
```

Expected: syntax check passes.

### Task 5: Final Verification and Reviews

**Files:**
- Validate all changed files.

- [x] Run adversarial code review and address material findings.
- [x] Run simplification review and address the most relevant recommendation.
- [x] Run:

```bash
cargo test -p voom-control-plane cases::policy_inputs::tests::create_policy_input_set_from_scan -- --nocapture
cargo test -p voom-cli commands::policy -- --nocapture
cargo test -p voom-cli --test scan_envelope policy_input_create_from_scan -- --nocapture
bash -n scripts/chaos-e2e-local.sh
just ci
```

Expected: all commands pass.
