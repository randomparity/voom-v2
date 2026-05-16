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
- the Commit Safety Gate (affected-scope closure across alias
  `FileLocation`s, fail-closed when alias resolution is incomplete,
  evidence revalidation, lease re-anchoring on rename/move, force-path
  semantics that never bypass evidence revalidation), including the
  pending-commit lock that serializes new use-lease acquires and
  alias-attaching `IdentityRepo` mutations against an in-flight
  destructive commit (§9.1, §9.2, §8.7, §9.3)
- `IdentityRepo::reconcile_rename` extended to re-anchor any non-terminal
  blocking and advisory leases scoped to the retired `FileLocation` to the
  new `FileLocation` inside the same transaction (preserving `lease_id`,
  `issuer`, `acquired_at`, `expires_at`, `last_heartbeat_at`,
  `blocking_mode`)
- the ancillary registries (external systems, issues, quality scores) as
  CRUD repos
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
| `voom-core` | New ID newtypes (see §12). New `ErrorCode` variants and `VoomError` cases. Test-only `FrozenClock` / `ManualClock` exposed via `voom-core::clock_test_support`. |
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
three entry points — `prepare_destructive_commit`,
`finalize_destructive_commit`, and `abort_destructive_commit` — owns
its own IMMEDIATE transaction internally and accepts `&dyn EventRepo`
(and `finalize` additionally `&dyn IdentityRepo`) so it can interleave
closure reads, evidence revalidation, the durable identity mutation,
and event writes in the precise order the architectural spec mandates.
The filesystem mutation supplied by the caller runs **between**
`prepare` and `finalize`, outside any DB transaction; the
`commit_intents` journal is what makes the two-phase split safe across
caller crashes.

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
- `commit_safety_gate` module → no repo trait; exposes the two-phase
  protocol `prepare_destructive_commit(...)`,
  `finalize_destructive_commit(...)`, `abort_destructive_commit(...)`,
  and `list_pending_commit_intents(...)` against the `commit_intents`
  table. Also owns `commit_intent_scope_members` — the per-closure-
  member rows that back the pending-commit lock consulted by
  `UseLeaseRepo::acquire_in_tx` and `IdentityRepo`'s alias-attaching
  paths (§9.1, §9.2, §8.7)
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
    CommitCompleted,                    // 'commit.completed' — Phase B success
    CommitAbortedByUseLease,            // 'commit.aborted_by_use_lease' — Phase A
    CommitAbortedByStaleEvidence,       // 'commit.aborted_by_stale_evidence' — Phase A
    CommitAbortedByClosureIncomplete,   // 'commit.aborted_by_closure_incomplete' — Phase A
    CommitAbortedPreMutation,           // 'commit.aborted_pre_mutation' — Phase C / Phase B NotPerformed
    CommitAbortedPostMutation,          // 'commit.aborted_post_mutation' — Phase B trip-wire (reason: 'closure_grew' | 'fresh_lease' | 'closure_grew_and_fresh_lease')
    CommitRecoveryRequired,             // 'commit.recovery_required' — emitted by the Sprint 5+ recovery worker
    CommitForcedOverride,               // 'commit.forced_override' — Phase A closure-bypass audit
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
| `next_eligible_at` | TEXT NOT NULL | ISO-8601; used for backoff after retriable failure. New tickets default to `created_at`; `LeaseRepo::fail(retriable=true)` sets it to `now + backoff(attempt)` |
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
- `leased` → `ready` via `LeaseRepo::fail` when `retriable && ticket.attempt < ticket.max_attempts`. `next_eligible_at` is set to `now + backoff(attempt)`. `attempt` is **not** bumped here — the bump happens on the next `acquire`. Sprint 1 uses a fixed backoff (5s × attempt); the policy will live in `voom-scheduler` later.
- `leased` → `failed` via `LeaseRepo::fail` otherwise.
- `leased` → `ready` via `LeaseRepo::expire_due` if retries remain (`ticket.attempt < ticket.max_attempts`); `leased` → `failed` otherwise. Same convention: no bump on requeue; the next `acquire` increments.

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
- `heartbeat(lease_id) -> Result<Lease>` — assert `state = 'held'`, set
  `last_heartbeat_at = now`, `expires_at = now + ttl`. No event in
  Sprint 1 (Sprint 6 daemon may emit a recovery event after a previously
  missed beat).
- `release(lease_id, ResultPayload) -> Result<()>` — assert `state = 'held'`,
  transition lease to `released`, transition ticket to `succeeded`, write
  `result` JSON. Use case emits `ticket.succeeded` + `lease.released`,
  then calls `TicketRepo::mark_ready_if_unblocked` for every dependent ticket.
- `fail(lease_id, FailureReason, retriable: bool) -> Result<()>` — assert
  `state = 'held'`. If `retriable && ticket.attempt < ticket.max_attempts`,
  transition ticket to `ready`, set `next_eligible_at = now + backoff(attempt)`,
  do **not** bump `attempt`; emit `ticket.failed_retriable`. Else transition
  ticket to `failed`; emit `ticket.failed_terminal`. Lease transitions to
  `released` with `release_reason = 'failed_retriable' | 'failed_terminal'`;
  emit `lease.released`.
- `expire_due(now) -> Result<ExpireReport>` — bulk: find leases with
  `state = 'held' AND expires_at < now`. Per row: transition lease to
  `expired` (`release_reason = 'issuer_lost'`, `released_at = now`),
  transition ticket to `ready` if retries remain
  (`ticket.attempt < ticket.max_attempts`) or `failed` otherwise. Same
  convention as `fail`: do **not** bump `attempt` on requeue — `acquire`
  will bump on the next dispatch. Emit `lease.expired` +
  `ticket.requeued_after_lease_expiry` or `ticket.failed_terminal` per row.
  Returns `ExpireReport { expired_leases: Vec<LeaseId>, requeued_tickets: Vec<TicketId>, failed_tickets: Vec<TicketId> }`.
