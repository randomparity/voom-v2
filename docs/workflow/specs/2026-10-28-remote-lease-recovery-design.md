# Automatic remote lease recovery and scan terminality

## Authority and scope

Issue #783, scope q783-bea55c28, operator-approved issue draft/exclusions and
quest request govern this design. Criteria C1–C5 are frozen in WORK:SCOPE.
Non-trivial, full-spec, L denominator 1000; one coherent PR. ADR 0118 records
service ownership and terminal scan recovery. No migration, new operation,
public JSON field or public error code is planned.

The affected production owners are voom-api (process lifecycle),
voom-control-plane (lease/scan authority and events), voom-store (SQL and
checked persisted projections), and voom-node-agent (local fencing/diagnostics).
Audio policy, codecs, commentary/transcription, polling/TTL tuning, performance
SLOs, weaker fencing and automatic resumption retain their approved external owners.

## Global Constraints

Use the existing workspace Rust/tokio/sqlx/axum stack; add no external packages
or versions. Node diagnostics may use existing workspace tracing and
tracing-subscriber dependencies.
SQL belongs to voom-store; transactions are caller-owned through voom_store::tx.
Preserve typed IDs, checked persisted values and existing public error/wire contracts.
Real SQLite tests use real Tokio time and ManualClock for domain time.
Unit tests use sibling _test.rs files. Run routine guardrails through just.
No audio-policy, polling/TTL configuration, automatic resumption or fencing weakening.

## Problem and evidence

A lease heartbeat delayed by database contention reaches its local TTL fence.
run_lease drops dispatch_outcome, maps the fence to TerminalElsewhere and does
not publish a failure. The standalone scan has no workflow executor ticking
expire_due, and voom-api does not drive remote_recover. A diagnostic against the
saved incident state also proved that explicit recovery expired the lease and
failed its ticket while its healthy owner's long-deadline scan remained running.
The two temporary diagnostic tests passed and were removed after evidence capture;
they are observations of existing behavior, not fix validation.

## Alternatives and selected ownership

1. Recommended: explicitly compose periodic recovery into the API process,
   reuse authoritative expiry, and reconcile scan terminality through existing
   control-plane/store owners. Keeps byte authority and existing explicit library
   recovery APIs; supports standalone scans without a workflow executor.
2. Recover only from the node or experiment driver. Rejected: client fencing
   cannot prove durable terminality and recovery must survive client disappearance.
3. Add a general daemon framework or a new persisted scan-ticket association.
   Rejected for this bounded repair: canonical request_scan_run payloads already
   carry the typed session/root/node linkage; use checked existing records.

The API process becomes the production caller of the existing explicit recovery
primitive. ControlPlane::open, router constructors and inspection remain passive.
Existing explicit recovery callers and workflow executor expiry remain supported
because library embedding and workflow execution retain those public contracts.
No ownership move, replacement SQL owner, or obsolete compatibility route is needed.

## Recovery service (C1, C4, C5)

Add a small voom-api recovery module containing an explicitly polled async loop.
Its first pass is immediate; after a pass finishes it waits five seconds before
starting another. One pass at a time; no accumulated interval backlog or tight
retry. Bound a pass to thirty seconds. Use control_plane.clock().now() for the
existing remote_recover call. Existing bounded expiry batching stays in force.
Failures/timeouts emit a fixed structured warning with public error code and
retry on the next cadence; they do not claim completion or continue expired work.
Do not log raw database errors, paths, credentials or persisted payloads.

Compose this future with the termination future passed to RunningServer::shutdown_on.
The recovery future is polled while the listener is running and is dropped on
signal or listener exit. No detached task, global singleton or router-side spawn.
An unexpectedly returning recovery future requests graceful server shutdown with
an INTERNAL runtime diagnostic. A panic fails the process rather than leaving an
apparently healthy listener without recovery. Existing listener grace is unchanged.
Cancellation may roll back an in-progress recovery transaction; the existing
cancellation-safe transaction openers retain ownership of late SQLite opens.
The next service start safely repeats the pass.

The cadence is a processing bound while the database can complete recovery within
the per-pass deadline, not an unconditional wall-clock guarantee during unavailable
storage. A failed pass is observable; subsequent passes repair durable backlog.

## Coupled scan lifecycle (C2, C4, C5)

Keep existing lease-expiry/ticket transitions and their event order. Add a
control-plane scan recovery capability that closes requested/running sessions
whose canonical request_scan_run routing ticket is terminally failed. It must
also repair an existing failed-ticket/active-session pair after restart, not
only transitions produced by the current pass.

The existing canonical payload supplies ScanSessionId, StorageRootId and NodeId.
Decode persisted ticket fields and linkage before classifying absence, mismatch,
or terminal session state. Verify the session's stored root/owner against the
routing payload; malformed identifiers, incoherent linkage or corrupt stored rows
are contextual database errors, not missing work. Preserve typed IDs across
store projections and orchestration. Ignore unrelated/generic workflow tickets
that do not represent canonical request_scan_run work.

