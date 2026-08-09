# Bound Node Activation Growth Design

## Scope

Issue #445 bounds the durable growth caused by repeated fresh remote-node activations.
The v1 policy is fixed: one logical node may complete at most five fresh activations in
any rolling 60-second window. A completed idempotent replay returns its original outcome
before quota evaluation. A fresh request beyond the quota is rejected before the current
incarnation is superseded and before any incarnation, worker, capability, grant,
idempotency-completion, or event row is created.

The change also adds
`ControlPlane::prune_node_activation_history(node_id, cutoff)`. The method removes only
terminal incarnations older than both the caller's cutoff and the active quota window. It
removes retired workers only when no durable record references them, leaves referenced or
live workers and their incarnations intact, and never updates or deletes append-only events.

The quota is not configurable, pruning is not coupled to activation traffic, and this
change adds no CLI or HTTP maintenance route.

## Alternatives

### Chosen: count successful incarnation rows and prune explicitly

`node_incarnations.started_at` is already the durable fact that a fresh activation
completed. Count rows for the logical node at or after `now - 60 seconds` while holding the
activation's existing immediate SQLite transaction. This uses the existing
`node_incarnations_history` index and needs no second quota ledger. Explicit pruning keeps
deletion latency and failure out of the authentication path.

### Rejected: a dedicated activation-admission ledger

A separate table would decouple quota evidence from incarnation retention, but it would
create another growing history that needs its own retention contract and transactional
write. The requested 60-second evidence is already present and pruning can preserve it by
clamping the effective cutoff to the window boundary.

### Rejected: one activation per cooldown interval

A cooldown is simpler to query, but it is a different policy: it rejects short legitimate
restart bursts even when the rolling allowance has capacity. The approved policy permits
five successes anywhere in the window.

### Rejected: opportunistic pruning during activation

This bounds storage without an operator call, but makes an authenticated request perform
retention work and couples admission availability to unrelated historical references.
Pruning remains an explicit maintenance operation.

## Activation admission

The existing `remote_activate` transaction keeps its authority and ordering:

1. Validate the manifest before database access.
2. Begin an immediate transaction, wait for its writer lock, then sample the authoritative
   control-plane clock and authenticate the logical node.
3. Reject retired nodes.
4. Reserve or resolve the activation idempotency key.
5. Return a completed replay immediately.
6. For a fresh reservation, reject an incarnation ID that already exists, preserving the
   existing duplicate-incarnation conflict before quota classification.
7. Count successful incarnation starts whose `started_at` is at or after
   `now - 60 seconds`. Rows later than `now` are conservatively included if the clock moves
   backward.
8. If the count is five, emit a structured warning and return `VoomError::Conflict`.
   Dropping the transaction rolls back the fresh replay reservation.
9. Only an admitted request may supersede the active incarnation, register the new
   manifest, complete replay state, append events, and commit.

The lower boundary is inclusive. An activation exactly 60 seconds old therefore still
occupies capacity; it leaves the window only after time advances beyond that boundary.
Sampling after writer-lock acquisition prevents a queued request from evaluating with a
stale time captured before another activation committed. Conservatively counting
future-dated rows fails closed across backward clock adjustments.

Concurrent fresh requests serialize on the existing immediate transaction. Each admitted
transaction commits an incarnation row before the next requester counts, so no pair can
both observe the same remaining slot.

The repository method accepts `NodeId`, a checked `OffsetDateTime` lower bound, and a
transaction. It returns a checked `u32`; negative or out-of-range persisted counts are
database corruption, not quota state.

## Quota rejection logging

Quota rejection emits one `tracing::warn!` event before returning. Its stable message is
`remote node activation quota exceeded`; structured fields include the logical `node_id`,
`activation_count`, `activation_limit`, and `window_seconds`. It does not include the node
token, idempotency key, request hash, incarnation ID, or worker manifest.

The error is a conflict with an actionable message naming the node, limit, and rolling
window. The warning makes the reason visible to the existing API/CLI logging subscribers,
including file-backed deployments, without adding a second logging mechanism.

## Pruning

`prune_node_activation_history(node_id, cutoff)` starts an immediate transaction and uses
the control-plane clock to compute `quota_floor = now - 60 seconds`. The effective cutoff
is the earlier of the requested cutoff and `quota_floor`. Rows at the effective cutoff are
retained; only rows strictly older than it are candidates. This prevents maintenance from
erasing evidence still needed by admission even when a caller supplies a future cutoff.

The operation selects terminal incarnations for exactly one logical node in deterministic
`ended_at` order; `started_at` is quota evidence, not retention age. An incarnation whose
`ended_at` equals the effective cutoff is retained. For each candidate, the worker
repository considers only workers that are already `retired` and bound to that incarnation:

- capability and grant rows are the only worker-owned metadata permitted to cascade with
  an eligible worker;
- a worker referenced by any durable operational or decision row is retained. SQLite
  `RESTRICT` foreign keys enforce most holds. Because scheduler-decision worker references
  use `ON DELETE SET NULL`, the worker repository explicitly checks both
  `scheduler_decisions.request_worker_id` and `selected_worker_id` before deletion;
- a foreign-key rejection is treated as retained eligibility, not as permission to drop
  the reference;
- a live/non-retired worker is never attempted;
- if any worker remains bound, the incarnation remains;
- an empty terminal incarnation can be deleted after its workers are gone.

