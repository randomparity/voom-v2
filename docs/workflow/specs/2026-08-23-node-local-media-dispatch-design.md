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
   extract-audio, transcode-video, remux, backup-file, verify-artifact)
   execute on the storage-owner node agent.
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
- Policy-tool readiness preflight retargeting — #424.
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
  provider_relative_locator: ProviderRelativeLocator, overwrite: bool }` —
  a planned file that does not exist yet (relative locator created
  deterministically by the control plane from branch/output identity).
- `#[serde(tag = "operation", rename_all = "snake_case")] enum
  MediaDispatch { ... }` whose variants are newtype wrappers over
  `#[serde(deny_unknown_fields)]` content structs (ADR 0013 durable-enum
  rule), one per operation:
  - `Probe { schema, source: MediaSourceRef, expected: ExpectedFileFacts }`
  - `TranscodeAudio { schema, source, expected: AudioExpectedFacts,
    output_container, output: MediaPlannedOutput, selection,
    settings: TranscodeAudioSettings }`
  - `ExtractAudio { schema, source, expected, output_container,
    outputs: Vec<MediaExtractOutput> }` where `MediaExtractOutput
    { output_id, selection: AudioStreamRef, audio_codec, output }` is the
    handle-shaped counterpart of the path-based extraction descriptor
  - `TranscodeVideo { schema, source, expected: TranscodeVideoExpectedFacts,
    output_container, output_video_codec, output, profile,
    hardware_assignment?, copy_video }`
  - `Remux { schema, source, expected: RemuxExpectedFacts, output_container,
    output, selection: RemuxSelection }`
  - `BackUpFile { schema, source, destination: MediaPlannedOutput }`
  - `VerifyArtifact { schema, target: MediaSourceRef,
    expected: VerifyArtifactExpectedFacts }` (staging-rooted)
- The add-only commit staging copy is deliberately **not** an envelope
  variant: the fenced commit intent is its authoritative channel (C6).
- Each content struct embeds `schema: u32` enforced against
  `PROTOCOL_VERSION` by `decode_media_dispatch`, so a same-version binary
  with divergent payload shape still fails decode before lease execution.
- Existing child request/result wire types are unchanged (path-based,
  trusted boundary). The `PROTOCOL_VERSION` bump covers child handshake skew.

### C2 — Ticket rendering (voom-control-plane)

`workflow/plan/binding.rs` renderers emit a nested `media_dispatch`
(`MediaDispatch`) object for the seven operations while keeping the existing
scalar keys (`source_storage_root_id`, `source_location_id`) untouched:
`WorkflowTicketPayload::validate_artifact_access` keeps deriving the
declaration from those scalar keys and its equality check is unchanged.
Storage sources come from `TicketStorageSource` (location → `MediaSourceRef`
from the location's rooted address); verify-artifact tickets are always
location-sourced from the producing operation's recorded staged output
address (never whole-root). Staging/output/backup destinations resolve to
their configured storage roots (`LibraryRoot.default_output/staging/
backup_root_id`; unset → render error) with deterministic relative locators
derived from branch/output identity. Renderers stop emitting `staging_root`
path strings and emit `overwrite: false` on every planned output (real
workers reject `overwrite: true`).

### C3 — Agent-side media executor (voom-node-agent)

New `media.rs` routed from `dispatch_outcome` for every byte-touching
operation except `scan_library` (existing pump):

1. Strict-decode `MediaDispatch` from the raw `dispatch_payload` **before**
   `augment_payload` runs (the scan-pump precedent: `scan_library_outcome`
   bypasses augmentation) — the injected `artifact_access_plan` sibling keys
   would otherwise break strict decode. Wrong/missing schema or unknown
   shape → `LeaseOutcome::Failure(MalformedWorkerResult)` **before any child
   dispatch**.
2. Resolve each handle via `storage_root_bindings` (canonicalized bound root,
   component-wise relative-locator join, traversal/symlink/escape rejection —
   same discipline as `commit::resolve_rooted_path`, factored into a shared
   helper). Binding miss → failure before child dispatch, so a worker can
   never resolve or mutate a root owned by another node.
3. Pre-dispatch: observe the source (stat + blake3 via the commit path's
   observation helper, factored out for reuse) against `expected`; mismatch →
   failure, no child dispatch.
4. Planned outputs hold no durable identity until completion evidence lands,
   so retry is idempotent: the agent clears stale residue at the planned
   output path (left by a crashed prior attempt) before child dispatch, then
   builds the path-based child request with `overwrite: false`.
5. Dispatch to the lease's own child (same stream/progress/cancellation
   handling as today).
6. Post-dispatch: parse the worker result; independently observe output
   file(s); compare observed vs worker-reported facts; on probe-needing
   operations, probe staged outputs through the ffprobe child
   (`ChildEndpointRegistry::resolve(ProbeFile)`); attach an
   `agent_observed` evidence block (typed, serialized into the completion
   result JSON). Mismatch → `Failure` with evidence.
7. Completion/failure settlement stays exactly as today (durable lease truth;
   no agent-side retry beyond the existing child restart budget).

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
`stage_copy`) before promoting staging→target; receipts record both steps
(additive extension). `artifact/stage.rs` byte copy and the `artifact/fs.rs`
copy/promote helpers used only by it are deleted; prepare/recovery stay
DB-only/receipt-only. ADR 0074's state machine is otherwise unchanged (same
states, epochs, fence).

### C7 — Deletion sweep (control plane)

Delete: `worker_process.rs` bundled launch sites for media operations (and
the module if orphaned), control-plane ffprobe staged-result launches, the
local runtime adapters (`dispatch_control_plane_transcode[_audio|
_extract]`, remux/transcode equivalents), `audio/worker_contract.rs`
CP-side request builders (rebuilt agent-side), and `artifact/worker.rs`
verify launcher. The `scan/worker.rs` bundled ffprobe launcher **survives**
for its remaining consumer, the policy tool preflight
(`cases/policy/tool_preflight.rs` — #424 scope); the probe-helper imports in
`audio|remux|transcode/commit.rs` die with the validator rewrite.

## Failure & durability mapping

- Child crash/timeout/malformed result → existing `LeaseOutcome::Failure`
  classification and durable fail settlement (unchanged).
- Retry → lease re-acquisition re-executes idempotently: pre-dispatch source
  observation gates every attempt; planned-output residue from a crashed
  attempt is cleared before re-dispatch (C3 step 4).
- Cancellation/shutdown → existing cancellation outcomes (unchanged).
- Commit intents → ADR 0074 epochs/fence/receipts (unchanged; extended
  receipts only).

## Test strategy

- Protocol: envelope round-trip + deny-unknown-fields rejection tests
  (`operations/dispatch_test.rs`), schema-mismatch rejection.
- Agent: `media_test.rs` with registry fixtures (pattern of
  `scan_session_test.rs`): binding miss, locator escape, source mismatch,
  output mismatch, stale-residue clearing, probe attach, crash/timeout
  classification.
- Control plane: renderer tests (handles in, scalar keys preserved, no
  absolute paths out); data-only validator tests; commit-intent staging-copy
  tests via `SimulatedOwnerNode` extended with the source handle.
- E2E: extend `operator_execution_e2e.rs` to drive media ops through an
  in-process `AgentRuntime` (lifecycle pattern) instead of control-plane
  adapters; chaos overrides move to the agent-side executor.
