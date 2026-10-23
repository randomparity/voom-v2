# ADR 0108: Candidate counts qualify scheduler suppression identity

## Status

Accepted

Supersedes ADR0071 only for retaining the old aggregate suppression-key shape.

## Context

#729's operator-approved repair addresses remote acquire conflicts when candidate
count changes under an otherwise identical suppression key. Producer rows carry a
bounded count; the store requires equality before suppressing. Retained #339
execution exited on this conflict. An in-memory reproduction of the exact table,
index and store upsert refuses count2 to1 under the same key; the failed live
request's precise delta was not captured. ADR0071's aggregate-key shape omits count.

## Decision

Extend the existing remote-acquire key with `:candidates:{count}` before its bucket.
Score/capacity producers use the same saturated u32 count for row and key; the shared
constant-one changed-gate caller passes1. Preserve existing identity segments and
the distinct locality-rejection fingerprint. Selected decisions remain unsuppressed.
Keep the store's incompatibility guard, historical rows and replay unchanged.
No migration, public wire change, eligibility change or new retry path is needed.

## Consequences

Repeated equivalent counts suppress; changed persisted counts receive distinct rows.
Old keys remain readable and replayable without rewrite. Existing saturation above
u32::MAX is retained, so those raw counts share their persisted-count identity.
ADR0071's ticket/locality requirements and ADR0072/0073 invariants remain in force.

## Considered & rejected

- Weaken count equivalence in the store. judgment: this would merge evidence the
  existing guarded contract treats as distinct, contrary to the approved repair.
- Retry the conflict in the agent. judgment: it leaves the colliding identity and
  durable failure intact, instead of fixing the producer's key/row mismatch.
- Rewrite historical keys. judgment: unnecessary persistence churn; opaque old
  keys remain valid evidence and idempotent replay uses its recorded decision.