SQLite foreign keys remain enabled throughout. The method does not disable constraints,
null references, or rewrite history to make a row eligible. Events have no foreign key to
workers or incarnations and are append-only by trigger; pruning never touches them, so
their payload IDs remain audit evidence after eligible catalog rows are removed.

Completed activation idempotency rows are also retained unchanged. Their serialized
outcomes are historical facts, not live-resource handles: replay after pruning returns the
same outcome bytes and remains non-mutating even when an eligible incarnation or worker ID
is no longer resolvable through current catalog reads. The external replay requirement is
non-mutation; it does not promise indefinite catalog retention. Treating every completed
activation response as a retention hold would make every normal successful incarnation
ineligible and defeat the requested prune path.

A schema-inventory regression enumerates every foreign key that targets `workers`. It
permits `CASCADE` only for `worker_capabilities.worker_id` and
`worker_grants.worker_id`, permits `SET NULL` only for the two explicitly prechecked
scheduler-decision columns, and requires every other worker reference to remain
`RESTRICT`. A new permissive reference action therefore fails tests until pruning
classifies and protects it.

The method returns `Result<(), VoomError>`; callers inspect normal repository state if they
need counts. A partial prune is not exposed: any unexpected database error rolls back the
whole transaction. Ineligible references are expected and simply leave their worker and
incarnation present.

No schema migration is required. Migration number 0036 remains reserved to this campaign
issue but is intentionally unused: the existing history and incarnation-worker indexes
cover both queries, and a redundant index or ledger would add write cost and retention
surface.

## Error handling and corruption

All timestamps use the repository's canonical ISO-8601 encoder. Count and affected-row
values use checked conversions. Malformed persisted incarnation IDs, ownership mismatch,
invalid lifecycle states, or unexpected SQL failures remain `VoomError::Database` with
operation context. Quota exhaustion is a domain conflict, not a database failure.

Pruning never converts corrupt persisted state into eligibility. It reads typed terminal
incarnations before deletion and fails the transaction if their stored fields cannot be
validated.

## Threat model

### Boundaries and actors

- Existing widened boundary: an authenticated node process controls fresh activation
  keys, incarnation IDs, and worker manifests. A leaked valid node token or crash loop can
  drive requests. Authentication remains the existing control; the new quota limits the
  durable effect after authentication.
- New maintenance boundary: an in-process operator caller supplies a logical node ID and
  cutoff. No network or CLI route is added. The caller is trusted to request maintenance,
  while the method still clamps the cutoff and relies on foreign keys so a bad timestamp
  cannot erase live or referenced state.
- Logging boundary: quota metadata crosses into operator logs. Only non-secret node and
  numeric policy fields cross; credentials and replay material are excluded.

### Controls

- Immediate transactions serialize admission and place the check before mutation.
- Completed replay resolution precedes quota evaluation, preserving idempotency.
- The pruning cutoff clamp preserves live quota evidence.
- Typed ownership filters, terminal/retired predicates, and enabled SQLite foreign keys
  prevent cross-node, live, or referenced deletion.
- Append-only event triggers and an explicit no-event-delete implementation preserve audit
  payloads.
- Structured logging has a fixed reason and a secret-minimized field set.

### Out of scope

This design does not mitigate stolen-token activity other than activation growth, add token
revocation, configure per-node limits, schedule pruning, or bound unrelated idempotency and
event histories. Those concerns are not required to satisfy issue #445.

## Tests

Control-plane tests use a real SQLite pool and the injected `ManualClock` without pausing
Tokio time. They prove:

- five distinct fresh activation keys succeed and the sixth is a conflict;
- the exact 60-second lower boundary remains counted and one nanosecond beyond it admits;
- queued evaluation samples time after serialization, and a backward clock adjustment
  conservatively counts future-dated starts;
- replay of a completed key remains non-mutating even when the node is at quota;
- reuse of an existing incarnation under a fresh key retains the duplicate-incarnation
  error at quota and emits no quota warning;
- quota rejection leaves the active incarnation, node epoch, incarnation/worker/
  capability/grant counts, replay completions, and events unchanged;
- the warning subscriber observes the fixed quota-exceeded reason and non-secret fields;
- pruning removes eligible old terminal incarnations and retired unreferenced workers,
  including their owned capabilities and grants;
- pruning retains active/recent incarnations, live workers, and workers referenced by
  durable operational or scheduler history, including both scheduler-decision columns;
- the worker foreign-key inventory rejects an unclassified `CASCADE` or `SET NULL` edge;
- events that named pruned rows remain unchanged;
- completed activation replay after pruning returns the original historical outcome
  without recreating any catalog row;
- an injected prune failure rolls back earlier eligible deletions.

Repository tests separately cover inclusive window counting, checked count conversion,
deterministic prune candidate ordering, eligible deletion, and referenced/live retention.

## Verification

Focused commands:

- `cargo test -p voom-store node_incarnations`
- `cargo test -p voom-store workers`
- `cargo test -p voom-control-plane remote_activation`

The release gate is bare `just ci`. The known environment-specific skip baseline is six:
one FFmpeg hardware test and five Toxiproxy network-resilience tests. Any additional skip
is a failure to report and resolve.

## Governing decision

See [ADR 0065](../../adr/0065-bound-node-activation-admission-and-pruning.md).
