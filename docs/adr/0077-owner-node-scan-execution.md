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
  supervised by the agent instead of the control plane. The path it receives is
  reconstructed by the agent as the canonical root joined with the validated relative
  locator — always absolute, therefore never option-like — but probing remains a pathname
  reopen: reference-passing through probing is #423's remaining work, so the probe leg does
  not yet bind an open object the way the scan and hash legs do.

Separate worker declarations yield separate capability grants (`can_execute`) derived at
activation (ADR 0057); scheduler eligibility (ADR 0072) is what prevents a scan worker from
acquiring hash work and vice versa.

The node agent owns a scan-session pump for `scan_library` leases: decode the payload, start
the session, dispatch enumeration to the scan worker, pipeline hash and probe dispatches per
candidate through its other children, assemble observations, submit ordered idempotent
batches, then complete or fail the session before settling the lease. Children never receive
control-plane credentials; only the agent holds the bearer token and incarnation fence.

Agreement is a pump-side predicate with an exact fact set: evidence attaches only when (a)
the hash worker's post-read stat equals its pre-read stat, and (b) the probe result's
`pre_probe` and `post_probe` facts each match the hash result's `size_bytes`, `content_hash`,
and `modified_at`. A mutation between hash read and probe read fails (b) and leaves the
observation evidence-less; nothing compares beyond this fact set anywhere else.

The predicate governs primary-file identity only. Sidecars are hashed on the node but never
probed — media metadata exists only for primaries — so a primary's sidecar digests ride
inside its evidence payload once the primary agrees, and no observation is ever emitted for
a sidecar alone.

**Evidence and publication.** Migration 0041 adds a nullable strict JSON `evidence` payload
to `scan_observations` (additive, ADR 0013 inventory registered). An observation carries
evidence if and only if current hash and probe results agree on stable facts; an
evidence-less observation records existence so a concurrently mutated file can never be
retired as absent, while publishing no identity. Each enumerated candidate gets exactly one
hash/probe attempt per session and is recorded immediately as evidence-less when that
attempt fails — the pump never re-dispatches a failed candidate, because the server-enforced
`(session_id, locator)` uniqueness would turn any later success into a duplicate-locator
conflict and an unbounded retry loop; a later scan session covers the file. Vanishing counts
differently from failing: a candidate that no longer exists when hash or probe reaches it
(ENOENT) yields no observation at all, because its absence is real and completion may retire
it; every other failed attempt (unreadable file, content drift, malformed media,
infrastructure error) yields an evidence-less observation that blocks false retirement.
Completion publishes policy-ready identity inside the completion transaction — same-address
replay, hardlink attach by inode facts, or fresh ingest with sidecar bundles — using the
DB-only relocation of today's persist logic, then retires unobserved pre-start locations
exactly as ADR 0067 specifies. The control plane never opens discovered bytes.

**Removal.** The control-plane-local scan pipeline is deleted: the discovery walk
(`scan/discovery.rs`), byte hashing (`scan/hash.rs`), candidate grouping and probe dispatch
with the direct-path entry points in `scan/mod.rs`, the filesystem checks of the old
`scan/library.rs`, and the direct-path CLI dispatch. Everything else that still serves
non-scan surfaces stays: `scan/worker.rs`'s bundled-ffprobe launcher and readiness helper,
and `scan/bootstrap.rs`'s built-in worker registration are consumed by audio/remux/transcode
commit probing and policy tool preflight today; #423/#424 own their later deletion or
relocation. `voom scan --root` requests the run, then polls session inspection until
terminal and prints the outcome envelope.

## Consequences

- A control plane host without access to library storage can now run complete scans; the
  owner node must be running its agent with all three workers declared.
- A mutated file degrades to an evidence-less observation instead of failing the session;
  content-level failures never publish stale identity, and infrastructure failures fail the
  session without partial reconciliation.
- Ticket failure or loss leaves the session `requested` until the inactivity deadline stales
  it; the session must then be re-requested by hand — ADR 0067 lets only the trusted local
  operator request sessions, so an unattended deployment stalls until a human re-requests
  the scan.
- The workflow scanner-ticket result shape changes from per-file `{path, file_location_id}`
  rows to a run summary keyed by scan session; downstream consumption of scanner results must
  be reintroduced against published locations in a follow-up (#423-adjacent surface).
- Discovery and hash byte access binds to a component-wise, symlink-free descent from the
  canonical root — the race-free property debt 0004 demands for those legs. The probe leg
  still reopens a reconstructed absolute pathname until #423 lands reference-passing; the
  drift checks around each probe bound what that residual can publish, but they do not bind
  an open object.
- Observation rows now carry up to one strict evidence payload each. Batches are bounded by
  both the 1000-observation route limit and an accumulated-evidence byte budget under the
  API's request-body cap (~1 MiB): the pump flushes whichever bound binds first, so a
  sidecar-dense root produces more, smaller batches instead of a deterministically rejected
  submission.
- ADR 0067's 100,000 cumulative-observation session ceiling is inherited unchanged: a root
  enumerating more than that fails batch acceptance deterministically and re-requests
  reproduce the failure, where the transitional local scanner had no ceiling. Raising or
  chunking the cap is ADR 0067's decision to revisit, not this one's.
- The scan path stops consuming `local_node_id`; the field itself keeps its
  transform/commit consumers (artifact stage and commit preparation, policy verification,
  audio/remux/transcode source selection, workflow promotion) owned by #423 and successors
  and is not removed by this change.

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
- **Keep the transitional control-plane-local scan path until #423's reference-passing
  design lands.** judgment: it avoids new worker surface today, but ADR 0050 already
  commits byte ownership to the owner node, debt 0004 records the current path as not
  race-free, and the durable session substrate this builds on is otherwise unused — waiting
  keeps a known defect alive for no simplification that survives #423.
- **Harden the probe leg now with a root+locator payload and worker-side component-wise
  descent.** judgment: it would narrow the ancestor-replacement residual before #423, but it
  adds a second addressing mode to the shared `ProbeFileRequest` contract that #423 replaces
  one issue later, and debt 0004 explicitly warns against duplicating or preempting the
  dispatch-reference contract; the pre/post drift checks bound what the pathname reopen can
  publish in the interim.
- **Keep session orchestration in the control plane while owner-node workers do the byte
  work.** verified: it is structurally simpler, but ADR 0067 gates every start/batch/success/
  failure route on the remote-node bearer token plus the current-incarnation fence, so only
  the agent can drive a session — a control-plane pump has no authorized caller.
