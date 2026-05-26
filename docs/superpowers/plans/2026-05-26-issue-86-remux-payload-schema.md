# Issue 86 Remux Payload Schema Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the duplicated raw remux operation payload contracts with one typed payload model shared by planning, binding, and execution.

**Architecture:** Add the shared payload model to `voom-plan::remux`, because `voom-plan` already owns remux planning helpers and `voom-control-plane` can depend on it without violating crate layering. The planner serializes the typed model, workflow binding validates by parsing that model, and remux selection consumes the same model while retaining media-fact-dependent execution checks.

**Tech Stack:** Rust, serde, serde_json, voom-policy, voom-worker-protocol, cargo test, just.

---

### Task 1: Add Shared Payload Model Tests

**Files:**
- Modify: `crates/voom-plan/src/remux_test.rs`
- Modify: `crates/voom-plan/src/remux.rs`

- [ ] **Step 1: Write failing tests for the shared contract**

Add tests that call `RemuxOperationPayload::try_from_value` and assert:

```rust
#[test]
fn remux_payload_defaults_optional_collections() {
    let payload = RemuxOperationPayload::try_from_execution_value(json!({
        "type": "remux",
        "container": "mkv",
        "source_media_snapshot_id": 99
    }))
    .unwrap();

    assert!(payload.track_actions.is_empty());
    assert!(payload.defaults.is_empty());
    assert_eq!(
        payload.track_order,
        vec![
            RemuxTrackGroup::Video,
            RemuxTrackGroup::Audio,
            RemuxTrackGroup::Subtitle,
        ]
    );
}

#[test]
fn remux_payload_rejects_invalid_contract_fields() {
    assert_remux_payload_error(json!({"type": "copy", "container": "mkv", "source_media_snapshot_id": 99}), "remux payload missing `type: remux`");
    assert_remux_payload_error(json!({"type": "remux", "container": "mp4", "source_media_snapshot_id": 99}), "remux payload `container` must be mkv");
    assert_remux_payload_error(json!({"type": "remux", "container": "mkv"}), "remux payload `source_media_snapshot_id` must be a positive integer");
    assert_remux_payload_error(json!({"type": "remux", "container": "mkv", "source_media_snapshot_id": 0}), "remux payload `source_media_snapshot_id` must be a positive integer");
    assert_remux_payload_error(json!({"type": "remux", "container": "mkv", "source_media_snapshot_id": 99, "track_actions": [{"type": "copy_tracks", "target": "audio"}]}), "remux track_actions[0] type `copy_tracks` is unsupported");
    assert_remux_payload_error(json!({"type": "remux", "container": "mkv", "source_media_snapshot_id": 99, "track_actions": [{"type": "keep_tracks", "target": "attachment"}]}), "remux track_actions[0] target `attachment` is unsupported");
    assert_remux_payload_error(json!({"type": "remux", "container": "mkv", "source_media_snapshot_id": 99, "track_order": []}), "remux track_order must include at least one group");
    assert_remux_payload_error(json!({"type": "remux", "container": "mkv", "source_media_snapshot_id": 99, "track_order": ["video", "audio", "audio"]}), "remux track_order[2] duplicates target `audio`");
}

#[test]
fn remux_payload_allows_missing_snapshot_id_for_planner_serialization() {
    let payload = RemuxOperationPayload::try_from_value(json!({
        "type": "remux",
        "container": "mkv"
    }))
    .unwrap();

    assert_eq!(payload.source_media_snapshot_id, None);
}
```

- [ ] **Step 2: Verify red**

Run: `cargo test -p voom-plan remux_payload_`

Expected: compile failure because `RemuxOperationPayload` does not exist yet.

### Task 2: Implement Shared Payload Model

**Files:**
- Modify: `crates/voom-plan/src/remux.rs`

- [ ] **Step 1: Add public payload types**

Define `RemuxOperationPayload`, `RemuxTrackAction`, `RemuxTrackActionKind`, `RemuxDefaultAction`, and `RemuxPayloadError`.

- [ ] **Step 2: Add serde parsing and validation**

Implement `RemuxOperationPayload::try_from_value(Value) -> Result<Self, RemuxPayloadError>`, `try_from_execution_value(Value) -> Result<Self, RemuxPayloadError>`, and `into_value(self) -> Value`. Validate type, mkv container, positive source snapshot id when present, supported action kind, non-attachment action targets, and track order constraints. The execution parser additionally requires a positive `source_media_snapshot_id`.

- [ ] **Step 3: Verify green**

Run: `cargo test -p voom-plan remux_payload_`

Expected: the new payload tests pass.

### Task 3: Use Typed Payload in Planner

**Files:**
- Modify: `crates/voom-plan/src/planner.rs`
- Test: `crates/voom-plan/src/planner_test.rs`

- [ ] **Step 1: Replace raw JSON construction**

Change `remux_payload` to construct `RemuxOperationPayload` and serialize it. Keep the emitted JSON keys and explicit `track_actions`, `track_order`, and `defaults` fields stable, and keep omitting `source_media_snapshot_id` when the input snapshot has no existing media snapshot ID.

- [ ] **Step 2: Verify planner behavior**

Run: `cargo test -p voom-plan remux`

Expected: existing remux planner tests pass without snapshot changes.

### Task 4: Use Typed Payload in Binding and Selection

**Files:**
- Modify: `crates/voom-control-plane/src/workflow/binding.rs`
- Modify: `crates/voom-control-plane/src/workflow/binding_test.rs`
- Modify: `crates/voom-control-plane/src/remux/selection.rs`
- Modify: `crates/voom-control-plane/src/remux/selection_test.rs`

- [ ] **Step 1: Replace binding raw validators**

Have `render_policy_remux_payload` parse `RemuxOperationPayload` with `try_from_execution_value`, serialize the typed payload back to JSON, and embed it under `remux`. Remove the private raw validation helpers that become unused.

- [ ] **Step 2: Replace selection private raw payload**

Have `selection_from_payload_and_snapshot` parse `RemuxOperationPayload` from `voom-plan::remux` with `try_from_execution_value`. Remove the private raw payload/action/default structs and duplicated track-order conversion.

- [ ] **Step 3: Update tests for shared defaults**

Keep tests that assert binding maps typed payload errors. Move detailed schema validation assertions to `voom-plan::remux` tests and keep selection tests for source media behavior.

- [ ] **Step 4: Verify control-plane behavior**

Run:

```bash
cargo test -p voom-control-plane workflow::binding
cargo test -p voom-control-plane remux::selection
```

Expected: both targeted suites pass.

### Task 5: Reviews, Full Verification, and Commit

**Files:**
- Modify: all files changed above

- [ ] **Step 1: Run adversarial review on implementation diff**

Review `git diff main...HEAD` for contract drift, missing media-fact checks, and behavior changes not covered by tests. Address material findings.

- [ ] **Step 2: Run simplification review**

Look for remaining duplicate schema logic or single-use abstractions. Address the most relevant recommendation.

- [ ] **Step 3: Run full verification**

Run:

```bash
just fmt
just ci
```

Expected: formatting is clean and CI passes locally.

- [ ] **Step 4: Commit**

Commit all #86 changes with message:

```bash
git commit -m "refactor(plan): share remux payload schema"
```
