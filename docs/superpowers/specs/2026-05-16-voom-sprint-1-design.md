---
name: voom-sprint-1-design
description: Sprint 1 (Durable Control Plane MVP) design for VOOM — schema, repositories, ControlPlane use cases, and read-only CLI inspection for the durable execution model (jobs/tickets/leases/workers/artifacts/events), the durable identity model (work/variant/bundle/asset/version/location/evidence/snapshot), and the use-lease + commit-safety-gate machinery (full closure resolution, fail-closed, evidence revalidation, lease re-anchoring on rename/move, force-release audit path). Three migrations land in milestone order on a single branch; all writes go through ControlPlane use cases exercised by tests; the CLI exposes only read-only inspection.
status: proposed
date: 2026-05-16
sprint: 1
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-15-voom-sprint-0-design.md
  - docs/adr/0001-durable-jobs-over-events.md
  - docs/adr/0002-out-of-process-workers-only.md
  - docs/adr/0003-sqlx-and-tokio-foundation.md
  - docs/adr/0004-sibling-unit-tests.md
---

# VOOM Sprint 1 — Durable Control Plane MVP

## 1. Goal & Scope

Sprint 1 turns the empty-but-real Sprint 0 control plane into a
**durable-but-callerless** one. Every durable surface the architectural spec
names exists as schema, repository trait, `ControlPlane` use-case method,
event emission, and read-only CLI inspection — but no worker protocol, no
policy engine, and no filesystem watcher exist yet. Tests at the
`ControlPlane` and repository layer drive every write; the CLI exposes only
read-only inspection.

The top-level design's Sprint 1 exit criteria are:

- Tests can create jobs, lease tickets, expire leases, and recover work.
- Tests can create a file asset, add versions and locations, and report its
  event/evidence history.
- Tests can create a bundle, open and prioritize an issue, record a quality
  score, and block a commit with a use lease.
- Events are recorded for all state transitions.
- In-memory SQLite tests exercise the same repositories as disk mode.

This spec is how we get there.

Out of scope for Sprint 1 (deferred to named later sprints):

- Worker wire protocol, real worker process supervision (Sprint 2).
- Policy grammar, parser, compiler, planner (Sprint 3).
- Remote-node lease acquisition over the network (Sprint 4).
- Real ffprobe / FFmpeg / MKVToolNix / backup / verify / commit workers
  (Sprint 5).
- Filesystem watcher, scan reconciliation against live disk, daemon loop
  (Sprint 6).
- Web UI (Sprint 7).
- Plugin SDK, namespaced operation schema registration (Sprint 8).
- Approval gates, rollback flows, metrics endpoint, trace-ID propagation
  across plan/ticket/worker/artifact records (Sprint 9 — Sprint 1 ships
  only the `trace_id` column on `events`).
- Production packaging, upgrade migration tests, security review (Sprint 10).
- A `merge` operation that collapses two `FileAsset` lineages
  (architectural spec explicitly defers this).

## 2. Milestone Plan

Sprint 1 is committed to `feat/sprint-1` as three milestones in order. Each
milestone ships migrations, repositories, ControlPlane use-cases, tests, and
(M3 only) read-only CLI inspection. Each milestone is independently
committable; M2 and M3 build on M1's `events` table and repository pattern.

### M1 — Durable execution & events

Tables: `jobs`, `tickets`, `ticket_dependencies`, `leases`, `workers`,
`worker_capabilities`, `worker_grants`, `artifact_handles`,
`artifact_locations`, `artifact_lineage`, `events`. Migration:
`0002_durable_execution.sql`.

Lifecycle: worker-execution lease acquire / heartbeat / release / fail /
`expire_due` / `force_release`. Ticket state machine: `pending` → `ready` →
`leased` → `succeeded` | `failed`, with `leased` → `ready` on retriable
failure.

**Exit:** the architectural-spec clause *"Tests can create jobs, lease
tickets, expire leases, and recover work"* passes.

### M2 — Durable identity & bundles

Tables: `media_works`, `media_variants`, `asset_bundles`,
`asset_bundle_members`, `file_assets`, `file_versions`, `file_locations`,
`identity_evidence`, `media_snapshots`. Migration: `0003_identity.sql`.

Implements the Ingest Identity Invariants (each newly-discovered object →
new `FileAsset` unless immutable-generation alias proof; hash matches
recorded as evidence, not identity). Implements
`IdentityRepo::reconcile_rename` as a single-table transaction that retires
the prior `FileLocation` and records a new one on the same `FileVersion`.
M3 extends this same function with lease re-anchoring without changing its
signature.

**Exit:** the architectural-spec clause *"Tests can create a file asset, add
versions and locations, and report its event/evidence history"* passes.

### M3 — Use leases, commit safety gate, ancillary registries, inspection CLI

Tables: `asset_use_leases`, `commit_intents`, `commit_intent_scope_members`,
`external_systems`, `external_system_links`, `external_path_mappings`,
`issues`, `issue_links`, `quality_scoring_profiles`, `quality_scores`.
Migration: `0004_use_leases_ancillary.sql`.

Implements:

- the full `asset_use_leases` lifecycle (TTL-bound + manual locks, terminal
  release reasons, force-release audit path)
- the Commit Safety Gate (three-phase `prepare` / `authorize` /
  `finalize` protocol, affected-scope closure across alias
  `FileLocation`s, fail-closed when alias resolution is incomplete,
  evidence revalidation, lease re-anchoring on rename/move,
  force-path semantics that never bypass evidence revalidation or
  the closure-shift check), including the architectural
  "immediately before the irreversible filesystem mutation" recheck
  that runs in `authorize_destructive_commit` and the pending-commit
  lock that serializes new use-lease acquires against in-flight
  destructive commits (`pending` + `authorized`) (§9.1, §9.2, §9.3)
- `IdentityRepo::reconcile_rename` extended to re-anchor any non-terminal
  blocking and advisory leases scoped to the retired `FileLocation` to the
  new `FileLocation` inside the same transaction (preserving `lease_id`,
  `issuer`, `acquired_at`, `expires_at`, `last_heartbeat_at`,
  `blocking_mode`)
- the ancillary registries (external systems, issues, quality scores) as
  CRUD repos
- the terminal-failure → `IssueRepo` auto-open wiring on
  `ControlPlane::fail_lease` /
  `ControlPlane::expire_due` (§10.2 / S3): every
  `ticket.failed_terminal` transition opens exactly one new
  `terminal_failure` issue linked to the ticket and last lease in the
  same transaction. This is the architectural DLQ analogue (arch spec
  → Error Handling And Recovery / Issue Model). M1's `LeaseRepo`
  emits the event but does not open the issue; M3 adds the wiring
  without changing the M1 `LeaseRepo` API.
- the resource-then-verb CLI inspection surface (read-only)
- the Sprint 1 smoke recipe additions

**Exit:** the architectural-spec clauses *"Tests can create a bundle, open
and prioritize an issue, record a quality score, and block a commit with a
use lease"* and *"Events are recorded for all state transitions"* pass,
plus the full safety-gate scenarios (closure across aliases, fresh blocking
lease aborts in-flight commit, force-release writes audit event, external
rename re-anchors leases).

## 3. Workspace & Crate Deltas

| Crate | Sprint 1 contents added |
|---|---|
| `voom-core` | New ID newtypes (see §12.2). New `ErrorCode` variants and `VoomError` cases (§12.1). New `voom-core::failure` module exposing `FailureClass` and `FailureRetryClass` with `is_retriable` / `retry_class` / `issue_severity` / `issue_priority` / `into_error_code` (§12.5). New `voom-core::issue` module exposing `IssueSeverity` and `IssuePriority` enums — shared by `voom-core::failure`, `voom-events` payloads, `voom-store::repo::issues`, and the CLI inspection surface (§10.2). Test-only `FrozenClock` / `ManualClock` exposed via `voom-core::clock_test_support`, and test-only `FrozenRng` / `SeededRng` exposed via `voom-core::rng_test_support` (§7.5 backoff seam). |
| `voom-store` | `repo/jobs.rs`, `repo/tickets.rs`, `repo/leases.rs`, `repo/workers.rs`, `repo/artifacts.rs`, `repo/events.rs`, `repo/identity.rs`, `repo/bundles.rs`, `repo/use_leases.rs`, `repo/commit_safety_gate.rs`, `repo/external_systems.rs`, `repo/issues.rs`, `repo/quality_scores.rs`. Three new SQL migrations under `migrations/`. |
| `voom-events` | `EventKind` enum (one variant per state transition Sprint 1 emits), `EventEnvelope`, per-kind typed payload structs, the `Event` sum type that pairs kind with payload, `AssertionKind` enum used by `identity_evidence.assertion_type` validation. No DB code — emission goes through `voom-store::repo::events::EventRepo`. |
| `voom-control-plane` | One use-case method per durable write the tests need (job/ticket creation, dependency declaration, lease lifecycle calls, ingest, rename reconciliation, evidence acceptance, use-lease lifecycle, destructive-commit runner, bundle membership, issue lifecycle, score recording, external-system registration). Each use case opens one transaction and composes the repo `_in_tx` calls. |
| `voom-cli` | `commands/` module subdirectory with one file per resource group (job, ticket, lease, worker, artifact, work, variant, bundle, asset, evidence, issue, score, external-system, use-lease, commit-intent, event). Read-only verbs only. |
| `voom-api`, `voom-policy`, `voom-plan`, `voom-scheduler`, `voom-artifact`, `voom-worker-protocol` | Untouched. No Sprint 1 deliverables land here. |

`voom-control-plane`'s exposed methods consumed by `voom-cli`'s inspection
commands stay narrow (list/get per resource); the broader write surface is
internal to the crate, exercised by tests in the `voom-control-plane/tests/`
directory.

## 4. Schema Overview

Three new migrations applied in numeric order by `sqlx::migrate!` from
`../../migrations`. Each migration starts with a comment block naming its
milestone, the architectural-spec sections it implements, and the diff
target on review.

| Migration | Milestone | Tables introduced |
|---|---|---|
| `0002_durable_execution.sql` | M1 | `jobs`, `tickets`, `ticket_dependencies`, `leases`, `workers`, `worker_capabilities`, `worker_grants`, `artifact_handles`, `artifact_locations`, `artifact_lineage`, `events` |
| `0003_identity.sql` | M2 | `media_works`, `media_variants`, `asset_bundles`, `asset_bundle_members`, `file_assets`, `file_versions`, `file_locations`, `identity_evidence`, `media_snapshots` |
| `0004_use_leases_ancillary.sql` | M3 | `asset_use_leases`, `commit_intents`, `commit_intent_scope_members`, `external_systems`, `external_system_links`, `external_path_mappings`, `issues`, `issue_links`, `quality_scoring_profiles`, `quality_scores` |

All tables are `STRICT` with explicit `NOT NULL` and `CHECK` constraints.
Primary keys are `INTEGER PRIMARY KEY` (SQLite rowid, mapped to `u64`
newtypes in `voom-core`). Foreign keys are
`INTEGER NOT NULL REFERENCES <table>(id) ON DELETE RESTRICT` unless the
parent's lifecycle owns the child (`worker_capabilities` and
`worker_grants` use `ON DELETE CASCADE FROM workers`;
`asset_bundle_members` uses `ON DELETE CASCADE FROM asset_bundles`;
`issue_links` uses `ON DELETE CASCADE FROM issues`).

Soft-delete (a `retired_at TEXT NULL` timestamp) is used wherever durable
history matters: `file_locations`, `file_versions`, `file_assets`, `leases`
(via `release_reason`), `asset_use_leases` (via `release_reason`),
`external_system_links`, `external_path_mappings`, `external_systems`,
`quality_scoring_profiles`. The `events` table is never deleted from; rows
are immortal facts enforced by SQL triggers (see §6).

Tables that face concurrent writers (`tickets`, `leases`,
`asset_use_leases`, `commit_intents`, `file_locations`, `issues`,
`media_works`, `media_variants`) carry an `epoch INTEGER NOT NULL DEFAULT 0`
column. Every UPDATE includes `WHERE id = ? AND epoch = ?` and bumps
`epoch = epoch + 1`. A zero-rows-affected result becomes
`VoomError::Conflict` → `ErrorCode::Conflict` so callers retry without
manual re-reads.

Sprint 1 deliberately relies on SQLite's `BEGIN IMMEDIATE`
single-writer serialization — there is no `SKIP LOCKED` and no
scheduler yet to need one (see design doc → Data Storage for the
SQLite/Postgres dequeue contract). Sprint 4's scheduler reasoning
starts from that fact rather than re-deriving it.

The full SQL lives in the migrations. Per-column descriptions appear in
each domain section below (§§6–10).

## 5. Repository Pattern at Scale

Sprint 0's `SchemaMetaRepo` set the template:
`#[async_trait] trait XxxRepo: Repository`, `SqliteXxxRepo(SqlitePool)`
impl, `Result<T, VoomError>` returns. Sprint 1 follows the same shape with
two extensions.

### 5.1 Transaction-scoped methods

Every write method on a Sprint 1 repo comes in two forms:

```rust
async fn create(&self, input: NewTicket) -> Result<Ticket, VoomError>;

async fn create_in_tx<'tx>(
    &self,
    tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
    input: NewTicket,
) -> Result<Ticket, VoomError>;
```

The `_in_tx` form is the primitive; the bare form is sugar that opens a
transaction, calls `_in_tx`, and commits. `ControlPlane` use cases that
need to compose multiple writes call the `_in_tx` forms inside a single
`pool.begin()` so the durable mutation and the event row commit atomically.

### 5.2 Event-emission contract

Every durable mutation in Sprint 1 emits at least one event in the same
transaction. Domain repos do **not** take an `EventRepo` parameter; instead
`ControlPlane` use cases are the single layer responsible for "wrote the
row AND wrote the event," composing the repo `_in_tx` call with
`EventRepo::append_in_tx` inside one transaction. This keeps each domain
repo focused on one table and lets reviewers see the event contract
whenever they read a use case.

The one named exception is the commit safety gate (§9.3), which is a
host-side multi-table helper rather than a domain repo. Each of its
four entry points — `prepare_destructive_commit`,
`authorize_destructive_commit`, `finalize_destructive_commit`, and
`abort_destructive_commit` — owns its own IMMEDIATE transaction
internally and accepts the repo dependencies that phase needs.
`prepare` and `authorize` accept `&dyn AliasResolver` (for closure
walking) and `&dyn EventRepo`. `finalize` accepts `&dyn AliasResolver`
(for the Phase C trip-wire recompute of `closure_final`),
`&dyn EventRepo`, and `&dyn IdentityRepo` (for every Sprint 1
`CommitTarget` variant — all of which resolve to a durable
identity-table mutation). `abort` accepts only `&dyn EventRepo`
since it never recomputes or applies. Bundle-target commits
(`ArchiveBundle` / `DeleteBundle`) are deferred to Sprint 5, so no
`&dyn BundleRepo` parameter is needed in Sprint 1's gate API. The
filesystem mutation supplied by the caller runs **between**
`authorize` and `finalize`, outside any DB transaction; the
`commit_intents` journal is what makes the three-phase split safe
across caller crashes.

### 5.3 Repository ownership

Repos own one table, or a tight cluster when an FK is internal and writers
always operate on both:

- `JobRepo` → `jobs`
- `TicketRepo` → `tickets` + `ticket_dependencies`
- `LeaseRepo` → `leases`
- `WorkerRepo` → `workers` + `worker_capabilities` + `worker_grants`
- `ArtifactRepo` → `artifact_handles` + `artifact_locations` + `artifact_lineage`
- `EventRepo` → `events`
- `IdentityRepo` → `media_works` + `media_variants` + `file_assets` + `file_versions` + `file_locations` + `identity_evidence` + `media_snapshots`
- `BundleRepo` → `asset_bundles` + `asset_bundle_members`
- `UseLeaseRepo` → `asset_use_leases`
- `commit_safety_gate` module → no repo trait; exposes the three-phase
  protocol `prepare_destructive_commit(...)`,
  `authorize_destructive_commit(...)`,
  `finalize_destructive_commit(...)`, `abort_destructive_commit(...)`,
  and `list_pending_commit_intents(...)` against the `commit_intents`
  table. `abort_destructive_commit` is **pending-only** — once an
  intent reaches `state = 'authorized'`, the only sanctioned
  pre-success termination path is
  `finalize_destructive_commit(_, _, _, permit,
  MutationOutcome::NotPerformed)`, gated by the `CommitPermit`
  (§9.3.1, §9.3.2). Also owns `commit_intent_scope_members` — the
  per-closure-member rows that back the pending-commit lock consulted
  by `UseLeaseRepo::acquire_in_tx` (§9.1, §9.2)
- `ExternalSystemRepo` → `external_systems` + `external_system_links` + `external_path_mappings`
- `IssueRepo` → `issues` + `issue_links`
- `QualityScoreRepo` → `quality_scoring_profiles` + `quality_scores`

Each trait exposes per-table CRUD plus the named operations the
architectural spec calls out (`TicketRepo::mark_ready`,
`LeaseRepo::acquire`, `LeaseRepo::heartbeat`, `LeaseRepo::expire_due`,
`UseLeaseRepo::reanchor_on_move`, etc.). No generic "object store"; named
verbs per domain.

### 5.4 Optimistic locking

Updates on tables with concurrent-writer concerns (see §4) include
`WHERE id = ? AND epoch = ?` and bump the epoch by 1. Zero rows affected
returns `VoomError::Conflict` → `ErrorCode::Conflict`. Callers re-read and
retry; the spec does not prescribe a retry policy because Sprint 1 has no
daemon driving repeated writes.

## 6. Event Log Model

Single table, one shape:

```sql
CREATE TABLE events (
    event_id     INTEGER PRIMARY KEY,
    occurred_at  TEXT NOT NULL,           -- ISO-8601 UTC, control-plane clock
    kind         TEXT NOT NULL,           -- EventKind::as_str()
    subject_type TEXT NOT NULL,           -- 'ticket' | 'lease' | 'file_asset' | ...
    subject_id   INTEGER,                 -- nullable for system-scope events
    trace_id     TEXT,                    -- nullable; Sprint 9 trace work populates
    payload      TEXT NOT NULL,           -- JSON
    CHECK (json_valid(payload))
) STRICT;

CREATE INDEX events_by_subject ON events (subject_type, subject_id, occurred_at, event_id);
CREATE INDEX events_by_kind_time ON events (kind, occurred_at, event_id);
CREATE INDEX events_by_time ON events (occurred_at, event_id);

CREATE TRIGGER events_no_update
BEFORE UPDATE ON events
BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;

CREATE TRIGGER events_no_delete
BEFORE DELETE ON events
BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;
```

### 6.1 `voom-events` API

```rust
pub enum EventKind {
    SchemaInitialized,                  // 'schema.initialized' — emitted by voom init
    TicketCreated,                      // 'ticket.created'
    TicketReady,                        // 'ticket.ready'
    TicketLeased,                       // 'ticket.leased'
    TicketSucceeded,                    // 'ticket.succeeded'
    TicketFailedRetriable,              // 'ticket.failed_retriable'
    TicketFailedTerminal,               // 'ticket.failed_terminal'
    TicketRequeuedAfterLeaseExpiry,     // 'ticket.requeued_after_lease_expiry'
    TicketRequeuedAfterForceRelease,    // 'ticket.requeued_after_force_release'
    JobOpened,                          // 'job.opened'
    JobSucceeded,                       // 'job.succeeded'
    JobFailed,                          // 'job.failed'
    JobCancelled,                       // 'job.cancelled'
    LeaseAcquired,                      // 'lease.acquired'
    LeaseReleased,                      // 'lease.released'
    LeaseExpired,                       // 'lease.expired'
    LeaseForceReleased,                 // 'lease.force_released'
    WorkerRegistered,                   // 'worker.registered'
    WorkerCapabilityRecorded,           // 'worker.capability_recorded'
    WorkerGrantRecorded,                // 'worker.grant_recorded'
    WorkerRetired,                      // 'worker.retired'
    ArtifactHandleCreated,              // 'artifact_handle.created'
    ArtifactLocationRecorded,           // 'artifact_location.recorded'
    ArtifactLocationRetired,            // 'artifact_location.retired'
    ArtifactLineageRecorded,            // 'artifact_lineage.recorded'
    MediaWorkCreated,                   // 'media_work.created'
    MediaVariantCreated,                // 'media_variant.created'
    AssetBundleCreated,                 // 'asset_bundle.created'
    AssetBundleMemberAdded,             // 'asset_bundle.member_added'
    AssetBundleMemberRemoved,           // 'asset_bundle.member_removed'
    FileAssetCreated,                   // 'file_asset.created'
    FileVersionCreated,                 // 'file_version.created'
    FileLocationRecorded,               // 'file_location.recorded'
    FileLocationAliased,                // 'file_location.aliased'
    FileLocationRetiredByMove,          // 'file_location.retired_by_move'
    FileLocationRecordedByMove,         // 'file_location.recorded_by_move'
    IdentityEvidenceRecorded,           // 'identity_evidence.recorded'
    IdentityEvidenceAccepted,           // 'identity_evidence.accepted'
    IdentityEvidenceSuperseded,         // 'identity_evidence.superseded'
    MediaSnapshotRecorded,              // 'media_snapshot.recorded'
    UseLeaseAcquired,                   // 'use_lease.acquired'
    UseLeaseReleased,                   // 'use_lease.released'
    UseLeaseExpired,                    // 'use_lease.expired'
    UseLeaseForceReleased,              // 'use_lease.force_released'
    UseLeaseRecoveredStaleIssuer,       // 'use_lease.recovered_stale_issuer'
    UseLeaseReanchoredByMove,           // 'use_lease.reanchored_by_move'
    CommitIntentRecorded,               // 'commit.intent_recorded' — Phase A success
    CommitAuthorized,                   // 'commit.authorized' — Phase B success
    CommitCompleted,                    // 'commit.completed' — Phase C success
    CommitAbortedByUseLease,            // 'commit.aborted_by_use_lease' — Phase A or Phase B (payload.phase distinguishes)
    CommitAbortedByStaleEvidence,       // 'commit.aborted_by_stale_evidence' — Phase A or Phase B (payload.phase distinguishes)
    CommitAbortedByClosureIncomplete,   // 'commit.aborted_by_closure_incomplete' — Phase A or Phase B (payload.phase distinguishes)
    CommitAbortedByClosureGrew,         // 'commit.aborted_by_closure_grew' — Phase B (closure shift detected by authorize recheck)
    CommitAbortedPreMutation,           // 'commit.aborted_pre_mutation' — abort_destructive_commit / finalize NotPerformed (payload.prior_state distinguishes)
    CommitAbortedPostMutation,          // 'commit.aborted_post_mutation' — Phase C trip-wire (reason: 'closure_grew' | 'fresh_lease' | 'closure_grew_and_fresh_lease')
    CommitRecoveryRequired,             // 'commit.recovery_required' — emitted by the Sprint 5+ recovery worker
    CommitForcedOverride,               // 'commit.forced_override' — closure-incomplete bypass audit (Phase A or Phase B)
    ExternalSystemRegistered,           // 'external_system.registered'
    ExternalSystemHealthChanged,        // 'external_system.health_changed'
    ExternalSystemLinked,               // 'external_system.linked'
    ExternalSystemUnlinked,             // 'external_system.unlinked'
    ExternalPathMappingRecorded,        // 'external_path_mapping.recorded'
    IssueOpened,                        // 'issue.opened'
    IssuePriorityChanged,               // 'issue.priority_changed'
    IssueSuppressed,                    // 'issue.suppressed'
    IssueAccepted,                      // 'issue.accepted'
    IssueResolved,                      // 'issue.resolved'
    IssueLinked,                        // 'issue.linked'
    QualityProfileRegistered,           // 'quality_profile.registered'
    QualityScoreRecorded,               // 'quality_score.recorded'
    QualityScoreSuperseded,             // 'quality_score.superseded'
}

impl EventKind {
    pub const fn as_str(self) -> &'static str { /* ... */ }
}

pub struct EventEnvelope {
    pub kind: EventKind,
    pub occurred_at: OffsetDateTime,
    pub subject_type: SubjectType,
    pub subject_id: Option<u64>,
    pub trace_id: Option<TraceId>,
    pub payload: Event,                  // tagged union, see below
}

pub enum SubjectType {
    System, Job, Ticket, Lease, Worker, ArtifactHandle, ArtifactLocation,
    MediaWork, MediaVariant, AssetBundle, FileAsset, FileVersion,
    FileLocation, IdentityEvidence, MediaSnapshot, UseLease, Commit,
    ExternalSystem, ExternalSystemLink, ExternalPathMapping,
    Issue, QualityProfile, QualityScore,
}

/// One variant per EventKind. Payload struct names mirror the kind.
pub enum Event {
    SchemaInitialized(SchemaInitializedPayload),
    TicketCreated(TicketCreatedPayload),
    /* ... one variant per EventKind ... */
}
```

