# ADR 0107: Normalize known operations during scheduling

## Status

Accepted

## Context

Issue #714 reproduces a canonical node-agent capability failing to discover a namespaced
workflow scan ticket. ADR 0069 makes operation classification total and unknown reserved
operations ineligible; ADR 0072 deliberately deferred candidate-building equivalence.
The operator separately approved correcting that scheduling boundary for #436/#339.

## Decision

Extend core's existing classification with supported matching tokens: both encodings for
known operations, one exact token for custom operations, none for unknown reserved ones.
Use that vocabulary consistently in store candidate discovery, eligibility and associated
capacity/capability projections. Canonicalize set keys before subtracting denies and decode
persisted operation entries before classification. Preserve canonical dispatch, stored
representations, SQL eligibility/order and authenticated atomic acquisition fences.

This supersedes only ADR 0069's raw-token candidate matching restriction and ADR 0072's
deferral of candidate equivalence. Their remaining decisions continue to govern.

## Consequences

Known aliases become scheduling equivalents, including grant/deny and capacity keys;
operator declarations remain canonical. Unknown reserved operations cannot gain eligibility
through an exact capability/grant. Custom operation names remain exact. No migration is
needed; mixed known encodings in existing rows are interpreted consistently.

## Considered & rejected

- Do nothing. verified: #714 records a real scan with a ready namespaced ticket and zero
  leases; source at c221a7b7 filters raw `tickets.kind` before dispatch normalization.
- Duplicate aliases in bootstrap declarations. judgment: moves scheduling vocabulary into
  callers and leaves deny/capacity interpretation inconsistent.
- Normalize persisted values on write with a data migration. judgment: unnecessary stored
  representation change when the existing classifier can interpret both encodings on read.
