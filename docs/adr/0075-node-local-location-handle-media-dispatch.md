# ADR 0075: Node-local location-handle media dispatch

## Status

Accepted

## Context

Issue #423 closes the remaining media-byte legs of the transitional
control-plane filesystem path (ADR 0050, AGENTS.md). After #421 (scan/hash/probe)
and #422 (fenced commit intents), the still-path-based surfaces are:

- Worker-protocol payloads carry raw absolute paths (`ProbeFileRequest.path`,
  `BackUpFileRequest.source_path/destination_path`, `RemuxInput.path`,
  `TranscodeVideoInput/Output`, `TranscodeAudioInput/Output`,
  `ExtractAudioInput/Output`, `VerifyArtifactRequest.path/staging_root`).
  No payload references `StorageRootId` or any location handle; ADR 0002
  requires workers to receive artifact handles instead of raw-path assumptions.
- The control plane canonicalizes and stats sources before dispatch
  (`operation_source::select_local_source` /
  `resolve_rooted_existing_path`), re-validates source facts pre-dispatch
  (`revalidate_source_file`, three copies) and observes output bytes
  post-dispatch (`require_output_file_matches_result`, three copies).
- The control plane launches bundled workers directly
  (`worker_process::BundledWorkerProcess`) for ffmpeg/mkvtoolnix/ffprobe/
  backup/verify operations, copies source bytes into staging itself
  (`artifact/stage.rs`), and probes staged synthesis results with an in-process
  ffprobe launch (`audio|remux|transcode/commit.rs`).
- A remote agent receiving a real-media lease today forwards an opaque payload
  its children cannot execute: there is no node-local resolver and no unified
  media contract between "control-plane local" adapters and agent execution.

