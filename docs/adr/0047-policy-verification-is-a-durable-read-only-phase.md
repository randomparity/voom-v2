---
status: accepted
date: 2026-07-27
deciders: [VOOM core]
---

# 0047 — Policy verification is a durable read-only phase

## Context

ADR 0017 publishes the fieldless `verify artifact` operation and its compiled
`{"type":"verify_artifact"}` representation, but deliberately deferred execution. The existing
artifact verifier is bundled and out of process, persists evidence, and is restricted to staged
commit inputs. Policy execution needs to verify both dependency-produced artifacts and existing
active files without changing that published grammar or weakening the commit gate.

Workflow file-phase outcomes currently distinguish mutation (`committed`), no work (`skipped`), and
failure/exclusion (`blocked`). A successful verification performs durable work but does not mutate
the file chain, so mapping it to either existing success value would misstate operator-visible
history and `run_if ... modified` semantics.

Design:
[`docs/superpowers/specs/2026-07-27-issue-334-policy-verification-design.md`](../superpowers/specs/2026-07-27-issue-334-policy-verification-design.md).

## Decision

Policy verification is an explicit durable read-only phase outcome named `verified`.

The workflow ticket pins the exact selected file version/location. A store-owned immediate
transaction resolves that identity to an artifact:

- reuse the committed artifact handle when the version was produced by an artifact commit; or
- create/reuse one immutable active-file artifact handle otherwise.

The handle has a live `local_path` artifact location for the exact selected path. Public staged
verification keeps its existing exactly-one-staging-location contract; policy execution uses an
internal exact-location entry point with equivalent revalidation.

The executor bootstraps and selects the built-in worker through the normal capability/grant/deny
predicate, acquires the normal workflow lease, and then dispatches through an explicit bundled
transport. Verification remains out of process; no synthetic HTTP runtime is registered.

Policy evidence records its owning workflow ticket and lease through additive nullable foreign
keys. A unique partial index on the lease makes one dispatch attempt idempotent without suppressing
the ticket's normal new-lease retries.

A successful file-phase row records `verified`, the unchanged active version/location/snapshot,
the artifact handle, the exact verification, and its ticket. `verified` is completed but not
modified. Resume carries it forward and starts at the next phase. If the prior process persisted
successful evidence but died before writing that row, resume validates the evidence against the
same branch, phase, version, and live location and seeds the replacement job as verified without
redispatch. Compliance execute and stored-run report views expose ticket-correlated and seeded
verification evidence for both success and failure.

## Consequences

- Existing source DSL and compiled-policy JSON are unchanged.
- Dependency-produced and existing active artifacts use one verification path and one evidence
  model.
- Failed verification gates dependants through normal ticket failure and phase error policy.
- Operators can retire or deny the built-in worker; bootstrap never overrides either action.
- Existing verification and workflow rows remain readable after migration. Existing output values
  retain their meanings; `verified` appears only for the newly executable operation.
- Older binaries reject the newer schema under the normal schema-version contract. Rollback uses
  the compatible backup procedure, not reinterpretation of newer writes.

## Considered and rejected alternatives

### Record verification as `skipped`

Rejected. The worker ran and durable evidence exists. Reporting no work would hide verification
from progress, audit, and resume semantics.

### Record verification as `committed`

Rejected. The active file chain does not advance, and `run_if ... modified` must remain false.

### Register a dummy HTTP runtime for the bundled worker

Rejected. It invents an endpoint/client that must never be called and obscures the actual dispatch
boundary.

### Copy every active file to staging before verification

Rejected. It verifies a copy rather than the selected active bytes, adds avoidable I/O, and creates
new staging/commit semantics not present in the published operation.

### Add artifact arguments to the policy language

Rejected. The published operation is fieldless; the plan target and phase dependency determine the
artifact.
