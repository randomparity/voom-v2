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

One logical node may complete five fresh activations in a rolling 60-second window. After
the existing immediate activation transaction acquires its writer lock, it samples the
clock. Completed replay returns first, and the existing incarnation-uniqueness check then
precedes quota evaluation. A genuinely fresh request counts incarnation starts at or after
the inclusive lower bound; future-dated rows count conservatively if the clock moves
backward. Persisted timestamp offsets are decoded and compared as instants rather than as
text. An indexable lexical lower envelope starts 26 hours before the UTC lower bound because
the pinned timestamp parser accepts offsets through ±25:59:59; typed comparison remains
authoritative. It rejects at the limit before supersession or any activation mutation.
Rejection emits a structured warning with a fixed quota-exceeded reason and non-secret
policy fields.

Admission uses existing `node_incarnations` rows rather than a second ledger. Concurrent
requests remain serialized by the immediate transaction.

An explicit `ControlPlane::prune_node_activation_history(node_id, cutoff)` operation prunes
terminal incarnations whose `ended_at` is strictly older than the earlier of the caller
cutoff and the active quota-window floor. `started_at` remains quota evidence and is not
retention age. It deletes only retired workers with no durable relational references,
retains an incarnation while any worker remains bound, and leaves append-only events and
completed activation replay rows untouched. Foreign keys remain enabled and unexpected
failures roll back the prune transaction. The only permitted worker-owned cascades are
capabilities and grants. Both scheduler-decision worker columns use `SET NULL`, so pruning
explicitly treats them as retention holds; a schema-inventory regression prevents a new
permissive worker foreign key from bypassing classification.

Completed activation replay outcomes are historical facts rather than live-resource
handles. After eligible catalog pruning, replay returns the same serialized outcome and
remains non-mutating even if an identity in that outcome is no longer resolvable. The
approved replay guarantee is non-mutation, not indefinite catalog retention; making every
completed activation response a retention hold would leave no normal successful
incarnation eligible for pruning.

## Consequences

Five rapid restarts remain available, while a sixth fresh activation waits for the rolling
window to advance. A completed replay remains callable at quota and performs no mutation.
The quota is fixed in v1 and changing it requires a code and decision update.

Pruning is operator-driven and can leave terminal history in place when operational or
decision records still reference a retired worker. Event payloads continue to name pruned
identities, and completed replay rows retain their original response, preserving historical
facts without retaining every catalog row. This decision does not bound append-only event
or completed replay growth; it bounds activation frequency and makes the heavier mutable
incarnation/worker manifest history reclaimable.

No schema migration is needed: existing node-history and incarnation-worker indexes serve
the count and prune queries. Campaign migration number 0036 is reserved but unused rather
than adding a redundant index or another retention obligation.

## Considered & rejected

- Do nothing. Authentication and the one-active-incarnation index constrain identity and
  concurrency, not repeated successful fresh keys; a crash loop or valid-token caller can
  still grow incarnation and worker-manifest rows without bound. This decision knowingly
  retains append-only events and completed replay rows rather than pretending to bound all
  database growth.
- Add a dedicated admission ledger. It separates quota evidence from incarnation history,
  but duplicates a durable fact and creates another unbounded history requiring cleanup.
- Enforce a one-activation cooldown. It is simpler but rejects legitimate restart bursts
  that the approved five-per-window policy permits.
- Prune opportunistically during activation. It couples deletion latency and historical
  reference failures to the authenticated admission path.
- Delete terminal incarnations and let worker references null or cascade. That weakens
  required operational history; referenced workers and their incarnation remain instead.