The `Event` sum type pairs each kind with its typed payload. The compiler
prevents writers from emitting a payload that doesn't match the kind.
Per-kind payload schemas are part of the wire contract; the spec defines
them by Rust struct definition in `voom-events`, not in this prose
document.

The `TicketFailedRetriable` and `TicketFailedTerminal` payloads each
carry a `class: FailureClass` field (see §12.5) so audit can
reconstruct the retriability decision that drove the transition.
`TicketFailedRetriable` additionally carries the `next_eligible_at`
the backoff seam (§7.5) produced; `TicketFailedTerminal` carries the
`issue_id` of the `terminal_failure` issue the use case opened
alongside the transition (§10.2 / S3), or `null` on the M1 milestone
where `IssueRepo` does not yet exist. No new event kinds are added;
the payloads grow.

### 6.2 `EventRepo` interface

```rust
#[async_trait]
pub trait EventRepo: Repository {
    async fn append_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        envelope: EventEnvelope,
    ) -> Result<EventId, VoomError>;

    async fn list(&self, filter: EventFilter, page: Page) -> Result<EventPage, VoomError>;
    async fn get(&self, event_id: EventId) -> Result<Option<EventRow>, VoomError>;
    async fn tail(&self, filter: EventFilter, page: Page) -> Result<EventPage, VoomError>;
}
```

`append_in_tx` is the single write path. `list`/`tail`/`get` serve the CLI
inspection commands.

Sprint 1 extends `voom_store::init` so that a successful migration run on
a DB that wasn't already initialized is followed by a single transaction
that writes a `schema.initialized` event (`subject_type = system`,
`subject_id = NULL`). The event records `migrations_applied` and
`schema_init_at` in its payload, matching what `init` already returns in
`InitReport`. The event is **not** emitted when
`InitReport.already_initialized == true`, preserving `init`'s idempotency
contract — repeated `voom init` invocations against a current DB do not
double-write the event. The follow-up transaction keeps `voom event list`
non-empty on a freshly-initialized DB, which the Sprint 1 smoke recipe
asserts.

### 6.3 Why a single JSON-payload table

The architectural spec lists ~60+ event kinds at full system surface, with
payloads ranging from single-field (`ticket.ready { ticket_id }`) to wide
(`commit.aborted_by_closure_incomplete { commit_id, target_scope,
unreachable_locations, alias_provider, error }`). A per-kind column model
forces a migration on every payload change; a single JSON-payload table
plus a typed Rust wrapper keeps column-level typing on the Rust side
without locking the on-disk schema. Sprint 9's trace-ID work and future
event-kind additions cost a new `EventKind` variant + payload struct only.

## 7. Durable Execution Model (M1)

### 7.1 `jobs`

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `kind` | TEXT NOT NULL | Free-form for Sprint 1; Sprint 3 narrows to compiled-policy op names |
| `state` | TEXT NOT NULL | `open` \| `succeeded` \| `failed` \| `cancelled` |
| `priority` | INTEGER NOT NULL DEFAULT 0 | Higher integers rank first |
| `created_at` | TEXT NOT NULL | |
| `updated_at` | TEXT NOT NULL | |
| `epoch` | INTEGER NOT NULL DEFAULT 0 | |

Sprint 3 adds an `execution_plan_id` column linking the job to its compiled
plan. Sprint 1 jobs have no plan.

### 7.2 `tickets`

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `job_id` | INTEGER NULL REFERENCES `jobs(id)` | Nullable for ad-hoc tickets that don't belong to a job |
| `kind` | TEXT NOT NULL | Op name; Sprint 1 accepts any string |
| `state` | TEXT NOT NULL | `pending` \| `ready` \| `leased` \| `succeeded` \| `failed` |
| `priority` | INTEGER NOT NULL DEFAULT 0 | |
| `payload` | TEXT NOT NULL | JSON; opaque to the repo |
| `result` | TEXT NULL | JSON; opaque; populated on `succeeded`/`failed` |
| `attempt` | INTEGER NOT NULL DEFAULT 0 | Number of times this ticket has been acquired by a worker. New tickets start at 0. Each successful `LeaseRepo::acquire` increments by 1. Never decremented; never bumped on `fail`/`expire_due` (the bump happens on the next acquire). |
| `max_attempts` | INTEGER NOT NULL DEFAULT 1 | |
| `next_eligible_at` | TEXT NOT NULL | ISO-8601; used for backoff after retriable failure. New tickets default to `created_at`; `LeaseRepo::fail` with a retriable `FailureClass` (S2) sets it to `now + TicketRepo::default_backoff(attempt, clock, rng)` (see §7.5 below; arch spec → Error Handling And Recovery → Retry policy) |
| `created_at` | TEXT NOT NULL | |
| `state_changed_at` | TEXT NOT NULL | |
| `epoch` | INTEGER NOT NULL DEFAULT 0 | |

State transitions, all atomic via `_in_tx`:

- `pending` → `ready` when every row in `ticket_dependencies` for this
  ticket is in `succeeded` state. Driven by `TicketRepo::mark_ready_if_unblocked(ticket_id)`,
  which the `LeaseRepo::release` use case calls for every downstream
  dependent of the ticket that just completed.
- `ready` → `leased` via `LeaseRepo::acquire`.
- `leased` → `succeeded` via `LeaseRepo::release`.
- `leased` → `ready` via `LeaseRepo::fail` when the failure's `FailureClass` (S2) is retriable and `ticket.attempt < ticket.max_attempts`. `next_eligible_at` is set to `now + TicketRepo::default_backoff(attempt, clock, rng)` (see below). `attempt` is **not** bumped here — the bump happens on the next `acquire`.
- `leased` → `failed` via `LeaseRepo::fail` otherwise.
- `leased` → `ready` via `LeaseRepo::expire_due` if retries remain (`ticket.attempt < ticket.max_attempts`); `leased` → `failed` otherwise. Same convention: no bump on requeue; the next `acquire` increments.
- `leased` → `ready` via `LeaseRepo::force_release(_, _, _, also_requeue = true)`, gated on `attempt < max_attempts` (retries-exhausted callers receive `VoomError::Conflict` and the lease/ticket/event log stay unchanged — see §7.5). Same no-bump convention; `next_eligible_at` is set to `now` (operator-driven requeue, no backoff). Emits `ticket.requeued_after_force_release`.
- `leased` → `failed` via `LeaseRepo::force_release(_, _, _, also_requeue = false)` — terminal, equivalent to the `fail` terminal branch with implicit `FailureClass::UserCancellation`. Emits `ticket.failed_terminal`.

### 7.3 `ticket_dependencies`

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `ticket_id` | INTEGER NOT NULL REFERENCES `tickets(id)` ON DELETE CASCADE | |
| `depends_on_ticket_id` | INTEGER NOT NULL REFERENCES `tickets(id)` ON DELETE RESTRICT | |
| `kind` | TEXT NOT NULL | `phase` for Sprint 1 |

Unique on `(ticket_id, depends_on_ticket_id)`. `TicketRepo::add_dependency`
rejects self-references and runs a cycle check against existing dependency
rows; cycle detection returns `ErrorCode::DependencyCycle`.

### 7.4 `leases` (worker-execution leases)

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `ticket_id` | INTEGER NOT NULL REFERENCES `tickets(id)` | |
| `worker_id` | INTEGER NOT NULL REFERENCES `workers(id)` | |
| `state` | TEXT NOT NULL | `held` \| `released` \| `expired` \| `force_released` |
| `acquired_at` | TEXT NOT NULL | |
| `expires_at` | TEXT NOT NULL | |
| `last_heartbeat_at` | TEXT NOT NULL | Initialized to `acquired_at` |
| `ttl_seconds` | INTEGER NOT NULL | |
| `release_reason` | TEXT NULL | Mirrors `state` after terminal transition |
| `released_at` | TEXT NULL | NULL while non-terminal |
| `epoch` | INTEGER NOT NULL DEFAULT 0 | |

Index on `(state, expires_at)` filtered to non-terminal rows to keep
`expire_due` fast.

### 7.5 `LeaseRepo` lifecycle

- `acquire(NewLease) -> Result<Lease>` — IMMEDIATE transaction: pick the
  ticket row by ID, assert `state = 'ready' AND next_eligible_at <= now AND attempt < max_attempts`.
  Transition `state` to `leased`. Increment `attempt` by 1. Insert lease
  row with `expires_at = now + ttl`. Use case emits `ticket.leased` +
  `lease.acquired`.

  > **Deferred to Sprint 3+:** `worker_capabilities`,
  > `worker_grants.can_execute`, `denies`, and `max_parallel` are NOT
  > consulted at acquire time in Sprint 1. The tables exist so Sprint 1
  > use cases (`record_capability`, `record_grant`) can populate them;
  > acquire-time gating ships with policy compilation in Sprint 3 and
  > remote-worker acquisition in Sprint 4. Until then, callers (the
  > Sprint 1 in-process scheduler) are the sole eligibility authority.

- `heartbeat(lease_id) -> Result<Lease>` — assert `state = 'held'`, set
  `last_heartbeat_at = now`, `expires_at = now + ttl`. No event in
  Sprint 1 (Sprint 6 daemon may emit a recovery event after a previously
  missed beat).
- `release(lease_id, ResultPayload) -> Result<()>` — assert `state = 'held'`,
  transition lease to `released`, transition ticket to `succeeded`, write
  `result` JSON. Use case emits `ticket.succeeded` + `lease.released`,
  then calls `TicketRepo::mark_ready_if_unblocked` for every dependent ticket.
- `fail(lease_id, class: FailureClass) -> Result<()>` — assert
  `state = 'held'`. The free `(FailureReason, retriable: bool)` pair
  is gone: retriability is derived from `class.is_retriable()`
  (`voom-core::failure::FailureClass`), so a caller cannot retry a
  `stale_identity_evidence` or `closure_resolution_incomplete`
  failure that the architectural taxonomy (design doc → Error
  Handling And Recovery → Failure taxonomy) requires to fail-closed.
  If `class.is_retriable() && ticket.attempt < ticket.max_attempts`,
  transition ticket to `ready`, set `next_eligible_at = now +
  TicketRepo::default_backoff(attempt, clock, rng)`, do **not** bump
  `attempt`; emit `ticket.failed_retriable` with payload carrying
  the `FailureClass`. Else transition ticket to `failed`; emit
  `ticket.failed_terminal`, also carrying the `FailureClass`. Lease
  transitions to `released` with `release_reason =
  'failed_retriable' | 'failed_terminal'`; emit `lease.released`.
  On the terminal branch, the use case (§10.2 / S3) also opens a
  new `terminal_failure` issue in the same transaction (M3 onwards;
  M1 emits the event with `issue_id = null`).
- `expire_due(now) -> Result<ExpireReport>` — bulk: find leases with
  `state = 'held' AND expires_at < now`. Per row: transition lease to
  `expired` (`release_reason = 'issuer_lost'`, `released_at = now`),
  transition ticket to `ready` if retries remain
  (`ticket.attempt < ticket.max_attempts`) or `failed` otherwise. The
  expiry path's implicit `FailureClass` is `worker_crash` (retriable);
  callers do not supply one. Same convention as `fail`: do **not**
  bump `attempt` on requeue — `acquire` will bump on the next
  dispatch. Emit `lease.expired` +
  `ticket.requeued_after_lease_expiry` or `ticket.failed_terminal` per
  row; the terminal payload carries `FailureClass::WorkerCrash` so
  audit can reconstruct the decision. On the terminal branch the use
  case (§10.2 / S3) also opens a new `terminal_failure` issue in the
  same transaction (M3 onwards; M1 emits the event with `issue_id =
  null`). Returns
  `ExpireReport { expired_leases: Vec<LeaseId>, requeued_tickets:
  Vec<TicketId>, failed_tickets: Vec<TicketId> }`.
