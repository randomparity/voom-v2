---
status: accepted
date: 2026-07-24
deciders: [VOOM core]
---

# 0034 — Policy tool requirements use worker capabilities

## Context

The published policy grammar accepts `metadata.requires_tools`, but compiled
policies retain it only in the generic metadata map and emit a deferred warning.
Execution therefore starts without checking whether the declared tools are
available. Issue #327 closes that gap for the published vocabulary: `ffmpeg`,
`ffprobe`, and `mkvtoolnix`.

VOOM already represents executable work through durable worker capabilities and
grants. Run-local mutation workers start a concrete bundled provider whose
startup preflights its executable, and bundled ffprobe can prove readiness by
starting across the existing worker-process boundary. The missing pieces are a
typed policy view, supervisor-owned provider identity, and an execution
preflight over those facts.

The design is detailed in
[`docs/superpowers/specs/2026-07-24-issue-327-requires-tools-preflight-design.md`](../superpowers/specs/2026-07-24-issue-327-requires-tools-preflight-design.md).

## Decision

1. `voom-policy` defines the closed `PolicyTool` enum for the three published
   names and exposes a typed `CompiledPolicy::required_tools()` view over the
   existing `metadata.requires_tools` JSON. The compiler validates that new
   source uses a list of unquoted identifiers from this finite vocabulary and
   emits no deferred warning for valid declarations. Repeating the
   `requires_tools` metadata key is a validation error; lowering must never let
   a later declaration silently replace requirements from an earlier one.

   No field is added to serialized `CompiledPolicy`: schema version 2, stored
   JSON bytes, and source hashes remain unchanged, so #327 introduces no new
   cross-version read shape. Existing compiled policies remain readable. The
   pre-existing mismatch between the payload inventory and the higher-layer
   typed `CompiledPolicy` read is tracked independently by #344.

   Legacy JSON has already erased whether a string originated as a quoted
   string or identifier; the compatibility view accepts canonical tool strings
   regardless of that lost lexical provenance. Newly compiled source remains
   identifier-only. Malformed or unknown legacy values fail execution loudly.

2. Concrete mutation-provider identities are reserved control-plane namespaces:

   - `local-ffmpeg-<unique>` with `WorkerKind::Local`, no node, and the ffmpeg
     operation family;
   - `local-mkvtoolnix-<unique>` with `WorkerKind::Local`, no node, and `remux`.

   General worker registration rejects these reserved prefixes. Only the
   run-local supervisor's internal registration path may create them, after
   selecting the matching version-locked binary. The child advertises readiness
   only after its dependency preflight succeeds.

   Worker protocol v2 adds a server-authenticating identity challenge. The
   control plane sends a fresh random challenge without the bearer credential.
   The worker returns its ID, epoch, protocol version, and a domain-separated
   BLAKE3 proof keyed from its secret over the canonical challenge and response
   identity. The control plane verifies that proof with the recorded secret and
   requires the returned ID and epoch to match the reserved durable
   incarnation. An endpoint that merely echoes request fields cannot satisfy
   the proof without the secret. Challenges use an OS-seeded CSPRNG, and proof
   bytes are compared in constant time; a captured proof cannot authenticate a
   later challenge. A successful unauthenticated version handshake alone is not
   identity evidence. The identity round-trip, including response collection,
   has the same ten-second ceiling as the handshake. Bumping `PROTOCOL_VERSION`
   from 1 to 2 is the ADR 0016 flag day; bundled workers and the control plane
   move together.

3. The finite published vocabulary maps to effective capabilities:

   - `ffmpeg` requires a live reserved ffmpeg incarnation with an effective
     grant for at least one of `transcode_video`, `transcode_audio`, or
     `extract_audio`;
   - `mkvtoolnix` requires a live reserved mkvtoolnix incarnation with an
     effective `remux` grant;
   - `ffprobe` requires a fresh successful startup and handshake of the bundled
     ffprobe worker, followed by a live built-in ffprobe incarnation with an
     effective `probe_file` capability and grant.

   An effective capability has a matching capability row and grant, no matching
   deny across any grant row, and worker status `registered` or `active`.
   Endpoint-backed workers must prove the recorded identity challenge.
   Stale, retired, denied, ungranted, dead-endpoint, remote, synthetic,
   credential-mismatched, or wrong-identity workers do not satisfy a concrete
   tool token.

4. Built-in ffprobe identity is recoverable. The reserved bootstrap path adopts
   a live legacy `builtin.ffprobe` row when present. If no live incarnation
   exists because the row is absent, stale, or retired, it creates a new
   node-less local incarnation under a reserved unique suffix and records the
   standard capability and grant. General registration cannot claim this
   namespace. A denied live incarnation is not silently replaced; preflight
   fails with the deny context.

   `voom-ffprobe-worker` treats a failed, timed-out, or malformed
   `ffprobe -version` probe as a startup dependency error and does not advertise
   `BOUND`. Preflight starts a short-lived bundled worker, completes the
   protocol identity challenge, and shuts it down before accepting or creating
   the durable incarnation. Every success, timeout, and error path shuts down
   and reaps the child. A durable row alone is never readiness proof.

   Resolution runs under the repository's `BEGIN IMMEDIATE` single-writer
   pattern. After acquiring the write reservation it re-reads all live reserved
   ffprobe incarnations. It adopts the sole live row, creates one row only when
   none exists, and treats multiple live rows as a reserved-identity invariant
   error. Concurrent preflights therefore converge on one live incarnation
   rather than creating siblings or surfacing a transient SQLite busy error.

