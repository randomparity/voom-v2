# Audio Extraction Worker Session Reuse

Issue: #367

## Goal

Reuse one healthy bundled verification process and one healthy bundled ffprobe
process across the ordered outputs of a single audio extraction invocation.
Preserve single-item protocol requests, per-output durable evidence, external
worker deadlines, all-or-none publication, and deterministic process cleanup.

## Current State

`verify_and_probe_extract_members` first verifies every staged output and then
probes every verified output. The production verification and probe dispatchers
launch and shut down a bundled process for each request. A two-output extraction
therefore pays for four process startups even though each worker already
supports multiple authenticated requests.

The existing worker wrappers are reusable and already enforce distinct leases
and idempotency keys, per-request heartbeat and idle deadlines, termination
after worker-level faults, and bounded graceful shutdown. Durable verification
rows and events are recorded outside the worker wrapper for each output.

## Decisions

### Operation-scoped adapters

Add an object-safe `start_session` method to both extraction dependencies:

- `VerifyArtifactDispatcher`
- `AudioResultProbeDispatcher`

The default session delegates each request to the existing dispatcher and has a
no-op shutdown. Existing test and non-extraction dispatchers therefore retain
their current behavior.

The bundled dispatchers override `start_session` with lazy operation-scoped
sessions. The first request launches the bundled worker. Later ordered requests
reuse that exact authenticated process. The session accepts only one worker
identity and fails loudly if a later request attempts to change it.

No public worker protocol, CLI, DSL, or persisted shape changes.

### Lifetime and failure boundary

`execute_extract_audio_with_dispatchers` creates both sessions immediately
before extraction orchestration and shuts both down after success or failure.
Each child receives the existing five-second graceful shutdown bound, after
which the worker-process wrapper kills and reaps it.

The session lifetime is bounded by one extraction invocation. Every request
retains the existing 30-second heartbeat and progress-idle deadlines. There is
no cross-ticket pool and no long-lived background process.

Verification remains a complete ordered pass before probing begins. The first
failed or wedged member stops the pass, so no later member is probed or
published. Earlier successful verification evidence remains auditable, while
the existing extraction failure event and claim release record the operation
failure. Retry reuses durable staged outputs without duplicating successful
commit rows.

A worker-level fault already terminates the unhealthy process inside
`dispatch_verify_artifact` or `dispatch_probe_file`. The operation aborts rather
than restarting and continuing later members; restarting within the same
all-or-none attempt would obscure the failure boundary. A later workflow retry
starts a fresh session and process.

### Shutdown errors

Shutdown remains best-effort for the primary operation result, matching current
single-dispatch behavior. The process wrapper kills and asynchronously reaps a
child from `Drop` if graceful shutdown cannot complete. Session shutdown
failures are logged without replacing the durable operation failure or a
successful committed result.

## Verification

1. A plural extraction creates one verifier session and one probe session,
   dispatches both ordered outputs through each, shuts both sessions once, and
   commits all output, lineage, snapshot, and bundle rows.
2. A mid-set verification failure stops before any probe or publication,
   records durable failed verification and extraction events, shuts both
   sessions once, and retries without duplicate committed rows.
3. Bundled verifier and ffprobe session tests send consecutive single-item
   protocol requests through one process and perform explicit shutdown.
4. Existing worker crash, timeout, malformed-result, lease-id, and child-reaping
   tests remain green, proving session reuse does not bypass their boundaries.
5. Focused audio, artifact-worker, and scan-worker tests, strict Clippy, and
   `just ci` pass.

## Rejected Alternatives

- **Batch protocol operation:** rejected. It would add an unpublished worker
  wire shape and duplicate per-output timeout and evidence semantics.
- **Global worker pool:** rejected. It creates ownership, health, eviction, and
  shutdown policy outside this issue.
- **Concurrent member dispatch:** rejected. Ordered serial passes keep resource
  use bounded and preserve the existing deterministic failure boundary.
- **Restart after a mid-set crash:** rejected. The operation must fail at the
  exact unhealthy member; retry is the existing recovery boundary.
- **Share one process between verifier and ffprobe:** rejected. They are
  different worker kinds, capabilities, and protocol implementations.

## Implementation Plan

1. Add red session lifecycle tests for plural success and mid-set failure.
2. Add dispatcher session traits with default per-dispatch adapters.
3. Add lazy bundled verifier and ffprobe sessions using existing process
   wrappers and dispatch methods.
4. Scope session creation and shutdown around extraction orchestration.
5. Run focused tests, strict Clippy, adversarial review, simplification, and
   `just ci`.
