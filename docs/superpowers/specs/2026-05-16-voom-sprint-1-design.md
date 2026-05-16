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

Tables: `asset_use_leases`, `external_systems`, `external_system_links`,
`external_path_mappings`, `issues`, `issue_links`,
`quality_scoring_profiles`, `quality_scores`. Migration:
`0004_use_leases_ancillary.sql`.

Implements:

- the full `asset_use_leases` lifecycle (TTL-bound + manual locks, terminal
  release reasons, force-release audit path)
- the Commit Safety Gate (affected-scope closure across alias
  `FileLocation`s, fail-closed when alias resolution is incomplete,
  evidence revalidation, lease re-anchoring on rename/move, force-path
  semantics that never bypass evidence revalidation)
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
| `voom-cli` | `commands/` module subdirectory with one file per resource group (job, ticket, lease, worker, artifact, work, variant, bundle, asset, evidence, issue, score, external-system, use-lease, event). Read-only verbs only. |
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
| `0004_use_leases_ancillary.sql` | M3 | `asset_use_leases`, `external_systems`, `external_system_links`, `external_path_mappings`, `issues`, `issue_links`, `quality_scoring_profiles`, `quality_scores` |

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
`asset_use_leases`, `file_locations`, `issues`, `media_works`,
`media_variants`) carry an `epoch INTEGER NOT NULL DEFAULT 0` column. Every
UPDATE includes `WHERE id = ? AND epoch = ?` and bumps `epoch = epoch + 1`.
A zero-rows-affected result becomes `VoomError::Conflict` →
`ErrorCode::Conflict` so callers retry without manual re-reads.

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
host-side multi-table helper rather than a domain repo. It accepts
`&dyn EventRepo` and owns its own IMMEDIATE transaction internally
because the gate's algorithm interleaves closure reads, evidence
revalidation, the caller-supplied mutation, and event writes in a
specific order that no outer use-case caller can fully express.

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
- `commit_safety_gate` module → no repo trait; exposes `run_destructive_commit(...)`
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
    CommitCompleted,                    // 'commit.completed'
    CommitAbortedByUseLease,            // 'commit.aborted_by_use_lease'
    CommitAbortedByStaleEvidence,       // 'commit.aborted_by_stale_evidence'
    CommitAbortedByClosureIncomplete,   // 'commit.aborted_by_closure_incomplete'
    CommitAbortedByMutation,            // 'commit.aborted_by_mutation'
    CommitForcedOverride,               // 'commit.forced_override'
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
| `attempt` | INTEGER NOT NULL DEFAULT 0 | Incremented on each lease |
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
- `leased` → `ready` via `LeaseRepo::fail` when `retriable && attempt + 1 < max_attempts`. Sets `attempt = attempt + 1` and `next_eligible_at = now + backoff(attempt)`. Sprint 1 uses a fixed backoff (5s × attempt); the policy will live in `voom-scheduler` later.
- `leased` → `failed` via `LeaseRepo::fail` otherwise.
- `leased` → `ready` via `LeaseRepo::expire_due` if retries remain; `leased` → `failed` otherwise.

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
  ticket row by ID, assert `state = 'ready' AND next_eligible_at <= now AND attempt < max_attempts`,
  transition to `leased`, increment `attempt`, insert lease row with
  `expires_at = now + ttl`. Use case emits `ticket.leased` + `lease.acquired`.
- `heartbeat(lease_id) -> Result<Lease>` — assert `state = 'held'`, set
  `last_heartbeat_at = now`, `expires_at = now + ttl`. No event in
  Sprint 1 (Sprint 6 daemon may emit a recovery event after a previously
  missed beat).
- `release(lease_id, ResultPayload) -> Result<()>` — assert `state = 'held'`,
  transition lease to `released`, transition ticket to `succeeded`, write
  `result` JSON. Use case emits `ticket.succeeded` + `lease.released`,
  then calls `TicketRepo::mark_ready_if_unblocked` for every dependent ticket.