5. Policy/input loading plus tool preflight produces prepared phase-barrier
   inputs. Compliance execute prepares once before applying report findings and
   passes those inputs to the coordinator without repeating the preflight.
   Direct phase-barrier execution and resume use the same preparation path
   before opening a new job. A failed initial preflight therefore creates no
   compliance issue, job, ticket, lease, or execution event.

   This is an observational prerequisite interval, not an atomic snapshot or
   reservation: each requirement was independently observed satisfied during
   preparation. The design does not claim the full set was simultaneously
   available or remains available when preparation returns. Provider state can
   change before a later phase; existing dispatch failures remain possible and
   must report their partial durable outcome honestly.

   This readiness preflight does not make or enforce the later dispatch
   authorization decision. The current scheduler evaluates grant rows
   independently, so a split allow/deny state or authorization change after
   preparation can still permit dispatch despite a deny. Atomic effective
   eligibility at lease acquisition is separate work tracked by #343. Issue
   #327 establishes provider readiness only and does not claim that the
   scheduler's deny semantics are trustworthy until #343 is resolved.

6. Requirement checks produce one typed observation per tool. Missing, stale,
   retired, ungranted, denied, dead-endpoint, wrong-identity, and bundled
   dependency failures are tool-unavailable observations. All such observations
   are evaluated and rendered in source order as one `POLICY_EXECUTION_ERROR`,
   with a reason and operator guidance for every unavailable tool.

   Malformed compiled metadata fails before observation. Database failures and
   durable reserved-identity invariant violations abort observation immediately
   with their specific error context; the complete-list promise does not apply
   when the system cannot reliably inspect its state.

7. The bundled verify-artifact worker remains represented by its existing
   `verify_artifact` capability but satisfies no published tool token. The V1
   metadata vocabulary does not publish a verify tool, and this change does not
   add one.

## Consequences

- New compiled policies have the same durable shape as old policies. The typed
  view makes existing metadata executable without a migration, schema-version
  bump, new payload field, or rollback hazard.
- Existing stored deferred warnings remain in stored JSON. The control plane
  removes that obsolete warning in memory after successfully typing the
  requirement, so reports do not claim an enforced prerequisite is deferred.
- Concrete `ffmpeg` and `mkvtoolnix` requirements are intentionally satisfied
  only by supervisor-owned run-local workers. A future remote provider-identity
  contract must be designed before remote workers can satisfy these literal
  tokens.
- The initial preflight records one successful observation per requirement
  during preparation and prevents a requirement that fails its observation from
  opening execution state. It cannot guarantee simultaneous or future
  availability without leases or a long-lived worker reservation.
- The worker protocol bumps to v2 and adds one read-only identity
  challenge-response route. The probe never transmits the worker secret.
  Version-locked bundled workers move with the control plane; v1 workers fail
  the exact-match check before they can satisfy a tool requirement.
- No new dependency or parser production is introduced.

## Considered and rejected alternatives

- **Add `required_tools` to serialized `CompiledPolicy`.** Rejected because the
  existing metadata already contains the declaration. A new durable typed field
  would add rollback and ADR 0013 obligations without carrying new information.
- **Check executable paths in the control-plane process.** Rejected because it
  describes the host rather than the provider. Bundled ffprobe readiness starts
  the actual worker and crosses the ADR 0002 protocol boundary.
- **Create a separate tool-availability table.** Rejected because it duplicates
  worker capabilities and introduces a second lifecycle.
- **Infer provider identity from operation names or caller-chosen names.**
  Rejected because capabilities are implementation-neutral and general
  registration can choose names. Reserved supervisor-owned identities bind the
  literal tool claim to the worker incarnation.
- **Send the bearer credential to an identity endpoint.** Rejected because an
  endpoint impersonating the recorded address could echo the credential and
  claimed identity. A fresh challenge proves possession without disclosing the
  secret in the probe.
- **Require all ffmpeg operations on one worker.** Rejected because a real
  ffmpeg provider may intentionally expose a safe subset. Per-operation
  scheduling still enforces the narrower capability during dispatch.
- **Keep a preflight ffprobe process alive for the entire multi-phase run.**
  Rejected because metadata preflight is a prerequisite snapshot, not a resource
  lease, and the normal operation path owns worker lifetime. Later provider loss
  remains an ordinary, honestly reported execution failure.
- **Add `verify` to the metadata vocabulary.** Rejected because it is not in the
  published grammar and would be an unpublished DSL extension.

## Later decision: owner-scoped remote readiness

ADR 0050 supersedes the blanket exclusion of remote workers from concrete tool
readiness. Under the node-agent model, a byte-touching ticket first identifies
its required storage owner. A concrete `ffmpeg`, `ffprobe`, or `mkvtoolnix`
requirement may be satisfied by an authenticated, exact-version provider
supervised by that owner's active node incarnation.

Typed requirements, reserved supervisor authority, challenge-response identity,
deny-wins effective grants, dependency startup proof, and fail-before-execution
behavior remain. Readiness is owner-scoped; a healthy provider on another node
does not satisfy the ticket. Issue #424 replaces the run-local-only selection
without weakening those checks.