- `force_release(lease_id, actor, reason, also_requeue: bool) -> Result<()>` —
  admin/test path: transition lease to `force_released`, ticket to
  `ready` if `also_requeue` else `failed`. Both branches emit
  `lease.force_released` with `{ actor, reason }` in the payload, and
  both emit a matching `ticket.*` event so the ticket state transition
  is durably recorded (§14: "Events are recorded for all state
  transitions"). Specifically:

  - **Failed branch (`also_requeue = false`)** — the ticket transition
    is itself a terminal failure, equivalent to the `fail` terminal
    branch. The use case emits `ticket.failed_terminal` with
    `class = FailureClass::UserCancellation` (the operator's `actor`
    and `reason` are preserved on the accompanying
    `lease.force_released` payload, not duplicated onto
    `ticket.failed_terminal`), and on M3 onwards opens a new
    `terminal_failure` issue per the §10.2 / S3 auto-open contract;
    on M1 the `issue_id` payload field is `null`.
  - **Requeue branch (`also_requeue = true`)** — the ticket
    transitions from `leased` back to `ready`. The branch has a
    retry-budget precondition: `ticket.attempt < ticket.max_attempts`
    must hold at call time. If retries are exhausted
    (`attempt >= max_attempts`), the call rejects with
    `VoomError::Conflict` (message names the ticket id, current
    `attempt`, `max_attempts`, and tells the operator to use
    `also_requeue = false` for the terminal path); the lease, ticket,
    and event log are **all unchanged** — no `lease.force_released`,
    no ticket transition, no issue. Mirrors the same retry-budget
    rule `fail` and `expire_due` already enforce, and prevents
    stranding a ticket in `ready` that no subsequent `acquire` will
    claim (acquire's precondition includes `attempt < max_attempts`).
    On success, the use case emits a
    `ticket.requeued_after_force_release` event (parallel to
    `ticket.requeued_after_lease_expiry` for the expire-driven
    requeue) whose payload carries `{ ticket_id, lease_id, actor,
    reason }`. `attempt` is **not** bumped on the requeue itself —
    the next `acquire` bumps it, matching the convention §7.5
    already establishes for `fail` and `expire_due` requeue paths.
    `next_eligible_at` is set to `now` (no backoff — the operator
    explicitly chose to requeue immediately; this differs from the
    backoff-driven `fail`-retriable requeue).

**Backoff seam.** Backoff is exposed as a named function on
`TicketRepo` so the policy is replaceable without changing the lease
lifecycle:

```rust
pub trait TicketRepo: Repository {
    /// Returns the duration to wait before the next acquire is eligible.
    /// Sprint 1 implementation produces a deterministic seeded-jitter
    /// value (or a stable fixed step picked to keep existing tests
    /// stable); Sprint 4+ replaces the body with the architectural
    /// capped-exponential-with-jitter shape from the design doc
    /// (Error Handling And Recovery → Retry policy). The signature
    /// stays stable across that swap.
    fn default_backoff(
        attempt: u32,
        clock: &dyn Clock,
        rng:   &mut dyn RngCore,
    ) -> Duration;
    /* + all other TicketRepo methods */
}
```

The `clock` and `rng` parameters mirror the existing `Clock`-injection
seam used by §12.3 — `RngCore` is injected through a small
`voom-core::rng_test_support` module that exposes a deterministic
`FrozenRng` (returns a single configured value) and a `SeededRng` for
property-style tests. `ControlPlane::fail_lease` constructs the
RNG from its injected `Arc<dyn Rng>` and passes it through to
`LeaseRepo::fail`, which forwards to `TicketRepo::default_backoff`.
The architectural retry-policy parameters (`base`, `cap`) are owned by
scheduling policy in Sprint 4+; Sprint 1 hard-codes whatever
deterministic shape keeps the §13 integration tests stable, since the
backoff *shape* does not affect the attempt-accounting invariant the
worked example below exercises.

**Worked example: `max_attempts = 2`.** Using a retriable class
(`FailureClass::WorkerTimeout` here, but any class for which
`class.is_retriable()` is `true` would behave identically), this
sequence yields the expected two dispatched attempts before terminal
failure:

1. Initial: `attempt = 0`, `state = ready`.
2. `acquire` → `attempt = 1`, `state = leased`.
3. `fail(FailureClass::WorkerTimeout)` → `is_retriable() = true` and
   `1 < 2`, so the ticket requeues: `attempt = 1`, `state = ready`.
4. `acquire` → `attempt = 2`, `state = leased`.
5. `fail(FailureClass::WorkerTimeout)` → `is_retriable() = true` but
   `2 < 2` is `false`, so the ticket goes terminal:
   `attempt = 2`, `state = failed`.

Total dispatched attempts: 2. The same convention applies to
`expire_due` in place of `fail` (its implicit class is
`FailureClass::WorkerCrash`, also retriable). Passing a non-retriable
or operator-required class to `fail` short-circuits this entirely:
`is_retriable()` returns `false`, so the ticket transitions to
`failed` on the first call regardless of `attempt` vs. `max_attempts`.

### 7.6 `workers`, `worker_capabilities`, `worker_grants`

`workers`:

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `name` | TEXT NOT NULL UNIQUE | |
| `kind` | TEXT NOT NULL | `synthetic` \| `local` \| `remote` |
| `status` | TEXT NOT NULL | `registered` \| `active` \| `stale` \| `retired` |
| `registered_at` | TEXT NOT NULL | |
| `last_seen_at` | TEXT NOT NULL | |
| `retired_at` | TEXT NULL | |
| `epoch` | INTEGER NOT NULL DEFAULT 0 | |

`worker_capabilities`:

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `worker_id` | INTEGER NOT NULL REFERENCES `workers(id)` ON DELETE CASCADE | |
| `operation` | TEXT NOT NULL | Op name (`transcode_video`, `probe_file`, ...) |
| `codecs` | TEXT NOT NULL | JSON array of TEXT |
| `hardware` | TEXT NOT NULL | JSON array of TEXT |
| `artifact_access` | TEXT NOT NULL | JSON array of TEXT |
| `extra` | TEXT NOT NULL DEFAULT '{}' | JSON, forward-compat tail |

`worker_grants`:

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `worker_id` | INTEGER NOT NULL REFERENCES `workers(id)` ON DELETE CASCADE | |
| `can_execute` | TEXT NOT NULL | JSON array of TEXT |
| `can_access_read` | TEXT NOT NULL | JSON array of TEXT |
| `can_access_write` | TEXT NOT NULL | JSON array of TEXT |
| `denies` | TEXT NOT NULL | JSON array of TEXT |
| `max_parallel` | TEXT NOT NULL | JSON object mapping op → integer |

`WorkerRepo::register(NewWorker)` creates a `workers` row and emits
`worker.registered`. Subsequent calls (`record_capability`, `record_grant`,
`retire`) each emit their matching event.

No worker process exists in Sprint 1 — these tables are populated by tests
calling `WorkerRepo::register`.

### 7.7 Artifact catalog

`artifact_handles`:

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `media_work_id` | INTEGER NULL REFERENCES `media_works(id)` | |
| `media_variant_id` | INTEGER NULL REFERENCES `media_variants(id)` | |
| `asset_bundle_id` | INTEGER NULL REFERENCES `asset_bundles(id)` | |
| `file_asset_id` | INTEGER NULL REFERENCES `file_assets(id)` | |
| `file_version_id` | INTEGER NULL REFERENCES `file_versions(id)` | |
| `size_bytes` | INTEGER NULL | |
| `checksum` | TEXT NULL | |
| `privacy_class` | TEXT NOT NULL | |
| `durability_class` | TEXT NOT NULL | |
| `allowed_access_modes` | TEXT NOT NULL | JSON array of TEXT |
| `mutability` | TEXT NOT NULL | |
| `source_lineage` | TEXT NULL | JSON; opaque to repo |
| `created_at` | TEXT NOT NULL | |

`artifact_locations`:

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `artifact_handle_id` | INTEGER NOT NULL REFERENCES `artifact_handles(id)` | |
| `kind` | TEXT NOT NULL | `local_path` \| `shared_mount` \| `object_store` \| `staging` \| `backup` |
| `value` | TEXT NOT NULL | |
| `observed_at` | TEXT NOT NULL | |
| `retired_at` | TEXT NULL | |

`artifact_lineage`:

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `parent_artifact_id` | INTEGER NOT NULL REFERENCES `artifact_handles(id)` | |
| `child_artifact_id` | INTEGER NOT NULL REFERENCES `artifact_handles(id)` | |
| `operation` | TEXT NOT NULL | |
| `recorded_at` | TEXT NOT NULL | |

`ArtifactRepo` exposes CRUD + lineage append. Placement scoring and the
resolver remain in the empty `voom-artifact` crate (Sprint 4).

## 8. Durable Identity Model (M2)

Five-layer model, plus evidence and snapshots. The Ingest Identity
Invariants from the architectural spec are enforced at the
`IdentityRepo` layer; the type system makes the alternatives explicit.

### 8.1 `media_works`

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `kind` | TEXT NOT NULL | `movie` \| `episode` \| `personal` \| `unknown` |
| `display_title` | TEXT NOT NULL | |
| `provisional` | INTEGER NOT NULL DEFAULT 1 | 1 = uncertain identity |
| `created_at` | TEXT NOT NULL | |
| `epoch` | INTEGER NOT NULL DEFAULT 0 | |

External IDs (TVDB, TMDB, IMDB, AniDB) attach via `identity_evidence`,
never as columns here.

### 8.2 `media_variants`

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `media_work_id` | INTEGER NOT NULL REFERENCES `media_works(id)` | |
| `label` | TEXT NOT NULL | `hd` \| `4k` \| `theatrical` \| `directors_cut` \| `unknown` \| custom |
| `provisional` | INTEGER NOT NULL DEFAULT 1 | |
| `created_at` | TEXT NOT NULL | |
| `epoch` | INTEGER NOT NULL DEFAULT 0 | |

### 8.3 `asset_bundles` + `asset_bundle_members`

`asset_bundles`:

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `media_variant_id` | INTEGER NOT NULL REFERENCES `media_variants(id)` | |
| `display_name` | TEXT NOT NULL | |
| `created_at` | TEXT NOT NULL | |
| `epoch` | INTEGER NOT NULL DEFAULT 0 | |

`asset_bundle_members`:

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `bundle_id` | INTEGER NOT NULL REFERENCES `asset_bundles(id)` ON DELETE CASCADE | |
| `file_asset_id` | INTEGER NOT NULL REFERENCES `file_assets(id)` | |
| `role` | TEXT NOT NULL | `primary_video` \| `commentary_audio` \| `external_subtitle` \| `poster` \| `nfo` \| `trailer` \| `transcript` \| `thumbnail` \| `report` |

Unique on `(file_asset_id)` — an asset is a member of at most one bundle
at a time. Replacing a primary video swaps the bundle membership row, not
the bundle.

### 8.4 `file_assets`, `file_versions`, `file_locations`

`file_assets`:

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `created_at` | TEXT NOT NULL | |
| `retired_at` | TEXT NULL | |
| `epoch` | INTEGER NOT NULL DEFAULT 0 | |

No path, no hash, no kind — pure identity.

`file_versions`:

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `file_asset_id` | INTEGER NOT NULL REFERENCES `file_assets(id)` | |
| `content_hash` | TEXT NOT NULL | Hex SHA-256 |
| `size_bytes` | INTEGER NOT NULL | |
| `produced_by` | TEXT NOT NULL | `ingest` \| `transcode` \| `remux` \| `restore` \| `external_observed` |
| `produced_from_version_id` | INTEGER NULL REFERENCES `file_versions(id)` | Required for non-`ingest`/non-`external_observed` rows |
| `created_at` | TEXT NOT NULL | |
| `retired_at` | TEXT NULL | |
| `epoch` | INTEGER NOT NULL DEFAULT 0 | |

A new FileVersion under an existing FileAsset is only created by a
host-committed lineage operation that names a prior `produced_from_version_id`
on the same asset. The repo enforces: `produced_by IN ('ingest', 'external_observed')`
allows NULL `produced_from_version_id`; everything else requires it to
reference a `FileVersion` whose `file_asset_id` matches.

`file_locations`:

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `file_version_id` | INTEGER NOT NULL REFERENCES `file_versions(id)` | |
| `kind` | TEXT NOT NULL | `local_path` \| `shared_mount` \| `object_store_key` \| `backup_path` \| `historical` |
| `value` | TEXT NOT NULL | Path or `s3://bucket/key#version` |
| `proof_kind` | TEXT NULL | `file_id_generation` \| `object_version_id` \| NULL |
| `proof_value` | TEXT NULL | |
| `observed_at` | TEXT NOT NULL | |
| `retired_at` | TEXT NULL | |
| `epoch` | INTEGER NOT NULL DEFAULT 0 | |

Multiple `FileLocation`s per `FileVersion` are normal: live primary,
shared mount, alias, historical, backup, ....

### 8.5 `identity_evidence`

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `target_type` | TEXT NOT NULL | `media_work` \| `media_variant` \| `asset_bundle` \| `file_asset` \| `file_version` \| `file_location` |
| `target_id` | INTEGER NOT NULL | |
| `assertion_type` | TEXT NOT NULL | Validated against `voom_events::AssertionKind` |
| `candidate_id` | INTEGER NULL | When the assertion references another row |
| `candidate_value` | TEXT NULL | When the assertion references an external value |
| `provider` | TEXT NOT NULL | |
| `provider_version` | TEXT NOT NULL | |
| `confidence` | REAL NOT NULL | 0.0–1.0 |
| `provenance` | TEXT NOT NULL | JSON |
| `observed_at` | TEXT NOT NULL | |
| `superseded_at` | TEXT NULL | |
| `superseded_by_id` | INTEGER NULL REFERENCES `identity_evidence(id)` | |
| `accepted_at` | TEXT NULL | |
| `accepted_user_id` | TEXT NULL | |
| `accepted_policy_id` | INTEGER NULL | Sprint 3 wires this; nullable for Sprint 1 |
| `pinned_file_version_ids` | TEXT NULL | JSON array; populated at accept-time |
| `pinned_hashes` | TEXT NULL | JSON array |
| `pinned_locations` | TEXT NULL | JSON array of `(kind, value)` pairs |

Assertion kinds defined in `voom-events::AssertionKind`:

```rust
pub enum AssertionKind {
    BelongsToWork, BelongsToVariant, SameAsAsset, DuplicateOfAsset,
    PreferredVariant, UserLabel, ExternalIdMatch, PathRuleMatch,
    HashMatch, RuntimeSimilarityMatch, FrameFingerprintMatch,
    AudioFingerprintMatch,
}
```

Accepted rows are immutable from the repo perspective — no UPDATE method
touches `accepted_*` or `pinned_*` columns after acceptance. Supersession
inserts a **new** evidence row referencing the old one through
`superseded_by_id` and writes `superseded_at` on the original; the
original's pinned values stay frozen.

### 8.6 `media_snapshots`

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `file_version_id` | INTEGER NOT NULL REFERENCES `file_versions(id)` | |
| `probed_by` | INTEGER NULL REFERENCES `workers(id)` | NULL for synthetic/test |
| `probed_at` | TEXT NOT NULL | |
| `payload` | TEXT NOT NULL | JSON — full ffprobe-style snapshot |

Sprint 1 stores opaque JSON. Sprint 5 builds typed accessors when ffprobe
lands.

### 8.7 Ingest Identity Invariants in code

`IdentityRepo::record_discovered_file` is the single entry point for new
file discovery. Its signature makes the three possible outcomes explicit:

```rust
pub struct DiscoveredFile {
    pub location_kind: FileLocationKind,
    pub location_value: String,
    pub content_hash: String,        // hex SHA-256
    pub size_bytes: u64,
    pub observed_at: OffsetDateTime,
    /// Physical-object proof the watcher captured at the new path.
    /// Persisted on the resulting `file_locations` row as
    /// `(proof_kind, proof_value)` so future rename reconciliation can
    /// prove same-physical-object identity against this location.
    /// `None` is back-compat for filesystems without a generation-
    /// stamped file ID and for arrival paths where the watcher could
    /// not capture a proof.
    pub proof: Option<LocationProof>,
}

pub enum LocationProof {
    /// Local filesystem: generation-stamped file ID.
    LocalFileIdGeneration { file_id: u128, generation: u64 },
    /// Object store: immutable per-generation version identity.
    ObjectStoreVersion    { bucket: String, key: String, version_id: String },
}

pub enum AliasProof {
    /// Local filesystem: generation-stamped file ID + prior live location.
    LocalFileIdGeneration {
        file_id: u128,
        generation: u64,
        prior_location_id: FileLocationId,
    },
    /// Object store: immutable generation/version ID of the specific
    /// object generation, plus prior live location.
    ObjectStoreVersion {
        bucket: String,
        key: String,
        version_id: String,
        prior_location_id: FileLocationId,
    },
}

pub struct ObservedBytes {
    pub content_hash: String,        // hex SHA-256
    pub size_bytes: u64,
}

/// Proof required to reconcile a same-physical-object rename. Carries
/// the physical-object identity (so the repo can match it against the
/// retired location's stored proof), the new path, and the caller's
/// assertion that the prior path is gone. The repo cross-checks every
/// field — proof kind, proof bytes, prior-path absence, and the
/// `FileVersion`'s hash/size against `ObservedBytes` — before retiring
/// the prior location. Any mismatch returns `Conflict` and leaves the
/// prior location live.
pub enum RenameProof {
    /// Local filesystem: caller observed the same (file_id,
    /// generation) at the new path, and observed the prior path is
    /// gone. The repo cross-checks against the retired location's
    /// stored proof and the FileVersion's hash/size.
    LocalFileIdGeneration {
        prior_location_id: FileLocationId,
        new_kind: FileLocationKind,
        new_value: String,
        file_id: u128,
        generation: u64,
        prior_path_missing: bool,
    },
    /// Object store: caller observed the same (bucket, key,
    /// version_id) at the new key, and observed the prior key is
    /// gone (or moved). In practice the (bucket, version_id) pin is
    /// the physical-object identity; the key may shift.
    ObjectStoreVersion {
        prior_location_id: FileLocationId,
        new_kind: FileLocationKind,
        new_value: String,
        bucket: String,
        key: String,
        version_id: String,
        prior_key_missing: bool,
    },
}

pub enum IngestOutcome {
    NewFileAsset {
        file_asset_id: FileAssetId,
        file_version_id: FileVersionId,
        file_location_id: FileLocationId,
    },
    AliasAttached {
        file_version_id: FileVersionId,
        new_file_location_id: FileLocationId,
    },
    RenameReconciled {
        file_version_id: FileVersionId,
        retired_location_id: FileLocationId,
        new_file_location_id: FileLocationId,
    },
}

#[async_trait]
pub trait IdentityRepo: Repository {
    async fn record_discovered_file_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        discovered: DiscoveredFile,
        alias_proof: Option<AliasProof>,
    ) -> Result<IngestOutcome, VoomError>;

    async fn reconcile_rename_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        proof: RenameProof,
        observed: ObservedBytes,
        observed_at: OffsetDateTime,
    ) -> Result<RenameReconciledOutcome, VoomError>;

    /* + CRUD on every identity table + evidence accept/supersede */
}
```

Behavior of `record_discovered_file_in_tx`:

- **`alias_proof = None`** → always `NewFileAsset`. If `content_hash`
  matches any existing `FileVersion`, the repo also writes an
  `identity_evidence(hash_match)` row against the existing
  `FileAsset` referencing the new `FileVersion`. Hash matches never
  collapse identity. ETag matches arrive at this code path with no
  alias proof and produce identical behavior. The new `file_locations`
  row carries `discovered.proof`: if `Some(LocalFileIdGeneration { … })`,
  the row's `proof_kind = 'file_id_generation'` and `proof_value`
  serializes `(file_id, generation)`; if
  `Some(ObjectStoreVersion { … })`, `proof_kind = 'object_version_id'`
  and `proof_value` serializes `(bucket, key, version_id)`; if `None`,
  both columns are NULL (back-compat semantics for filesystems without
  a generation-stamped file ID and for arrival paths where the watcher
  could not capture a proof).
- **`alias_proof = Some(LocalFileIdGeneration { … })`** → validate: prior
  location is live (`retired_at IS NULL`), its `proof_kind = 'file_id_generation'`,
  its `proof_value` parses to a `(file_id, generation)` matching what the
  proof carries, and the existing `FileVersion.content_hash` and
  `size_bytes` match `discovered`. On match → `AliasAttached`. On any
  mismatch → `NewFileAsset` + `identity_evidence(path_rule_match)`. Inode
  alone (a `file_id` reuse after delete/recreate without generation match)
  is **not** sufficient — the spec is explicit. On the `AliasAttached`
  outcome the repo persists `discovered.proof` onto the new alias
  `file_locations` row so a later rename reconciliation against this
  new location has a basis to verify. The persisted proof on the new
  alias location must match the alias_proof bytes — a mismatch returns
  `VoomError::Conflict("proof drift on alias attach")` and does not
  insert the row.
- **`alias_proof = Some(ObjectStoreVersion { … })`** → analogous: prior
  location's `proof_kind = 'object_version_id'`, `proof_value` matches the
  `(bucket, key, version_id)` triple, hash and size match. On match →
  `AliasAttached`. On any mismatch → `NewFileAsset` +
  `identity_evidence(path_rule_match)` and (if hash matches an existing
  version) also `identity_evidence(hash_match)`. On the `AliasAttached`
  outcome the repo persists `discovered.proof` onto the new alias
  `file_locations` row under the same matching discipline as the
  local variant; a `(bucket, key, version_id)` mismatch between
  `discovered.proof` and `alias_proof` returns
  `VoomError::Conflict("proof drift on alias attach")` and does not
  insert the row.

The repo never produces `RenameReconciled` from `record_discovered_file`;
that outcome is reserved for `reconcile_rename`. Identity collapse via a
`merge` operation does not exist in Sprint 1.

**Pending-commit lock consultation (alias-attach branch).** The
architectural spec is explicit (`docs/specs/voom-control-plane-design.md`
lines 1038–1043) that while a destructive commit is in progress, new
`FileLocation`s that alias discovery would attach to an in-scope
`FileVersion` are blocked or held. Alias attachment is therefore
**not** exempt from the lock — only external rename/move
reconciliation is exempt (arch spec lines 697–708), because the
physical bytes have moved on disk outside the host's authority and
refusing to record that would leave durable state stale. An
alias-attach is different: the host is recording that bytes the
scanner observed at a new path are the same physical bytes as an
existing `FileVersion`, and that record adds a new `FileLocation` to
the closure of any in-flight destructive commit on that version.
Before inserting the new `file_location` row, the
`AliasAttached` branch consults `commit_intent_scope_members`
against the resolved `file_version_id` (and, transitively, the
parent `file_asset_id` and any bundle membership the version
inherits — the lock query is the same UNION-of-FK-columns shape as
§9.2):

```sql
SELECT csm.commit_intent_id
  FROM commit_intent_scope_members csm
  JOIN commit_intents ci ON ci.id = csm.commit_intent_id
 WHERE ci.state IN ('pending', 'authorized')
   AND (
        csm.scope_version_id = :file_version_id
     OR csm.scope_asset_id   = :parent_file_asset_id
     OR csm.scope_bundle_id  IN (:bundle_ids_for_asset)
       )
 LIMIT 1;
```

If any row returns → `VoomError::BlockedByPendingCommit(...)` →
`ErrorCode::BlockedByPendingCommit` (§12.1). The lock covers both
`pending` and `authorized` states so the architectural recheck
window is never crossed by a fresh alias attach. The `NewFileAsset`
outcome does **not** consult the lock — a newly-discovered file
asset is by definition not in any pre-existing closure. The
`AliasResolver` (used by the gate during Phase A `prepare` and
Phase B `authorize`) is a separate path that resolves cross-host
aliases of bytes the host already knows about; it is what picks up
between-phase closure shifts driven by remote mounts coming online
or object-store probes succeeding on retry. The Phase B authorize
recheck (§9.3.2) observes those resolver-driven shifts and rename
reconciliation-driven shifts, but does **not** need to observe
local alias-attach shifts because the lock prevents them from
landing during the in-flight window.

**No pending-commit lock on rename reconciliation.**
`reconcile_rename_in_tx` is the **one** byte-preserving operation
the architectural spec exempts from the pending-commit lock
(`docs/specs/voom-control-plane-design.md` lines 697–708): "The
`Commit Safety Gate` does not block reconciliation: the physical
move has already happened outside the host's authority, and
refusing to record it would only leave durable state stale." A
rename reconciliation that lands while a destructive commit is
in-flight shifts the closure, and the Phase B `authorize` recheck
(§9.3.2) catches that shift and aborts the commit with
`BlockedByClosureGrew { added_locations, removed_locations }`.

Behavior of `reconcile_rename_in_tx` (M2 form):

1. Bind `prior_location_id` from the `proof` payload; load the row.
   Require `retired_at IS NULL`. Otherwise return `Conflict`.
2. Require the prior location's `proof_kind` matches the
   `RenameProof` variant (`file_id_generation` ↔ `LocalFileIdGeneration`;
   `object_version_id` ↔ `ObjectStoreVersion`). Mismatch → `Conflict`.
3. Parse the prior location's `proof_value`. Require the parsed
   bytes match the caller-supplied proof bytes:
   - `LocalFileIdGeneration`: `(file_id, generation)` must match.
   - `ObjectStoreVersion`: `(bucket, key, version_id)` must match.

   Mismatch → `Conflict`.
4. Require the caller's `prior_path_missing` / `prior_key_missing`
   flag is `true`. The spec's contract is that reconciliation only
   records moves the host has *observed* outside its own authority;
   a caller that did not verify prior-path absence is bypassing the
   architectural invariant. `false` → `Conflict` ("rename requires
   prior path missing").
5. Load the `FileVersion` bound to the prior location. Require
   `observed.content_hash == fv.content_hash` AND
   `observed.size_bytes == fv.size_bytes`. Mismatch → `Conflict`
   ("hash drift during rename" / "size drift during rename") — the
   bytes are no longer the same physical object, so this is a new
   file_asset story, not a rename.
6. Retire the prior location (`retired_at = observed_at`).
7. Insert a new `FileLocation` on the same `FileVersion` with the
   new `kind`, `value`, and the caller-supplied `(proof_kind,
   proof_value)` carried over from the `RenameProof` payload.
8. Emit `file_location.retired_by_move` and
   `file_location.recorded_by_move` events referencing both IDs.
9. Append `identity_evidence(path_rule_match)` observing the new
   location.

Every `Conflict` path leaves the prior location live and the
`FileVersion` untouched. Re-anchoring leases (the M3 extension)
runs only after step 6 succeeds — the lease scope is never updated
against an unproven rename.

M3 extends `reconcile_rename_in_tx` to additionally:

- Find all non-terminal `asset_use_leases` with
  `scope_location_id = prior_location_id`.
- Update their `scope_location_id = new_location_id` and bump `epoch`,
  preserving all other lease state.
- Emit `use_lease.reanchored_by_move` per affected lease (blocking,
  advisory, and manual locks all re-anchor).

The signature stays stable across M2 and M3. Because Sprint 1 has no
concurrent callers, mid-sprint extension is safe.

## 9. Asset Use Leases & Commit Safety Gate (M3)

### 9.1 `asset_use_leases`

```sql
CREATE TABLE asset_use_leases (
    id                  INTEGER PRIMARY KEY,
    kind                TEXT NOT NULL,        -- 'playback'|'scan'|'copy'|'manual_lock'
                                              -- |'external_lock'|'worker_operation'
    scope_asset_id      INTEGER NULL REFERENCES file_assets(id)    ON DELETE RESTRICT,
    scope_bundle_id     INTEGER NULL REFERENCES asset_bundles(id)  ON DELETE RESTRICT,
    scope_version_id    INTEGER NULL REFERENCES file_versions(id)  ON DELETE RESTRICT,
    scope_location_id   INTEGER NULL REFERENCES file_locations(id) ON DELETE RESTRICT,
    issuer_kind         TEXT NOT NULL,        -- 'user'|'control_plane'|'worker'|'external_system'
    issuer_ref          TEXT NOT NULL,        -- user id, subsystem name, worker_id, or external_system_id
    blocking_mode       TEXT NOT NULL,        -- 'blocking'|'advisory'
    ttl_bound           INTEGER NOT NULL,     -- 1 = TTL-bound; 0 = manual lock (explicit-release only)
    acquired_at         TEXT NOT NULL,
    expires_at          TEXT,                 -- NULL only when ttl_bound = 0
    last_heartbeat_at   TEXT,                 -- NULL until first renewal
    clock_source        TEXT NOT NULL,        -- 'control_plane' for Sprint 1
    release_reason      TEXT,                 -- NULL while non-terminal;
                                              -- 'released'|'expired'|'issuer_lost'
                                              -- |'superseded'|'force_released'
    released_at         TEXT,                 -- NULL while non-terminal
    epoch               INTEGER NOT NULL DEFAULT 0,
    CHECK (
        (ttl_bound = 1 AND expires_at IS NOT NULL)
        OR (ttl_bound = 0 AND expires_at IS NULL)
    ),
    CHECK (
        (release_reason IS NULL AND released_at IS NULL)
        OR (release_reason IS NOT NULL AND released_at IS NOT NULL)
    ),
    CHECK (
        (CASE WHEN scope_asset_id    IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN scope_bundle_id   IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN scope_version_id  IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN scope_location_id IS NULL THEN 0 ELSE 1 END)
      = 1
    )
) STRICT;

CREATE INDEX use_leases_by_asset
  ON asset_use_leases (scope_asset_id)    WHERE scope_asset_id    IS NOT NULL AND release_reason IS NULL;
CREATE INDEX use_leases_by_bundle
  ON asset_use_leases (scope_bundle_id)   WHERE scope_bundle_id   IS NOT NULL AND release_reason IS NULL;
CREATE INDEX use_leases_by_version
  ON asset_use_leases (scope_version_id)  WHERE scope_version_id  IS NOT NULL AND release_reason IS NULL;
CREATE INDEX use_leases_by_location
  ON asset_use_leases (scope_location_id) WHERE scope_location_id IS NOT NULL AND release_reason IS NULL;

CREATE INDEX use_leases_by_expiry
  ON asset_use_leases (expires_at) WHERE release_reason IS NULL AND ttl_bound = 1;
```

The three `CHECK` constraints enforce the three invariants the spec is
most explicit about: TTL-bound vs. manual locks differ on `expires_at`
presence; terminal state requires both `release_reason` and `released_at`
together; and exactly one of the four `scope_*_id` columns is non-NULL
(the one-of constraint replaces the polymorphic `scope_type / scope_id`
pair so that FK enforcement is automatic).

At the Rust boundary the scope is a `LeaseScope` enum:

```rust
pub enum LeaseScope {
    Asset(FileAssetId),
    Bundle(BundleId),
    Version(FileVersionId),
    Location(FileLocationId),
}
```

`UseLeaseRepo::acquire_in_tx` translates the enum to the matching FK
column. On an FK violation (`SQLITE_CONSTRAINT_FOREIGNKEY`) the repo
returns `VoomError::NotFound(...)` → `ErrorCode::NotFound`. FK
enforcement alone does **not** cover liveness: soft-deletes leave the
parent row in place with `retired_at IS NOT NULL`. The repo therefore
runs a liveness check (target row exists and `retired_at IS NULL`) in
the same transaction as the insert, before the insert. If the target is
retired the repo returns `VoomError::Conflict(...)` →
`ErrorCode::Conflict` with a message naming the scope.

#### `commit_intents`

M3 also introduces `commit_intents` — a durable journal for the
three-phase destructive-commit protocol (§9.3.1). One row per attempted
destructive commit, recording the closure and evidence the operator
declared at prepare time so that the recovery path can reason about
intents whose callers crashed between `prepare`, `authorize`, and
`finalize`:

```sql
CREATE TABLE commit_intents (
    id                     INTEGER PRIMARY KEY,
    target                 TEXT NOT NULL,    -- JSON CommitTarget
    closure_initial        TEXT NOT NULL,    -- JSON AffectedScopeClosure
    closure_authorized     TEXT,             -- JSON AffectedScopeClosure; set at Phase B success
    accepted_evidence_ids  TEXT NOT NULL,    -- JSON array of evidence IDs
    override_token         TEXT,             -- JSON ForcePathToken | NULL
    state                  TEXT NOT NULL,    -- 'pending' | 'authorized' | 'completed' | 'aborted' | 'recovery_required'
    started_at             TEXT NOT NULL,
    authorized_at          TEXT,
    finalized_at           TEXT,
    aborted_at             TEXT,
    abort_reason           TEXT,             -- 'mutation_failed' | 'closure_grew' | 'fresh_lease' | 'closure_incomplete' | 'stale_evidence' | 'operator_cancel' | ...
    epoch                  INTEGER NOT NULL DEFAULT 0,
    CHECK (
           (state = 'pending'           AND authorized_at IS NULL     AND finalized_at IS NULL     AND aborted_at IS NULL     AND abort_reason IS NULL)
        OR (state = 'authorized'        AND authorized_at IS NOT NULL AND finalized_at IS NULL     AND aborted_at IS NULL     AND abort_reason IS NULL)
        OR (state = 'completed'         AND authorized_at IS NOT NULL AND finalized_at IS NOT NULL AND aborted_at IS NULL     AND abort_reason IS NULL)
        OR (state = 'aborted'           AND finalized_at IS NULL      AND aborted_at IS NOT NULL   AND abort_reason IS NOT NULL)
        OR (state = 'recovery_required' AND authorized_at IS NOT NULL AND finalized_at IS NULL     AND aborted_at IS NULL     AND abort_reason IS NULL)
    ),
    CHECK (override_token IS NULL OR json_valid(override_token))
) STRICT;

CREATE INDEX commit_intents_in_flight
  ON commit_intents (state, started_at) WHERE state IN ('pending', 'authorized');
```

The CHECK uses an exclusive-shape encoding: each non-`aborted` `state`
value owns the full column shape, so contradictory rows (e.g., both
`finalized_at` and `aborted_at` non-null, or a stale `abort_reason`
left over on a `recovery_required` row) are unrepresentable.
`recovery_required` keeps `abort_reason IS NULL` deliberately: the
*reason* the post-mutation trip-wire fired (closure grew vs. fresh
lease vs. lock bypass) is recorded in the corresponding
`commit.aborted_post_mutation` event payload (§9.3.2 Phase C), not on
the intent row, so the reason has a single source of truth. The
`aborted` shape leaves `authorized_at` unconstrained: it is NULL if
Phase A or `abort_destructive_commit` aborted before authorize, and
NOT NULL if the Phase B authorize recheck aborted the intent or the
Phase C finalize was called with `MutationOutcome::NotPerformed`. The
`closure_authorized` column is NULL while the intent is `pending` and
populated once Phase B authorize succeeds; it captures the
"immediately before the irreversible filesystem mutation" closure
that the architectural spec requires (lines 1044–1052 of
`docs/specs/voom-control-plane-design.md`).

The override-token decision is persisted on
`commit_intents.override_token` so that
`authorize_destructive_commit` and the Sprint 5+ recovery worker can
read it inside their own IMMEDIATE transactions without a cross-table
join against the event log. The `commit.intent_recorded` event
payload (`actor`, `reason`, `bypass`) also carries the token for
audit and replay, but the durable column is the source-of-truth for
safety decisions; the event journal is audit. The `json_valid` CHECK
keeps a corrupted token from silently passing the bypass check
downstream.

#### `commit_intent_scope_members`

A `commit_intents` row in `state IN ('pending', 'authorized')` acts as
an application-level reservation: while the row is in the in-flight
window, no new blocking or advisory `asset_use_lease` may be acquired
on its closure (§9.2) **and** no new `FileLocation` may be attached
as an alias of an in-scope `FileVersion` via
`IdentityRepo::record_discovered_file_in_tx`'s `AliasAttached`
branch (§8.7). External rename/move reconciliation
(`IdentityRepo::reconcile_rename_in_tx`) is the one byte-preserving
operation exempt from the lock — the architectural spec at lines
697–708 mandates this exemption because the physical bytes have
already moved on disk outside the host's authority, and refusing to
record the move would leave durable state stale. A rename that
lands during an in-flight commit shifts the closure; the Phase B
`authorize` recheck (§9.3.2) catches the shift and aborts the
commit. The closure is recorded in `commit_intents.closure_initial`
as JSON for audit, but JSON is opaque to SQL and to the
lease-acquire and alias-attach fast paths. `commit_intent_scope_members`
expands the same closure across the four granularities the
architectural spec serializes against, giving the lock-consultation
query a direct equality match by FK column:

```sql
CREATE TABLE commit_intent_scope_members (
    id                INTEGER PRIMARY KEY,
    commit_intent_id  INTEGER NOT NULL REFERENCES commit_intents(id) ON DELETE CASCADE,
    scope_asset_id    INTEGER NULL REFERENCES file_assets(id)    ON DELETE RESTRICT,
    scope_bundle_id   INTEGER NULL REFERENCES asset_bundles(id)  ON DELETE RESTRICT,
    scope_version_id  INTEGER NULL REFERENCES file_versions(id)  ON DELETE RESTRICT,
    scope_location_id INTEGER NULL REFERENCES file_locations(id) ON DELETE RESTRICT,
    CHECK (
        (CASE WHEN scope_asset_id    IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN scope_bundle_id   IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN scope_version_id  IS NULL THEN 0 ELSE 1 END)
      + (CASE WHEN scope_location_id IS NULL THEN 0 ELSE 1 END)
      = 1
    )
) STRICT;

CREATE INDEX commit_intent_scope_members_by_asset
  ON commit_intent_scope_members (scope_asset_id)    WHERE scope_asset_id    IS NOT NULL;
CREATE INDEX commit_intent_scope_members_by_bundle
  ON commit_intent_scope_members (scope_bundle_id)   WHERE scope_bundle_id   IS NOT NULL;
CREATE INDEX commit_intent_scope_members_by_version
  ON commit_intent_scope_members (scope_version_id)  WHERE scope_version_id  IS NOT NULL;
CREATE INDEX commit_intent_scope_members_by_location
  ON commit_intent_scope_members (scope_location_id) WHERE scope_location_id IS NOT NULL;
```

The four `scope_*_id` columns mirror `asset_use_leases` so the
lock-consultation query against a `LeaseScope` is a direct equality
match by column. `ON DELETE CASCADE FROM commit_intents` keeps the FK
relationship clean (Sprint 1 never deletes intent rows; Sprint 5+ may).
The table has no `epoch` column — it is append-only-by-pending-intent
and its rows live and die with the parent `commit_intents` row.

`commit_intent_scope_members` is populated by `prepare_destructive_commit`
inside the Phase A IMMEDIATE transaction (§9.3.2) and is updated by
`authorize_destructive_commit` in the Phase B IMMEDIATE transaction
when the recomputed closure differs from `closure_initial` (rows are
deleted for removed members and inserted for added members, so the
pending-commit lock continues to cover the recomputed closure). The
lock-consultation query in `UseLeaseRepo::acquire_in_tx` (§9.2) and
in `IdentityRepo::record_discovered_file_in_tx`'s `AliasAttached`
branch (§8.7) is the same shape: any match against a row whose
parent intent is in `state IN ('pending', 'authorized')` returns
`VoomError::BlockedByPendingCommit(...)` (§12.1) and the caller's
mutation is rejected before it lands. The lock is **not** consulted
by `IdentityRepo::reconcile_rename_in_tx` — rename reconciliation is
the architecturally-mandated exception (arch spec lines 697–708),
and the Phase B authorize recheck is what catches rename-driven
closure shifts during an in-flight commit. Because all the relevant
entry points run under IMMEDIATE transactions, the SQLite write-lock
serializes them: whichever transaction commits first wins, the
other observes the committed row and rejects with no race window.

### 9.2 `UseLeaseRepo` lifecycle

- `acquire(NewUseLease) -> Result<UseLeaseId>` — IMMEDIATE transaction:
  validate `clock_source = "control_plane"`; for TTL-bound leases require
  a positive TTL; for manual locks require no `expires_at`. Translate the
  caller's `LeaseScope` enum to the matching `scope_*_id` FK column.
  Before insert, run a liveness check in the same transaction: the
  referenced parent row must exist (FK violation →
  `VoomError::NotFound`) and must not be soft-deleted
  (`retired_at IS NULL`; otherwise → `VoomError::Conflict` with a message
  naming the scope). The IMMEDIATE tx serializes against in-flight
  destructive commits on the same scope (see §9.3); if a concurrent
  commit holds the write lock, this acquire blocks (SQLite WAL behavior)
  or returns `Conflict` after busy-timeout. After the liveness check
  and before insert, consult the pending-commit lock against
  `commit_intent_scope_members` (§9.1):

  ```sql
  SELECT csm.commit_intent_id
    FROM commit_intent_scope_members csm
    JOIN commit_intents ci ON ci.id = csm.commit_intent_id
   WHERE ci.state IN ('pending', 'authorized')
     AND (
          (:scope_asset_id    IS NOT NULL AND csm.scope_asset_id    = :scope_asset_id)
       OR (:scope_bundle_id   IS NOT NULL AND csm.scope_bundle_id   = :scope_bundle_id)
       OR (:scope_version_id  IS NOT NULL AND csm.scope_version_id  = :scope_version_id)
       OR (:scope_location_id IS NOT NULL AND csm.scope_location_id = :scope_location_id)
     )
   LIMIT 1;
  ```

  If a row returns, the acquire is rejected with
  `VoomError::BlockedByPendingCommit(...)` →
  `ErrorCode::BlockedByPendingCommit`; the message names the blocking
  `commit_id` and the matched scope. The lock covers the full
  in-flight window — both `pending` (between `prepare` and `authorize`)
  and `authorized` (between `authorize` and `finalize`) — so a fresh
  lease cannot slip in either before the architectural recheck has
  run or after it has run but before the caller's filesystem
  mutation. The check applies to both blocking and advisory leases —
  the architectural spec does not carve out advisory, and a destructive
  commit serializes against every in-scope use-lease acquire. Because
  `prepare_destructive_commit`, `authorize_destructive_commit`, and
  `acquire` all run under IMMEDIATE transactions, the SQLite write-
  lock serializes them: whichever commits first wins, the other
  observes the committed row and rejects. Emits `use_lease.acquired`.
- `heartbeat(lease_id) -> Result<UseLease>` — TTL-bound only. Bumps
  `last_heartbeat_at` and `expires_at = now + ttl`. Manual locks reject
  heartbeat with `ErrorCode::Conflict` (message: "manual locks do not
  heartbeat"). No event emitted in Sprint 1; Sprint 6 daemon may add a
  conditional "recovered after missed beat" event when it owns the
  missed-heartbeat-warning path.
- `release(lease_id, reason)` — ordinary issuer-driven release.
  Accepted reasons: `released`, `superseded`. The caller is the lease
  issuer exercising its own release path. Transitions to terminal,
  sets `released_at = now`, `release_reason = reason`. Emits
  `use_lease.released` (payload carries `release_reason`; no actor
  field because the issuer is the caller). `force_released` and
  `issuer_lost` are **not** accepted reasons here — they require the
  dedicated audited paths below; passing either rejects with
  `VoomError::Config`.
- `force_release(lease_id, actor, reason)` — admin/operator audited
  path for clearing a blocking lease (typical use case: operator
  needs to run a destructive commit and must terminate a stuck
  blocking lease first; see §9.3.3). Accepts both blocking and
  advisory leases, TTL-bound and manual. Transitions the lease to
  terminal with `release_reason = 'force_released'`, `released_at =
  now`. Emits `use_lease.force_released` with payload `{ lease_id,
  actor, reason }`. The `actor` and `reason` are mandatory audit
  fields. Sprint 1 does not enforce permissions on this path
  (operator workflow is test-driven); Sprint 9 wires policy-based
  authorization. The method signature stays stable across that
  addition.
- `expire_due(now) -> Result<ExpireReport>` — bulk: find non-terminal
  TTL-bound leases with `expires_at < now`. Per row: transition to
  `release_reason = 'expired'`, `released_at = now`. Manual locks are
  filtered out (`ttl_bound = 0`). Emits `use_lease.expired` per row.
- `recover_stale_issuer(lease_id, actor, reason) -> Result<()>` —
  manual-lock-specific path. Caller passes `actor` and `reason`.
  Transitions lease to `release_reason = 'issuer_lost'`, `released_at
  = now`. Emits `use_lease.recovered_stale_issuer` with `{ actor,
  reason }` payload. This is the **only** path that sets
  `release_reason = 'issuer_lost'`; `release(lease_id, reason)`
  rejects `issuer_lost` with `VoomError::Config` so the audit
  trail for stale-issuer recovery always carries `{actor, reason}`
  on the `use_lease.recovered_stale_issuer` event.
- `reanchor_on_move(retired_location_id, new_location_id, now) -> Result<ReanchorReport>` —
  invoked by `IdentityRepo::reconcile_rename_in_tx` (M3 extension). Finds
  all non-terminal leases on `scope_location_id = retired_location_id`,
  updates `scope_location_id = new_location_id` and bumps `epoch`. Because
  the scope is now a single column, the update is a straightforward SQL
  statement with no JSON unpacking:
  ```sql
  UPDATE asset_use_leases
     SET scope_location_id = :new_location_id,
         epoch = epoch + 1
   WHERE scope_location_id = :retired_location_id
     AND release_reason IS NULL;
  ```
  Other fields preserved: `lease_id`, `issuer_kind`, `issuer_ref`,
  `acquired_at`, `expires_at`, `last_heartbeat_at`, `blocking_mode`,
  `ttl_bound`. Emits `use_lease.reanchored_by_move` per affected lease —
  blocking, advisory, and manual locks all re-anchor (the spec is
  explicit). Returns the list of re-anchored IDs so the caller can
  include them in its own event.

### 9.3 Commit Safety Gate

The gate is a host-side helper in `voom-store::repo::commit_safety_gate`.
Sprint 1's only callers are tests; Sprint 5+ adds real callers (the
host-side commit transaction for transcode/remux/restore/delete/archive).
The gate is the single source of truth for the four abort errors the
architectural spec names.

### 9.3.1 API

The gate exposes a three-phase prepare / authorize / finalize / abort
protocol. The architectural spec's host-owned-commit invariant requires
that "a worker crash does not leave the control plane believing a
final mutation succeeded"; a single transaction wrapped around a
caller-supplied filesystem mutation cannot honor this, because a DB
rollback after the filesystem bytes are already changed leaves the
durable state lagging behind reality. The architectural spec at lines
1044–1052 of `docs/specs/voom-control-plane-design.md` adds a second,
stricter requirement: "Immediately before the irreversible filesystem
mutation, the host recomputes the affected-scope closure under the
same isolation … and re-evaluates blocking leases against the
recomputed closure." `authorize_destructive_commit` is that explicit
"immediately before" recheck: the caller's filesystem mutation runs
**between** `authorize` and `finalize`, so the recheck observes
exactly the state the destructive mutation is about to act on. The
three-phase API journals a durable `commit_intents` row at prepare
time, transitions it to `authorized` once the recheck passes, and
transitions it to `completed` after the caller's mutation lands and
finalize commits the durable identity change — so the recovery path
(Sprint 5+) can reason about callers that crashed in any of the
three phase windows.

```rust
pub struct DestructiveCommit {
    pub target: CommitTarget,
    pub accepted_evidence_ids: Vec<EvidenceId>,
    pub override_token: Option<ForcePathToken>,
}

pub enum CommitTarget {
    DeleteFileLocation(FileLocationId),
    DeleteFileVersion(FileVersionId),
    ArchiveFileVersion(FileVersionId),
    ReplaceFileLocation { retired: FileLocationId, new: FileLocationProposal },
    MoveFileLocation     { retired: FileLocationId, new: FileLocationProposal },
    // `ArchiveBundle(BundleId)` and `DeleteBundle(BundleId)` deferred
    // to Sprint 5: the `asset_bundles` schema does not carry the
    // soft-delete/archive columns those targets need, and no Sprint 1
    // worker initiates a bundle commit. Adding them later is purely
    // additive (a new enum variant + new `BundleRepo` methods + a
    // schema migration); the three-phase gate protocol is unchanged.
}

pub struct AffectedScopeClosure {
    pub file_assets:    Vec<FileAssetId>,
    pub file_versions:  Vec<FileVersionId>,
    pub file_locations: Vec<FileLocationId>,    // hardlinks, bind-mounts, shared-mounts, object-store aliases
    pub bundles:        Vec<BundleId>,
    pub resolution_warnings: Vec<ClosureWarning>,
}

pub struct CommitIntent {
    pub commit_id: CommitId,
    pub closure_initial: AffectedScopeClosure,
    pub evaluated_lease_ids: Vec<UseLeaseId>,
    pub revalidated_evidence: Vec<EvidenceRevalidationResult>,
    pub epoch: u64,
}

/// Returned by `authorize_destructive_commit` on success. Carries the
/// recomputed closure, the lease IDs evaluated against it, and the
/// evidence revalidation results — all snapshotted at the moment the
/// architectural "immediately before the irreversible filesystem
/// mutation" recheck passed. The caller must pass this permit to
/// `finalize_destructive_commit`; the permit's `epoch` is checked
/// against the durable `commit_intents` row so a stale permit
/// (e.g., from a previously-aborted attempt) cannot drive a later
/// finalize.
pub struct CommitPermit {
    pub commit_id: CommitId,
    pub authorized_at: OffsetDateTime,
    pub closure_authorized: AffectedScopeClosure,
    pub evaluated_lease_ids: Vec<UseLeaseId>,
    pub revalidated_evidence: Vec<EvidenceRevalidationResult>,
    pub epoch: u64,
}

pub enum MutationOutcome {
    /// Caller performed the filesystem mutation and it is durable on
    /// disk. Optionally carries the observed post-mutation closure if
    /// the caller's mutation touched aliases the gate could not see.
    Applied { observed: Option<AffectedScopeClosure> },
    /// Caller obtained a permit but decided not to mutate.
    /// `finalize_destructive_commit` transitions the intent to
    /// `aborted` with reason `'operator_cancel'`, emits
    /// `commit.aborted_pre_mutation`, and returns
    /// `Ok(CommitGateOutcome { result:
    /// CommitGateResult::CancelledAfterAuthorize, ... })`. This is the
    /// **only** sanctioned post-authorize pre-success termination path;
    /// the `abort_destructive_commit` entry point is pending-only and
    /// rejects `state = 'authorized'` with `Conflict` (§9.3.2). Recovery
    /// callers must read the durable `commit_intents` row to learn the
    /// terminal state; an `Ok(CancelledAfterAuthorize)` is idempotent
    /// against retry-by-row-inspection because a second finalize on a
    /// consumed permit hits the Phase C step-1 state/epoch check and
    /// returns `Err(Conflict)` cleanly.
    NotPerformed,
}

pub struct CommitGateOutcome {
    pub commit_id: CommitId,
    pub closure_initial:    AffectedScopeClosure,
    pub closure_authorized: AffectedScopeClosure,
    pub closure_final:      AffectedScopeClosure,
    pub evaluated_lease_ids: Vec<UseLeaseId>,
    pub revalidated_evidence: Vec<EvidenceRevalidationResult>,
    pub result: CommitGateResult,
}

pub enum CommitGateResult {
    Allowed,
    /// `finalize_destructive_commit` was called with
    /// `MutationOutcome::NotPerformed`. The intent is durably
    /// transitioned to `aborted` with `abort_reason =
    /// 'operator_cancel'`; emits `commit.aborted_pre_mutation` with
    /// `payload.prior_state = 'authorized'`. This is a successful
    /// cancellation — callers treat it as an Ok outcome, not an
    /// error. Distinct from `Allowed` (commit did not happen).
    CancelledAfterAuthorize,
    BlockedByUseLease           { lease_id: UseLeaseId, lease_scope: LeaseScope },
    BlockedByStaleEvidence      { evidence_id: EvidenceId, drift: EvidenceDrift },
    BlockedByClosureIncomplete  { reason: ClosureFailure, unreachable: Vec<ClosureWarning> },
    /// Closure delta detected against `closure_initial`. Returned in
    /// two places:
    /// 1. `authorize_destructive_commit` (the architectural
    ///    "immediately before" recheck): the recomputed closure
    ///    differs from `closure_initial` — either because an alias
    ///    resolver discovered new locations, because external rename
    ///    reconciliation retired one location and recorded another,
    ///    or both. This is the primary detection path.
    /// 2. `finalize_destructive_commit` defensive trip-wire: under
    ///    normal flow the pending-commit lock and the authorize
    ///    recheck make this branch unreachable; firing it indicates
    ///    a lock-bypass or a resolver escape between authorize and
    ///    finalize.
    ///
    /// Carries the delta across all four granularities. Sets are
    /// disjoint by construction: an ID appears in `added_*` if it
    /// is in `closure_authorized`/`closure_final` but not in
    /// `closure_initial`, and in `removed_*` if it is in
    /// `closure_initial` but not in `closure_authorized`/`closure_final`.
    /// External rename reconciliation typically produces a non-empty
    /// `removed_locations` (the retired prior location) and a
    /// non-empty `added_locations` (the new location); alias
    /// discovery typically produces only non-empty `added_*` sets.
    BlockedByClosureGrew {
        added_assets:      Vec<FileAssetId>,
        added_bundles:     Vec<BundleId>,
        added_versions:    Vec<FileVersionId>,
        added_locations:   Vec<FileLocationId>,
        removed_assets:    Vec<FileAssetId>,
        removed_bundles:   Vec<BundleId>,
        removed_versions:  Vec<FileVersionId>,
        removed_locations: Vec<FileLocationId>,
    },
}

pub enum CommitIntentState {
    Pending,
    Authorized,
    Completed,
    Aborted,
    RecoveryRequired,
}

/// Inspection record for an in-flight destructive commit. Covers
/// both `pending` and `authorized` states — the lifecycle name stays
/// `PendingCommitIntent` for continuity with the CLI surface
/// (`voom commit-intent list`), but the `state` field on the record
/// distinguishes the two phase windows for callers that need it.
/// `list_pending_commit_intents` returns only `pending` and
/// `authorized` records; terminal states (`completed`, `aborted`,
/// `recovery_required`) are read via the `voom commit-intent list
/// --state <terminal>` CLI path against the same row store.
pub struct PendingCommitIntent {
    pub commit_id: CommitId,
    pub target: CommitTarget,
    pub state: CommitIntentState,           // 'pending' | 'authorized'
    pub closure_initial: AffectedScopeClosure,
    pub closure_authorized: Option<AffectedScopeClosure>,  // Some when state == 'authorized'
    pub accepted_evidence_ids: Vec<EvidenceId>,
    pub started_at: OffsetDateTime,
    pub authorized_at: Option<OffsetDateTime>,
}

pub enum AbortReason {
    OperatorCancel,
    MutationFailed,
    ClosureGrew,
    ClosureIncomplete,
    FreshLease,
    StaleEvidence,
    Other(String),
}

pub async fn prepare_destructive_commit(
    pool: &SqlitePool,
    alias_resolver: &dyn AliasResolver,
    event_repo: &dyn EventRepo,
    input: DestructiveCommit,
) -> Result<CommitIntent, VoomError>;

/// Architectural "immediately before the irreversible filesystem
/// mutation" recheck. One IMMEDIATE transaction. Recomputes the
/// closure against current DB + `AliasResolver`, re-evaluates blocking
/// leases against the recomputed closure, and re-validates accepted
/// evidence. On success transitions the intent to `authorized` and
/// returns a `CommitPermit` the caller must hand back to
/// `finalize_destructive_commit`. On any check failure transitions
/// the intent to `aborted` with the matching `abort_reason` and
/// returns the corresponding `Blocked*` error — the caller must not
/// proceed with the filesystem mutation.
pub async fn authorize_destructive_commit(
    pool: &SqlitePool,
    alias_resolver: &dyn AliasResolver,
    event_repo: &dyn EventRepo,
    commit_id: CommitId,
) -> Result<CommitPermit, VoomError>;

/// Called after the caller's filesystem mutation is durable on disk
/// (or after the caller decided not to mutate; see
/// `MutationOutcome::NotPerformed`). The permit is consumed by value
/// to discourage stale-permit reuse; the durable `commit_intents`
/// row's `epoch` is still checked inside the transaction so a
/// concurrent abort racing the finalize is caught. Takes the same
/// `&dyn AliasResolver` as `prepare` and `authorize` because Phase C
/// recomputes `closure_final` against current state for the
/// defensive trip-wire (§9.3.2). Takes `&dyn IdentityRepo` for the
/// durable identity mutation that every Sprint 1 `CommitTarget`
/// variant resolves to (see the trimmed `CommitTarget` enum
/// above — `ArchiveBundle`/`DeleteBundle` are deferred to Sprint 5).
pub async fn finalize_destructive_commit(
    pool: &SqlitePool,
    alias_resolver: &dyn AliasResolver,
    event_repo: &dyn EventRepo,
    identity_repo: &dyn IdentityRepo,
    permit: CommitPermit,
    outcome: MutationOutcome,
) -> Result<CommitGateOutcome, VoomError>;

/// Aborts an intent in `state = 'pending'` only. After `authorize`,
/// the only valid pre-success termination path is
/// `finalize_destructive_commit(_, _, _, permit,
/// MutationOutcome::NotPerformed)` — the caller must hold a
/// `CommitPermit` and the epoch check inside `finalize` is what
/// prevents a stale permit / concurrent abort race from desyncing
/// durable state from filesystem state. Recovery of a stuck
/// `authorized` intent (caller crashed after authorize) is the
/// Sprint 5+ recovery worker's job; it calls `finalize` with the
/// appropriate `MutationOutcome` based on filesystem inspection.
pub async fn abort_destructive_commit(
    pool: &SqlitePool,
    event_repo: &dyn EventRepo,
    commit_id: CommitId,
    reason: AbortReason,
) -> Result<(), VoomError>;

/// Lists in-flight intents (`state IN ('pending', 'authorized')`).
/// The name stays `pending` for CLI continuity; callers that need to
/// distinguish phase windows inspect the `state` field on each
/// returned record.
pub async fn list_pending_commit_intents(
    pool: &SqlitePool,
    older_than: Option<OffsetDateTime>,
) -> Result<Vec<PendingCommitIntent>, VoomError>;
```

The caller's filesystem mutation runs **between** `authorize` and
`finalize`, outside any DB transaction. Mutations must be idempotent
or staged so that crash-and-retry yields the same end state — the
architectural spec's staged-artifact + rollback-metadata requirements
apply unchanged. `finalize_destructive_commit` takes
`&dyn IdentityRepo` because the durable identity mutation that gives
the closure check its meaning (e.g.,
`IdentityRepo::retire_file_location_in_tx` for a `DeleteFileLocation`
target) runs inside the same finalize transaction. The window between
`prepare` and `authorize` is short and host-local — typically a single
async tick while the caller marshals the call — and exists so the
architectural "immediately before" recheck has a well-defined recheck
point distinct from the initial gate evaluation.

`prepare_destructive_commit` is the initial writer of
`commit_intent_scope_members` (§9.1): inside its Phase A IMMEDIATE
transaction it inserts one `commit_intents` row plus one
`commit_intent_scope_members` row per element of `closure_initial`,
expanded across all four granularities. `authorize_destructive_commit`
is the second writer: if the Phase B recheck succeeds with a closure
that differs from `closure_initial`, it deletes
`commit_intent_scope_members` rows for removed members and inserts
rows for added members, so the lock keeps covering the recomputed
closure until the intent transitions out of the in-flight window.
Those rows are the durable backing-store for the pending-commit lock
that `UseLeaseRepo::acquire` (§9.2) consults before issuing a new
lease. The lock is an implementation detail behind the gate — no API
signature changes for `UseLeaseRepo` or `IdentityRepo`, no new field
on `CommitIntent` beyond the `epoch` echoed onto `CommitPermit`. The
`commit.intent_recorded` and `commit.authorized` event payloads carry
`closure_initial` and `closure_authorized` respectively, which are the
source of truth for the `scope_members` rows, so audit can reconstruct
the lock from events without a separate field. The error
`BlockedByPendingCommit` (§12.1) shows up on the existing
`Result<_, VoomError>` returns of `UseLeaseRepo::acquire` and
`IdentityRepo::record_discovered_file_in_tx`'s `AliasAttached`
branch; structured detail (which `commit_intent_scope_members` row
matched) lives on a `BlockedByPendingCommitDetail` struct in
`voom-store::repo::commit_safety_gate`, parallel to the existing
`CommitGateResult`, `LeaseScope`, `EvidenceDrift`, `ClosureWarning`
types, for Sprint 9 report consumers.

### 9.3.2 Algorithm

The gate runs in three phases — `prepare` (Phase A), `authorize`
(Phase B), and `finalize` (Phase C) — plus a separate
`abort_destructive_commit` entry point that terminates a
`state = 'pending'` intent (only) before authorize. Each phase is
one IMMEDIATE transaction. Phase B is the architectural "immediately
before the irreversible filesystem mutation" recheck
(`docs/specs/voom-control-plane-design.md` lines 1044–1052) and is
the primary safety gate; Phase C's recheck is a defensive trip-wire
that catches lock-bypass and resolver-escape paths the lock could
not. Once an intent reaches `state = 'authorized'`, the only
sanctioned pre-success termination path is
`finalize_destructive_commit(_, _, _, permit,
MutationOutcome::NotPerformed)` — `abort_destructive_commit` rejects
authorized intents with `Conflict` (§9.3.2 `abort_destructive_commit`
algorithm, §9.3.2 Phase C `NotPerformed` branch).

#### Phase A — `prepare_destructive_commit`

One IMMEDIATE transaction. SQLite WAL serializes writers; this is the
spec's serialization point.

1. Compute `closure_initial`:
   - Walk target → `FileVersion`(s) → live `FileLocation`s on those
     versions (SQL).
   - Ask the `AliasResolver` for additional locations representing the
     same physical bytes (hardlinks, bind-mounts, shared mounts,
     object-store aliases).
   - Add the `AssetBundle`(s) of the affected `FileAsset`(s).
   - If any `AliasResolver` call fails or returns
     `AliasResolutionError::Unreachable`, record a `ClosureWarning` and
     surface `BlockedByClosureIncomplete` unless `input.override_token`
     is present and grants `closure_incomplete` (see §9.3.3). Emit
     `commit.aborted_by_closure_incomplete` with
     `payload.phase = 'prepare'` (Phase A abort, see §9.3.5);
     return `BlockedByClosureIncomplete`.
2. Evaluate every blocking `asset_use_lease` against `closure_initial`,
   using the per-FK indexes from §9.1. The query is a UNION over the
   four `scope_*_id` columns, filtered to the closure's IDs in each
   column and to `release_reason IS NULL`:
   ```sql
   SELECT id FROM asset_use_leases
    WHERE release_reason IS NULL AND blocking_mode = 'blocking'
      AND (
            scope_asset_id    IN (?, ?, ...)        -- closure.file_assets
         OR scope_bundle_id   IN (?, ?, ...)        -- closure.bundles
         OR scope_version_id  IN (?, ?, ...)        -- closure.file_versions
         OR scope_location_id IN (?, ?, ...)        -- closure.file_locations
          );
   ```
   Terminal leases don't count (`release_reason IS NOT NULL`).
   TTL-bound leases past `expires_at` don't count, regardless of
   whether cleanup has run yet. Manual locks always count until
   terminal. Advisory leases never block. If a fresh blocking lease
   overlaps → `BlockedByUseLease`; emit `commit.aborted_by_use_lease`
   with `payload.phase = 'prepare'` (Phase A abort); return. This
   check has **no force bypass** (§9.3.3).
3. Revalidate every accepted evidence row in `input.accepted_evidence_ids`:
   compare `pinned_file_version_ids` against current `FileVersion` IDs
   of the scope, `pinned_hashes` against current `content_hash` values,
   and `pinned_locations` against current live locations. Any drift →
   `BlockedByStaleEvidence`; emit `commit.aborted_by_stale_evidence`
   with `payload.phase = 'prepare'` (Phase A abort); return.
   Accepted-evidence rows are not rewritten — drift forces the caller
   to re-collect and re-accept. **The force path never bypasses
   evidence revalidation.**
4. Insert a `commit_intents` row with `state = 'pending'`,
   `target = input.target`, `closure_initial`, `accepted_evidence_ids`,
   `override_token = <serialized ForcePathToken | NULL>` (§9.1), and
   `started_at = now`. Inside the same IMMEDIATE transaction, expand
   `closure_initial` into `commit_intent_scope_members` rows so the
   pending-commit lock (§9.1) is durable before the transaction commits:
   ```text
   For each asset_id    in closure_initial.file_assets:    insert (commit_intent_id, scope_asset_id    = asset_id)
   For each bundle_id   in closure_initial.bundles:        insert (commit_intent_id, scope_bundle_id   = bundle_id)
   For each version_id  in closure_initial.file_versions:  insert (commit_intent_id, scope_version_id  = version_id)
   For each location_id in closure_initial.file_locations: insert (commit_intent_id, scope_location_id = location_id)
   ```
   The expansion across all four granularities is what gives the
   lock-consultation query its precision: an acquire at any
   granularity (asset, bundle, version, or location) matches a row of
   the same type if Phase A walked that granularity. Write
   `commit.intent_recorded` with payload
   `{ commit_id, target, closure_initial, evaluated_lease_ids,
   revalidated_evidence, override_token: { actor, reason, bypass }
   | null }`. COMMIT. Once the transaction commits, the pending-commit
   lock is live: from this point until the intent transitions out of
   the in-flight window (`pending` or `authorized`), no new
   `UseLeaseRepo::acquire` on the closure can succeed (§9.2) and no
   new `IdentityRepo::record_discovered_file_in_tx` `AliasAttached`
   can succeed against an in-closure `FileVersion` (§8.7). The lock
   does **not** apply to `IdentityRepo::reconcile_rename_in_tx` —
   rename reconciliation is the architecturally-exempt
   observation-of-reality path (arch spec lines 697–708), and
   rename-driven closure shifts are caught by the Phase B authorize
   recheck (§9.3.2 Phase B).
5. Return `CommitIntent`. The caller's next step is to call
   `authorize_destructive_commit` to obtain a `CommitPermit` before
   performing any filesystem mutation. No filesystem mutation is
   permitted between Phase A and Phase B.

#### Phase B — `authorize_destructive_commit`

Called immediately before the caller intends to perform the filesystem
mutation. This is the architectural "immediately before the
irreversible filesystem mutation" recheck
(`docs/specs/voom-control-plane-design.md` lines 1044–1052) — the
single point at which the gate observes the state the destructive
mutation is about to act on. One IMMEDIATE transaction.

1. Read the `commit_intents` row for `commit_id`; require
   `state = 'pending'`. The SELECT returns `state`,
   `closure_initial`, `accepted_evidence_ids`, `epoch`, **and
   `override_token`** in a single round-trip so every safety decision
   below evaluates against the durable column rather than parsing the
   event journal. Missing row, terminal state, or `epoch` mismatch →
   return `Conflict` without writing.
2. Recompute `closure_authorized` against the current DB state and
   the `AliasResolver`, following the same walk as Phase A step 1. If
   any `AliasResolver` call fails or returns
   `AliasResolutionError::Unreachable`, surface
   `BlockedByClosureIncomplete` unless the parsed
   `override_token` read in step 1 granted `closure_incomplete` (the
   bypass is honored through to authorize so the operator does not
   have to re-prepare; see §9.3.3). On
   abort: emit `commit.aborted_by_closure_incomplete` with
   `payload.phase = 'authorize'`, transition the intent to
   `state = 'aborted'`, `authorized_at = NULL`, `aborted_at = now`,
   `abort_reason = 'closure_incomplete'`. COMMIT. Return
   `BlockedByClosureIncomplete`.
3. Compute the delta between `closure_authorized` and
   `closure_initial` across all four granularities. **Any non-empty
   delta** — members added (e.g., a newly-discovered alias), members
   removed (e.g., an external rename retired the prior location and
   recorded a new one), or both — aborts the commit: the safety
   properties of `closure_initial` no longer cover what is actually
   on disk. Emit `commit.aborted_by_closure_grew` with payload
   `{ commit_id, phase: 'authorize', closure_initial,
   closure_authorized, added_*, removed_* }`. Transition the intent
   to `state = 'aborted'`, `authorized_at = NULL`, `aborted_at = now`,
   `abort_reason = 'closure_grew'`. COMMIT. Return
   `CommitGateResult::BlockedByClosureGrew { added_*, removed_* }`.
   The variant name keeps the architectural-spec terminology
   ("grown") but represents any closure delta — additions, removals,
   or both.
4. Re-evaluate the blocking-lease query from Phase A step 2 against
   `closure_authorized`. With the pending-commit lock covering
   `state IN ('pending', 'authorized')` (§9.2), fresh leases on the
   closure cannot be acquired through `UseLeaseRepo`; firing this
   check therefore indicates either a lease that landed via a
   lock-bypass path or a closure shift that pulled in a member that
   already had a blocking lease before prepare. On match: emit
   `commit.aborted_by_use_lease` with `payload.phase = 'authorize'`,
   transition the intent to `state = 'aborted'`, `authorized_at = NULL`,
   `aborted_at = now`, `abort_reason = 'fresh_lease'`. COMMIT. Return
   `BlockedByUseLease`. This check has **no force bypass** (§9.3.3).
5. Re-validate every accepted evidence row in
   `commit_intents.accepted_evidence_ids` against current state, the
   same way Phase A step 3 does. Any drift: emit
   `commit.aborted_by_stale_evidence` with `payload.phase = 'authorize'`,
   transition the intent to `state = 'aborted'`,
   `authorized_at = NULL`, `aborted_at = now`,
   `abort_reason = 'stale_evidence'`. COMMIT. Return
   `BlockedByStaleEvidence`. **The force path never bypasses evidence
   revalidation**, here or in Phase A.
6. All checks pass. Reconcile `commit_intent_scope_members` with
   `closure_authorized`: delete rows whose member is in
   `closure_initial` but not in `closure_authorized`, and insert rows
   for members in `closure_authorized` but not in `closure_initial`,
   across all four granularities. Update the `commit_intents` row:
   set `state = 'authorized'`, `closure_authorized = <recomputed>`,
   `authorized_at = now`, bump `epoch`. Emit `commit.authorized` with
   payload `{ commit_id, closure_initial, closure_authorized,
   evaluated_lease_ids, revalidated_evidence }`. COMMIT. Return
   `CommitPermit { commit_id, authorized_at, closure_authorized,
   evaluated_lease_ids, revalidated_evidence, epoch }`. The pending-
   commit lock continues to cover the closure through the
   `authorized` state until Phase C resolves the intent.

The caller now performs the filesystem mutation outside any DB
transaction. Mutations must be idempotent or staged so that
crash-and-retry yields the same end state — the architectural spec's
staged-artifact + rollback-metadata requirements apply unchanged.

#### Phase C — `finalize_destructive_commit`

Called once the caller's filesystem mutation is durable on the
filesystem (or the caller has decided not to mutate; see
`MutationOutcome::NotPerformed`). One IMMEDIATE transaction.

1. Read the `commit_intents` row for `permit.commit_id`; require
   `state = 'authorized'` **and** `epoch == permit.epoch`. Missing
   row, wrong state, or epoch mismatch → return `Conflict` without
   writing. The epoch check rejects stale permits (e.g., a permit
   left over from a previously-aborted authorize attempt against the
   same `commit_id`, or a concurrent `abort_destructive_commit` that
   bumped the row underneath the permit holder).
2. If `outcome == MutationOutcome::NotPerformed`: transition the
   intent to `state = 'aborted'`, `aborted_at = now`,
   `abort_reason = 'operator_cancel'` (the `authorized_at` value is
   preserved per the §9.1 CHECK, which leaves `authorized_at`
   unconstrained on `aborted`). Bump `epoch`. Emit
   `commit.aborted_pre_mutation` with
   `payload.prior_state = 'authorized'` so audit can distinguish
   "aborted before authorize" (handled by `abort_destructive_commit`,
   `prior_state = 'pending'`) from "authorized but caller chose not
   to mutate" (handled here, `prior_state = 'authorized'`). COMMIT.

   Return `Ok(CommitGateOutcome { commit_id,
   closure_initial: <from intent row>,
   closure_authorized: permit.closure_authorized,
   closure_final: permit.closure_authorized,  // no FS mutation,
                                              // no Phase C recheck
   evaluated_lease_ids: permit.evaluated_lease_ids,
   revalidated_evidence: permit.revalidated_evidence,
   result: CommitGateResult::CancelledAfterAuthorize })`.

   `closure_final` carries the authorized closure unchanged because
   no filesystem mutation was applied and the Phase C defensive
   trip-wire is skipped on the `NotPerformed` branch. The
   `CancelledAfterAuthorize` result is distinct from `Allowed`: the
   durable identity mutation did not run, and consumers must not
   treat this as a completed commit.

   The `NotPerformed` branch is the **only** sanctioned way to abort
   an intent that has reached `authorized` without applying the
   durable mutation. It is gated by the `CommitPermit` and the
   in-transaction epoch check, both of which prove the caller is the
   rightful holder of the authorize decision. Callers that obtained
   a permit and then decided not to mutate (whether because the
   operator changed their mind or because the filesystem-mutation
   step failed without producing partial on-disk state) **must**
   route through `finalize(permit, NotPerformed)`;
   `abort_destructive_commit` rejects `state = 'authorized'` with
   `Conflict` precisely so this path cannot be bypassed.
   `Err(Conflict)` is reserved for cases where no state transition
   was applied (the Phase C step-1 wrong-state/epoch path); a
   successful cancellation always returns
   `Ok(CancelledAfterAuthorize)`.
3. Otherwise (`outcome == MutationOutcome::Applied { observed }`):
   defensive trip-wire — recompute `closure_final` against the
   current DB state and the `AliasResolver`, and re-evaluate the
   blocking-lease query against `closure_final`. With the pending-
   commit lock covering both `pending` and `authorized` states
   (§9.1, §9.2), and with Phase B's recheck having just observed
   the closure, both subchecks should be empty under normal
   operation. Firing the trip-wire indicates a lock-bypass or
   resolver escape between Phase B commit and Phase C start. Known
   escape paths:

   - A bug in `UseLeaseRepo::acquire_in_tx`'s lock-consultation
     logic.
   - An external SQL writer that bypassed the repos (e.g., a manual
     `INSERT INTO asset_use_leases` run against the DB file).
   - An `AliasResolver` that returned a smaller closure for Phase B
     but newly discovers an alias by Phase C (e.g., a remote mount
     came online between phases, an object-store probe succeeded on
     retry).

   The two subchecks may fire independently or together. The
   `commit.aborted_post_mutation` event payload is uniform across
   all trip-wire firings — it always carries both the closure delta
   (vs. `closure_authorized`) and the fresh-lease list, with empty
   arrays for the dimension that didn't escape — so the durable
   audit record preserves every escape the gate observed:

   ```text
   payload = {
       commit_id,
       reason,                            -- 'closure_grew' | 'fresh_lease' | 'closure_grew_and_fresh_lease'
       escape,                            -- which trip-wire path the gate suspects
       closure_initial,
       closure_authorized,
       closure_final,
       added_assets,      removed_assets,      -- possibly empty
       added_bundles,     removed_bundles,     -- possibly empty
       added_versions,    removed_versions,    -- possibly empty
       added_locations,   removed_locations,   -- possibly empty
       fresh_lease_ids,                        -- possibly empty
   }
   ```

   The two subcheck results map to `CommitGateResult` as follows:

   - **Closure grew or shifted** (delta non-empty vs.
     `closure_authorized`, no fresh lease): emit
     `commit.aborted_post_mutation` with `reason='closure_grew'`,
     transition the intent to `state = 'recovery_required'` (leaving
     `finalized_at`, `aborted_at`, and `abort_reason` all NULL per
     the §9.1 CHECK), do **not** apply the durable mutation, COMMIT,
     return `CommitGateResult::BlockedByClosureGrew { added_*,
     removed_* }`.
   - **Fresh blocking lease** (delta empty, but a blocking lease now
     covers `closure_final` whose ID is not in
     `permit.evaluated_lease_ids`): emit
     `commit.aborted_post_mutation` with `reason='fresh_lease'`,
     transition the intent to `state = 'recovery_required'`, do
     **not** apply the durable mutation, COMMIT, return
     `CommitGateResult::BlockedByUseLease { lease_id, lease_scope }`
     naming the first such lease (deterministic by `lease_id`).
   - **Both fire**: emit one `commit.aborted_post_mutation` event
     with `reason='closure_grew_and_fresh_lease'`, **both** populated
     `added_*`/`removed_*` arrays **and** populated
     `fresh_lease_ids` — so the recovery worker and audit see every
     escape, not just one. The intent transition is the same. Return
     `CommitGateResult::BlockedByClosureGrew { added_*, removed_* }`
     (closure shift is the more fundamental escape — the fresh-lease
     check would have been re-evaluated against the wrong baseline
     anyway).

   In every trip-wire branch the filesystem mutation has already
   happened, so the durable state lags behind reality; the
   `recovery_required` state flags this intent for the Sprint 5+
   recovery worker. The trip-wire reason has a single source of
   truth on the event payload, not the intent row. Under the
   three-phase API these branches are expected to be rare: the
   authorize recheck (Phase B) catches the closure-shift escape
   before the FS mutation, so the trip-wire fires only on a
   genuine lock-bypass or a resolver that changes its mind between
   authorize and finalize.
4. Otherwise (trip-wire silent): apply the matching durable
   mutation via `IdentityRepo` inside this same transaction. Every
   Sprint 1 `CommitTarget` variant resolves to an identity-table
   mutation: `DeleteFileLocation` →
   `IdentityRepo::retire_file_location_in_tx`; `DeleteFileVersion`
   → `IdentityRepo::retire_file_version_in_tx`; `ArchiveFileVersion`
   → `IdentityRepo::archive_file_version_in_tx`;
   `ReplaceFileLocation` and `MoveFileLocation` → an
   `IdentityRepo::replace_file_location_in_tx` that atomically
   retires the prior location and records the new one on the same
   `FileVersion`. The durable mutation is what makes the closure
   check meaningful — it must run inside the same tx as the recheck
   and the intent transition.
5. Update the `commit_intents` row to `state = 'completed'`,
   `finalized_at = now`, bump `epoch`. Emit `commit.completed` with
   payload `{ commit_id, target, closure_initial,
   closure_authorized, closure_final, evaluated_lease_ids,
   revalidated_evidence }`. COMMIT. Return
   `CommitGateOutcome { result: Allowed, ... }`.

#### `abort_destructive_commit`

Called when the caller decides not to proceed before `authorize`
has been called (the caller holds a `CommitIntent`, not a
`CommitPermit`). One IMMEDIATE transaction.

1. Read the `commit_intents` row for `commit_id`; require
   `state = 'pending'`. Missing row, `state = 'authorized'`, or any
   terminal state → return `Conflict`. The `Conflict` path is the
   safety property the caller relies on: a stuck `authorized` intent
   cannot be terminated through this entry point, only through
   `finalize` with a valid permit.
2. Update the row to `state = 'aborted'`, `aborted_at = now`,
   `abort_reason = reason`. (`authorized_at` is NULL by construction
   since `state = 'pending'` means it has never been set.)
3. Emit `commit.aborted_pre_mutation` with payload
   `{ commit_id, prior_state: 'pending', reason }`. COMMIT.

The post-authorize counterpart is the `MutationOutcome::NotPerformed`
branch of `finalize_destructive_commit` (see Phase C below). That is
the **only** sanctioned way to abort an intent that has reached
`authorized` without applying the durable mutation. It is gated by
the `CommitPermit` and the in-transaction epoch check, both of which
prove the caller is the rightful holder of the authorize decision.
The `commit.aborted_pre_mutation` event payload sets
`prior_state = 'authorized'` on that path.

#### Recovery contract

`list_pending_commit_intents(older_than)` returns intents in
`state IN ('pending', 'authorized')`, optionally older than a
threshold. A stuck pending intent indicates the caller crashed
between Phase A and Phase B; a stuck authorized intent indicates
the caller crashed between Phase B and Phase C (most concerning,
since the FS mutation may have happened). Sprint 1 ships:

- the `commit_intents` table,
- the prepare / authorize / finalize / abort / list functions,
- the `commit.recovery_required` event kind, which the Sprint 5+
  recovery worker emits as it reconciles a stuck intent against
  filesystem state.

Sprint 1 does **not** ship the filesystem-aware reconciliation worker
itself — that lives with the real workers (Sprint 5+), and §15 records
the deferral. The three-phase API makes the worker's job easier
(`pending` vs. `authorized` tells it whether to expect any FS state
change), but does not change what the worker must do.

A stuck `authorized` intent (caller crashed between Phase B and
Phase C) cannot be terminated by `abort_destructive_commit`. The
Sprint 5+ recovery worker inspects filesystem state to decide
whether to:

- call `finalize(permit, Applied { observed: Some(...) })` if the
  FS mutation has visibly succeeded (the trip-wire runs and either
  completes the intent or transitions it to `recovery_required`),
  or
- call `finalize(permit, NotPerformed)` only if the worker can
  prove no FS mutation occurred (the intent transitions cleanly to
  `aborted`).

The recovery worker reconstructs the `CommitPermit` from the durable
`commit_intents` row and the journaled `commit.authorized` event
payload. Sprint 1 ships the journal and the API; the worker arrives
in Sprint 5+.

### 9.3.3 Force path

`override_token: Some(ForcePathToken)` is a separately audited path the
spec mandates ("Operators who need to commit despite incomplete resolution
use a separately audited, permissioned force path that records its own
override event and reason"). The `ForcePathToken` carries `actor`,
`reason`, and a `bypass` bitset declaring which checks are skipped. The
**only** allowed bypass kind is:

- `closure_incomplete` — skip the closure-resolution abort the gate
  raises when `AliasResolver` fails or returns
  `AliasResolutionError::Unreachable`. The bypass applies in both
  Phase A (`prepare`) and Phase B (`authorize`) — once granted at
  prepare time, the token's `closure_incomplete` bit is honored
  through to authorize, since otherwise the authorize-phase recheck
  could trap a commit the operator already chose to force.
  Implementation: the `override_token` is persisted on
  `commit_intents.override_token` at prepare time (§9.1) and also
  mirrored into the `commit.intent_recorded` event payload for audit
  and replay. `authorize_destructive_commit` reads the token from the
  durable column inside its own IMMEDIATE transaction and applies the
  same bypass logic to the authorize-phase unreachable-closure abort.
  The event journal is audit, not state; authorize never parses the
  event log for safety decisions.

The architectural spec scopes the force path to incomplete closure
resolution specifically. Fresh blocking use-leases are **not**
force-bypassable — they always fail the gate, in either Phase A or
Phase B. A token whose `bypass` set carries `blocked_by_use_lease` is
rejected before the gate runs (`VoomError::Config`). Likewise
`stale_evidence` is not a valid bypass: stale evidence is a
correctness problem, not a permissions problem. And `closure_grew` is
not a valid bypass either: a closure shift means the safety properties
the operator authorized no longer cover what is actually on disk, and
the force path is not a tool for ignoring that.

**Operator workflow when a blocking lease must be cleared.** An
operator who needs a destructive commit to proceed while a blocking
`asset_use_lease` exists must first terminate that lease through the
audited path:

```
UseLeaseRepo::force_release(lease_id, actor, reason)
```

That call writes its own `use_lease.force_released` event recording
`{ actor, reason }`. Once the lease is terminal, the gate's
blocking-lease check sees no live lease on the scope, and the operator
reruns `prepare_destructive_commit` / `authorize_destructive_commit`
/ `finalize_destructive_commit` without a `blocked_by_use_lease`
bypass. The gate itself is unchanged between attempts — the audit
trail lives on the lease release, not on the commit.

A force-path run that bypasses closure resolution emits
`commit.forced_override` recording the token's `actor`, `reason`, and
the `bypass` set, in addition to whatever `commit.*` events the
underlying path produces. The override is also captured in the
durable `commit_intents` row's audit payload (see §9.3.2). Force-released
leases on the scope still don't block (they're terminal); the spec's
"forced release does not bypass the safety gate on any later
destructive commit" is already covered by the terminal-state rule and
does not require special handling in the gate.

### 9.3.4 `AliasResolver`

```rust
pub trait AliasResolver: Send + Sync {
    async fn aliases_for_version(
        &self,
        file_version_id: FileVersionId,
    ) -> Result<Vec<FileLocationId>, AliasResolutionError>;
}

pub struct SqliteAliasResolver { pool: SqlitePool }
```

Sprint 1 ships `SqliteAliasResolver`, which returns every live
`FileLocation` row on the given `FileVersion` — no cross-host alias
detection. The trait exists so Sprint 4 (remote nodes, shared mounts) and
Sprint 5 (object-store providers) can layer their own resolvers, and the
fail-closed semantics are exercised today by a test-only
`FailingAliasResolver` that returns `Unreachable` for configured
`FileVersionId`s.

### 9.3.5 Pre-mutation abort-event durability

The two-transaction pattern applies **only** to Phase A aborts
(pre-mutation, before a `commit_intents` row exists): each `Blocked*`
branch in Phase A emits its `commit.aborted_by_*` event before
ROLLBACK and survives the abort via a follow-up tiny transaction that
writes the event row. Phase A abort events have no durable
`commit_intents` state to atomically commit alongside them (the
intent row is the thing being rolled back), so the two-tx pattern is
correct.

Phase B aborts (authorize-phase recheck failures —
`closure_incomplete`, `closure_grew`, `fresh_lease`,
`stale_evidence`) do **not** use the two-tx pattern: the
`commit_intents` row already exists by the time `authorize` runs, so
the abort transitions the intent to `aborted` with the matching
`abort_reason` **inside the same IMMEDIATE transaction** as the
event row. The intent state transition and the audit event commit
atomically; no follow-up transaction is needed and no race window
exists.

Phase C trip-wire aborts (post-mutation closure_grew/fresh_lease)
likewise write `commit.aborted_post_mutation` inside the finalize
transaction itself, which always commits the intent-state transition
to `recovery_required`. What the post-mutation abort skips is the
durable identity mutation (step 4); the intent row update, the
`recovery_required` transition, and the event row commit together as
one atomic record of "the FS mutation happened but the closure check
fell behind."

The `abort_destructive_commit` entry point (Phase A, pending-only)
runs the abort + event emission in a **single** IMMEDIATE
transaction, not the two-tx pattern: the `commit_intents` row already
exists (it was inserted by `prepare`), so the intent transition to
`aborted` and the `commit.aborted_pre_mutation` event row commit
atomically together. The two-tx pattern is specific to Phase A
*gate-check* aborts where the gate is rejecting the
`prepare_destructive_commit` call itself and no `commit_intents` row
ever materializes.

### 9.4 Rename reconciliation × evidence revalidation × authorize recheck

Three interactions sit on top of the same fact — external rename
reconciliation is a byte-preserving observation of physical reality
that always records, and it shifts the closure of any in-flight
destructive commit on the affected `FileVersion`:

1. **Rename always proceeds.** `reconcile_rename_in_tx` does **not**
   consult the pending-commit lock (§8.7). A watcher or rescan that
   observes a real external move records it immediately, retiring
   the prior `FileLocation` and recording the new one on the same
   `FileVersion`. Refusing to record the move would leave durable
   identity stale exactly when closure accuracy matters most.
2. **Authorize observes the shift.** A rename that lands while a
   destructive commit on the same `FileVersion` is in
   `state = 'pending'` will cause the Phase B `authorize` recheck
   (§9.3.2) to compute a `closure_authorized` whose `file_locations`
   set differs from `closure_initial`: the prior `FileLocation` is
   absent (it has been retired), and the new `FileLocation` is
   present. This produces a non-empty `removed_locations` and
   `added_locations` and aborts the intent with
   `BlockedByClosureGrew { added_*, removed_* }`. The operator
   re-prepares against the new closure, re-authorizes, and proceeds.
3. **Pinned evidence still drifts on rename.**
   `reconcile_rename_in_tx` re-anchors **leases** to the new location
   but does **not** rewrite accepted **evidence** (whose
   `pinned_locations` array still names the retired location). A
   later destructive commit acting on that pinned evidence will hit
   `BlockedByStaleEvidence` because `pinned_locations` no longer
   matches the current state, and must re-collect & re-accept. This
   is independent of the closure-shift abort above: a rename produces
   *both* `BlockedByClosureGrew` (in the authorize recheck of the
   current commit) and `BlockedByStaleEvidence` (on the next attempt
   with the now-stale evidence) — the operator re-collects evidence
   first, then re-prepares.

The integration test `commit_safety_gate_after_rename.rs` covers the
end-to-end sequence under the three-phase API.

## 10. Ancillary Registries (M3)

### 10.1 External systems

`external_systems`:

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `kind` | TEXT NOT NULL | `plex` \| `jellyfin` \| `emby` \| `radarr` \| `sonarr` \| `bazarr` \| `s3` \| `filesystem` \| `custom` |
| `display_name` | TEXT NOT NULL | |
| `connection_profile` | TEXT NOT NULL | JSON; opaque |
| `auth_ref` | TEXT NOT NULL | Secret-store key; Sprint 9 owns the store |
| `health_status` | TEXT NOT NULL | `unknown` \| `healthy` \| `degraded` \| `unreachable` |
| `rate_limit_config` | TEXT NOT NULL DEFAULT '{}' | JSON |
| `created_at` | TEXT NOT NULL | |
| `retired_at` | TEXT NULL | |
| `epoch` | INTEGER NOT NULL DEFAULT 0 | |

`external_system_links`:

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `external_system_id` | INTEGER NOT NULL REFERENCES `external_systems(id)` | |
| `target_type` | TEXT NOT NULL | `media_work` \| `media_variant` \| `asset_bundle` \| `file_asset` |
| `target_id` | INTEGER NOT NULL | |
| `external_ref` | TEXT NOT NULL | |
| `created_at` | TEXT NOT NULL | |
| `retired_at` | TEXT NULL | |

`external_path_mappings`:

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `external_system_id` | INTEGER NOT NULL REFERENCES `external_systems(id)` | |
| `internal_prefix` | TEXT NOT NULL | |
| `external_prefix` | TEXT NOT NULL | |
| `visibility` | TEXT NOT NULL | `read_only` \| `read_write` |
| `created_at` | TEXT NOT NULL | |
| `retired_at` | TEXT NULL | |

`ExternalSystemRepo` exposes CRUD + `update_health(id, status)`. Each
mutation emits its matching `external_system.*` event. No sync jobs in
Sprint 1.

### 10.2 Issues

`issues`:

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `kind` | TEXT NOT NULL | `unknown_identity` \| `missing_subtitle` \| `duplicate_candidate` \| `policy_noncompliant` \| `health_failed` \| `external_sync_failed` \| `artifact_unavailable` \| `variant_retention_conflict` \| `worker_untrusted` \| `terminal_failure` |
| `severity` | TEXT NOT NULL | `critical` \| `high` \| `medium` \| `low` \| `info` |
| `priority` | TEXT NOT NULL | `urgent` \| `high` \| `normal` \| `low` \| `someday` |
| `priority_source` | TEXT NOT NULL | `system` \| `user` \| `policy` \| `external` |
| `priority_reason` | TEXT NULL | |
| `status` | TEXT NOT NULL | `open` \| `planned` \| `resolved` \| `suppressed` \| `accepted` |
| `suppressed_until` | TEXT NULL | |
| `title` | TEXT NOT NULL | |
| `body` | TEXT NOT NULL | |
| `created_at` | TEXT NOT NULL | |
| `updated_at` | TEXT NOT NULL | |
| `resolved_at` | TEXT NULL | |
| `epoch` | INTEGER NOT NULL DEFAULT 0 | |

`issue_links`:

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `issue_id` | INTEGER NOT NULL REFERENCES `issues(id)` ON DELETE CASCADE | |
| `link_type` | TEXT NOT NULL | `evidence` \| `file_asset` \| `bundle` \| `worker` \| `external_system` \| `ticket` \| `lease` \| `use_lease` |
| `target_type` | TEXT NOT NULL | |
| `target_id` | INTEGER NOT NULL | |
| `created_at` | TEXT NOT NULL | |

`IssueRepo` operations:

- `open(NewIssue) -> IssueId` → `issue.opened`
- `open_in_tx(tx, NewIssue) -> IssueId` → `issue.opened` inside the
  caller's transaction
- `reprioritize(issue_id, priority, source, reason)` → `issue.priority_changed`
- `resolve(issue_id)` → `issue.resolved`
- `suppress(issue_id, until)` → `issue.suppressed`
- `accept(issue_id, actor)` → `issue.accepted`
- `link(issue_id, link)` → `issue.linked`
- `link_in_tx(tx, issue_id, link)` → `issue.linked` inside the
  caller's transaction

**Terminal-failure auto-open contract (arch spec → Issue Model;
research-note §11.2 DLQ analogue).** Whenever a ticket transitions to
`failed` terminally — via any of:

- `LeaseRepo::fail` with a non-retriable or operator-required
  `FailureClass`, or with retries exhausted on a retriable class,
- `LeaseRepo::expire_due` past `max_attempts` (implicit
  `FailureClass::WorkerCrash`),
- `LeaseRepo::force_release(_, _, _, also_requeue = false)` (implicit
  `FailureClass::UserCancellation` — the operator's `actor` /
  `reason` are captured on the accompanying `lease.force_released`
  event payload, not on the issue itself)

— the corresponding control-plane use case
(`ControlPlane::fail_lease` /
`ControlPlane::expire_due` /
`ControlPlane::force_release_lease`) calls, **in the same transaction
that writes `ticket.failed_terminal` and the matching
`lease.released` / `lease.expired` / `lease.force_released` event**:

- `IssueRepo::open_in_tx(tx, NewIssue { kind: terminal_failure,
  severity, priority, priority_source: 'system', title: …, body: … })`
  followed by `link_in_tx` for `{ link_type: 'ticket', target_type:
  'ticket', target_id: ticket_id }` and `{ link_type: 'lease',
  target_type: 'lease', target_id: last_lease_id }`.

Cardinality, by milestone:

- **M1** — no `terminal_failure` issue is opened (the `issues` table
  does not exist yet; `IssueRepo` lands in M3). M1's
  `LeaseRepo::fail` and `LeaseRepo::expire_due` emit the
  `ticket.failed_terminal` event with `issue_id = null`. M1
  integration tests that exercise terminal transitions assert the
  null payload and confirm the `issues` table is unaffected (no rows
  exist because no table exists).
- **M3** — every terminal transition opens exactly one new
  `terminal_failure` issue. There is no "update existing" branch: the
  ticket state machine (§7.2) makes `failed` terminal, so a given
  ticket transitions to `failed` at most once, and there is no prior
  `terminal_failure` issue for the same ticket to update. Aggregation
  across multiple failed tickets of the same job is deferred to
  Sprint 3 once jobs acquire an execution plan and a natural roll-up
  scope. The `ticket.failed_terminal` payload's `issue_id` is set to
  the newly-opened issue's id.

On the M3 path the `severity` and `priority` fields of `NewIssue` are
derived from the `FailureClass` value the use case has in hand,
through the methods defined in §12.5:

- `severity = class.issue_severity()` —
  `FailureRetryClass::OperatorRequired` and `NonRetriable` map to
  `IssueSeverity::High`; `Retriable` (only reachable on the terminal
  branch with retries exhausted) maps to `IssueSeverity::Medium`.
- `priority = class.issue_priority()` —
  `FailureRetryClass::OperatorRequired` and `NonRetriable` map to
  `IssuePriority::High`; `Retriable` maps to `IssuePriority::Normal`.

Both methods are total and `const`; the use case never has to invent
a default. The `title` and `body` strings are composed by the use case
from the ticket's `kind`, the `FailureClass` variant, and the last
lease's worker — the spec does not pin their exact wording because
they are operator-facing diagnostic text, not part of any machine
contract. `TicketFailedTerminal`'s event payload carries `issue_id`
(§6.1) so audit and the CLI can navigate from the event to the issue.

The M3 wiring extends the M1 `LeaseRepo` API by zero — the issue
auto-open is a use-case-layer composition of `_in_tx` calls on
`IssueRepo` and the existing `LeaseRepo` / `TicketRepo` / `EventRepo`
methods, not a new repo method on `LeaseRepo` or `TicketRepo`. The M3
migration that introduces the `issues` / `issue_links` tables is the
same migration that flips the use-case wiring on; before that
migration runs, `ControlPlane::fail_lease` /
`ControlPlane::expire_due` follow the M1 path and write the
`ticket.failed_terminal` event with `issue_id = null`.

### 10.3 Quality scores

`quality_scoring_profiles`:

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `name` | TEXT NOT NULL UNIQUE | |
| `version` | INTEGER NOT NULL | |
| `definition` | TEXT NOT NULL | JSON; dimensions + weights |
| `created_at` | TEXT NOT NULL | |
| `retired_at` | TEXT NULL | |

`quality_scores`:

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | |
| `profile_id` | INTEGER NOT NULL REFERENCES `quality_scoring_profiles(id)` | |
| `profile_version` | INTEGER NOT NULL | |
| `provider` | TEXT NOT NULL | |
| `provider_version` | TEXT NOT NULL | |
| `target_type` | TEXT NOT NULL | `media_variant` \| `asset_bundle` \| `file_asset` \| `file_version` |
| `target_id` | INTEGER NOT NULL | |
| `total_score` | REAL NOT NULL | |
| `dimension_scores` | TEXT NOT NULL | JSON |
| `provenance` | TEXT NOT NULL | JSON |
| `observed_at` | TEXT NOT NULL | |
| `superseded_at` | TEXT NULL | |

`QualityScoreRepo`:

- `register_profile(NewProfile) -> ScoreProfileId` → `quality_profile.registered`
- `record(NewScore) -> ScoreId` → `quality_score.recorded`
- `supersede(score_id)` → `quality_score.superseded`

No scoring math in Sprint 1 — scores are caller-provided. The math arrives
with the `fake-quality-scorer` in Sprint 2.

## 11. CLI Inspection Surface (M3)

`voom-cli` gains a `commands/` module subdirectory with one file per
resource group. The full Sprint 1 subcommand tree:

```text
voom version                                    (Sprint 0)
voom health                                     (Sprint 0)
voom init                                       (Sprint 0)
voom job        list [--state] [--kind] [--limit] [--cursor]
                get  <id>
voom ticket     list [--job] [--state] [--kind] [--limit] [--cursor]
                get  <id>
                deps <id>
voom lease      list [--worker] [--state] [--limit] [--cursor]
                get  <id>
voom worker     list [--status] [--kind] [--limit] [--cursor]
                get  <id>                       (returns capabilities + grants inline)
voom artifact   list [--bundle] [--asset] [--limit] [--cursor]
                get  <id>                       (returns locations + lineage inline)
voom work       list [--limit] [--cursor]
                get  <id>
voom variant    list [--work] [--limit] [--cursor]
                get  <id>
voom bundle     list [--variant] [--limit] [--cursor]
                get  <id>                       (returns members inline)
voom asset      list [--variant] [--bundle] [--limit] [--cursor]
                get  <id>                       (returns versions + locations + bundle membership inline)
voom evidence   list [--target-type] [--target-id] [--accepted] [--limit] [--cursor]
                get  <id>
voom issue      list [--status] [--severity] [--priority] [--kind] [--limit] [--cursor]
                get  <id>
voom score      list [--profile] [--target-type] [--target-id] [--limit] [--cursor]
                profiles list
voom external-system  list [--limit] [--cursor]
                      get  <id>
voom use-lease  list [--scope-type] [--scope-id] [--state] [--limit] [--cursor]
                get  <id>
voom commit-intent  list [--state pending|authorized|completed|aborted|recovery_required] [--older-than] [--limit] [--cursor]
                    get  <id>                       (returns scope_members + closure_authorized inline when applicable)
voom event      list [--since] [--until] [--kind] [--subject-type] [--subject-id] [--limit] [--cursor]
                tail [--since] [--kind] [--subject-type] [--subject-id]
                get  <id>
```

### 11.1 Envelope shape

Every command emits the Sprint 0 JSON envelope (`schema_version`,
`command`, `status`, `data`, `local`, `warnings`, `error`).

- `list` commands return `data: { items: [...], cursor: "..." | null, total_known: <bool> }`.
- `get` commands return `data: { <resource> }`. Unknown ID returns
  `status: "error"`, `error.code: "NOT_FOUND"`.
- `tail` returns the same shape as `list` but with `cursor` always
  present so agents can poll a stable next-page token.

`local` is populated on the CLI (host paths, db url, config path) and
remains structurally absent from the not-yet-implemented API path.

### 11.2 Pagination

`--limit` defaults to 50, capped at 500. `--cursor` is an opaque
base64-encoded `(sort_key, id)` pair. Sort key is `occurred_at` for
events, `started_at` for commit-intents (the start time never changes
once the intent is recorded, so it gives a stable ordering across
phase transitions), `updated_at` otherwise. The CLI never paginates
lazily — agents drive the cursor explicitly so command boundaries are
stable.

### 11.3 Snapshot coverage

Insta snapshots in `voom-cli/tests/snapshots/` assert the exact envelope
JSON for at least:

- `voom event list` (empty DB — never happens after `init`; fixture
  rolls back the schema event before the snapshot test)
- `voom event list` (post-init, asserting one `schema.initialized` row)
- `voom job list` (empty)
- `voom ticket get <unknown_id>` (`status: "error"`, `error.code: "NOT_FOUND"`)
- `voom worker get <id>` showing capabilities + grants inline
- `voom asset get <id>` showing versions + locations + bundle membership
- `voom commit-intent list --state pending` showing the
  `PendingCommitIntent` shape with `state = "pending"`,
  `closure_authorized = null` (fixture inserts a pending intent via
  the gate's `prepare` API)
- `voom commit-intent list --state authorized` showing the
  `PendingCommitIntent` shape with `state = "authorized"`,
  `closure_authorized` populated, `authorized_at` non-null (fixture
  inserts a pending intent via `prepare` and then transitions it via
  `authorize_destructive_commit`)
- `voom commit-intent get <id>` for a pending intent, asserting the
  `scope_members` array is rendered inline so operators can see what
  the intent is currently blocking (asset / bundle / version /
  location members)
- `voom commit-intent get <id>` for an authorized intent, asserting
  `closure_authorized` and `authorized_at` are rendered inline and
  the `scope_members` array reflects the recomputed closure
- The Sprint-1-specific error envelopes for `BLOCKED_BY_USE_LEASE`,
  `BLOCKED_BY_PENDING_COMMIT`, `BLOCKED_BY_CLOSURE_GREW`,
  `STALE_IDENTITY_EVIDENCE`, `CLOSURE_RESOLUTION_INCOMPLETE`,
  `DEPENDENCY_CYCLE`, and `CONFLICT`, shaped via fixture insertion +
  forced invocation of the relevant control-plane use case. The
  `BLOCKED_BY_CLOSURE_GREW` envelope is shaped by triggering the
  Phase B authorize-recheck closure-shift abort against a fixture
  that runs an external rename reconciliation between `prepare` and
  `authorize` (rename is the architecturally-exempt path that
  shifts the closure; local alias attach is blocked by the
  pending-commit lock and cannot reach authorize).

## 12. Cross-cutting Concerns

### 12.1 Error codes

`voom-core::error::ErrorCode` gains:

- `BlockedByUseLease` → `"BLOCKED_BY_USE_LEASE"`
- `BlockedByPendingCommit` → `"BLOCKED_BY_PENDING_COMMIT"`
- `BlockedByClosureGrew` → `"BLOCKED_BY_CLOSURE_GREW"`
- `StaleIdentityEvidence` → `"STALE_IDENTITY_EVIDENCE"`
- `ClosureResolutionIncomplete` → `"CLOSURE_RESOLUTION_INCOMPLETE"`
- `DependencyCycle` → `"DEPENDENCY_CYCLE"`
- `Conflict` → `"CONFLICT"`

In addition, `FailureClass::into_error_code` (§12.5) maps each
ticket-failure category to an `ErrorCode`. Most reuse the variants
above (`BlockedByUseLease`, `StaleIdentityEvidence`,
`ClosureResolutionIncomplete`); the remaining failure categories add
their own ErrorCode variants so the JSON envelope's `error.code`
field is unambiguous on a `ticket.failed_terminal` surfacing path:

- `WorkerTimeout` → `"WORKER_TIMEOUT"`
- `WorkerCrash` → `"WORKER_CRASH"`
- `NoEligibleWorker` → `"NO_ELIGIBLE_WORKER"`
- `ArtifactUnavailable` → `"ARTIFACT_UNAVAILABLE"`
- `ArtifactChecksumMismatch` → `"ARTIFACT_CHECKSUM_MISMATCH"`
- `ExternalSystemUnavailable` → `"EXTERNAL_SYSTEM_UNAVAILABLE"`
- `ExternalSystemRateLimited` → `"EXTERNAL_SYSTEM_RATE_LIMITED"`
- `VerificationFailure` → `"VERIFICATION_FAILURE"`
- `BackupFailure` → `"BACKUP_FAILURE"`
- `CommitFailure` → `"COMMIT_FAILURE"`
- `PolicyParseError` → `"POLICY_PARSE_ERROR"`
- `PolicyValidationError` → `"POLICY_VALIDATION_ERROR"`
- `MissingCapability` → `"MISSING_CAPABILITY"`
- `MalformedWorkerResult` → `"MALFORMED_WORKER_RESULT"`
- `UserCancellation` → `"USER_CANCELLATION"`
- `ApprovalRequired` → `"APPROVAL_REQUIRED"`
- `PriorityPolicyConflict` → `"PRIORITY_POLICY_CONFLICT"`

Sprint 1 has no callers that *emit* most of these (no worker process,
no policy parser); they exist to make the `FailureClass →
ErrorCode` mapping total at the type level so tests can construct
synthetic terminal failures of any class and the CLI envelope shape
stays stable.

`VoomError` gains matching `(String)`-tuple variants — matching Sprint 0's
pattern. The message text carries the human-readable context (lease ID,
evidence drift summary, blocking `commit_id` plus scope type and id for
`BlockedByPendingCommit`, closure-delta summary for
`BlockedByClosureGrew`, etc.); structured detail for Sprint 9's reports
lives on the gate-result types in `voom-store::repo::commit_safety_gate`
(`CommitGateResult`, `LeaseScope`, `EvidenceDrift`, `ClosureWarning`,
`BlockedByPendingCommitDetail` for the pending-commit-lock matches
described in §9.1, §9.2, and `BlockedByClosureGrewDetail` for the
authorize-recheck and finalize-tripwire closure-delta payloads
described in §9.3.2), which the control-plane use cases consult
before mapping to `VoomError`.

```rust
pub enum VoomError {
    /* ... Sprint 0 variants ... */
    BlockedByUseLease(String),
    BlockedByPendingCommit(String),
    BlockedByClosureGrew(String),
    StaleIdentityEvidence(String),
    ClosureResolutionIncomplete(String),
    DependencyCycle(String),
    Conflict(String),
    /* FailureClass-derived variants, see list above */
    WorkerTimeout(String),
    WorkerCrash(String),
    NoEligibleWorker(String),
    ArtifactUnavailable(String),
    ArtifactChecksumMismatch(String),
    ExternalSystemUnavailable(String),
    ExternalSystemRateLimited(String),
    VerificationFailure(String),
    BackupFailure(String),
    CommitFailure(String),
    PolicyParseError(String),
    PolicyValidationError(String),
    MissingCapability(String),
    MalformedWorkerResult(String),
    UserCancellation(String),
    ApprovalRequired(String),
    PriorityPolicyConflict(String),
}
```

The mapping in `VoomError::error_code` extends accordingly. Existing
variants are unchanged.

### 12.2 ID newtypes

`voom-core::ids` gains, via the existing `define_id!` macro for `u64`
identifiers:

- `MediaWorkId`, `MediaVariantId`, `BundleId`, `FileAssetId`,
  `FileVersionId`, `FileLocationId`, `EvidenceId`
- `ExternalSystemId`, `ExternalSystemLinkId`, `ExternalPathMappingId`
- `IssueId`, `IssueLinkId`
- `ScoreId`, `ScoreProfileId`
- `ArtifactHandleId`, `ArtifactLocationId`, `UseLeaseId`, `CommitId`
  (`NodeId` deferred to Sprint 4, when the `nodes` table lands
  alongside remote-node lease acquisition — see the amended
  architectural spec deliverables list)

Sprint 0's placeholder `MediaId` (a single u64 newtype standing in for the
yet-to-be-split identity layers) is removed in M2 because every Sprint 1
caller wants the specific layer (`MediaWorkId` for the logical title,
`MediaVariantId` for a retained version, `FileAssetId` for managed file
lineage, …). No call sites carry forward — Sprint 0's only `MediaId`
references are in the `ids_test.rs` smoke test, which is updated to
reference the new newtypes.

And one `String`-backed newtype:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TraceId(pub String);
```

`TraceId` is `Option<TraceId>` everywhere it surfaces in Sprint 1 — only
written if the Sprint 9 trace propagation eventually sets one. Sprint 1
leaves it `None`.

### 12.3 Clock injection

`ControlPlane` carries `clock: Arc<dyn Clock>`. Every repo method that
needs "now" takes `now: OffsetDateTime` as an explicit argument — the repo
never calls `OffsetDateTime::now_utc()` directly. `ControlPlane` passes
its clock through to repo calls.

`voom-core::clock_test_support` exposes:

```rust
pub struct FrozenClock { now: OffsetDateTime }
impl FrozenClock { pub fn new(now: OffsetDateTime) -> Self; }
impl Clock for FrozenClock { fn now(&self) -> OffsetDateTime { self.now } }

pub struct ManualClock { now: Mutex<OffsetDateTime> }
impl ManualClock {
    pub fn new(now: OffsetDateTime) -> Self;
    pub fn advance(&self, delta: Duration);
    pub fn set(&self, now: OffsetDateTime);
}
impl Clock for ManualClock { fn now(&self) -> OffsetDateTime { *self.now.lock().unwrap() } }
```

Gated behind `#[cfg(any(test, feature = "test-support"))]` per the Sprint 0
pattern.

### 12.4 Logging & config

No new logging or config surface. Sprint 1 reuses Sprint 0's `tracing`
setup verbatim. Config file parsing remains deferred to Sprint 5. New
tracing targets land per repo module (`voom_store::repo::leases`,
`voom_store::repo::commit_safety_gate`, …) as a natural consequence of
the module layout.

### 12.5 `FailureClass`

`voom-core::failure::FailureClass` enumerates the failure categories
defined by the architectural spec's Failure taxonomy table (Error
Handling And Recovery → Failure taxonomy). It is the single source of
truth for retriability decisions across `LeaseRepo::fail`,
`LeaseRepo::expire_due`, and the `ticket.failed_*` event payloads.

```rust
pub enum FailureClass {
    // Retriable
    WorkerTimeout,
    WorkerCrash,
    NoEligibleWorker,
    ArtifactUnavailable,
    ArtifactChecksumMismatch,
    ExternalSystemUnavailable,
    ExternalSystemRateLimited,
    VerificationFailure,
    BackupFailure,
    CommitFailure,
    // Non-retriable
    PolicyParseError,
    PolicyValidationError,
    MissingCapability,
    MalformedWorkerResult,
    UserCancellation,
    // Operator-required
    StaleIdentityEvidence,
    ClosureResolutionIncomplete,
    BlockedByActiveUseLease,
    ApprovalRequired,
    PriorityPolicyConflict,
}

impl FailureClass {
    /// True if a fresh attempt against the same input could plausibly
    /// succeed without operator intervention or upstream change.
    pub const fn is_retriable(self) -> bool { /* ... */ }

    /// Coarse-grained retry class — used by the terminal-failure
    /// auto-open path (§10.2) to derive issue priority and severity.
    pub const fn retry_class(self) -> FailureRetryClass { /* ... */ }

    /// Severity to stamp on the `terminal_failure` issue opened by
    /// the auto-open path (§10.2 / S3). `OperatorRequired` and
    /// `NonRetriable` classes default to `IssueSeverity::High`;
    /// `Retriable` (always reached only with retries exhausted)
    /// defaults to `IssueSeverity::Medium`.
    pub const fn issue_severity(self) -> IssueSeverity {
        match self.retry_class() {
            FailureRetryClass::OperatorRequired | FailureRetryClass::NonRetriable
                => IssueSeverity::High,
            FailureRetryClass::Retriable
                => IssueSeverity::Medium,
        }
    }

    /// Priority to stamp on the `terminal_failure` issue opened by
    /// the auto-open path (§10.2 / S3). `OperatorRequired` and
    /// `NonRetriable` classes default to `IssuePriority::High`;
    /// `Retriable` (retries exhausted) defaults to
    /// `IssuePriority::Normal`.
    pub const fn issue_priority(self) -> IssuePriority {
        match self.retry_class() {
            FailureRetryClass::OperatorRequired | FailureRetryClass::NonRetriable
                => IssuePriority::High,
            FailureRetryClass::Retriable
                => IssuePriority::Normal,
        }
    }

    /// Maps to the `ErrorCode` the CLI envelope surfaces on a
    /// `ticket.failed_terminal` path (§12.1).
    pub const fn into_error_code(self) -> ErrorCode { /* ... */ }
}

pub enum FailureRetryClass {
    Retriable,
    NonRetriable,
    OperatorRequired,
}
```

`IssueSeverity` and `IssuePriority` are the enum forms of the
`severity` and `priority` columns on `issues` (§10.2); both live in
`voom-core::issue` so cross-crate consumers (events, control plane,
CLI) reference one type. The retriability partition mirrors the
architectural taxonomy exactly: the ten `retriable` variants return
`true` from `is_retriable` and `FailureRetryClass::Retriable` from
`retry_class`; the five `non_retriable` variants return
`FailureRetryClass::NonRetriable`; the five `operator_required`
variants return `FailureRetryClass::OperatorRequired`. The
compiler-derived totality of the match inside each method is what
prevents a future variant from silently defaulting. Operator-required
variants are non-retriable from the lease lifecycle's perspective —
they still surface a `terminal_failure` issue (§10.2 / S3), and the
issue's body names the operator action that, once taken, makes a
fresh attempt viable.

`FailureClass` is also the payload field on
`TicketFailedRetriable`/`TicketFailedTerminal` events (§6.1), so
audit can reconstruct the retriability decision without re-deriving
it from the message text.

## 13. Testing Strategy

Sprint 0 ADR 0004 established sibling unit tests (`foo.rs` + `foo_test.rs`)
with `#[path]`-linked `mod tests`. Sprint 1 follows verbatim.

### 13.1 Sibling unit tests

Each new repo source file ships its sibling `_test.rs` covering:

- the repo's pure-SQL operations against a `:memory:` pool via
  `voom-store::test_support::test_pool()`
- the optimistic-locking conflict path (`Conflict` returned on stale epoch)
- one happy-path event emission per public write method

`repo/use_leases_test.rs` additionally covers the §9.2 reason-routing
contract (Finding 2):

- `release_rejects_force_released_reason`: `release(lease_id, reason
  = force_released)` returns `VoomError::Config`; the lease row
  is unchanged; no event row written.
- `release_rejects_issuer_lost_reason`: `release(lease_id, reason =
  issuer_lost)` returns `VoomError::Config`; the lease row is
  unchanged; no event row written.
- `release_happy_path_released`: `release(lease_id, reason =
  released)` transitions the lease to terminal with `release_reason
  = 'released'`; emits `use_lease.released`.
- `release_happy_path_superseded`: same shape with `reason =
  superseded` → `release_reason = 'superseded'`; emits
  `use_lease.released`.
- `force_release_emits_audit_payload`: `force_release(lease_id,
  actor = "alice", reason = "clearing stuck lease for destructive
  commit")` succeeds; lease transitions to terminal with
  `release_reason = 'force_released'`; emits
  `use_lease.force_released` whose payload includes both `actor`
  and `reason` fields.
- `force_release_accepts_advisory_and_blocking`: parameterized
  coverage that `force_release` works against both blocking and
  advisory leases, TTL-bound and manual.
- `recover_stale_issuer_is_only_path_to_issuer_lost`: asserts
  `release` cannot reach `issuer_lost` (both rejections above) and
  that `recover_stale_issuer(lease_id, actor, reason)` is the only
  call that sets `release_reason = 'issuer_lost'` and emits
  `use_lease.recovered_stale_issuer`.

### 13.2 Integration tests (`crates/voom-store/tests/`)

Cross-repo flows live as integration tests:

- `ticket_lease_lifecycle.rs` — ticket pending → ready (no deps) → leased
  → heartbeat (multiple) → expired (via `expire_due`) → requeued → leased
  → succeeded. Asserts every event row. Also covers attempt accounting:
  with `max_attempts = 2`, exercises the §7.5 worked example end-to-end
  through `fail(FailureClass::WorkerTimeout)` (two dispatched attempts
  before `ticket.failed_terminal`) and through `expire_due` (same
  convention); with `max_attempts = 3`, exercises a mixed `fail` +
  `expire_due` sequence and asserts the final state matches the
  documented semantics. Also exercises the new `FailureClass`-driven
  retriability seam: `fail(FailureClass::StaleIdentityEvidence)`
  transitions the ticket to `failed` immediately (no
  `is_retriable`), regardless of remaining attempts; the
  `ticket.failed_terminal` payload carries
  `class = stale_identity_evidence`. Additionally, exercises the
  `force_release` requeue-budget precondition: with `max_attempts =
  1`, after `acquire` (which bumps `attempt` to 1),
  `force_release(_, _, _, also_requeue = true)` returns
  `VoomError::Conflict` (`attempt = 1`, `max_attempts = 1` —
  no headroom for requeue); asserts the lease state is unchanged,
  the ticket is still `leased`, and no `lease.force_released` or
  `ticket.requeued_after_force_release` event row was written. The
  same fixture then calls `force_release(_, _, _, also_requeue =
  false)` and asserts the terminal path lands cleanly (lease
  `force_released`, ticket `failed`, single `lease.force_released`
  + `ticket.failed_terminal` pair). This locks the invariant that
  the requeue branch never produces a ticket no future `acquire`
  can claim.
- `terminal_failure_opens_issue.rs` (M3) — covers the §10.2 / S3
  auto-open contract. The test is parameterized over the three
  terminal-transition paths defined in §7.5; each case shares the
  same issue/event assertions except for the lease event kind, which
  is fixed by the trigger:

  | Trigger | Implicit `FailureClass` | Lease event |
  |---|---|---|
  | `fail(_, FailureClass::MalformedWorkerResult)` | `MalformedWorkerResult` | `lease.released` |
  | `expire_due` past `max_attempts` | `WorkerCrash` | `lease.expired` |
  | `force_release(_, actor, reason, also_requeue = false)` | `UserCancellation` | `lease.force_released` |

  Per case, with a fresh ticket and no prior issues, the trigger
  results in: exactly one new row in `issues` with
  `kind = 'terminal_failure'`, `priority_source = 'system'`, status
  `open`, `severity = class.issue_severity()`,
  `priority = class.issue_priority()` (§12.5); exactly one
  `issue_links` row of `link_type = 'ticket'` referencing the failed
  ticket and one of `link_type = 'lease'` referencing the last
  lease; exactly one `ticket.failed_terminal` event whose payload's
  `class` matches the row's implicit `FailureClass` and whose
  `issue_id` matches the new issue id; exactly one lease event of
  the kind named in the trigger row above; all rows committed
  atomically — a fixture that injects a panic mid-transaction proves
  no partial state survives. The `force_release` case additionally
  asserts the operator's `actor` / `reason` ride on the
  `lease.force_released` event payload and are **not** duplicated
  onto the issue.
  - Retriable-then-terminal: with `max_attempts = 2`, the first
    `fail(FailureClass::WorkerTimeout)` opens **no** issue (the
    `issues` table is empty after the call; the
    `ticket.failed_retriable` event payload has no `issue_id` field
    per §6.1, only `next_eligible_at`); a second
    `fail(FailureClass::WorkerTimeout)` exhausts retries, transitions
    the ticket to `failed`, and opens exactly one `terminal_failure`
    issue whose id appears in the `ticket.failed_terminal` payload's
    `issue_id` field — proving the auto-open path fires only on the
    terminal transition, never on a retriable one.
  - Negative test: a retriable failure with `attempt <
    max_attempts` does **not** open a `terminal_failure` issue —
    the `issues` table is empty after the call. This is the
    invariant the DLQ analogue rests on: a `terminal_failure` issue
    is exactly as durable as the terminal transition, and never
    appears on a retry-able path.
- `lease_expire_and_recover.rs` — bulk `expire_due` with hundreds of
  leases; per-row events; second invocation is a no-op.
- `ticket_dependency_unlock.rs` — DAG of N tickets with `phase`
  dependencies; assert ready-unlock order matches dep edges as upstream
  tickets succeed; cycle attempt → `DEPENDENCY_CYCLE`.
- `ingest_identity_invariants.rs` — covers every named ingest case:
  - new filesystem object → `NewFileAsset`
  - `LocalFileIdGeneration` proof with matching hash → `AliasAttached`
  - `LocalFileIdGeneration` proof with mismatched hash → `NewFileAsset` + `path_rule_match`
  - inode match without generation match → `NewFileAsset`
  - object-store key match without `version_id` → `NewFileAsset` + `path_rule_match`
  - object-store `(bucket, key, version_id)` match → `AliasAttached`
  - hash match without alias proof → `NewFileAsset` + `hash_match` evidence
  - ETag match (same code path as hash match) → `NewFileAsset` + `hash_match`

  Additional coverage for the §8 `DiscoveredFile.proof` persistence
  contract:
  - `discover_with_local_proof_persists_on_initial_location`:
    `DiscoveredFile.proof = Some(LocationProof::LocalFileIdGeneration
    { file_id, generation })` → `NewFileAsset` outcome; the new
    `file_locations` row has `proof_kind = 'file_id_generation'` and
    `proof_value` matching the supplied bytes.
  - `discover_with_object_store_proof_persists`: analogous for
    `LocationProof::ObjectStoreVersion` → `proof_kind =
    'object_version_id'` and `proof_value` matching `(bucket, key,
    version_id)`.
  - `discover_without_proof_persists_nulls`: `discovered.proof = None`
    → the new row's `proof_kind` and `proof_value` are both NULL
    (back-compat semantics).
  - `alias_attach_persists_proof_on_new_location`: an `AliasAttached`
    outcome carries the matching proof onto the new alias
    `file_locations` row so a later rename has a basis to verify.
  - `alias_attach_proof_drift_rejected`: `alias_proof` and
    `discovered.proof` disagree on `(file_id, generation)` →
    `VoomError::Conflict("proof drift on alias attach")`; no row
    inserted, no events.
- `rename_reconciliation.rs` — `reconcile_rename` retires old + records
  new on the same `FileVersion`; M3 step re-anchors blocking, advisory,
  and manual leases; pinned evidence is preserved (not rewritten); new
  evidence appended; events emitted. A dedicated sub-test confirms that
  `reconcile_rename_in_tx` proceeds even when a pending or authorized
  `commit_intents` row covers the affected `FileVersion` — rename
  reconciliation is the one architecturally-exempt observation path
  (arch spec lines 697–708); local alias attachment is **not** exempt
  and is covered by separate tests in `commit_safety_gate.rs` below.

  Additional coverage for the §8 `RenameProof` validation contract.
  Every `Conflict` path asserts the prior location stays live, no new
  `file_locations` row is inserted, and no `file_location.*_by_move`
  events are written:
  - `reconcile_rename_rejects_proof_kind_mismatch`: prior location has
    `proof_kind = 'file_id_generation'`; caller passes
    `RenameProof::ObjectStoreVersion` → `Conflict`; prior location
    stays live; no events.
  - `reconcile_rename_rejects_proof_value_mismatch_local`: same
    `proof_kind`, different `(file_id, generation)` → `Conflict`.
  - `reconcile_rename_rejects_proof_value_mismatch_object_store`: same
    `proof_kind = 'object_version_id'`, different `version_id` →
    `Conflict`.
  - `reconcile_rename_rejects_prior_path_present`: caller sets
    `prior_path_missing = false` → `Conflict` ("rename requires prior
    path missing").
  - `reconcile_rename_rejects_hash_drift`: proof matches but
    `observed.content_hash != fv.content_hash` → `Conflict`; verifies
    the bytes-still-the-same invariant.
  - `reconcile_rename_rejects_size_drift`: same shape on `size_bytes`.
  - `reconcile_rename_rejects_prior_retired`: prior_location was
    already retired by an earlier reconciliation → `Conflict`.
  - `reconcile_rename_happy_path_local`: full validation passes →
    prior retired, new recorded with carried-over `(proof_kind,
    proof_value)`, events emitted, M3 lease re-anchoring runs.
  - `reconcile_rename_happy_path_object_store`: same shape for
    `RenameProof::ObjectStoreVersion`.
- `commit_safety_gate.rs` — covers each abort path under the
  three-phase prepare/authorize/finalize/abort API (§9.3.1).

  **Phase A (`prepare`) abort coverage:**
  - `prepare_blocked_by_use_lease`: a blocking lease scoped to a
    closure member is detected at prepare time, returns
    `BlockedByUseLease`, emits `commit.aborted_by_use_lease` with
    `payload.phase = 'prepare'`, does **not** insert a
    `commit_intents` row.
  - `prepare_blocked_by_stale_evidence`: pinned hash mismatch at
    prepare time, returns `BlockedByStaleEvidence`, emits
    `commit.aborted_by_stale_evidence` with `payload.phase = 'prepare'`.
  - `prepare_blocked_by_closure_incomplete`: `FailingAliasResolver`
    causes the closure walk to fail, returns
    `BlockedByClosureIncomplete`, emits
    `commit.aborted_by_closure_incomplete` with
    `payload.phase = 'prepare'`.

  **Phase B (`authorize`) recheck coverage — the architectural
  "immediately before" gate, which is now the primary detection
  surface for closure shifts, fresh leases, and stale evidence that
  appear between prepare and authorize:**
  - `authorize_blocked_by_closure_grew_resolver`: prepare with
    closure C₁ against a stateful `AliasResolver` that returns C₁
    during Phase A; between prepare and authorize, the resolver
    starts returning C₂ ⊋ C₁ (simulating a remote mount coming
    online or an object-store probe succeeding on retry). Authorize
    observes the enlarged closure, transitions the intent to
    `aborted` with `abort_reason = 'closure_grew'`, emits
    `commit.aborted_by_closure_grew` with `payload.phase = 'authorize'`,
    returns `BlockedByClosureGrew { added_locations, ... }`.
    Asserts no durable identity mutation ran. (Local alias attach
    via `IdentityRepo::record_discovered_file_in_tx`'s `AliasAttached`
    branch is *blocked by the pending-commit lock* and so cannot be
    used to construct this scenario; the resolver-driven path is
    the architecturally-mandated escape that authorize observes —
    arch spec line 1047 "picking up any `FileLocation`s that have
    been attached as aliases of in-scope `FileVersion`s since the
    initial gate check".)
  - `authorize_blocked_by_closure_grew_rename`: prepare with closure
    C₁ including `FileLocation` L_old; between prepare and authorize,
    call `IdentityRepo::reconcile_rename_in_tx` to retire L_old and
    record L_new (rename is the architecturally-exempt path; it
    proceeds despite the in-flight intent). Authorize observes the
    shift, transitions the intent to `aborted` with `abort_reason =
    'closure_grew'`, emits `commit.aborted_by_closure_grew` with the
    same `payload.phase`, returns `BlockedByClosureGrew {
    added_locations: [L_new], removed_locations: [L_old], ... }`.
  - `authorize_blocked_by_use_lease`: a blocking lease that landed
    via a lock-bypass path (test inserts via direct SQL between
    prepare and authorize) is detected, transitions the intent to
    `aborted` with `abort_reason = 'fresh_lease'`, emits
    `commit.aborted_by_use_lease` with `payload.phase = 'authorize'`,
    returns `BlockedByUseLease`.
  - `authorize_blocked_by_stale_evidence`: pinned evidence drifts
    between prepare and authorize (the fixture mutates a
    `FileVersion`'s `content_hash`), authorize detects the drift,
    transitions the intent to `aborted` with `abort_reason =
    'stale_evidence'`, emits `commit.aborted_by_stale_evidence` with
    `payload.phase = 'authorize'`, returns `BlockedByStaleEvidence`.
  - `authorize_blocked_by_closure_incomplete`: the `AliasResolver`
    starts returning `Unreachable` between prepare and authorize.
    Authorize transitions the intent to `aborted` with `abort_reason
    = 'closure_incomplete'`, emits
    `commit.aborted_by_closure_incomplete` with
    `payload.phase = 'authorize'`, returns
    `BlockedByClosureIncomplete`. A companion test verifies the
    force-path `closure_incomplete` bypass token from the original
    `prepare` is honored through to `authorize` — the second run
    succeeds even when the resolver is still failing.

  **`override_token` durability coverage (§9.1, §9.3.3):**
  - `prepare_persists_override_token_on_row`: `prepare` with
    `override_token = Some(ForcePathToken { actor, reason, bypass: {
    closure_incomplete } })` → query
    `commit_intents.override_token` for the inserted row and assert
    it parses back to the same `ForcePathToken` shape.
  - `prepare_no_override_token_persists_null`: `prepare` without a
    token → `commit_intents.override_token IS NULL`.
  - `authorize_honors_durable_override_token_without_event_journal`:
    `prepare` with `Some(ForcePathToken { bypass: {
    closure_incomplete } })`; delete the matching
    `commit.intent_recorded` event row via direct SQL (simulating an
    event-store inaccessibility or a fixture that doesn't replay the
    journal); then run `authorize` against a `FailingAliasResolver`.
    Assert the bypass is still honored — proving authorize reads the
    durable column, not the event payload.
  - `prepare_rejects_invalid_override_token_at_check`: directly
    insert a row with `override_token = 'not-json'` via raw SQL
    (bypassing prepare) and assert the schema's `json_valid` CHECK
    rejects the write. This is defense-in-depth; the prepare path
    always serializes valid JSON.
  - `authorize_scope_members_unchanged_on_success`: prepare with
    closure C₁; no closure-mutating activity occurs between prepare
    and authorize. Authorize succeeds with `closure_authorized ==
    closure_initial` and `commit_intent_scope_members` rows are
    unchanged (step 6 of Phase B is a no-op when there is no
    delta). This is the common success-path shape under Sprint 1's
    strict abort-on-any-delta rule.
  - `authorize_aborted_scope_members_persist_for_audit`: under the
    closure_grew abort fixture, the intent transitions to
    `state = 'aborted'`. The `commit_intent_scope_members` rows
    inserted at prepare time persist (Sprint 1 does not delete
    intent rows; Sprint 5+ cleanup may remove them) so the audit
    record of the original `closure_initial` is preserved.
  - `authorize_then_lease_acquire_still_blocked`: after authorize
    succeeds and the intent is in `state = 'authorized'`, a
    `UseLeaseRepo::acquire` on a closure member is still rejected
    with `BlockedByPendingCommit` — the pending-commit lock covers
    the full in-flight window (§9.1, §9.2).

  **Phase C (`finalize`) coverage:**
  - `finalize_happy_path`: prepare → authorize → caller mutation
    (test helper applies the FS change) → `finalize(permit)` →
    durable mutation applied, intent in `state = 'completed'`,
    `commit.completed` event emitted. The lock is released
    (transition out of `authorized`) and a subsequent
    `UseLeaseRepo::acquire` on the scope succeeds.
  - `finalize_not_performed`: prepare → authorize → caller decides
    not to mutate → `finalize(permit, MutationOutcome::NotPerformed)`
    returns `Ok(CommitGateOutcome { result:
    CommitGateResult::CancelledAfterAuthorize, ... })`; intent in
    `state = 'aborted'` with `abort_reason = 'operator_cancel'`;
    emits `commit.aborted_pre_mutation` with `payload.prior_state =
    'authorized'`; the returned outcome's `closure_final` equals
    `permit.closure_authorized` (no mutation, no recheck).
  - `finalize_not_performed_then_retry_returns_conflict`: prepare →
    authorize → `finalize(permit, NotPerformed)` →
    reconstruct a synthetic permit (in a test, by capturing the
    original permit bytes before consumption) and call finalize
    again. The intent is now `aborted`, so Phase C step 1's state
    check returns `Err(VoomError::Conflict)`. This locks the rule
    that retry-after-success cleanly errors rather than silently
    re-cancelling. (The test's "synthetic permit reconstruction"
    simulates the future recovery worker's permit-from-row
    reconstruction path described in the §9.3.2 Recovery contract.)
  - `finalize_stale_permit`: prepare → authorize → concurrent
    `abort_destructive_commit` bumps the epoch (or finalize is called
    with a permit whose `epoch` no longer matches the durable
    `commit_intents` row) → `finalize` returns `Conflict` and does
    **not** apply the durable mutation.
  - `finalize_tripwire_fresh_lease`: prepare → authorize (succeeds)
    → between authorize and finalize, a direct-SQL insert into
    `asset_use_leases` (bypassing the repo and the lock) creates a
    blocking lease on a closure member → caller mutation → finalize
    detects the lease, transitions the intent to
    `recovery_required`, emits `commit.aborted_post_mutation` with
    `reason='fresh_lease'`, returns
    `CommitGateResult::BlockedByUseLease`, and asserts no durable
    identity mutation ran.
  - `finalize_tripwire_closure_grew`: prepare → authorize (succeeds
    against a stateful `AliasResolver` that returns C₁) →
    between authorize and finalize, the resolver starts returning
    C₂ ⊋ C₁ (e.g., a remote mount comes online) → caller mutation
    → finalize detects the delta, transitions the intent to
    `recovery_required`, emits `commit.aborted_post_mutation` with
    `reason='closure_grew'`, returns
    `CommitGateResult::BlockedByClosureGrew { added_*, removed_* }`,
    no durable identity mutation ran. These trip-wire tests exist
    to prove the trip-wire fires when something escapes the lock
    *and* the authorize recheck; under normal operation the
    trip-wire never fires (it is defensive code, not the primary
    detection point).
  - `finalize_tripwire_tie`: combines the previous two fixtures so
    closure grows **and** a fresh blocking lease lands between
    authorize and finalize. Asserts a single
    `commit.aborted_post_mutation` event with
    `reason='closure_grew_and_fresh_lease'` and both populated
    delta arrays **and** populated `fresh_lease_ids`, the intent in
    `recovery_required`, and the returned `CommitGateResult` is
    `BlockedByClosureGrew`.

  **`abort_destructive_commit` coverage (Phase A only — §9.3.2):**
  - `abort_pending_intent_succeeds`: prepare an intent so the row
    sits in `state = 'pending'`, call `abort_destructive_commit`,
    assert the intent terminates in `state = 'aborted'` with
    `abort_reason` matching the caller-supplied reason and the
    `commit.aborted_pre_mutation` event payload sets
    `prior_state = 'pending'`.
  - `abort_authorized_intent_returns_conflict`: prepare → authorize
    → `abort_destructive_commit` returns
    `Err(VoomError::Conflict(...))` and does **not** transition the
    intent (state stays `authorized`, no event row written). This
    verifies the safety property that backs the recovery contract
    in §9.3.2 — a stuck `authorized` intent cannot be terminated
    through this entry point.
  - `abort_terminal_intent_returns_conflict`: a completed (after
    `finalize_happy_path` fixture) or aborted intent rejects a
    subsequent `abort_destructive_commit` with `Conflict`.
  - `abort_missing_intent_returns_conflict`: `abort_destructive_commit`
    against an unknown `commit_id` returns `Conflict`.

  **IdentityRepo paths during in-flight intents:**

  *Alias attachment is locked (arch spec lines 1038–1043):*
  - `alias_attach_during_pending_intent_blocked`: a pending
    `commit_intents` row covers `FileVersion` V; a subsequent call
    to `IdentityRepo::record_discovered_file_in_tx` (alias-attach
    branch, with an `alias_proof` resolving to V) is **rejected**
    with `BlockedByPendingCommit`. The architectural spec at lines
    1038–1043 requires that `FileLocation`s alias discovery would
    attach to an in-scope `FileVersion` are blocked or held during
    an in-flight destructive commit.
  - `alias_attach_during_authorized_intent_blocked`: same as above
    but the intent is in `state = 'authorized'`. Confirms the
    pending-commit lock covers the full in-flight window for
    alias attach.
  - Granularity coverage: a pending intent on `BundleId` blocks an
    alias-attach whose resolved `FileVersion` belongs (transitively
    via `FileAsset`) to that bundle. A pending intent on
    `FileAssetId` blocks an alias-attach against any
    `FileVersion` of that asset. A pending intent on
    `FileVersionId` blocks an alias-attach against that version.
  - `alias_attach_new_file_asset_proceeds_during_pending_intent`:
    negative test — `record_discovered_file_in_tx` called with no
    `alias_proof` (NewFileAsset outcome) **succeeds** even while a
    pending intent exists, because a newly-discovered file asset is
    not in any pre-existing closure.
  - `alias_attach_proceeds_after_intent_resolves`: after
    `finalize_destructive_commit` (completed) or
    `abort_destructive_commit` (aborted) terminates the intent, the
    same alias-attach call succeeds.

  *Rename reconciliation is exempt (arch spec lines 697–708):*
  - `rename_during_pending_intent_proceeds`: a pending
    `commit_intents` row covers `FileVersion` V; a subsequent call
    to `IdentityRepo::reconcile_rename_in_tx` against a live
    `FileLocation` on V **succeeds** (retires the prior location,
    records the new one, emits the move events). The companion
    `authorize_blocked_by_closure_grew_rename` test catches the
    safety property: the in-flight commit aborts at the next
    authorize.
  - `rename_during_authorized_intent_proceeds`: same as above but
    the intent is in `state = 'authorized'`. Confirms the
    rename-reconciliation exemption applies through the entire
    in-flight window.

  **Pending-commit-lock coverage on `UseLeaseRepo::acquire` (the
  second path that consults the lock alongside the alias-attach
  branch covered above — §9.1, §9.2):**
  - A pending intent over `FileLocation` Y blocks a subsequent
    `UseLeaseRepo::acquire(LeaseScope::Location(Y))` with
    `BlockedByPendingCommit`.
  - The same intent blocks
    `UseLeaseRepo::acquire(LeaseScope::Asset(X))` when Y's parent
    asset is X (the asset-granularity `scope_members` row matches).
    Analogous cases for `LeaseScope::Bundle` and
    `LeaseScope::Version` exercise the other two granularities.
  - An **authorized** intent (state = `'authorized'`) blocks the
    same acquires — the lock covers the full in-flight window.
  - After `finalize_destructive_commit` (completed) or
    `abort_destructive_commit` (aborted) clears the intent's
    in-flight state, the same acquire calls succeed.

  **Force-path coverage:**
  - `closure_incomplete` bypass is honored in both `prepare` and
    `authorize` (one test per phase).
  - An `override_token` whose `bypass` set carries
    `blocked_by_use_lease` is rejected at the gate boundary
    (`VoomError::Config`); same for `stale_evidence` and
    `closure_grew`.
  - A separate test exercises the documented operator workflow
    ("force-release the blocking lease via
    `UseLeaseRepo::force_release(lease_id, actor, reason)`, then
    rerun the gate") and asserts the second attempt is allowed
    without any bypass token.

  **Inspection:**
  - Pending intents and authorized intents are visible via
    `list_pending_commit_intents`; the `state` field on each
    record distinguishes phase.
  - `recovery_required` state for a stuck intent whose caller never
    finalized is set by the trip-wire tests above; a dedicated
    inspection test reads the row back via the same list API.
- `commit_safety_gate_after_rename.rs` — end-to-end against the
  three-phase API: ingest → evidence-acceptance →
  `reconcile_rename` (M3, re-anchors leases) →
  `prepare_destructive_commit` against pinned evidence →
  `StaleIdentityEvidence` abort in Phase A; re-collect & re-accept
  evidence → second `prepare` → `authorize_destructive_commit` →
  caller mutation → `finalize_destructive_commit` allowed. A second
  scenario exercises rename happening *between* prepare and
  authorize: prepare succeeds, rename reconciliation lands,
  authorize aborts with `BlockedByClosureGrew { added_locations,
  removed_locations }`, operator re-prepares, evidence is re-collected,
  and the second three-phase attempt succeeds.
- `use_lease_scope_validation.rs` — covers the four-FK scope contract:
  - `acquire` with `LeaseScope::Location` referencing a nonexistent
    `FileLocationId` → `NotFound` (FK violation, translated by the
    repo).
  - `acquire` with `LeaseScope::Location` referencing a retired
    location → `Conflict` (liveness check rejects soft-deleted
    parents).
  - `acquire` with `LeaseScope::Asset` referencing a retired
    `FileAsset` → `Conflict` (same liveness rule applied per scope).
  - Positive test: the commit-safety-gate closure query in §9.3 picks
    up the lease through the correct `scope_*_id` FK column for each
    closure-member type (asset, bundle, version, location).
- `event_log_append_only.rs` — `UPDATE` and `DELETE` on `events` fail at
  the SQL level via triggers; insert remains allowed.
- `disk_mode.rs` — replays the lifecycle from `ticket_lease_lifecycle.rs`
  against a `tempfile`-backed disk DB, satisfying the architectural-spec
  exit clause about in-memory and disk parity.

### 13.3 Sprint 1 smoke recipe additions

The `just smoke` recipe (defined in Sprint 0) is extended with read-only
inspection calls at the tail:

```bash
# After Sprint 0's version/health/init sequence, against the same ephemeral DB:
voom event list --limit 5         # asserts at least one schema.initialized row
voom job list                      # empty items array, valid envelope
voom ticket list                   # empty items array, valid envelope
voom worker list                   # empty items array, valid envelope
voom event tail --kind schema.initialized --limit 1
# Asserts: cursor is non-null even when at end-of-stream
```

Each line pipes through `jq -e` asserting `.schema_version == "0"` and
the expected shape.

### 13.4 Test data fixtures

`voom-store::test_support` gains builder helpers (`TicketBuilder`,
`WorkerBuilder`, `FileAssetBuilder`, `BundleBuilder`,
`AcceptedEvidenceBuilder`, `UseLeaseBuilder`) so integration tests don't
hand-construct full structs. Each builder defaults every non-essential
field to a deterministic value (`OffsetDateTime::UNIX_EPOCH + N`), making
snapshot diffs stable.

## 14. Exit Criteria (verification map)

| Architectural-spec exit clause | How verified |
|---|---|
| Tests can create jobs, lease tickets, expire leases, and recover work. | `crates/voom-store/tests/ticket_lease_lifecycle.rs`, `lease_expire_and_recover.rs` |
| Tests can create a file asset, add versions and locations, and report its event/evidence history. | `crates/voom-store/tests/ingest_identity_invariants.rs`, `rename_reconciliation.rs`; the `voom asset get` CLI snapshot exercises the read-side report |
| Tests can create a bundle, open and prioritize an issue, record a quality score, and block a commit with a use lease. | `crates/voom-store/tests/commit_safety_gate.rs::prepare_blocked_by_use_lease` (Phase A `BlockedByUseLease` under the three-phase API; the matching `authorize_blocked_by_use_lease` test exercises the architectural pre-mutation recheck path) together with `repo/issues_test.rs::prioritize` and `repo/quality_scores_test.rs::record`. The DLQ-analogue terminal-failure → issue wiring (§10.2 / S3) is verified by `crates/voom-store/tests/terminal_failure_opens_issue.rs`. |
| Events are recorded for all state transitions. | Every repo `_test.rs` asserts the matching `events` row; `event_log_append_only.rs` asserts immutability |
| In-memory SQLite tests exercise the same repositories as disk mode. | Repos inherit `test_support::test_pool()` for `:memory:`; `tests/disk_mode.rs` runs the same fixture flow against a `tempfile`-backed disk DB; `just ci` runs both |

## 15. Out of Scope

See §1 for the full list. The most likely-to-be-asked exclusions:

- No worker process, no wire protocol (Sprint 2).
- No policy parser, no planner, no execution-plan tables (Sprint 3).
- No remote-node lease acquisition (Sprint 4).
- No node registry. The architectural spec is amended in this
  revision to defer the dedicated `nodes` table / `NodeRepo` to
  Sprint 4 alongside remote-node lease acquisition. Sprint 1's
  `workers` table (with `kind = synthetic | local | remote`) absorbs
  the local/remote distinction; future Sprint 4 work adds a separate
  `nodes` table and an FK from `workers.node_id`.
- No real ingest, transcode, remux, restore, backup, verify, or commit
  workers (Sprint 5).
- No filesystem watcher; ingest is exercised by tests calling
  `IdentityRepo::record_discovered_file_in_tx` directly (Sprint 6 owns
  the watcher).
- No daemon binary, no API server binary (Sprints 6 / 7).
- No web UI (Sprint 7).
- No plugin SDK (Sprint 8).
- No approval gates, rollback flows, metrics endpoint, or trace-ID
  propagation across plan/ticket/worker/artifact records — Sprint 1 ships
  only the `trace_id` column on `events`, always written as NULL until
  Sprint 9 wires the propagation.
- No installation packaging, upgrade migration tests, or security review
  (Sprint 10).
- No `merge` operation on `FileAsset` lineages (architectural spec
  explicitly defers; requires its own follow-on spec).
- No real `auth_ref` secret store (Sprint 9); Sprint 1 just persists the
  opaque key.
- No CLI write commands. Sprint 1's CLI is read-only inspection;
  durable writes go through `ControlPlane` use cases exercised by tests.
- No `ArchiveBundle` / `DeleteBundle` `CommitTarget` variants. The
  `asset_bundles` table does not carry the soft-delete/archive
  columns those targets need, and no Sprint 1 worker initiates a
  bundle commit. Sprint 5 adds the schema columns, the
  `BundleRepo::archive_in_tx` / `delete_in_tx` verbs, and the new
  `CommitTarget` variants; the three-phase gate protocol is unchanged
  (the gate gains a `&dyn BundleRepo` parameter on `finalize` then).
- No filesystem-aware recovery of stuck `commit_intents` rows. Sprint 1
  ships the durable intent table, the prepare / authorize / finalize /
  abort / list API, and the `commit.recovery_required` event kind; the
  reconciliation worker that inspects filesystem state and decides
  whether to roll forward or roll back lives with the real workers
  (Sprint 5+).