- `force_release(lease_id, actor, reason, also_requeue: bool) -> Result<()>` —
  admin/test path: transition lease to `force_released`, ticket to
  `ready` if `also_requeue` else `failed`. Emit `lease.force_released`
  with `{ actor, reason }` in the payload.

**Worked example: `max_attempts = 2`.** This sequence yields the expected
two dispatched attempts before terminal failure:

1. Initial: `attempt = 0`, `state = ready`.
2. `acquire` → `attempt = 1`, `state = leased`.
3. `fail(retriable = true)` → `attempt = 1`, `state = ready` (1 < 2).
4. `acquire` → `attempt = 2`, `state = leased`.
5. `fail(retriable = true)` → `attempt = 2`, `state = failed` (2 < 2 is
   false, so the ticket transitions to terminal failure).

Total dispatched attempts: 2. The same convention applies to
`expire_due` in place of `fail`.

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
        prior_location_id: FileLocationId,
        new_kind: FileLocationKind,
        new_value: String,
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
  alias proof and produce identical behavior.
- **`alias_proof = Some(LocalFileIdGeneration { … })`** → validate: prior
  location is live (`retired_at IS NULL`), its `proof_kind = 'file_id_generation'`,
  its `proof_value` parses to a `(file_id, generation)` matching what the
  proof carries, and the existing `FileVersion.content_hash` and
  `size_bytes` match `discovered`. On match → `AliasAttached`. On any
  mismatch → `NewFileAsset` + `identity_evidence(path_rule_match)`. Inode
  alone (a `file_id` reuse after delete/recreate without generation match)
  is **not** sufficient — the spec is explicit.
- **`alias_proof = Some(ObjectStoreVersion { … })`** → analogous: prior
  location's `proof_kind = 'object_version_id'`, `proof_value` matches the
  `(bucket, key, version_id)` triple, hash and size match. On match →
  `AliasAttached`. On any mismatch → `NewFileAsset` +
  `identity_evidence(path_rule_match)` and (if hash matches an existing
  version) also `identity_evidence(hash_match)`.

The repo never produces `RenameReconciled` from `record_discovered_file`;
that outcome is reserved for `reconcile_rename`. Identity collapse via a
`merge` operation does not exist in Sprint 1.

**Pending-commit lock consultation (alias-attach branch only).**
The `AliasAttached` branch is the one outcome that enlarges an existing
closure — it attaches a new `FileLocation` to a `FileVersion` that
might already be inside the closure of an in-flight destructive
commit. Before inserting the new `file_location` row, the repo
consults `commit_intent_scope_members` against the resolved
`file_version_id` (and, transitively, the parent `file_asset_id` and
any bundle membership the version inherits — the lock query is the
same UNION-of-FK-columns shape as §9.2):

```sql
SELECT csm.commit_intent_id
  FROM commit_intent_scope_members csm
  JOIN commit_intents ci ON ci.id = csm.commit_intent_id
 WHERE ci.state = 'pending'
   AND csm.scope_version_id = :file_version_id
 LIMIT 1;
```

If any row returns → `VoomError::BlockedByPendingCommit(...)` →
`ErrorCode::BlockedByPendingCommit` (§12.1). The `NewFileAsset`
outcome does **not** consult the lock — a newly-discovered file asset
is by definition not in any pre-existing closure. Only `AliasAttached`
enlarges an existing closure and so only `AliasAttached` needs the
guard.

Behavior of `reconcile_rename_in_tx` (M2 form):

- Look up `prior_location_id`; assert it's live; bind its `FileVersion`.
- Consult the pending-commit lock against the bound `FileVersion`
  (same query shape as the alias-attach guard above, filtered on
  `csm.scope_version_id = :file_version_id`). If any pending intent
  covers the version → return
  `VoomError::BlockedByPendingCommit(...)` →
  `ErrorCode::BlockedByPendingCommit` (§12.1). A rename that retires
  the prior `FileLocation` and records a new one mutates the closure
  of any in-flight destructive commit on that version; the lock
  serializes the rename against the commit so the closure cannot
  shift mid-prepare.
- Retire the prior location (`retired_at = observed_at`).
- Insert a new `FileLocation` on the same `FileVersion` with the new
  `kind` and `value`.
- Emit `file_location.retired_by_move` and `file_location.recorded_by_move`
  events with payload referencing both IDs.
- Append a new `identity_evidence` row (assertion `PathRuleMatch`)
  observing the new location.

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
two-phase destructive-commit protocol (§9.3.1). One row per attempted
destructive commit, recording the closure and evidence the operator
declared at prepare time so that the recovery path can reason about
intents whose callers crashed between `prepare` and `finalize`:

```sql
CREATE TABLE commit_intents (
    id                     INTEGER PRIMARY KEY,
    target                 TEXT NOT NULL,    -- JSON CommitTarget
    closure_initial        TEXT NOT NULL,    -- JSON AffectedScopeClosure
    accepted_evidence_ids  TEXT NOT NULL,    -- JSON array of evidence IDs
    state                  TEXT NOT NULL,    -- 'pending' | 'completed' | 'aborted' | 'recovery_required'
    started_at             TEXT NOT NULL,
    finalized_at           TEXT,
    aborted_at             TEXT,
    abort_reason           TEXT,             -- 'mutation_failed' | 'closure_grew' | 'fresh_lease' | 'operator_cancel' | ...
    epoch                  INTEGER NOT NULL DEFAULT 0,
    CHECK (
           (state = 'pending'           AND finalized_at IS NULL     AND aborted_at IS NULL     AND abort_reason IS NULL)
        OR (state = 'completed'         AND finalized_at IS NOT NULL AND aborted_at IS NULL     AND abort_reason IS NULL)
        OR (state = 'aborted'           AND finalized_at IS NULL     AND aborted_at IS NOT NULL AND abort_reason IS NOT NULL)
        OR (state = 'recovery_required' AND finalized_at IS NULL     AND aborted_at IS NULL     AND abort_reason IS NULL)
    )
) STRICT;

CREATE INDEX commit_intents_pending
  ON commit_intents (state, started_at) WHERE state = 'pending';
```

The CHECK uses an exclusive-shape encoding: each `state` value owns the
full column shape, so contradictory rows (e.g., both `finalized_at` and
`aborted_at` non-null, or a stale `abort_reason` left over on a
`recovery_required` row) are unrepresentable. `recovery_required` keeps
`abort_reason IS NULL` deliberately: the *reason* the post-mutation
trip-wire fired (closure grew vs. fresh lease vs. lock bypass) is
recorded in the corresponding `commit.aborted_post_mutation` event
payload (§9.3.2 Phase B), not on the intent row, so the reason has a
single source of truth.

The override-token audit payload (`actor`, `reason`, `bypass`) is
captured in the `commit.intent_recorded` event written alongside the
row at prepare time; the table itself stores only the durable state
the recovery path needs.

#### `commit_intent_scope_members`

A `commit_intents` row in `state = 'pending'` acts as an
application-level reservation: while the row lives, no new blocking
or advisory `asset_use_lease` may be acquired and no new
`FileLocation` may be attached on its closure. The closure is recorded
in `commit_intents.closure_initial` as JSON for audit, but JSON is
opaque to SQL and to the lease-acquire fast path. `commit_intent_scope_members`
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
inside the Phase A IMMEDIATE transaction (§9.3.2). The
lock-consultation query in `UseLeaseRepo::acquire_in_tx` (§9.2) and in
`IdentityRepo::record_discovered_file_in_tx` /
`IdentityRepo::reconcile_rename_in_tx` (§8.7) is the same shape: any
match against a row whose parent intent is in `state = 'pending'`
returns `VoomError::BlockedByPendingCommit(...)` (§12.1) and the
caller's mutation is rejected before it lands. Because both phases run
under IMMEDIATE transactions, the SQLite write-lock serializes them:
whichever transaction commits first wins, the other observes the
committed row and rejects with no race window.

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
   WHERE ci.state = 'pending'
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
  `commit_id` and the matched scope. The check applies to both
  blocking and advisory leases — the architectural spec does not carve
  out advisory, and a destructive commit serializes against every
  in-scope use-lease acquire. Because `prepare_destructive_commit` and
  `acquire` both run under IMMEDIATE transactions, the SQLite write-
  lock serializes them: whichever commits first wins, the other
  observes the committed row and rejects. Emits `use_lease.acquired`.
- `heartbeat(lease_id) -> Result<UseLease>` — TTL-bound only. Bumps
  `last_heartbeat_at` and `expires_at = now + ttl`. Manual locks reject
  heartbeat with `ErrorCode::Conflict` (message: "manual locks do not
  heartbeat"). No event emitted in Sprint 1; Sprint 6 daemon may add a
  conditional "recovered after missed beat" event when it owns the
  missed-heartbeat-warning path.
- `release(lease_id, reason)` — explicit release. Accepted reasons:
  `released`, `force_released`, `superseded`, `issuer_lost`. Manual locks
  accept `released`, `force_released`, or `issuer_lost` (the
  stale-owner-recovery path; see `recover_stale_issuer`). Transitions to
  terminal, sets `released_at = now`. Emits `use_lease.released` (or
  `use_lease.force_released` carrying the actor + reason payload required
  for audit).
- `expire_due(now) -> Result<ExpireReport>` — bulk: find non-terminal
  TTL-bound leases with `expires_at < now`. Per row: transition to
  `release_reason = 'expired'`, `released_at = now`. Manual locks are
  filtered out (`ttl_bound = 0`). Emits `use_lease.expired` per row.
- `recover_stale_issuer(lease_id, actor, reason) -> Result<()>` —
  manual-lock-specific path. Caller passes `actor` and `reason`. Transitions
  lease to `release_reason = 'issuer_lost'`, `released_at = now`. Emits
  `use_lease.recovered_stale_issuer` with `{ actor, reason }` payload.
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

The gate exposes a two-phase prepare / finalize / abort protocol. The
architectural spec's host-owned-commit invariant requires that "a
worker crash does not leave the control plane believing a final
mutation succeeded"; a single transaction wrapped around a
caller-supplied filesystem mutation cannot honor this, because a DB
rollback after the filesystem bytes are already changed leaves the
durable state lagging behind reality. The two-phase API journals a
durable `commit_intents` row at prepare time so that the recovery path
(Sprint 5+) can reason about callers that crashed between phases.

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
    ArchiveBundle(BundleId),
    DeleteBundle(BundleId),
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
}

pub enum MutationOutcome {
    /// Caller performed the filesystem mutation and it is durable on
    /// disk. Optionally carries the observed post-mutation closure if
    /// the caller's mutation touched aliases the gate could not see.
    Applied { observed: Option<AffectedScopeClosure> },
    /// Caller decided not to mutate; `finalize_destructive_commit`
    /// transitions the intent to `aborted` with reason `operator_cancel`.
    NotPerformed,
}

