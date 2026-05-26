# Issue 91 SQLite Locked Retry Helper Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace duplicated SQLite locked retry loops in the workflow executor with one local helper while preserving lease behavior.

**Architecture:** Keep retry policy local to `crates/voom-control-plane/src/workflow/executor.rs`. A generic private async helper owns the retry loop; existing lease-specific helpers retain their signatures and closures retain operation-specific work.

**Tech Stack:** Rust, tokio, sqlx-backed control plane, `cargo test`, `just`.

---

## Files

- Modify: `crates/voom-control-plane/src/workflow/executor.rs`
- Verify: `crates/voom-control-plane/src/workflow/executor_test.rs`

### Task 1: Introduce The Local Retry Helper

- [ ] **Step 1: Add focused retry helper tests**

Add these tests to `crates/voom-control-plane/src/workflow/executor_test.rs`:

```rust
#[tokio::test]
async fn retry_on_database_locked_retries_locked_errors_until_success() {
    let attempts = AtomicU32::new(0);

    let result = retry_on_database_locked(|| {
        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
        async move {
            if attempt < 2 {
                Err(VoomError::Database("database is locked".to_owned()))
            } else {
                Ok("done")
            }
        }
    })
    .await
    .unwrap();

    assert_eq!(result, "done");
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn retry_on_database_locked_stops_after_eight_locked_errors() {
    let attempts = AtomicU32::new(0);

    let err = retry_on_database_locked(|| {
        attempts.fetch_add(1, Ordering::SeqCst);
        async { Err::<(), _>(VoomError::Database("database is locked".to_owned())) }
    })
    .await
    .unwrap_err();

    assert_eq!(err.to_string(), "database is locked");
    assert_eq!(attempts.load(Ordering::SeqCst), 8);
}

#[tokio::test]
async fn retry_on_database_locked_does_not_retry_other_errors() {
    let attempts = AtomicU32::new(0);

    let err = retry_on_database_locked(|| {
        attempts.fetch_add(1, Ordering::SeqCst);
        async { Err::<(), _>(VoomError::ConfigInvalid("bad lease".to_owned())) }
    })
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}
```

- [ ] **Step 2: Run helper tests to verify they fail before implementation**

Run:

```bash
cargo test -p voom-control-plane retry_on_database_locked
```

Expected: FAIL because `retry_on_database_locked` does not exist yet.

- [ ] **Step 3: Run existing targeted tests before refactoring**

Run:

```bash
cargo test -p voom-control-plane workflow::executor
```

Expected: PASS. This establishes the current behavior before the simplification.

- [ ] **Step 4: Add `retry_on_database_locked`**

In `crates/voom-control-plane/src/workflow/executor.rs`, add this helper above
`acquire_lease_with_retry`:

```rust
async fn retry_on_database_locked<T, Fut, Op>(mut operation: Op) -> Result<T, VoomError>
where
    Fut: Future<Output = Result<T, VoomError>>,
    Op: FnMut() -> Fut,
{
    let mut last = None;
    for _ in 0..8 {
        match operation().await {
            Ok(value) => return Ok(value),
            Err(err) if is_database_locked(&err) => {
                last = Some(err);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Err(err) => return Err(err),
        }
    }
    Err(last.unwrap_or_else(|| VoomError::Database("database is locked".to_owned())))
}
```

Also import `std::future::Future` near the existing `std` imports. The tests
already import `AtomicU32`, `Ordering`, `ErrorCode`, and `VoomError`.

- [ ] **Step 5: Verify helper tests pass**

Run:

```bash
cargo test -p voom-control-plane retry_on_database_locked
```

Expected: PASS.

- [ ] **Step 6: Convert lease helpers to use the retry helper**

Update these functions so each calls `retry_on_database_locked` with the existing
single-attempt body:

- `acquire_lease_with_retry`
- `release_lease_with_retry`
- `release_remux_lease_with_retry`
- `fail_lease_with_retry`
- `heartbeat_lease_with_retry`

Keep payload, reason, and input clones inside the closures where the old retry
loop cloned them per attempt.

- [ ] **Step 7: Run targeted tests after refactoring**

Run:

```bash
cargo test -p voom-control-plane workflow::executor
```

Expected: PASS.

- [ ] **Step 8: Run formatting check**

Run:

```bash
just fmt-check
```

Expected: PASS.
