# Issue 415: Operation heartbeat liveness implementation plan

## Scope

- Branch: `fix/workflow-claim-liveness-415`
- Base: `5dd4462c`
- Public API/schema/protocol changes: none

## Step 1: Add failing lifecycle regressions

Files:

- `crates/voom-control-plane/src/workflow/execution/executor/mod.rs`
- `crates/voom-control-plane/src/workflow/execution/executor/mod_test.rs`
- source-backed operation dispatcher modules

Add the `cfg(test)` post-worker synchronization seam. Add executor tests for
video transcode, remux, audio synthesis, audio extraction, and policy
verification. Drive Tokio and SQLite on real time, advance only the injected
domain clock, and prove the tests fail while heartbeats remain inside the
worker-only wrappers.

Focused verification:

```text
cargo test -p voom-control-plane post_worker -- --nocapture
```

## Step 2: Move heartbeat ownership to the adapter boundary

Files:

- `crates/voom-control-plane/src/workflow/execution/operation_adapters/mod.rs`
- `crates/voom-control-plane/src/workflow/execution/operation_adapters/policy_verify.rs`
- `crates/voom-control-plane/src/audio/workflow.rs`
- `crates/voom-control-plane/src/remux/workflow.rs`
- `crates/voom-control-plane/src/transcode/workflow.rs`

Split adapter selection from heartbeat ownership. Wrap the complete selected
adapter future once, prioritize its completion branch, and remove the nested
runtime and verification wrappers.

Focused verification:

```text
cargo test -p voom-control-plane post_worker -- --nocapture
cargo test -p voom-control-plane chaos_missed_heartbeat_uses_executor_watchdog
cargo test -p voom-store lease_heartbeat_cannot_resurrect_an_expired_operation_claim
cargo test -p voom-store terminal_dispatch_advance_fences_stale_generation_completion
```

## Step 3: Review and guardrails

- run format, clippy, and the focused control-plane suite;
- run adversarial review for terminal races, cancellation, and error masking;
- run three independent simplification reviews in isolated worktrees;
- fix defensible findings and repeat focused verification;
- run `just ci`.

## Step 4: Ship

- commit logical TDD changes with Conventional Commit messages;
- push the branch and open a PR closing #415;
- record review and trajectory annotations;
- wait for every required GitHub check;
- merge serially under the campaign authorization;
- remove status labels, branches, worktree, and temporary files;
- reconcile #414, #415, and parent #413.
