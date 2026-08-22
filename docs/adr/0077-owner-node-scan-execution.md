# ADR 0077: Owner-node scan execution

## Status

Accepted

## Context

Discovery, hashing, and probing still execute inside the control-plane process
(`crates/voom-control-plane/src/scan/{mod,discovery,hash}.rs`), and FFprobe receives a
control-plane-selected absolute path. The control plane therefore reads library bytes it may
not own, and the transitional local path is not race-free against concurrent ancestor
replacement (`docs/debt/0004`). ADR 0050 assigns byte ownership to storage-owner node agents;
ADR 0055 gave roots stable IDs, epochs, and provider-relative locators; issue #418/#419 built
the durable scan-session substrate (ADR 0067) whose routes accept ordered, idempotent,
completion-gated observation batches; issues #475–#478 built ticket-routed owner-local byte
work (ADRs 0069–0072): canonical artifact-access declarations, owner resolution gating before
scheduling, persisted owner evidence on every dispatch, and atomic guarded lease acquisition.
What is missing is any producer of that work: `scan_library` tickets have no production
producer, and the node agent has no scan-session code.

## Decision

Scan runs are ticket-routed onto the owner node and executed by three distinct out-of-process
workers supervised there.

**Request side.** `ControlPlane::request_scan_run(root_id)` replaces `scan_library_root`. It
fail-closes on root availability, requests one durable scan session (status `requested`, bound
to the root's owner node and current epoch), and creates one ready `scan_library` ticket whose
payload is a `WorkflowTicketPayload` carrying `scan_session_id` and `storage_root_id` in
`rendered_payload` plus the canonical read-only declaration on that root. The session ID rides
in scan work exactly as ADR 0067 requires; no pull queue is introduced.

**Execution side.** The node agent gains two new bundled worker binaries and reuses a third:

- `voom-scan-worker` implements `OperationKind::ScanLibrary`. It canonicalizes the root,
  walks it metadata-only, enforces root policies (leaf symlinks rejected, no escape from the
  canonical root, allowlist respected), classifies primaries and sidecars, and streams
  bounded candidate frames over progress messages, terminating with a traversal summary.
- `voom-hash-worker` implements `OperationKind::HashFile`. It opens the file by walking every
  path component beneath the canonical root without following symlinks, streams BLAKE3 over
  the bytes, stats before and after, hashes sidecars (SHA-256), and fails closed when facts
  changed mid-read.
- `voom-ffprobe-worker` continues to implement `OperationKind::ProbeFile`, now launched and
  supervised by the agent instead of the control plane.

Separate worker declarations yield separate capability grants (`can_execute`) derived at
activation (ADR 0057); scheduler eligibility (ADR 0072) is what prevents a scan worker from
acquiring hash work and vice versa.

The node agent owns a scan-session pump for `scan_library` leases: decode the payload, start
the session, dispatch enumeration to the scan worker, pipeline hash and probe dispatches per
candidate through its other children, assemble observations, submit ordered idempotent
batches, then complete or fail the session before settling the lease. Children never receive
control-plane credentials; only the agent holds the bearer token and incarnation fence.

**Evidence and publication.** Migration 0041 adds a nullable strict JSON `evidence` payload
to `scan_observations` (additive, ADR 0013 inventory registered). An observation carries
evidence if and only if current hash and probe results agree on stable facts; an
evidence-less observation records existence so a concurrently mutated file can never be
retired as absent, while publishing no identity. Completion publishes policy-ready identity
inside the completion transaction — same-address replay, hardlink attach by inode facts, or
fresh ingest with sidecar bundles — using the DB-only relocation of today's persist logic,
then retires unobserved pre-start locations exactly as ADR 0067 specifies. The control plane
never opens discovered bytes.

**Removal.** The control-plane-local discovery walk, byte hashing, path-grouping probe
dispatch, bundled-ffprobe launch/readiness helpers, built-in `builtin.ffprobe` worker
registration, and the direct-path CLI dispatch are deleted. `voom scan --root` requests the
run, then polls session inspection until terminal and prints the outcome envelope.

## Consequences

- A control plane host without access to library storage can now run complete scans; the
  owner node must be running its agent with all three workers declared.
- A mutated file degrades to an evidence-less observation instead of failing the session;
  content-level failures never publish stale identity, and infrastructure failures fail the
  session without partial reconciliation.
- Ticket failure or loss leaves the session `requested`; the inactivity deadline stales it.
  There is no new recovery mechanism beyond the existing deadline fencing.
- The workflow scanner-ticket result shape changes from per-file `{path, file_location_id}`
  rows to a run summary keyed by scan session; downstream consumption of scanner results must
  be reintroduced against published locations in a follow-up (#423-adjacent surface).
- Observation rows now carry up to one strict evidence payload each; batch size bounds keep
  worst-case payloads within the existing 1000-observation route limit.
- Root-policy enforcement moves into `voom-scan-worker`/`voom-hash-worker`, which bind every
  byte read to a component-wise, symlink-free descent from the canonical root — the race-free
  property debt 0004 demands for scans; #423 remains responsible for worker-dispatch
  references for transform operations.
- `local_node_id` loses its only production consumer and is removed from the control plane.

## Considered & rejected

- **Execute scan/hash inside the node-agent process.** judgment: simplest wiring, but it
  collapses the distinct scan/hash worker boundary the issue requires, forfeits the crash
  isolation and restart budget of `ChildSupervisor`, and contradicts the out-of-process
  pattern every other byte-touching executor follows (ADR 0002).
- **A dedicated evidence submission route beside batches.** judgment: a second mutable path
  into session state would need its own ordering, idempotency, and capacity rules that the
  batch ledger already provides; evidence rides the observation it belongs to.
- **Publish identity during batch acceptance.** verified: ADR 0067 §Consequences states batch
  acceptance is provisional catalog evidence and completion is the only absence
  linearization point; publishing earlier would make a failed session leave partial catalog
  mutations that cleanup must undo.
- **Keep the built-in ffprobe launcher and let the CP hand paths to it remotely.**
  verified: debt 0004 records that pathname reopen cannot bind a dispatched request to the
  validated object; moving supervision to the owner node is #421's stated expected behavior,
  and full reference-passing stays with #423.
- **Fail the whole session when any file mutates mid-hash.** judgment: one unstable file
  would veto an otherwise complete traversal; recording existence without evidence preserves
  the no-stale-facts guarantee at strictly lower cost.