pub struct CommitGateOutcome {
    pub commit_id: CommitId,
    pub closure_initial: AffectedScopeClosure,
    pub closure_final:   AffectedScopeClosure,
    pub evaluated_lease_ids: Vec<UseLeaseId>,
    pub revalidated_evidence: Vec<EvidenceRevalidationResult>,
    pub result: CommitGateResult,
}

pub enum CommitGateResult {
    Allowed,
    BlockedByUseLease           { lease_id: UseLeaseId, lease_scope: LeaseScope },
    BlockedByStaleEvidence      { evidence_id: EvidenceId, drift: EvidenceDrift },
    BlockedByClosureIncomplete  { reason: ClosureFailure, unreachable: Vec<ClosureWarning> },
    /// Phase B trip-wire: between `prepare` and `finalize` the
    /// `AliasResolver` discovered closure members that were not
    /// present in `closure_initial` (and so were never covered by
    /// evidence revalidation or the lease check). Carries the delta
    /// across the four granularities so the caller and Sprint 5+
    /// recovery worker can see what grew. Only ever returned from
    /// `finalize_destructive_commit`; Phase A's incomplete-closure
    /// abort uses `BlockedByClosureIncomplete` instead.
    BlockedByClosureGrew {
        added_assets:    Vec<FileAssetId>,
        added_bundles:   Vec<BundleId>,
        added_versions:  Vec<FileVersionId>,
        added_locations: Vec<FileLocationId>,
    },
}

pub struct PendingCommitIntent {
    pub commit_id: CommitId,
    pub target: CommitTarget,
    pub closure_initial: AffectedScopeClosure,
    pub accepted_evidence_ids: Vec<EvidenceId>,
    pub started_at: OffsetDateTime,
}

pub enum AbortReason {
    OperatorCancel,
    MutationFailed,
    ClosureGrew,
    FreshLease,
    Other(String),
}

pub async fn prepare_destructive_commit(
    pool: &SqlitePool,
    alias_resolver: &dyn AliasResolver,
    event_repo: &dyn EventRepo,
    input: DestructiveCommit,
) -> Result<CommitIntent, VoomError>;

pub async fn finalize_destructive_commit(
    pool: &SqlitePool,
    alias_resolver: &dyn AliasResolver,
    event_repo: &dyn EventRepo,
    identity_repo: &dyn IdentityRepo,
    commit_id: CommitId,
    outcome: MutationOutcome,
) -> Result<CommitGateOutcome, VoomError>;

pub async fn abort_destructive_commit(
    pool: &SqlitePool,
    event_repo: &dyn EventRepo,
    commit_id: CommitId,
    reason: AbortReason,
) -> Result<(), VoomError>;

