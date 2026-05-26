# Issue 90 Policy File Source Resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Consolidate duplicate policy file-source structs and resolver logic for transcode and remux workflow tickets.

**Architecture:** Keep the shared source type in `workflow/binding.rs` because both renderers consume it. Keep source resolution private to `WorkflowExecutor`; parameterize only the operation name needed for stable diagnostics.

**Tech Stack:** Rust, serde_json, sqlx-backed workflow tests, `cargo test`, `just`.

---

## Files

- Modify: `crates/voom-control-plane/src/workflow/binding.rs`
- Modify: `crates/voom-control-plane/src/workflow/binding_test.rs`
- Modify: `crates/voom-control-plane/src/workflow/executor.rs`
- Modify: `crates/voom-control-plane/src/workflow/executor_test.rs`

### Task 1: Characterize Retired File Location Errors

- [ ] **Step 1: Add retired-location workflow tests**

Add this helper to `ExecutorFixture` in
`crates/voom-control-plane/src/workflow/executor_test.rs`:

```rust
async fn retire_source_location(&self, source_location_id: voom_core::FileLocationId) {
    let result = sqlx::query("UPDATE file_locations SET retired_at = ? WHERE id = ?")
        .bind("1970-01-01T00:00:00Z")
        .bind(i64::try_from(source_location_id.0).unwrap())
        .execute(&self.cp.pool)
        .await
        .unwrap();
    assert_eq!(result.rows_affected(), 1);
}
```

Add these tests near the existing policy source-resolution tests:

```rust
#[tokio::test]
async fn policy_transcode_file_location_target_rejects_retired_location() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    let (_source_file_version_id, source_location_id) = fixture.seed_local_source().await;
    fixture.retire_source_location(source_location_id).await;
    fixture.plan = policy_transcode_plan(TargetRef::FileLocation {
        id: source_location_id,
    });

    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::ConfigInvalid);
    assert_eq!(
        err.source.to_string(),
        format!("config error: file_location {source_location_id} is retired")
    );
}

#[tokio::test]
async fn policy_remux_file_location_target_rejects_retired_location() {
    let mut fixture = ExecutorFixture::without_workers(0).await;
    let (_source_file_version_id, source_location_id) = fixture.seed_local_source().await;
    fixture.retire_source_location(source_location_id).await;
    fixture.plan = policy_remux_plan(TargetRef::FileLocation {
        id: source_location_id,
    });

    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::ConfigInvalid);
    assert_eq!(
        err.source.to_string(),
        format!("config error: file_location {source_location_id} is retired")
    );
}
```

- [ ] **Step 2: Run characterization tests before refactoring**

Run:

```bash
cargo test -p voom-control-plane file_location_target
```

Expected: PASS.

### Task 2: Consolidate Binding Source Type

- [ ] **Step 1: Replace duplicate source structs**

In `crates/voom-control-plane/src/workflow/binding.rs`, replace
`PolicyTranscodeSource` and `PolicyRemuxSource` with:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyFileSource {
    pub file_version_id: FileVersionId,
    pub location_id: Option<FileLocationId>,
}
```

Update both render functions to accept `PolicyFileSource`.

- [ ] **Step 2: Update binding tests**

In `crates/voom-control-plane/src/workflow/binding_test.rs`, update imports and
constructor names from `PolicyRemuxSource` to `PolicyFileSource`.

Run:

```bash
cargo test -p voom-control-plane workflow::binding
```

Expected: PASS.

### Task 3: Consolidate Executor Resolver

- [ ] **Step 1: Update executor imports and call sites**

In `crates/voom-control-plane/src/workflow/executor.rs`, import
`PolicyFileSource` instead of `PolicyRemuxSource` and `PolicyTranscodeSource`.
Change the transcode call site to:

```rust
self.resolve_policy_file_source(target, "transcode_video").await?
```

Keep the remux call site's non-file-target branch as a `BindingError`, and
change the file-target render branch to:

```rust
self.resolve_policy_file_source(target, "remux").await?
```

- [ ] **Step 2: Replace duplicate resolver methods**

Delete `resolve_policy_transcode_source` and `resolve_policy_remux_source`.
Add:

```rust
async fn resolve_policy_file_source(
    &self,
    target: &voom_plan::TargetRef,
    operation_name: &str,
) -> Result<PolicyFileSource, VoomError> {
    match target {
        voom_plan::TargetRef::FileVersion { id } => Ok(PolicyFileSource {
            file_version_id: *id,
            location_id: None,
        }),
        voom_plan::TargetRef::FileLocation { id } => {
            let location = self
                .control_plane
                .identity
                .get_file_location(*id)
                .await?
                .ok_or_else(|| VoomError::NotFound(format!("file_location {id}")))?;
            if location.retired_at.is_some() {
                return Err(VoomError::Config(format!("file_location {id} is retired")));
            }
            Ok(PolicyFileSource {
                file_version_id: location.file_version_id,
                location_id: Some(*id),
            })
        }
        other => Err(VoomError::Config(format!(
            "{operation_name} requires file_version or file_location target, got {other:?}"
        ))),
    }
}
```

- [ ] **Step 3: Run targeted workflow tests**

Run:

```bash
cargo test -p voom-control-plane file_location_target
```

Expected: PASS.

Run:

```bash
cargo test -p voom-control-plane workflow::binding
```

Expected: PASS.

- [ ] **Step 4: Run formatting check**

Run:

```bash
just fmt-check
```

Expected: PASS.
