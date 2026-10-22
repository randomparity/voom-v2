# 0106: Activate owner roots atomically with a node incarnation

## Status
Accepted

## Context
Issue #436 and accepted ADR0055 require authenticated owner-agent validation before
roots can be used. Node activation already authenticates the owner and atomically
creates its incarnation, workers and replay result. The operator approved extending
that boundary rather than introducing a separate lifecycle endpoint.

## Decision
Extend node activation with bounded root proofs from locally validated agent bindings.
The control plane checks root owner, provider, immutable locator and lifecycle, then
records root activation in the same transaction as the new incarnation and workers.
Reuse root lifecycle/event logic through a transaction-aware helper. Filesystem
canonicalization and identity collection remain on the owner. Preserve the existing
activation retry contract and root identity/epoch rules. The proof list is complete
for this incarnation: previously active owned roots omitted from it become unavailable
with validation-loss facts in this transaction. Empty root declarations
remain valid for worker-only clients. No unauthenticated operator activation exists.

## Consequences
Invalid proofs fail the complete activation. Configuration changes require restarting
the agent to validate and submit a fresh manifest. A startup proof does not implement
continuous filesystem monitoring. Acceptance helpers can cross the same authenticated
boundary as deployment agents without database lifecycle writes.

## Considered & rejected
- Separate authenticated root endpoint — judgment: adds a second lifecycle/retry
  sequence when the approved acceptance bootstrap already starts an agent incarnation.
- Operator CLI activation — verified: accepted ADR0055 reserves provider validation
  for the owner-node boundary; issue #436 explicitly rejects unauthenticated activation.
- Direct lifecycle SQL in scripts — verified: issue #436 prohibits bypassing the
  authenticated current-incarnation contract.
