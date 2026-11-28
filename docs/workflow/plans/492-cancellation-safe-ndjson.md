# #492 implementation plan

BASE_BRANCH: main (origin/main); branch: feat/cancellation-safe-ndjson-492.
Expected implementation size: 100–230 changed lines (M) — reader change, protocol matrix, workflow fixture.
Full-spec; denominator 250 from live #492 reader/dispatch assessment. No ADR.

## Task 1 — protocol framing
Files: crates/voom-worker-protocol/src/wire/ndjson.rs and ndjson_test.rs.
Interfaces: consumes next_frame(&mut self) -> Result<NdjsonOutcome, ProtocolError>;
exports the same signature with partial-line preservation when its future is dropped.
Verification (Mode: focused-test): cancelled_partial_frame_survives_reentry and
cancelled_partial_frame_keeps_validation; red is MalformedFrame or unexpected EOF;
green command `cargo test -p voom-worker-protocol cancelled_partial_frame`.
Write a duplex fixture, serialize existing progress/result helpers with line_for,
write a prefix, and poll next_frame with biased select against ready(()). The read
branch must remain pending. Write remaining bytes and assert exact frame equality.
Cover repeated partial delivery, duplicate followed by fragmented terminal,
coalescing and size/lease/gap/EOF outcomes. Existing protocol tests cover the
unchanged complete-line validations. Then replace the loop-entry clear with a
clear immediately after successful serde_json decoding:
```rust
let frame: ProgressFrame =
    serde_json::from_slice(json_slice).map_err(|e| ProtocolError::MalformedFrame {
        detail: format!("json decode: {e}"),
    })?;
self.line_buf.clear();
```
Add cancellation-safety API documentation. No await may intervene between decode
and state validation. Acceptance: all framed bytes survive cancellation, including
size accounting; duplicates are consumed once. Run `cargo test -p voom-worker-protocol`
(expected all pass), `just fmt-check` and `just check-test-layout` (exit 0).

## Task 2 — workflow consumer
File: crates/voom-control-plane/src/workflow/execution/executor/mod_test.rs.
Interfaces: existing ExecutorFixture::create_heartbeat_test_lease(worker_id),
DispatchStream { response: OperationResponse, frames: NdjsonStream }, and real
consume_dispatch_stream; expose its existing function only to its parent module
if required for sibling tests. Test-only fixture implements AsyncRead::poll_read.
Verification (Mode: focused-test): heartbeat_cancellation_preserves_partial_frame;
red is MalformedWorkerResult from dropped prefix; green command
`cargo test -p voom-control-plane heartbeat_cancellation_preserves_partial_frame`.
Create an ordinary fixture and held lease using existing helpers. Invoke the real
consumer with a serialized benign progress/result sequence. The fixture's first
poll supplies a strict prefix, second poll returns Pending and wakes the task,
and subsequent polls supply remaining bytes. The consumer's initial interval tick
is ready at the cancellation boundary. Assert success and released lease/ticket
completion. No paused Tokio time with the real database. Bound the consumer await
with a test timeout to diagnose hangs. All input remains in memory.
Acceptance: regression fails before the reader repair and passes after it; no
production timing policy or SQL changes. Run the focused command and existing
workflow executor tests (all pass), then `just ci` (exit 0).

## Review and delivery
Independent design review and scope audit precede implementation. Iterating branch
review includes the spec Failure model, followed by parser security review and
simplification. Keep all defensible findings dispositioned. Commit with mandatory
hooks; push through quest delivery, wait for exact-head hosted checks and publish
MERGE-READY handoff. Do not merge or clean the worktree.
Deferrals: none. Rollback: revert this PR; no durable state changes.