- `fail(lease_id, FailureReason, retriable: bool) -> Result<()>` — assert
  `state = 'held'`. If `retriable && ticket.attempt + 1 < ticket.max_attempts`,
  transition ticket to `ready` with bumped attempt and `next_eligible_at`;
  emit `ticket.failed_retriable`. Else transition ticket to `failed`; emit
  `ticket.failed_terminal`. Lease transitions to `released` with
  `release_reason = 'failed_retriable' | 'failed_terminal'`; emit
  `lease.released`.
- `expire_due(now) -> Result<ExpireReport>` — bulk: find leases with
  `state = 'held' AND expires_at < now`. Per row: transition lease to
  `expired` (`release_reason = 'issuer_lost'`, `released_at = now`),
  transition ticket to `ready` if retries remain or `failed` otherwise.
  Emit `lease.expired` + `ticket.requeued_after_lease_expiry` or
  `ticket.failed_terminal` per row. Returns
  `ExpireReport { expired_leases: Vec<LeaseId>, requeued_tickets: Vec<TicketId>, failed_tickets: Vec<TicketId> }`.
- `force_release(lease_id, actor, reason, also_requeue: bool) -> Result<()>` —
  admin/test path: transition lease to `force_released`, ticket to
  `ready` if `also_requeue` else `failed`. Emit `lease.force_released`
  with `{ actor, reason }` in the payload.

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

Behavior of `reconcile_rename_in_tx` (M2 form):

- Look up `prior_location_id`; assert it's live; bind its `FileVersion`.
- Retire the prior location (`retired_at = observed_at`).
- Insert a new `FileLocation` on the same `FileVersion` with the new
  `kind` and `value`.
- Emit `file_location.retired_by_move` and `file_location.recorded_by_move`
  events with payload referencing both IDs.
- Append a new `identity_evidence` row (assertion `PathRuleMatch`)
  observing the new location.

M3 extends `reconcile_rename_in_tx` to additionally:

- Find all non-terminal `asset_use_leases` with
  `(scope_type = 'location', scope_id = prior_location_id)`.
- Update their `scope_id = new_location_id`, preserving all other lease
  state.
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
    scope_type          TEXT NOT NULL,        -- 'asset'|'bundle'|'version'|'location'
    scope_id            INTEGER NOT NULL,
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
    )
) STRICT;

CREATE INDEX use_leases_by_scope
  ON asset_use_leases (scope_type, scope_id) WHERE release_reason IS NULL;

CREATE INDEX use_leases_by_expiry
  ON asset_use_leases (expires_at) WHERE release_reason IS NULL AND ttl_bound = 1;
```

The two `CHECK` constraints enforce the two invariants the spec is most
explicit about: TTL-bound vs. manual locks differ on `expires_at` presence,
and terminal state requires both `release_reason` and `released_at`
together.

### 9.2 `UseLeaseRepo` lifecycle

- `acquire(NewUseLease) -> Result<UseLeaseId>` — IMMEDIATE transaction:
  validate `clock_source = "control_plane"`; for TTL-bound leases require
  a positive TTL; for manual locks require no `expires_at`. The IMMEDIATE
  tx serializes against in-flight destructive commits on the same scope
  (see §9.3); if a concurrent commit holds the write lock, this acquire
  blocks (SQLite WAL behavior) or returns `Conflict` after busy-timeout.
  Emits `use_lease.acquired`.
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
  all non-terminal leases on `(scope_type='location', scope_id=retired_location_id)`,
  updates `scope_id = new_location_id`. Other fields preserved: `lease_id`,
  `issuer_kind`, `issuer_ref`, `acquired_at`, `expires_at`,
  `last_heartbeat_at`, `blocking_mode`, `ttl_bound`. Emits
  `use_lease.reanchored_by_move` per affected lease — blocking, advisory,
  and manual locks all re-anchor (the spec is explicit). Returns the list
  of re-anchored IDs so the caller can include them in its own event.

### 9.3 Commit Safety Gate

The gate is a host-side helper in `voom-store::repo::commit_safety_gate`.
Sprint 1's only callers are tests; Sprint 5+ adds real callers (the
host-side commit transaction for transcode/remux/restore/delete/archive).
The gate is the single source of truth for the four abort errors the
architectural spec names.

### 9.3.1 API

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
}

pub async fn run_destructive_commit<F, Fut, T>(
    pool: &SqlitePool,
    alias_resolver: &dyn AliasResolver,
    event_repo: &dyn EventRepo,
    input: DestructiveCommit,
    perform_mutation: F,
) -> Result<(CommitGateOutcome, Option<T>), VoomError>
where
    F: FnOnce(&AffectedScopeClosure) -> Fut,
    Fut: Future<Output = Result<T, VoomError>>;
```

