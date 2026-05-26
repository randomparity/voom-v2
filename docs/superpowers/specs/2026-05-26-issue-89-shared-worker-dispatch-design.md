# Issue 89 Shared Worker Dispatch Design

## Context

Issue #89 is still present. `remux/dispatch.rs` and `transcode/dispatch.rs`
both encode an operation payload, construct an `OperationRequest`, call
`ClientHandle::dispatch`, consume an NDJSON progress stream until a terminal
frame, enforce the progress idle timeout, decode the result payload, and map
worker terminal errors to `VoomError`. The same sibling-binary discovery pattern
also appears in remux, transcode, scan, and verify-artifact worker command
builders.

## Scope

- Add a shared control-plane helper for operation dispatch over the worker
  protocol.
- Keep operation-specific result decoding and remux progress recording explicit.
- Add a shared bundled-worker command discovery helper and reuse it from remux,
  transcode, scan, and verify-artifact command builders.
- Preserve all public error codes and existing observable error message text.
- Do not consolidate scan/verify worker error types or process lifecycle code in
  this issue.

## Design

Add private helper types/functions in `artifact::worker`, which already owns
`WorkerCommand` and `BundledWorkerProcess`:

- `WorkerStreamLabels` carries the operation-specific message fragments that are
  already observable in tests and CLI envelopes.
- `dispatch_operation_with_client` performs JSON encoding, request construction,
  `ClientHandle::dispatch`, timeout-wrapped frame consumption, terminal-result
  decoding, and terminal-error mapping to `VoomError`.
- `WorkerProgressHandler` lets remux record progress frames while transcode uses
  a no-op handler.
- `bundled_worker_command_from` centralizes configured env override, current-exe
  sibling search, `deps` directory fallback, and command fallback by binary name.
  A small callback can attach sibling tool environment, used by ffprobe to keep
  `VOOM_FFPROBE_BIN` behavior.

Remux and transcode dispatch modules keep request construction, source/result
validation, and dispatcher traits. They call the shared helper with their own
operation kind, idempotency key, lease id, timeout labels, result type, and
progress handler.

## Error Handling

The helper must preserve the current distinctions:

- protocol dispatch errors map to `WorkerCrash` with the operation-specific
  prefix (`remux dispatch failed`, `transcode dispatch failed`);
- idle timeouts map to `WorkerTimeout` with the current operation-specific
  timeout messages;
- stream protocol errors map to `MalformedWorkerResult` with the existing
  operation stream prefix;
- terminal worker errors keep the current `ErrorCode`/`FailureClass` mapping;
- early stream end remains `WorkerCrash`.

## Verification

Targeted checks:

```bash
cargo test -p voom-control-plane remux::dispatch
cargo test -p voom-control-plane transcode::dispatch
cargo test -p voom-control-plane scan::worker
cargo test -p voom-control-plane artifact::worker
```

Full closeout:

```bash
just fmt-check
just ci
```
