# ADR 0076: Owner-node policy tool readiness

## Status

Accepted

## Context

Policy `requires_tools` preflight (ADR 0034) satisfies concrete `ffmpeg`,
`ffprobe`, and `mkvtoolnix` tokens only through supervisor-owned, node-less
run-local providers (`local-ffmpeg-*`, `local-mkvtoolnix-*`) plus a
control-plane-host bundled ffprobe probe. After ADR 0050 and the node-agent
build-out (#417, #421, #422), every byte-touching media operation executes on
the storage owner's agent (ADR 0075): media tickets route to owner-node
envelopes, staged-result probing runs in the agent's own ffprobe child, and
the control-plane bundled adapters are deleted. The preflight therefore
describes the wrong host twice over: a fully remote deployment with healthy
owner-node workers fails before scheduling, while a policy whose targets sit
on a node without workers passes preflight on the strength of control-plane
providers that will never receive a lease. ADR 0034's "Later decision"
section already commits to owner-scoped remote readiness; issue #424 is its
implementation.

The durable substrate exists: worker rows carry `node_id`,
`node_incarnation_id`, and a monotonic epoch; nodes carry an auth token,
epoch, and exactly one active incarnation; node agents register declared
workers (`kind=remote`) with capability and grant rows at activation, and each
child must pass an identity challenge against its credentials before it stays
up.

## Decision

1. **Targets group by storage owner.** For a compliance run with stored
   inputs, tool readiness resolves every target file version to its live
   rooted location (the same fail-closed selection the executor uses) and
   groups the resulting storage roots by their owner node. A target with no
   live rooted location, or a root with no owner node, is itself an
   unavailable observation — it can never dispatch, so preflight reports it
   instead of guessing a host.

2. **Per-(node, tool) observation replaces per-tool fleet observation.** Each
   required tool is observed independently for every owner node:
   - candidates are ready remote workers owned by that node whose bound
     incarnation is the node's current active incarnation. Activation leaves a
     declaration in `registered`; the node agent changes it to `active` only
     after the child binds, completes the exact-version handshake, proves its
     identity, and finishes dependency preflight. A crash changes it back to
     `registered` before lease settlement and restart; a successful restart
     changes it to `active` before polling resumes;
   - the node must be non-retired, carry an active incarnation, and pass the
     same heartbeat-expiry predicate lease acquisition uses at the control
     plane's current clock time;
   - eligibility keeps ADR 0034 semantics: matching capability, effective
     grant with deny-wins, per the tool's operation set (`ffmpeg`:
     `transcode_video`/`transcode_audio`/`extract_audio`; `mkvtoolnix`:
     `remux`; `ffprobe`: `probe_file`);
   - video-backend readiness uses only those same owner's effective
     `transcode_video` candidates. An activation declaration for such a worker
     may carry one tagged `VideoAcceleratorDescriptor`. The node-agent
     configuration pins the expected probe result and supplies only its stable
     identity and session limit to the child. After dependency preflight, the
     child returns its actual structured `LocalWorkerBound` descriptor. The
     supervisor requires an exact match before it reports ready. Activation
     writes the verified-equal declaration's stable token to
     `worker_capabilities.hardware` and the descriptor to
     `worker_capabilities.extra.accelerator`, the existing scheduler contract.

   A healthy worker or matching accelerator on any other node contributes
   nothing to a target it does not own.

   The control plane does not open a direct child HTTP line. The node agent
   owns the bind, accelerator-metadata, exact-version, identity, and dependency
   proofs, then posts an authenticated node/incarnation/worker-fenced readiness
   transition. The transition is a naturally idempotent state assignment and
   creates no replay row that would retain terminal worker history.
   Authorization remains ordered token first, then active-incarnation and
   worker ownership, then heartbeat freshness, then mutation. Failed startup
   deactivates the incarnation; crash and restart bracket lease settlement and
   process downtime with not-ready/ready transitions. A failed or exhausted
   incarnation, an expired heartbeat, a declared-but-not-ready, stale, or
   retired row, or missing capability/grant rows each render their own
   diagnostic.

3. **ffprobe loses its control-plane-host shortcut.** The bundled ffprobe
   readiness probe and the built-in ffprobe bootstrap leave the policy tool
   preflight entirely; `ffprobe` is satisfied exactly like the other tools by
   the owner node's agent-supervised ffprobe worker. This is the retargeting
   ADR 0075 explicitly reserved for this change. The `builtin.ffprobe`
   reserved-name guard in general registration remains: legacy durable rows
   still exist, and impersonating them must stay impossible.

4. **Diagnostics are per node and deterministic.** All observations run
   before reporting; unavailable pairs render one line per (node, tool)
   ordered by node then tool, with target-level problems (unlocated target,
   unowned root) reported alongside. Guidance points the operator at the
   owning node — start or repair the node agent there — replacing the
   `voom worker run-local` guidance that described the control-plane host.

5. **Authorization stays where it was.** Preflight remains a prerequisite
   snapshot, not a reservation. Scheduler authorization continues to be
   re-checked atomically inside lease acquisition (#343); this change does
   not move it.

## Consequences

- A fully remote policy using software, NVIDIA NVENC, VAAPI, or
  `VideoToolbox` passes preflight when each target's owner has a ready
  agent-supervised worker whose child returned the exact configured accelerator
  descriptor, and fails with actionable per-node lines when it does not.
- Single-host deployments use the same owner-scoped path: the node agent on
  the host owns the tools and verifies accelerator declarations against child
  startup probes.
- Run-local reserved providers no longer satisfy any tool token. Operators who
  relied on them must run a node agent on the storage owner; the deleted
  control-plane execution path made those providers unable to serve remote
  leases anyway.
- There is no schema migration: existing worker status and capability columns
  carry the facts. The activation payload and node-agent worker configuration
  add the optional tagged accelerator descriptor, and the remote execution API
  adds the authenticated worker-readiness transition. This is a coordinated
  protocol/configuration contract change, not a read-only reinterpretation of
  old rows.

## Considered and rejected alternatives

- **Keep run-local providers as a fallback for unowned roots.** Rejected: a
  fallback host is the defect this change removes; post-ADR 0075 those
  providers cannot execute the bytes, so accepting them recreates the
  pass-preflight-fail-dispatch failure honestly reported today instead of
  fixing it.
- **One common owner across all policy targets (fold like ticket access).**
  Rejected: tickets are per-file and naturally span roots; folding would
  forbid legitimate multi-node policies that criterion 4 requires reporting
  deterministically rather than rejecting.
- **Have the control plane re-prove each child's identity over HTTP.**
  Rejected: there is no control-plane→child line left to prove it over; the
  agent already kills unprovable children, and duplicating that enforcement
  would need a new durable endpoint channel for zero added assurance.
- **Preflight leases or reserves the observed workers.** Rejected again, as in
  ADR 0034 §5: preparation is an observational interval; later provider loss
  is an ordinary, honestly reported dispatch failure.
- **New durable tool-availability table.** Rejected: duplicates worker
  capabilities and adds a second lifecycle, as in ADR 0034.