The pull-based prerequisites have shipped: HTTPS API (#416), the polling node
agent with child supervision and a cross-worker endpoint registry (#417),
provider-relative locations (#418), owner-local scheduling and access plans
(#420, ADRs 0069–0073), owner-node scan execution (#421, ADR 0077), and fenced
node-local commit intents (#422, ADR 0074) whose agent side already resolves
`(storage_root_id, provider_relative_locator)` handles to local paths.

Two boundaries matter. The control-plane↔agent boundary is untrusted naming:
paths exposed there defeat node ownership and shared-namespace assumptions.
The agent↔child boundary is trusted and node-local: children are processes the
agent supervises on the same host.

## Decision

### One lockstep protocol change; handles cross the untrusted boundary

`voom_core::PROTOCOL_VERSION` moves 2 → 3. Exact-version matching (ADR 0016)
already fails mixed binaries at handshake/activation, satisfying "protocol
mismatch fails before lease execution" for version skew.
For payload skew, voom-worker-protocol gains one dispatch-envelope family
(`operations/dispatch.rs`): a tagged enum over the byte-touching media
operations — probe, transcode-audio (including synthesize/add-track),
extract-audio, transcode-video, remux, backup-file, and verify-artifact.
Every variant carries only
stable vocabulary: `StorageRootId` + `ProviderRelativeLocator` pairs for
sources and planned outputs, expected-fact blocks, and (where the destination
is fenced) root/location epochs mirroring the ADR 0074 intent shape. No
variant carries an absolute path. Envelopes render into the durable ticket
payload under the ADR 0013 deny-unknown-fields contract; migration 0042
carries a preflight guard that aborts the upgrade when in-flight
non-terminal media workflow tickets exist, because their path-shaped
payloads cannot be re-rendered by the new binary (pre-release; identical
abort semantics to migration 0038).

The agent parses the envelope strictly (deny-unknown-fields decode) **before**
executing anything: an unparseable or wrong-schema dispatch fails the lease
without touching a child — payload mismatch fails before lease execution even
between same-version binaries. Child wire types stay path-based inside the
trusted node-local boundary. The agent resolves each handle against its
configured storage-root bindings (the `StorageRootBinding` map the commit
coordinator already indexes), canonicalizes the bound root once, joins the
relative locator with traversal/symlink/escape rejection, and builds the
existing path-based child request. A binding miss (root owned by another node,
or unbound) fails before child dispatch, so a worker can never resolve or
mutate a root owned by another node.

### Fact checks live where the bytes are

Pre-dispatch source observation (stat + blake3 against expected facts) and
post-dispatch output observation move into the agent; the three control-plane
`revalidate_source_file` / `require_output_file_matches_result` copies are
deleted. The agent attaches its independently observed input/output facts to
the lease completion result; the control plane validates results data-only
against pinned expectations. Staged-result probing moves into the agent: after
a mutation completes, the agent probes the staged output through its ffprobe
child (via the existing `ChildEndpointRegistry` cross-worker dispatch) and
attaches snapshot evidence to the completion result, replacing the
control-plane ffprobe launches.

### Staging copy joins the fenced commit intent

Add-only commit staging stops being a control-plane byte copy
(`artifact/stage.rs`). The fenced commit intent is the authoritative channel
for this copy — it is deliberately **not** part of the lease-envelope family,
so no second execution path exists. The ADR 0074 intent carries the source
location handle in addition to the staging/target addresses; the agent
materializes staging bytes during `applying` and reports receipts under the
existing fenced state machine. Prepare stays DB-only; recovery stays
receipt-only.

### One media contract

The control-plane local adapters (`dispatch_control_plane_*` in
audio/transcode/remux workflow modules), the direct bundled-worker launchers,
and the control-plane byte machinery used only by them are deleted. Local and
remote execution share one envelope, one fact-check placement, and one
executor shape: the node agent. Backup and artifact verification become
owner-node dispatches of the same family rather than in-process control-plane
worker sessions.

The policy-tool readiness preflight keeps its control-plane-side bundled
ffprobe check, which targets the wrong host once probing runs on the storage
owner; retargeting it is issue #424's assigned surface and is excluded here.

## Considered & rejected

- **Handle-shaped child wire types** (children resolve handles themselves):
  every worker binary would need root bindings and trust-boundary discipline;
  the agent already owns exactly this resolution for commit intents. Rejected
  for churn without a consumer.
- **Keep dual contracts** (control-plane adapters for co-located deployments,
  envelopes for agents): preserves the SQLite host in the byte path — the
  defect this change exists to remove — and doubles the validation surface.
  Single-host media work requires a node agent on the host; restoring local
  acceptance flows is issue #436's assigned surface. Rejected.
- **Capability-flag negotiation instead of a version bump**: exact-version
  matching is the established ADR 0016 posture; a capability matrix adds a
  second mechanism for skew that the bump already catches. Rejected.
- **A new durable table for media dispatches**: tickets/leases already carry
  the durable truth; a parallel table would fork retry/recovery semantics the
  lease state machine already owns. Rejected.
- **Do nothing**: leaves real-media execution broken on remote nodes (agents
  receive payloads their children cannot execute) and the control plane in the
  byte path, contradicting ADR 0050 and the epic's goal set. Rejected.

## Consequences

- Mixed-version control plane/agent binaries fail at activation, not mid-lease.
- Migration 0042 carries a preflight guard that aborts the upgrade when
  in-flight non-terminal media workflow tickets exist (pre-release; identical
  semantics to migration 0038).
- Single-host deployments run one node agent process to execute media work.
- Synthetic/fake providers keep operating unchanged: their results echo the
  dispatched evidence, and non-byte-touching operations never see envelopes.
- The operator inspection surface (`artifact/inspect.rs`) and the
  workflow-coordinator terminal-artifact move/reclaim path remain as-is;
  both are owned by the follow-up split around #436.
- Backup destinations move from control-plane-built absolute paths to
  planned outputs addressed on configured backup storage roots; operators
  provisioning nodes must bind those roots in agent configuration, and
  pre-existing control-plane backup trees are not migrated.
