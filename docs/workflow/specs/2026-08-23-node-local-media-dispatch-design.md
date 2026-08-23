# Node-local location-handle media dispatch — design spec

Issue #423 · ADR 0075 · branch `feat/node-local-media-dispatch-423` · base `main`

## Problem

Real media worker requests and post-worker validation carry absolute
source/staging/output paths selected by the control plane. The SQLite host
canonicalizes, stats, hashes, copies, and probes media bytes; remote agents
receive payloads their children cannot execute. One unified contract must
replace the split "control-plane local adapter" / "opaque remote payload"
media paths.

## Goals

1. Byte-touching media operations (probe, transcode-audio + synthesize,
   extract-audio, transcode-video, remux, backup-file, verify-artifact,
   add-only commit staging) execute on the storage-owner node agent.
2. Handles (`StorageRootId` + `ProviderRelativeLocator`) are the only location
   vocabulary crossing the control-plane↔agent boundary; absolute paths exist
   only inside the agent↔child boundary.
3. Protocol/payload mismatch fails before lease execution.
4. Crash, timeout, malformed result, retry, and cancellation preserve durable
   lease/commit truth (existing lease + ADR 0074 intent state machines own all
   durability; the agent adds no new durable state).
5. Local and remote media execution share one envelope and one executor shape.

## Non-goals

- Operator artifact inspection (`artifact/inspect.rs`) — unchanged.
- Workflow-coordinator terminal-artifact move/reclaim — #436.
- Object-store roots, cross-node transfer, root re-parenting — epic non-goals.

## Capabilities

### C1 — Dispatch envelopes (voom-worker-protocol)

New `operations/dispatch.rs`:

- `pub const MEDIA_DISPATCH_SCHEMA: u32` — equals `voom_core::PROTOCOL_VERSION`
  (bumped 2→3 in `voom-core`), re-exported so agent and control plane share
  one constant.
- `MediaSourceRef { storage_root_id: StorageRootId,
  provider_relative_locator: ProviderRelativeLocator }` — an existing,
  live rooted location.
- `MediaPlannedOutput { storage_root_id: StorageRootId,
  provider_relative_locator: String, container: String, overwrite: bool }` —
  a planned file that does not exist yet (relative locator is created
  deterministically by the control plane from branch/output identity).
- `#[serde(tag = "operation", rename_all = "snake_case")] enum
  MediaDispatch { ... }` whose variants are newtype wrappers over
  `#[serde(deny_unknown_fields)]` content structs (ADR 0013 durable-enum
  rule), one per operation:
  - `Probe { source: MediaSourceRef, expected: ExpectedFileFacts }`
  - `TranscodeAudio { source: MediaSourceRef, expected, output:
    MediaPlannedOutput, selection, settings }`
  - `ExtractAudio { source: MediaSourceRef, expected, outputs:
    Vec<ExtractAudioOutputDescriptor-shaped planned outputs>, selection }`
  - `TranscodeVideo { source: MediaSourceRef, expected, output:
    MediaPlannedOutput, profile, hardware_assignment, copy_video }`
  - `Remux { source: MediaSourceRef, expected, output: MediaPlannedOutput,
    selection }`
  - `BackUpFile { source: MediaSourceRef, destination: MediaPlannedOutput }`
  - `VerifyArtifact { target: MediaSourceRef, expected }` (staging-rooted)
  - `StageSource { source: MediaSourceRef, expected, target: MediaPlannedOutput }`
- Each content struct embeds `schema: u32` (exact-match field) so a
  same-version binary with divergent payload shape still fails decode.
- Existing child request/result wire types are unchanged (path-based, trusted
  boundary). `PROTOCOL_VERSION` bump covers child handshake skew.

### C2 — Ticket rendering (voom-control-plane)