### 9.3.2 Algorithm

1. Open an `IMMEDIATE` transaction on `pool`. SQLite WAL serializes
   writers; this is the spec's serialization point. Inside this tx, no
   other writer can insert a new lease, new `file_location`, or mutate
   the affected scope. New blocking-lease acquires that race the gate
   either block on the SQLite write lock or return
   `Conflict` after busy-timeout.
2. Compute `closure_initial`:
   - Walk target → `FileVersion`(s) → live `FileLocation`s on those
     versions (SQL).
   - Ask the `AliasResolver` for additional locations representing the
     same physical bytes (hardlinks, bind-mounts, shared mounts,
     object-store aliases).
   - Add the `AssetBundle`(s) of the affected `FileAsset`(s).
   - If any `AliasResolver` call fails or returns
     `AliasResolutionError::Unreachable`, record a `ClosureWarning` and
     surface `BlockedByClosureIncomplete` unless `override_token`
     is present and grants closure-bypass. Emit
     `commit.aborted_by_closure_incomplete` and ROLLBACK.
3. Evaluate every blocking `asset_use_lease` whose `(scope_type, scope_id)`
   falls within `closure_initial`. Terminal leases don't count
   (`release_reason IS NOT NULL`). TTL-bound leases past `expires_at`
   don't count, regardless of whether cleanup has run yet. Manual locks
   always count until terminal. Advisory leases never block. If a fresh
   blocking lease overlaps → `BlockedByUseLease`; emit
   `commit.aborted_by_use_lease`; ROLLBACK.
4. Revalidate every accepted evidence row in `input.accepted_evidence_ids`:
   compare `pinned_file_version_ids` against current `FileVersion` IDs of
   the scope, `pinned_hashes` against current `content_hash` values, and
   `pinned_locations` against current live locations. Any drift →
   `BlockedByStaleEvidence`; emit `commit.aborted_by_stale_evidence`;
   ROLLBACK. Accepted-evidence rows are not rewritten — drift forces the
   caller to re-collect and re-accept. **The force path never bypasses
   evidence revalidation.**
5. Call `perform_mutation(&closure_initial)`. Sprint 1 tests pass either
   a no-op closure or a recorded-call closure; Sprint 5+ passes the real
   filesystem-mutation closure. If `perform_mutation` errors → emit
   `commit.aborted_by_mutation`; ROLLBACK; propagate.
6. Recompute `closure_final` and re-evaluate blocking leases under the
   same isolation. The spec is explicit: "pick up any FileLocations that
   have been attached as aliases of in-scope FileVersions since the
   initial gate check." Because the IMMEDIATE tx holds the write lock,
   the only way `closure_final` differs from `closure_initial` is if
   `perform_mutation` itself touched aliases (e.g., a destructive op that
   traversed hardlinks). If `closure_final` includes a new in-scope
   location with a blocking lease → `BlockedByUseLease`; emit
   `commit.aborted_by_use_lease`; ROLLBACK.
7. Write the `commit.completed` event with payload
   `{ commit_id, target, closure_initial, closure_final, evaluated_lease_ids, revalidated_evidence }`.
   COMMIT.

### 9.3.3 Force path