SQL and checked row decoding remain in voom-store. Add only narrow capabilities
needed to enumerate relevant routing failures and inspect/terminalize their scan
sessions; use existing terminalize_in_tx where suitable. Avoid a second scan state
machine or reliance on events as scheduling authority. Preserve deterministic
query ordering and existing cap/continuation conventions; historical terminal
sessions must not starve active failed runs.

For newly expired exhausted scan tickets, close the linked active session and
append its existing failed lifecycle event in the caller-owned expiry transaction,
after the existing lease/ticket facts. Errors roll that transaction back. For
already-failed routing tickets, use a read-then-write reconciliation transaction
that commits each bounded recovery unit with its event. Already terminal scans
are preserved, including succeeded sessions; requeued tickets do not fail their
sessions. The stored reason identifies routing-ticket failure, within the existing
ScanTerminalReason bound. Root availability and stale-incarnation fences still apply.

Retain current remote_recover ordering for stale-node marking, scan idle expiry
and lease expiry; then reconcile residual routing failures. No wire response
shape is changed. Concurrent API processes/manual recovery calls serialize through
named transaction helpers; replays add no duplicate terminal events.

## Local fencing diagnostics (C3, C5)

Enable fixed WARN JSON-to-stderr logging in node binary startup, before runtime
execution. Use the existing workspace tracing and tracing-subscriber packages;
no new package/version, logging configuration, CLI/stdout/error contract or
library-global initialization is introduced. Direct startup-assembly subprocess
coverage verifies production logging is installed; scoped async subscriber capture
separately verifies the runtime fence emitter.

Use the existing Fenced outcome for a local heartbeat deadline/rejection fence;
Confirmed remote completion/failure settlement paths retain their behavior.
Remove the private TerminalElsewhere variant after correcting its sole producer;
no confirmed durable terminal observation was represented by that path.
The completion-settlement race must report the same local-fence diagnostic.
Local fencing stops dispatch and releases local capacity as today; it does not
send a late mutation using an expired lease or resume canceled work.
Emit a structured warning carrying event name, LeaseId and TicketId, stating that
remote recovery is pending. Split the existing combined no-op settlement branch
so a local fence cannot silently masquerade as a confirmed remote terminality.
Keep worker/node shutdown fencing safe and existing retry/permit behavior intact.

## Validation and success

The plan inventories red/green contract tests. Real SQLite tests use real Tokio
time plus ManualClock for domain deadlines; paused-time unit tests use fakes only.
A live owner plus expired scan lease converges to expired lease, failed ticket and
failed scan without waiting for the scan's idle deadline. Repeated/restarted
recovery preserves terminal states and event counts. Corruption rolls back the
coupled mutation. A runnable API process proves automatic recovery without a
workflow executor; termination and recovery-return tests prove supervision.
Node tests prove dispatch cancellation and an explicit local-fencing diagnostic.
Run repository guardrails and CI before PR handoff. Do not restart the media
experiment as part of this quest or claim this PR verifies its audio outputs.

## Failure model

Actors and deployments:
- Bundled voom-api process, authenticated owner node agents, local operators,
  existing explicit control-plane library callers, and controlled CI fixtures.
- Single or multiple API processes sharing one SQLite database; restarts and
  transient database contention are included.
Invariants and assets at stake:
- Durable lease/ticket/scan truth, append-only facts, no post-fence byte work,
  owner/root identity, checked stored data, passive inspection and bounded service work.
Accepted failure classes:
- Database unavailable or a pass exceeding thirty seconds: log failure, keep
  fencing, retry next cadence; do not assert successful recovery during outage.
- Cancellation before commit: transaction rollback and next-start replay.
- Corrupt persisted records: fail loud with DB classification; operator repair
  is required, and no incorrect scan is terminalized to make progress.
Covered elsewhere:
- Audio transformations/transcription: audio experiment/saga #771.
- Polling/TTL configuration and performance SLOs: experiment operator/#471.
- Node authentication, HTTP/TLS request bounds and storage-owner byte enforcement:
  existing API/node/store guardrails; no entry point or permission is widened.

## Threat model

Boundary inventory:
- Existing persisted ticket payloads/scan rows enter new recovery classification;
  existing API process authority performs lifecycle transitions. No new route.
Actor model:
- Database contents are untrusted persisted data; authenticated nodes and operators
  can produce malformed/conflicting records. Anonymous callers retain existing routing/auth.
Controls per boundary:
- Checked ID conversion, typed linkage, root/owner comparisons, transaction rollback,
  bounded pass/cadence and safe fixed diagnostics; no payload-selected file/network access.
Explicitly out of scope:
- Compromised host/SQLite engine and arbitrary operator SQL repair are outside this
  deployment's trust boundary; existing auth/TLS and owner access remain governed elsewhere.