`workflow/plan/binding.rs` renderers emit `MediaDispatch` JSON for the eight
operations: storage sources come from `TicketStorageSource` (root → whole-root
read; location → `MediaSourceRef` from the location's rooted address);
staging/output/backup destinations resolve to their configured storage roots
(`LibraryRoot.default_output/staging/backup_root_id`) and deterministic
relative locators. Renderers stop emitting `staging_root` path strings.
`WorkflowTicketPayload::validate_artifact_access` keeps deriving the
declaration from the same handle vocabulary (unchanged equality check).

### C3 — Agent-side media executor (voom-node-agent)

New `media.rs` routed from `dispatch_outcome` for every byte-touching
operation except `scan_library` (existing pump):

1. Strict-decode `MediaDispatch` from `dispatch_payload`; wrong/missing schema
   or unknown shape → `LeaseOutcome::Failure(MalformedWorkerResult)` **before
   any child dispatch**.
2. Resolve each handle via `storage_root_bindings` (canonicalized bound root,
   component-wise relative-locator join, traversal/symlink/escape rejection —
   same discipline as `commit::resolve_rooted_path`, factored into a shared
   helper). Binding miss → failure before child dispatch.
3. Pre-dispatch: observe the source (stat + blake3 via the commit path's
   observation helper, factored out for reuse) against `expected`; mismatch →
   failure, no child dispatch.
4. Build the existing path-based child request and dispatch to the lease's own
   child (same stream/progress/cancellation handling as today).
5. Post-dispatch: parse the worker result; independently observe output
   file(s); compare observed vs worker-reported facts; on probe-needing
   operations, probe staged outputs through the ffprobe child
   (`ChildEndpointRegistry::resolve(ProbeFile)`); attach an
   `agent_observed` evidence block (typed, serialized into the completion
   result JSON). Mismatch → `Failure` with evidence.
6. Completion/failure settlement stays exactly as today (durable lease truth;
   no agent-side retry beyond existing child restart budget).

### C4 — Control-plane validation goes data-only

Delete `revalidate_source_file` (audio/remux/transcode),
`require_output_file_matches_result` (audio/remux/transcode), and the
`select_local_source`/`resolve_rooted_existing_path` canonicalization on the
media dispatch paths. Result validators compare the worker-reported facts and
the `agent_observed` block against DB-pinned expectations. The byte-free
`select_location` (identity rows only) stays for declaration/prepare paths.

### C5 — Backup and verification as owner-node dispatches

- `backup.rs`: delete `BundledBackUpFileDispatcher` and the in-process
  dispatch path; the backup gate mints a `BackUpFile` ticket whose payload is
  the C1 envelope and awaits lease completion (scan-run wait pattern);
  `BackupId`/checksum evidence is consumed from the result DB-only.
- `artifact/verify.rs`: delete bundled verify dispatchers/sessions; pinned
  verification becomes a `VerifyArtifact` ticket + agent execution; observed
  facts land in the completion result and are consumed data-only.

### C6 — Staging copy joins the commit intent

`OpenCommitIntent` gains `source_storage_root_id`,
`source_provider_relative_locator`, `source_location_epoch` (+ expected
source facts). During `applying` the agent materializes staging bytes
(copy-with-expected-facts, no-replace, fsync discipline of the deleted
`stage_copy`) before promoting staging→target; receipts record both steps.
`artifact/stage.rs` byte copy and the `artifact/fs.rs` copy/promote helpers
used only by it are deleted; prepare/recovery stay DB-only/receipt-only.
ADR 0074's state machine is otherwise unchanged (same states, epochs, fence).

### C7 — Deletion sweep (control plane)

Delete: `worker_process.rs` bundled launch sites for media operations (and the
module if orphaned), control-plane ffprobe staged-result launches, the local
runtime adapters (`dispatch_control_plane_transcode[_audio|_extract]`,
remux/transcode equivalents), `audio/worker_contract.rs` CP-side request
builders (rebuilt agent-side), and `artifact/worker.rs` verify launcher.
`scan/worker.rs`'s bundled ffprobe launcher survives only if the scan pump
still shares it — verify before deleting.

## Failure & durability mapping

- Child crash/timeout/malformed result → existing `LeaseOutcome::Failure`
  classification and durable fail settlement (unchanged).
- Retry → lease re-acquisition re-executes idempotently: outputs are
  no-replace/overwrite-flagged planned files; a retried dispatch re-observes
  source facts first.
- Cancellation/shutdown → existing cancellation outcomes (unchanged).
- Commit intents → ADR 0074 epochs/fence/receipts (unchanged; extended
  receipts only).

## Test strategy

- Protocol: envelope round-trip + deny-unknown-fields rejection tests
  (`operations/dispatch_test.rs`), schema-mismatch rejection.
- Agent: `media_test.rs` with registry fixtures (pattern of
  `scan_session_test.rs`): binding miss, locator escape, source mismatch,
  output mismatch, probe attach, crash/timeout classification.
- Control plane: renderer tests (handles in, no absolute paths out);
  data-only validator tests; commit-intent staging-copy tests via
  `SimulatedOwnerNode` extended with the source handle.
- E2E: extend `operator_execution_e2e.rs` to drive media ops through an
  in-process `AgentRuntime` (lifecycle pattern) instead of control-plane
  adapters; chaos overrides move to the agent-side executor.
