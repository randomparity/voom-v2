# ADR 0065: Bound node activation admission and prune history explicitly

## Status

Accepted

## Context

Every fresh remote-node activation supersedes the prior incarnation and creates a new
incarnation, worker manifest, grants, capabilities, replay completion, and audit events.
Authentication and the single-active-incarnation constraint do not limit repeated fresh
keys, so a crash loop or valid-token caller can grow SQLite without bound.

The control plane must bound fresh successful activations while preserving completed
idempotent replay. It also needs a retention path that cannot erase live state, referenced
history, append-only events, or evidence still used by admission.

## Decision

One logical node may complete five fresh activations in a rolling 60-second window. Within
the existing immediate activation transaction, completed replay returns before quota
evaluation. A fresh request counts incarnation starts in the inclusive lower-bound window
and rejects at the limit before supersession or any activation mutation. Rejection emits a
structured warning with a fixed quota-exceeded reason and non-secret policy fields.

Admission uses existing `node_incarnations` rows rather than a second ledger. Concurrent
requests remain serialized by the immediate transaction.

An explicit `ControlPlane::prune_node_activation_history(node_id, cutoff)` operation prunes
terminal incarnations strictly older than the earlier of the caller cutoff and the active
quota-window floor. It deletes only retired workers with no durable references, retains an
incarnation while any worker remains bound, and leaves append-only events untouched.
Foreign keys remain enabled and unexpected failures roll back the prune transaction.

## Consequences

Five rapid restarts remain available, while a sixth fresh activation waits for the rolling
window to advance. A completed replay remains callable at quota and performs no mutation.
The quota is fixed in v1 and changing it requires a code and decision update.

Pruning is operator-driven and can leave terminal history in place when operational or
decision records still reference a retired worker. Event payloads continue to name pruned
identities, preserving audit facts without retaining every catalog row.

No schema migration is needed: existing node-history and incarnation-worker indexes serve
the count and prune queries. Campaign migration number 0036 is reserved but unused rather
than adding a redundant index or another retention obligation.

## Considered & rejected

- Add a dedicated admission ledger. It separates quota evidence from incarnation history,
  but duplicates a durable fact and creates another unbounded history requiring cleanup.
- Enforce a one-activation cooldown. It is simpler but rejects legitimate restart bursts
  that the approved five-per-window policy permits.
- Prune opportunistically during activation. It couples deletion latency and historical
  reference failures to the authenticated admission path.
- Delete terminal incarnations and let worker references null or cascade. That weakens
  required operational history; referenced workers and their incarnation remain instead.
