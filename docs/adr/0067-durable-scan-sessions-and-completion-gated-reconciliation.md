# ADR 0067: Durable scan sessions and completion-gated reconciliation

## Status

Accepted

## Context

VOOM's current root scan discovers, probes, and persists files inside one synchronous
control-plane invocation. It has no durable traversal identity, ordered replay boundary, or
proof that a missing locator came from a complete traversal. Retiring locations from such a
partial stream would turn disconnects, cancellation, and scan errors into false absence.

ADR 0050 requires manual scan sessions bound to one storage-root epoch and one owner-node
incarnation. ADR 0055 supplies rooted relative location identity, and issue #421 will move
discovery, hashing, and probing to the owner node. Issue #419 must provide the durable protocol
and reconciliation substrate without taking over #421's byte access or #420's scheduling
locality.

## Decision

Migration 0036 adds normalized `scan_sessions`, `scan_observation_batches`, and
`scan_observations` tables. A session snapshots the root ID,
root epoch, owner node, an inactivity timeout, and—when it starts—the authenticated current
owner incarnation and the highest pre-existing location ID for that root. A partial unique
index permits at most one `requested` or `running` session per root.

The lifecycle is `requested -> running -> succeeded | failed | cancelled | stale`, with
`requested -> cancelled | stale` also permitted. Terminal states are immutable. The existing
remote-node bearer and current-incarnation fence protects start, batch, success, and failure
routes. Session mutation does not schedule work: tickets and leases remain the only work-routing
mechanism, and issue #421 will carry the session ID in scan work.

Each non-empty observation batch has a zero-based contiguous sequence. The batch ledger stores
the stable request hash, and observation rows store validated provider-relative locators,
provider-local object identity and stat facts, and stability timestamps as scalar columns.
`(session_id, sequence)` is the batch identity and `(session_id, locator)` is unique. An exact
batch replay returns the original accepted outcome without another observation or event; a
different body at the same sequence, a duplicate locator, or any sequence other than the next
expected value is a conflict. The existing remote-idempotency ledger additionally makes lost
HTTP responses replayable.

Success supplies a complete-traversal watermark: `last_sequence = null` for an empty traversal,
or the last accepted sequence otherwise, plus the total observation count. In one `BEGIN
IMMEDIATE` transaction, the control plane revalidates the session's root epoch, owner, current
incarnation, availability, deadline, watermark, and in-flight commit locks. It tags and
retires only live rooted locations that belong to the session root, existed at or below the
start-time location high-water mark, and have no observation in the session. It then marks the
session `succeeded`, updates `library_roots.last_scan_session_id`, appends the summary fact event,
completes the idempotency response, and commits.

Failed, cancelled, stale, incomplete, prior-incarnation, expired, unavailable-root, and
root-epoch-mismatched sessions never update or retire file locations. Their accepted batches
remain inspectable. Incarnation termination and existing remote recovery mark affected or
expired sessions stale; every later mutation also revalidates the same fences before acting.

The inactivity deadline is initialized when the session is requested and reset only by a
successful start or a newly accepted contiguous batch. Replays, rejected requests, and inspection
do not extend it. At `now >= progress_deadline_at`, staleness wins over start, batch, success,
failure, or operator cancellation. Before that boundary, the first terminal transaction to obtain
the writer lock wins.

Session lifecycle and batch acceptance append one summary event per committed transition. The
retained location row stores the successful scan session that retired it; this is durable
per-location evidence without one event or auxiliary row per retirement. Inspection derives the
prior epoch from the retained row's incremented epoch.

## Consequences

- A successful empty traversal can retire every location that was live when the session began,
  while locations created concurrently after the high-water mark remain conservatively live for
  a later scan.
- Batch acceptance is provisional catalog evidence. It does not create or refresh a
  `file_location`; issue #421 publishes identity only after matching hash and probe evidence.
- Completion is the only absence linearization point. SQLite writer serialization prevents two
  root sessions or a session completion and location mutation from both observing stale state.
- `library_roots.last_scan_session_id` points only at a successful session. Session and
  reconciliation inspection can explain the last absence decision without replaying events.
- Existing synchronous control-plane discovery remains an explicitly transitional path until
  issue #421 replaces it; this decision neither extends it nor claims it emits owner-agent
  observations.
- Observation, event, and replay payload structs participate in ADR 0013's strict durable-payload
  inventory. Persisted numeric and timestamp fields are checked before business classification.
- Completion may return a retryable conflict while an in-flight commit owns a candidate
  location. It leaves the session running and performs no partial reconciliation.
- Successful completion performs an O(number of pre-start live root locations) anti-join and
  update in one SQLite writer transaction. The bounded batch and event counts do not make that
  transaction constant-cost; implementation must prove the supported root scale fits the API
  timeout or return to design rather than chunk a logically atomic reconciliation.

## Considered & rejected

- **Stamp `last_scan_session_id` directly onto live locations as batches arrive.** Rejected
  because a failed or malicious partial stream would mutate authoritative catalog state and make
  later absence classification depend on cleanup of provisional writes.
- **Add a separate reconciliation-evidence row for every retired location.** Rejected because the
  retained `file_locations` row already carries its retirement timestamp and incremented epoch.
  A completion-only `retired_by_scan_session_id` foreign key supplies the missing provenance
  without duplicating every retired ID.
- **Store each batch as one JSON blob.** Rejected because duplicate-locator detection,
  reconciliation joins, corruption diagnostics, and pagination would all depend on repeatedly
  decoding unbounded payloads. Normalized rows let SQLite enforce the identity and ordering
  keys.
- **Reconcile every live location present when completion runs.** Rejected because a location
  created concurrently after traversal began might never have been visible to the scanner. The
  start-time location high-water mark makes retirement conservative and deterministic.
- **Use a scan-session pull queue.** Rejected because it would create a second work-routing
  mechanism beside tickets and leases. The session records traversal facts; issue #421's scan
  ticket will route execution.
- **Emit one event for every retired location.** Rejected because a valid empty-root scan could
  create an unbounded event burst inside the completion transaction. The retained location rows
  are the detailed evidence and one session event records the aggregate fact.
- **Continue accepting batches after a sequence gap and reconcile at completion.** Rejected
  because a missing batch would be indistinguishable from a complete empty portion of the tree.
  Contiguous acceptance fails at the first gap.
