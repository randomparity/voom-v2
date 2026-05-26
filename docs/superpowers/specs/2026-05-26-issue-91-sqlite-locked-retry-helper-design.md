# Issue 91 SQLite Locked Retry Helper Design

## Context

Issue #91 tracks duplicated SQLite locked retry loops in
`crates/voom-control-plane/src/workflow/executor.rs`. The workflow executor
currently repeats the same attempt count, delay, locked-error detection, and
fallback error in lease acquire, lease release, remux lease release, lease
failure, and lease heartbeat helpers.

## Success Criteria

- Workflow lease helpers share one local async retry helper for
  `database is locked` failures.
- The retry policy remains unchanged: 8 attempts, 5 ms delay between locked
  failures, immediate return for non-locked failures, and
  `VoomError::Database("database is locked")` only if no locked error was
  captured.
- Operation-specific behavior remains at the call site. Payload cloning,
  transaction boundaries, event emission, clock reads, and return types are not
  generalized beyond the retry wrapper.
- Focused helper tests prove the retry policy: locked errors retry 8 attempts,
  non-locked errors return immediately, and success after a locked failure
  returns the successful value.
- Existing workflow executor tests continue to pass.

## Design

Add a private `async fn retry_on_database_locked<T, Fut, Op>(operation: Op) ->
Result<T, VoomError>` near the existing lease helpers in
`workflow/executor.rs`. `Op` is an `FnMut() -> Fut`, and `Fut` resolves to
`Result<T, VoomError>`. The helper owns only the retry loop and delegates all
domain work to the closure.

Each current `*_with_retry` helper keeps its public/private signature and calls
`retry_on_database_locked` with a closure containing the existing single-attempt
body. This keeps the refactor behavior-preserving and avoids creating a shared
abstraction outside `workflow/executor.rs` before another module needs it.

## Testing

This is a behavior-preserving simplification, but the shared helper gets direct
unit coverage so the retry policy cannot drift during the refactor. The targeted
verification command is:

```bash
cargo test -p voom-control-plane workflow::executor
```

Run `just fmt-check` after editing to verify formatting.