pub async fn list_pending_commit_intents(
    pool: &SqlitePool,
    older_than: Option<OffsetDateTime>,
) -> Result<Vec<PendingCommitIntent>, VoomError>;
```

The caller's filesystem mutation runs **between** `prepare` and
`finalize`, outside any DB transaction. Mutations must be idempotent
or staged so that crash-and-retry yields the same end state — the
architectural spec's staged-artifact + rollback-metadata requirements
apply unchanged. `finalize_destructive_commit` takes
`&dyn IdentityRepo` because the durable identity mutation that gives
the closure check its meaning (e.g.,
`IdentityRepo::retire_file_location_in_tx` for a `DeleteFileLocation`
target) runs inside the same finalize transaction.

`prepare_destructive_commit` is also the sole writer of
`commit_intent_scope_members` (§9.1): inside its Phase A IMMEDIATE
transaction it inserts one `commit_intents` row plus one
`commit_intent_scope_members` row per element of `closure_initial`,
expanded across all four granularities. Those rows are the durable
backing-store for the pending-commit lock that `UseLeaseRepo::acquire`
(§9.2) and `IdentityRepo`'s alias-attaching paths (§8.7) consult
before mutating. The lock is an implementation detail behind the
gate — no API signature changes, no new field on `CommitIntent`. The
`commit.intent_recorded` event payload already carries
`closure_initial`, which is the source of truth for the
`scope_members` rows, so audit can reconstruct the lock from events
without a separate field. The new error `BlockedByPendingCommit`
(§12.1) shows up on the existing `Result<_, VoomError>` returns of
the methods that consult the lock; no new struct in the gate API.
Structured detail (which `commit_intent_scope_members` row matched)
lives on a `BlockedByPendingCommitDetail` struct in
`voom-store::repo::commit_safety_gate`, parallel to the existing
`CommitGateResult`, `LeaseScope`, `EvidenceDrift`, `ClosureWarning`
types, for Sprint 9 report consumers.

### 9.3.2 Algorithm

The gate runs in three phases. Phase A and Phase C are each one
IMMEDIATE transaction; Phase B is also one IMMEDIATE transaction and
is what makes the closure recheck and the durable mutation atomic with
respect to the intent transition.

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
     `commit.aborted_by_closure_incomplete` (Phase A abort, see §9.3.5);
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
   (Phase A abort); return. This check has **no force bypass**
   (§9.3.3).
3. Revalidate every accepted evidence row in `input.accepted_evidence_ids`:
   compare `pinned_file_version_ids` against current `FileVersion` IDs
   of the scope, `pinned_hashes` against current `content_hash` values,
   and `pinned_locations` against current live locations. Any drift →
   `BlockedByStaleEvidence`; emit `commit.aborted_by_stale_evidence`
   (Phase A abort); return. Accepted-evidence rows are not rewritten —
   drift forces the caller to re-collect and re-accept. **The force
   path never bypasses evidence revalidation.**
4. Insert a `commit_intents` row with `state = 'pending'`,
   `target = input.target`, `closure_initial`, `accepted_evidence_ids`,
   and `started_at = now`. Inside the same IMMEDIATE transaction, expand
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
   `pending`, no new use-lease acquire and no new alias-attaching
   `IdentityRepo` mutation can touch the closure (§9.2, §8.7).
5. Return `CommitIntent`. The caller now performs the filesystem
   mutation outside any DB transaction. The architectural spec's
   staged-artifact + rollback-metadata requirements apply: mutations
   must be idempotent or staged so that crash-and-retry yields the
   same end state.

#### Phase B — `finalize_destructive_commit`

Called once the caller's filesystem mutation is durable on the
filesystem (or the caller has decided not to mutate; see
`MutationOutcome::NotPerformed`). One IMMEDIATE transaction.

1. Read the `commit_intents` row for `commit_id`; require
   `state = 'pending'`. Missing row, terminal state, or `epoch`
   mismatch → return `Conflict` without writing.
2. If `outcome == MutationOutcome::NotPerformed`: transition the
   intent to `state = 'aborted'`, `aborted_at = now`, and
   `abort_reason = 'operator_cancel'`. Emit
   `commit.aborted_pre_mutation`. COMMIT. This is the same outcome
   the caller would get from `abort_destructive_commit` (Phase C);
   the two entry points exist so callers that take the
   prepare-then-decide-not-to-mutate path can finalize cleanly
   without a second API call.
3. Otherwise (`outcome == MutationOutcome::Applied { observed }`):
   recompute `closure_final` against the current DB state and the
   `AliasResolver`. The intent's `closure_initial` is the baseline;
   the architectural spec's two-pass closure recheck rule is "pick
   up any FileLocations that have been attached as aliases of
   in-scope FileVersions since the initial gate check." If the
   caller passed `observed`, merge it into `closure_final`. Compute
   the delta between `closure_final` and `closure_initial` across all
   four granularities, then re-evaluate the blocking-lease query from
   Phase A step 2 against `closure_final`.

   With the pending-commit lock in place (§9.1, §9.2, §8.7), both
   subchecks are a **defensive trip-wire**, not the primary protection.
   Under normal operation the lock makes it impossible for a new
   use-lease or a new alias `FileLocation` to appear on the closure
   between Phase A commit and Phase B start, so the delta should be
   empty and the lease query should return no rows. Firing the
   trip-wire indicates a lock-bypass or closure-completeness escape
   that needs investigation. Three known escape paths:

   - A bug in the lock-consultation logic in `UseLeaseRepo::acquire_in_tx`
     or `IdentityRepo`'s alias-attaching paths.
   - An external SQL writer that bypassed the repos (e.g., a manual
     `INSERT INTO asset_use_leases` run against the DB file).
   - An `AliasResolver` that returned `Complete` for Phase A but newly
     discovers an alias by Phase B (e.g., a remote mount came online
     between phases, an object-store probe succeeded on retry).

   The two subchecks may fire independently or together. The
   `commit.aborted_post_mutation` event payload is uniform across all
   trip-wire firings — it always carries both the closure delta and
   the fresh-lease list, with empty arrays for the dimension that
   didn't escape — so the durable audit record preserves every escape
   the gate observed, regardless of which `CommitGateResult` variant
   the caller receives:

   ```text
   payload = {
       commit_id,
       reason,                            -- 'closure_grew' | 'fresh_lease' | 'closure_grew_and_fresh_lease'
       escape,                            -- which trip-wire path the gate suspects
       closure_initial,
       closure_final,
       delta_assets,                      -- possibly empty
       delta_bundles,                     -- possibly empty
       delta_versions,                    -- possibly empty
       delta_locations,                   -- possibly empty
       fresh_lease_ids,                   -- possibly empty
   }
   ```

   The two subcheck results map to `CommitGateResult` as follows:

   - **Closure grew** (delta non-empty, no fresh lease): the gate's
     understanding of which files the commit would affect was wrong;
     closure members that grew the closure (e.g., a newly-discovered
     alias `FileLocation`) were never covered by evidence revalidation
     or the Phase A lease check, so the mutation's safety properties
     no longer hold even if no lease references those new members.
     Emit `commit.aborted_post_mutation` with `reason='closure_grew'`
     and `fresh_lease_ids=[]`, transition the intent to
     `state = 'recovery_required'` (leaving `finalized_at`,
     `aborted_at`, and `abort_reason` all NULL per the §9.1 CHECK), do
     **not** apply the durable mutation, COMMIT, return
     `CommitGateResult::BlockedByClosureGrew { added_assets,
     added_bundles, added_versions, added_locations }`.
   - **Fresh blocking lease** (delta empty, but a blocking lease now
     covers `closure_final` whose ID is not in
     `evaluated_lease_ids`): a lease appeared inside the FS-mutation
     window. Emit `commit.aborted_post_mutation` with
     `reason='fresh_lease'` and empty `delta_*`, transition the intent
     to `state = 'recovery_required'` (same NULL-shape as above), do
     **not** apply the durable mutation, COMMIT, return
     `CommitGateResult::BlockedByUseLease { lease_id, lease_scope }`
     naming the first such lease (deterministic by `lease_id`).
   - **Both fire** (delta non-empty **and** a fresh lease covers the
     enlarged closure): emit one `commit.aborted_post_mutation` event
     with `reason='closure_grew_and_fresh_lease'`, **both** populated
     `delta_*` arrays **and** populated `fresh_lease_ids` — so the
     recovery worker and audit see every escape, not just one. The
     intent transition is the same as the other branches. Return
     `CommitGateResult::BlockedByClosureGrew { added_* }` (closure
     growth is the more fundamental escape — the fresh-lease check
     would have been re-evaluated against the wrong baseline anyway).
     The returned variant is a compact summary of the most actionable
     escape; the event payload is the full durable evidence and is
     what Sprint 5+ recovery and Sprint 9 audit consult.

   In every trip-wire branch the filesystem mutation has already
   happened, so the durable state lags behind reality; the
   `recovery_required` state flags this intent for the Sprint 5+
   recovery worker. The trip-wire reason has a single source of truth
   on the event payload, not the intent row.
4. Otherwise (no fresh blocking lease appeared): apply the matching
   durable mutation via the identity/bundle repos inside this same
   transaction (e.g., `IdentityRepo::retire_file_location_in_tx` for
   a `DeleteFileLocation` target, `BundleRepo::archive_in_tx` for an
   `ArchiveBundle` target). The durable mutation is what makes the
   closure check meaningful — it must run inside the same tx as the
   recheck and the intent transition.
5. Update the `commit_intents` row to `state = 'completed'` and
   `finalized_at = now` (with epoch bump). Emit `commit.completed`
   with payload `{ commit_id, target, closure_initial, closure_final,
   evaluated_lease_ids, revalidated_evidence }`. COMMIT. Return
   `CommitGateOutcome { result: Allowed, ... }`.

#### Phase C — `abort_destructive_commit`

Called when the caller decides not to perform the filesystem mutation.
One IMMEDIATE transaction.

1. Read the `commit_intents` row for `commit_id`; require
   `state = 'pending'`. Missing row, terminal state, or `epoch`
   mismatch → return `Conflict`.
2. Update to `state = 'aborted'`, `aborted_at = now`, and
   `abort_reason = reason`. Emit `commit.aborted_pre_mutation` with
   the reason in the payload. COMMIT.

#### Recovery contract

`list_pending_commit_intents(older_than)` returns intents in
`state = 'pending'` optionally older than a threshold. A stuck pending
intent indicates the caller crashed between Phase A and Phase B
finalize. Sprint 1 ships:

- the `commit_intents` table,
- the prepare / finalize / abort / list functions,
- the `commit.recovery_required` event kind, which the Sprint 5+
  recovery worker emits as it reconciles a stuck intent against
  filesystem state.

Sprint 1 does **not** ship the filesystem-aware reconciliation worker
itself — that lives with the real workers (Sprint 5+), and §15 records
the deferral.

### 9.3.3 Force path

`override_token: Some(ForcePathToken)` is a separately audited path the
spec mandates ("Operators who need to commit despite incomplete resolution
use a separately audited, permissioned force path that records its own
override event and reason"). The `ForcePathToken` carries `actor`,
`reason`, and a `bypass` bitset declaring which checks are skipped. The
**only** allowed bypass kind is:

- `closure_incomplete` — skip the closure-resolution abort the gate
  raises when `AliasResolver` fails or returns
  `AliasResolutionError::Unreachable`.

The architectural spec scopes the force path to incomplete closure
resolution specifically. Fresh blocking use-leases are **not**
force-bypassable — they always fail the gate. A token whose `bypass`
set carries `blocked_by_use_lease` is rejected before the gate runs
(`VoomError::Config`). Likewise `stale_evidence` is not a valid bypass:
stale evidence is a correctness problem, not a permissions problem.

**Operator workflow when a blocking lease must be cleared.** An
operator who needs a destructive commit to proceed while a blocking
`asset_use_lease` exists must first terminate that lease through the
audited path:

```
UseLeaseRepo::release(lease_id, reason = force_released, actor, reason)
```

That call writes its own `use_lease.force_released` event recording
`{ actor, reason }`. Once the lease is terminal, the gate's
blocking-lease check sees no live lease on the scope, and the operator
reruns `prepare_destructive_commit` / `finalize_destructive_commit`
without a `blocked_by_use_lease` bypass. The gate itself is unchanged
between attempts — the audit trail lives on the lease release, not on
the commit.

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
(pre-mutation): each `Blocked*` branch in Phase A emits its
`commit.aborted_by_*` event before ROLLBACK and survives the abort
via a follow-up tiny transaction that writes the event row. Phase A
abort events have no other durable state to atomically commit
alongside them, so the two-tx pattern is correct.

Phase B (finalize) does not use the two-tx pattern: the
`commit.aborted_post_mutation` event is written inside the finalize
transaction itself, which always commits the intent-state transition
to `recovery_required`. What the post-mutation abort skips is the
durable identity mutation (step 4); the intent row update, the
`recovery_required` transition, and the event row commit together as
one atomic record of "the FS mutation happened but the closure check
fell behind." Phase C aborts similarly write
`commit.aborted_pre_mutation` inside the abort transaction.

### 9.4 Rename reconciliation × evidence revalidation

The spec's most subtle interaction: `reconcile_rename_in_tx` re-anchors
**leases** to the new location, but does **not** rewrite accepted
**evidence** (whose `pinned_locations` array still names the retired
location). A later destructive commit acting on that pinned evidence
will hit `BlockedByStaleEvidence` because `pinned_locations` no longer
matches the current state, and must re-collect & re-accept. The
integration test `commit_safety_gate_after_rename.rs` covers this
sequence end-to-end.

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
| `kind` | TEXT NOT NULL | `unknown_identity` \| `missing_subtitle` \| `duplicate_candidate` \| `policy_noncompliant` \| `health_failed` \| `external_sync_failed` \| `artifact_unavailable` \| `variant_retention_conflict` \| `worker_untrusted` |
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
| `link_type` | TEXT NOT NULL | `evidence` \| `file_asset` \| `bundle` \| `worker` \| `external_system` \| `ticket` \| `use_lease` |
| `target_type` | TEXT NOT NULL | |
| `target_id` | INTEGER NOT NULL | |
| `created_at` | TEXT NOT NULL | |

`IssueRepo` operations:

- `open(NewIssue) -> IssueId` → `issue.opened`
- `reprioritize(issue_id, priority, source, reason)` → `issue.priority_changed`
- `resolve(issue_id)` → `issue.resolved`
- `suppress(issue_id, until)` → `issue.suppressed`
- `accept(issue_id, actor)` → `issue.accepted`
- `link(issue_id, link)` → `issue.linked`

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
voom commit-intent  list [--state pending|completed|aborted|recovery_required] [--older-than] [--limit] [--cursor]
                    get  <id>                       (returns scope_members inline)
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
  `PendingCommitIntent` shape (fixture inserts a pending intent via
  the gate's `prepare` API)
- `voom commit-intent get <id>` for a pending intent, asserting the
  `scope_members` array is rendered inline so operators can see what
  the intent is currently blocking (asset / bundle / version /
  location members)
- The Sprint-1-specific error envelopes for `BLOCKED_BY_USE_LEASE`,
  `BLOCKED_BY_PENDING_COMMIT`, `STALE_IDENTITY_EVIDENCE`,
  `CLOSURE_RESOLUTION_INCOMPLETE`, `DEPENDENCY_CYCLE`, and `CONFLICT`,
  shaped via fixture insertion + forced invocation of the relevant
  control-plane use case.

## 12. Cross-cutting Concerns

### 12.1 Error codes

`voom-core::error::ErrorCode` gains:

- `BlockedByUseLease` → `"BLOCKED_BY_USE_LEASE"`
- `BlockedByPendingCommit` → `"BLOCKED_BY_PENDING_COMMIT"`
- `StaleIdentityEvidence` → `"STALE_IDENTITY_EVIDENCE"`
- `ClosureResolutionIncomplete` → `"CLOSURE_RESOLUTION_INCOMPLETE"`
- `DependencyCycle` → `"DEPENDENCY_CYCLE"`
- `Conflict` → `"CONFLICT"`

`VoomError` gains matching `(String)`-tuple variants — matching Sprint 0's
pattern. The message text carries the human-readable context (lease ID,
evidence drift summary, blocking `commit_id` plus scope type and id for
`BlockedByPendingCommit`, etc.); structured detail for Sprint 9's reports
lives on the gate-result types in `voom-store::repo::commit_safety_gate`
(`CommitGateResult`, `LeaseScope`, `EvidenceDrift`, `ClosureWarning`,
and `BlockedByPendingCommitDetail` for the pending-commit-lock matches
described in §9.1, §9.2, §8.7), which the control-plane use cases
consult before mapping to `VoomError`.

```rust
pub enum VoomError {
    /* ... Sprint 0 variants ... */
    BlockedByUseLease(String),
    BlockedByPendingCommit(String),
    StaleIdentityEvidence(String),
    ClosureResolutionIncomplete(String),
    DependencyCycle(String),
    Conflict(String),
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
- `NodeId`, `ArtifactHandleId`, `ArtifactLocationId`, `UseLeaseId`, `CommitId`

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

## 13. Testing Strategy

Sprint 0 ADR 0004 established sibling unit tests (`foo.rs` + `foo_test.rs`)
with `#[path]`-linked `mod tests`. Sprint 1 follows verbatim.

### 13.1 Sibling unit tests

Each new repo source file ships its sibling `_test.rs` covering:

- the repo's pure-SQL operations against a `:memory:` pool via
  `voom-store::test_support::test_pool()`
- the optimistic-locking conflict path (`Conflict` returned on stale epoch)
- one happy-path event emission per public write method

### 13.2 Integration tests (`crates/voom-store/tests/`)

Cross-repo flows live as integration tests:

- `ticket_lease_lifecycle.rs` — ticket pending → ready (no deps) → leased
  → heartbeat (multiple) → expired (via `expire_due`) → requeued → leased
  → succeeded. Asserts every event row. Also covers attempt accounting:
  with `max_attempts = 2`, exercises the §7.5 worked example end-to-end
  through `fail(retriable = true)` (two dispatched attempts before
  `ticket.failed_terminal`) and through `expire_due` (same convention);
  with `max_attempts = 3`, exercises a mixed `fail` + `expire_due`
  sequence and asserts the final state matches the documented semantics.
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
- `rename_reconciliation.rs` — `reconcile_rename` retires old + records
  new on the same `FileVersion`; M3 step re-anchors blocking, advisory,
  and manual leases; pinned evidence is preserved (not rewritten); new
  evidence appended; events emitted.
- `commit_safety_gate.rs` — covers each abort path under the two-phase
  prepare/finalize/abort API (§9.3.1): `BlockedByUseLease` (blocking
  lease scoped to closure member, raised in `prepare`), `BlockedByStaleEvidence`
  (pinned hash mismatch, raised in `prepare`), `BlockedByClosureIncomplete`
  (via `FailingAliasResolver`, raised in `prepare`); allowed path with
  successful mutation through `prepare` → caller mutation → `finalize`;
  the `abort_destructive_commit` path when the caller decides not to
  mutate; pending intent visible via `list_pending_commit_intents`;
  `recovery_required` state for a stuck intent whose caller never
  finalized. Force-path coverage: `closure_incomplete` bypass is
  honored in `prepare`; an `override_token` whose `bypass` set carries
  `blocked_by_use_lease` is rejected at the gate boundary
  (`VoomError::Config`); a `stale_evidence` bypass is rejected the same
  way; a separate test exercises the documented operator workflow
  ("force-release the blocking lease via `UseLeaseRepo::release(reason
  = force_released)`, then rerun the gate") and asserts the second
  attempt is allowed without any bypass token.

  Pending-commit-lock coverage (the primary protection introduced by
  §9.1, §9.2, §8.7):
  - A pending intent over `FileLocation` Y blocks a subsequent
    `UseLeaseRepo::acquire(LeaseScope::Location(Y))` with
    `BlockedByPendingCommit`.
  - The same intent blocks
    `UseLeaseRepo::acquire(LeaseScope::Asset(X))` when Y's parent
    asset is X (the asset-granularity `scope_members` row matches,
    because Phase A walks closure across all four granularities).
    Analogous cases for `LeaseScope::Bundle` and
    `LeaseScope::Version` exercise the other two granularities.
  - The same intent blocks
    `IdentityRepo::record_discovered_file_in_tx` (alias-attach
    branch) for an alias whose `alias_proof` resolves to an
    in-closure `FileVersion`. A negative test asserts the
    `NewFileAsset` branch is **not** blocked: a freshly-discovered
    file with no alias proof succeeds even while the intent is
    pending.
  - The same intent blocks
    `IdentityRepo::reconcile_rename_in_tx` for a `FileVersion` in
    the closure.
  - After `finalize_destructive_commit` (completed) or
    `abort_destructive_commit` (aborted) clears the intent's
    `pending` state, the same acquire / alias-attach / rename calls
    succeed without surprises.
  - Defensive trip-wire — fresh-lease branch (Phase B step 3 of
    §9.3.2): a low-level test inserts an `asset_use_lease` row
    directly via SQL between Phase A commit and Phase B start
    (bypassing `UseLeaseRepo` and therefore the lock). Phase B's
    recheck observes the fresh lease, transitions the intent to
    `recovery_required`, emits `commit.aborted_post_mutation` with
    `reason='fresh_lease'`, returns
    `CommitGateResult::BlockedByUseLease`, and asserts the durable
    identity mutation did **not** run.
  - Defensive trip-wire — closure-grew branch (Phase B step 3 of
    §9.3.2): a test wires a stateful `AliasResolver` that returns
    `Complete` with closure C₁ during Phase A and returns
    `Complete` with closure C₂ ⊋ C₁ during Phase B (simulating an
    alias newly discovered between phases, e.g., a remote mount
    coming online). Phase B's recheck observes the non-empty delta,
    transitions the intent to `recovery_required`, emits
    `commit.aborted_post_mutation` with `reason='closure_grew'`,
    populated `delta_*`, and `fresh_lease_ids=[]`, returns
    `CommitGateResult::BlockedByClosureGrew { added_* }`, and asserts
    the durable identity mutation did **not** run.
  - Defensive trip-wire — tie branch (Phase B step 3 of §9.3.2):
    combines the previous two fixtures so the closure grows **and** a
    fresh blocking lease lands on a member of the enlarged closure
    between Phase A and Phase B. Asserts a single
    `commit.aborted_post_mutation` event with
    `reason='closure_grew_and_fresh_lease'` and both populated
    `delta_*` arrays **and** populated `fresh_lease_ids` (no escape
    evidence dropped), the intent in `recovery_required`, and the
    returned `CommitGateResult` is `BlockedByClosureGrew`. The
    fresh-lease evidence on the event payload is what guarantees the
    Sprint 5+ recovery worker can see both escapes from a single
    durable record. All three trip-wire tests exist to prove the
    trip-wire fires when something escapes the lock; under normal
    operation the trip-wire never fires.
- `commit_safety_gate_after_rename.rs` — end-to-end against the
  two-phase API: ingest → evidence-acceptance → `reconcile_rename`
  (M3, re-anchors leases) → `prepare_destructive_commit` against
  pinned evidence → `StaleIdentityEvidence` abort in Phase A;
  re-collect & re-accept evidence → second `prepare` → caller mutation
  → `finalize_destructive_commit` allowed.
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
| Tests can create a bundle, open and prioritize an issue, record a quality score, and block a commit with a use lease. | `crates/voom-store/tests/commit_safety_gate.rs::prepare_blocked_by_use_lease` (Phase A `BlockedByUseLease` under the two-phase API) together with `repo/issues_test.rs::prioritize` and `repo/quality_scores_test.rs::record` |
| Events are recorded for all state transitions. | Every repo `_test.rs` asserts the matching `events` row; `event_log_append_only.rs` asserts immutability |
| In-memory SQLite tests exercise the same repositories as disk mode. | Repos inherit `test_support::test_pool()` for `:memory:`; `tests/disk_mode.rs` runs the same fixture flow against a `tempfile`-backed disk DB; `just ci` runs both |

## 15. Out of Scope

See §1 for the full list. The most likely-to-be-asked exclusions:

- No worker process, no wire protocol (Sprint 2).
- No policy parser, no planner, no execution-plan tables (Sprint 3).
- No remote-node lease acquisition (Sprint 4).
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
- No filesystem-aware recovery of stuck `commit_intents` rows. Sprint 1
  ships the durable intent table, the prepare / finalize / abort /
  list API, and the `commit.recovery_required` event kind; the
  reconciliation worker that inspects filesystem state and decides
  whether to roll forward or roll back lives with the real workers
  (Sprint 5+).
