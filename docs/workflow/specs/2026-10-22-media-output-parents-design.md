# Planned media-output parent preparation (#727)

## Problem and authority

The approved #339 canary reaches bundled media workers after #726, but its three
media tickets reject a missing output parent. `planned_outputs` resolves handles;
worker preflight correctly requires existing staging directories. #727 and the
operator-approved concrete design authorize this repair under accepted ADR 0075.

## Scope and design

Extend `media.rs::planned_outputs`, the existing storage-owner destination owner.
For each planned write, resolve its handle with `rooted_path` before mutation,
create its parent with the existing node-local `tokio::fs::create_dir_all` pattern,
then resolve the same handle again and check the parent is a real directory.
Propagate preparation failures with an actionable path/operation error before
stale residue removal or child dispatch. Existing rooted resolution rejects
unbound roots, traversal and symlinked ancestors. Revalidation preserves those
checks after creation; parent metadata additionally rejects non-directories.
Keep output order and existing stale clearing, source-fact checks, child requests
and result validation. Read-only sources receive no parent creation.

No ownership transition, caller migration, compatibility path or new ADR is needed:
ADR 0075 already places local handle resolution and byte preparation at the agent.
Manual provisioning would hide generated policy paths; relaxing worker validation
would remove a useful boundary. Neither is the approved remedy.

## Success

- C1: a valid fresh nested output parent exists before a scripted child writes.
- C2: unbound, traversing, symlinked and non-directory parents fail before a child;
  directory-creation failure is reported, and source paths are not created.
- C3: retained real bundled-worker canary evidence shows whether the former
  missing-parent failure is removed, with another failure reported if encountered.

## Failure model

- Actors and deployments: configured storage-owner agents, authenticated control
  plane sending untrusted handles, supervised bundled children, local operator/CI.
- Invariants and assets: writes remain under configured roots; source media,
  strict decoding, stale clearing and child/result validation remain protected.
- Accepted failure classes: directory creation can leave empty parents after
  later validation/child failure; bounded residue is retained rather than deleted.
  Arbitrary hostile concurrent replacement of node-local filesystem components
  is outside the existing trusted-local-filesystem deployment; rechecks do not
  promise descriptor-based race freedom.
- Covered elsewhere: #339 owns broader output/resume/capacity and production
  acceptance; operator owns selection and retention. Commit/promotion, DSL,
  schemas, dependencies, public APIs, authorization and timeouts are excluded.

## Threat model

- Boundaries: existing control-plane handles become filesystem destinations;
  preparation widens existing planned-write behavior to missing directories.
  Agent-to-child requests remain the trusted local protocol.
- Actors: handle inputs are untrusted; configured bindings, local filesystem
  administration and bundled executables are trusted.
- Controls: strict envelope decode, rooted binding/traversal/symlink checks before
  mkdir, rooted revalidation and directory metadata checks before stale clearing
  and dispatch; failures use existing lease error handling, exposing no credentials.
- Out of scope: hostile local filesystem administration/races, and authorization
  redesign; the existing local trust model and approved exclusions apply.

## Validation

One coupled Cast implementation unit with a focused failing fresh-parent test,
boundary/error regressions and unchanged source/child checks. Run focused media
checks and repository guardrails, then real eight-input unchanged flagship canary
with at most four files in flight. Retain inputs, outputs, databases and evidence.
No full production execution or retained-artifact cleanup is authorized.
