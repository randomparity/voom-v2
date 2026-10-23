# Candidate-count suppression identity (#729)

## Problem and authority

Remote score/capacity producers persist candidate count but omit it from their
suppression identity. The store correctly refuses different counts under one key.
The retained #339 canary stopped with this conflict; the exact guard reproduces
with count2 to1 in memory, though the failed live request was not captured.
The operator approved #729's concrete persisted-count repair and exclusions.
ADR0108 narrowly supersedes ADR0071's old aggregate-key shape.

## Design and scope

Keep ownership at the existing acquire decision producers and shared key builder.
Compute each score/capacity decision's existing saturated u32 candidate count once;
pass that exact value to both the row and its suppression-key helper. Extend the
shared remote key with a `:candidates:{count}` segment before the existing bucket.
Keep node/worker, optional ticket, reason, operation fingerprint and bucket intact.
The constant-one changed-gate caller passes1 through the same shared builder.
Locality gate rejections retain their distinct refs-based identity and constant1.
Selected score decisions still have no suppression key.

Historical keys/rows remain untouched and readable; no key parser or schema changes.
Unchanged-count repeats suppress; changed counts get distinct rows. Counts above
u32::MAX retain the existing persisted saturation and therefore the same key count.
The store equivalence guard stays unchanged. Replays keep their recorded decision;
new requests use the new key shape. Authentication, epoch fences, eligibility,
transaction composition, ordering and lease/plan creation do not change.

Immediate test-only imports may expose existing producer functions to their sibling
module's fixtures, matching the existing key-helper test pattern. No new runtime
owner, abstraction, parallel execution path, dependency or public interface.

## Success

- C1: score/capacity producer rows and keys agree at counts1,2 and saturation;
  real repository persistence suppresses repeats and accepts changed-count rows.
- C2: existing incompatible-key rejection, ticket/operation/locality separation,
  selected-key absence and replay/fencing/atomic-acquire tests remain green.
- C3: resume retained job3 through the existing CLI with unchanged policy/inputs,
  maximum four files in flight, preserving evidence if another failure stops it.

## Failure model

- Actors and deployments: authenticated owner agents, existing control plane and
  SQLite repositories, operator/CI; configured local storage and tools are trusted.
- Invariants/assets: durable decision identity/history; same-key equivalence;
  replay, owner/epoch fences, ordering, atomic lease/plan binding; source media.
- Accepted classes: prior keys remain readable alongside new keys; extra historical
  decision rows for distinct counts are intended. Existing u32 count saturation is
  retained. Other engine failures may interrupt the attended canary: retain evidence
  and request separate scope, as approved, rather than claiming full acceptance.
- Covered elsewhere: #339 owns broader output/resume/capacity/full-production
  acceptance; operator owns production selection and retention. DSL/schema/dependency,
  public API/auth/timeouts/commit-promotion/unrelated engine changes are excluded.

## Threat model

- Boundaries: derived scheduler facts become a durable key and parameter-bound
  repository write; persisted historical decisions remain untrusted read data.
- Actors: control-plane inputs and stored data are untrusted; typed producer facts
  and existing validation/repository transaction controls govern this crossing.
- Controls: use the exact persisted bounded count, preserve typed IDs and existing
  key segments, parameter binding, store equivalence rejection and checked decoding.
- Out of scope: auth redesign, new key parsers, historical row rewrites or cleanup;
  no new boundary grant or source-media mutation is authorized.

## Validation

Use red-green producer/persistence tests, boundary counts, shared caller regression
and existing rejection/replay/fence tests. Run mandatory hooks and one final just ci
on the reviewed candidate; build exact clean binaries before the retained resume.