`override_token: Some(ForcePathToken)` is a separately audited path the
spec mandates ("Operators who need to commit despite incomplete resolution
use a separately audited, permissioned force path that records its own
override event and reason"). The `ForcePathToken` carries `actor`,
`reason`, and a `bypass` bitset declaring which checks are skipped.
Allowed bypass kinds:

- `closure_incomplete` — skip step 2's `BlockedByClosureIncomplete` abort.
- `blocked_by_use_lease` — skip step 3's blocking-lease check.

`stale_evidence` is **not** a valid bypass — stale evidence is a correctness
problem, not a permissions problem. A token requesting that bypass is
rejected before the gate runs (`VoomError::Config`).

A force-path run emits `commit.forced_override` recording the token's
actor, reason, and the set of bypassed checks, in addition to whatever
`commit.*` events the underlying path produces. Force-released leases on
the scope still don't block (they're terminal); the spec's "forced release
does not bypass the safety gate on any later destructive commit" is already
covered by the terminal-state rule and does not require special handling
in the gate.

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

### 9.3.5 Abort-event durability

Steps 2–6 that produce a `Blocked*` result emit their `commit.aborted_by_*`
event before ROLLBACK and survive the abort via a two-transaction
pattern: the outer `IMMEDIATE` rolls back the mutation attempt; a
follow-up tiny tx writes the abort event. Abort events have no other
durable state to atomically commit alongside them, so the two-tx pattern
is correct.

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
events, `updated_at` otherwise. The CLI never paginates lazily — agents
drive the cursor explicitly so command boundaries are stable.

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
- The Sprint-1-specific error envelopes for `BLOCKED_BY_USE_LEASE`,
  `STALE_IDENTITY_EVIDENCE`, `CLOSURE_RESOLUTION_INCOMPLETE`,
  `DEPENDENCY_CYCLE`, and `CONFLICT`, shaped via fixture insertion +
  forced invocation of the relevant control-plane use case.

## 12. Cross-cutting Concerns

### 12.1 Error codes

`voom-core::error::ErrorCode` gains:

- `BlockedByUseLease` → `"BLOCKED_BY_USE_LEASE"`
- `StaleIdentityEvidence` → `"STALE_IDENTITY_EVIDENCE"`
- `ClosureResolutionIncomplete` → `"CLOSURE_RESOLUTION_INCOMPLETE"`
- `DependencyCycle` → `"DEPENDENCY_CYCLE"`
- `Conflict` → `"CONFLICT"`

`VoomError` gains matching `(String)`-tuple variants — matching Sprint 0's
pattern. The message text carries the human-readable context (lease ID,
evidence drift summary, etc.); structured detail for Sprint 9's reports
lives on the gate-result types in `voom-store::repo::commit_safety_gate`
(`CommitGateResult`, `LeaseScope`, `EvidenceDrift`, `ClosureWarning`),
which the control-plane use cases consult before mapping to `VoomError`.

```rust
pub enum VoomError {
    /* ... Sprint 0 variants ... */
    BlockedByUseLease(String),
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
  → succeeded. Asserts every event row.
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
- `commit_safety_gate.rs` — covers each abort path: `BlockedByUseLease`
  (blocking lease scoped to closure member), `BlockedByStaleEvidence`
  (pinned hash mismatch), `BlockedByClosureIncomplete` (via
  `FailingAliasResolver`); allowed path with successful mutation;
  two-pass closure recheck detects new alias mid-mutation; force-path
  bypasses closure-incomplete and lease-block but is rejected for
  `stale_evidence`.
- `commit_safety_gate_after_rename.rs` — end-to-end: ingest →
  evidence-acceptance → `reconcile_rename` (M3, re-anchors leases) →
  destructive commit against pinned evidence → `StaleIdentityEvidence`
  abort; re-collect & re-accept evidence → second commit allowed.
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
| Tests can create a bundle, open and prioritize an issue, record a quality score, and block a commit with a use lease. | `crates/voom-store/tests/commit_safety_gate.rs::blocked_by_use_lease` together with `repo/issues_test.rs::prioritize` and `repo/quality_scores_test.rs::record` |
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
