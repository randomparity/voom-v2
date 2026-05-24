# VOOM Sprint 8 Remote Synthetic Execution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove node-authenticated remote synthetic execution through thin HTTP routes, durable leases, stale recovery, and typed synthetic artifact access plans.

**Architecture:** `voom-api` owns only request/response routing and envelope mapping; every state transition calls a `voom-control-plane` use case. `voom-control-plane` authenticates nodes, enforces worker ownership/capability/grants, performs idempotent remote mutations, and reuses existing lease success/failure/recovery semantics through shared in-transaction helpers. `voom-store` owns first-class persistence for idempotency records and artifact access plans so Sprint 9 can query remote dispatch evidence without mining opaque blobs.

**Tech Stack:** Rust 2024, tokio, axum, sqlx SQLite migrations, serde/serde_json, secrecy token handling, existing `voom-worker-protocol` dispatch types, sibling unit-test layout, integration tests, `just ci`.

---

## Assumptions And Decisions

- Add migration `0010_remote_execution.sql`. Sprint 7 already owns `0009_nodes.sql`; Sprint 8 must not rewrite it.
- Reuse existing `VoomError` variants for public route error codes: `NotFound`, `Conflict`, `Config` for `CONFIG_INVALID`, and existing database/internal errors. Do not add new error codes unless a compiler-enforced classification gap appears during implementation.
- Treat missing `X-Voom-Idempotency-Key` as `BAD_ARGS` at the API layer before the control-plane mutation runs.
- Persist idempotency request hashes with route scope, a non-null worker scope, and response bodies. This keeps retry replay deterministic across node heartbeat, acquire, lease heartbeat, complete, and fail.
- Store idempotency replay payloads as control-plane remote mutation results, not raw HTTP bytes and not `voom-api` envelopes. The API still owns `schema_version`, `command`, warnings, and HTTP status mapping.
- Keep lease heartbeats event-free. Idempotent replay of a lease heartbeat also emits no event.
- Implement remote acquire as a deterministic query owned by `voom-store`, but expose it only through a `voom-control-plane` use case that wraps validation, lease creation, event emission, and selected artifact access plan creation in one transaction.
- Use synthetic artifact access modes only: `shared_mount`, `control_plane_placeholder`, `staged_output_placeholder`.
- Remote runner support lives in `voom-fakes` and dispatches through `voom-worker-protocol` helper functions. It must not call provider logic directly as an in-process shortcut.
- HTTP worker registration and CLI grant authoring remain out of scope; tests seed nodes/workers/grants through existing control-plane setup.

## Success Criteria

- HTTP routes exist for remote node heartbeat, lease acquire, lease heartbeat, lease complete, and lease fail.
- Every mutating remote route requires bearer node token authentication and `X-Voom-Idempotency-Key`.
- Same-key/same-body retries replay the original response without applying the mutation again.
- Same-key/different-body retries return `CONFLICT` and do not mutate node, lease, ticket, events, idempotency replay payload, or artifact access state.
- Acquire rejects missing node, bad token, retired/stale/expired node, missing worker, worker/node mismatch, retired worker, missing capability, missing execution grant, and denied operation.
- Acquire returns an explicit idle outcome when no eligible ready ticket exists.
- Acquire selects ready tickets deterministically by priority, `next_eligible_at`, then ticket id.
- Complete and fail reuse existing lease semantics through shared in-transaction helpers so `lease.released`, `ticket.succeeded`, retry, failure, and dependency-promotion events remain identical to local execution while idempotency stays atomic.
- Lease heartbeat extends `last_heartbeat_at` and `expires_at` without heartbeat audit events.
- Recovery exposes a reusable control-plane path that tests and a later local CLI command can invoke to mark stale nodes and expire due leases.
- A stopped runner's lease expires and requeues or fails the ticket according to attempts.
- A stopped node becomes stale; stale nodes cannot acquire; a successful heartbeat reactivates the node and its non-retired workers can acquire future work.
- Every acquired remote dispatch records a selected artifact access plan that is queryable by lease, ticket, worker, node, access mode, and status.
- Remote complete/fail updates the selected artifact access plan to `consumed`, `rejected`, or `failed` in the same transaction as the terminal lease transition.
- The synthetic runner consumes compatible selected plans, rejects incompatible plans visibly, and returns typed artifact access evidence on success.
- Closeout documentation links every acceptance item to a test, fixture, or command.
- Required verification passes:

```bash
cargo test -p voom-api
cargo test -p voom-control-plane remote
cargo test -p voom-store artifact_access
cargo test -p voom-fakes remote
just ci
```

## File Map

- Modify: `Cargo.toml`, `Cargo.lock`: add shared `reqwest` only for the remote runner HTTP client if the implementation chooses not to use existing hyper utilities; keep request hashing on existing `blake3`.
- Modify: `crates/voom-api/Cargo.toml`: add `blake3 = { workspace = true }` for request hashing.
- Modify: `crates/voom-api/src/lib.rs`: preserve `/health`, add router construction for execution routes and common API envelope helpers.
- Create: `crates/voom-api/src/execution.rs`, `execution_test.rs`: route handlers, request/response JSON, auth/idempotency extraction, HTTP status mapping.
- Modify: `crates/voom-api/tests/health_route.rs`: update router fixtures if `router` now accepts both `HealthPlane` and optional `ControlPlane`.
- Create: `crates/voom-api/tests/remote_execution_route.rs`: end-to-end route tests for auth, acquire, heartbeat, complete, fail, idle, and idempotency.
- Create: `migrations/0010_remote_execution.sql`: `remote_idempotency_keys` and `artifact_access_plans`.
- Modify: `crates/voom-store/src/schema_test.rs`: migration inventory and table/constraint assertions.
- Modify: `crates/voom-store/src/repo/mod.rs`: export new repositories.
- Create: `crates/voom-store/src/repo/remote_idempotency.rs`, `remote_idempotency_test.rs`: scoped idempotency record insert/replay/reject operations.
- Create: `crates/voom-store/src/repo/artifact_access_plans.rs`, `artifact_access_plans_test.rs`: typed plan persistence, status transition, query filters.
- Modify: `crates/voom-store/src/repo/workers.rs`, `workers_test.rs`: add query helpers for capability/grant checks and node-owned worker inspection in transactions.
- Modify: `crates/voom-store/src/repo/tickets.rs`, `tickets_test.rs`: add deterministic ready-ticket selection for a worker's allowed operations.
- Modify: `crates/voom-store/src/repo/leases.rs`, `leases_test.rs`: add lease ownership lookup/check helper for remote heartbeat/complete/fail.
- Modify: `crates/voom-control-plane/Cargo.toml`: depend on any new internal repo module through `voom-store`; add no HTTP dependencies here.
- Modify: `crates/voom-control-plane/src/lib.rs`: add new repo fields and accessors.
- Modify: `crates/voom-control-plane/src/cases/mod.rs`: export remote execution case module.
- Modify: `crates/voom-control-plane/src/cases/nodes.rs`, `nodes_test.rs`: extract node heartbeat event emission into a shared in-transaction helper used by local and remote heartbeat paths.
- Modify: `crates/voom-control-plane/src/cases/leases.rs`, `leases_test.rs`: extract acquire, release, and fail event emission into shared in-transaction helpers used by local and remote lease paths.
- Create: `crates/voom-control-plane/src/cases/remote_execution.rs`, `remote_execution_test.rs`: node-authenticated acquire, heartbeat, complete, fail, recovery, and idempotency use cases.
- Create: `crates/voom-control-plane/src/cases/artifact_access.rs`, `artifact_access_test.rs`: plan status transitions and evidence recording. Keep acquire-time selected-plan creation in `remote_execution.rs`.
- Modify: `crates/voom-fake-support/src/lib.rs`, `lib_test.rs`: validate and emit synthetic artifact access evidence from operation payloads.
- Modify: `crates/voom-fakes/Cargo.toml`: add `voom-api`, `voom-control-plane`, `voom-store`, `reqwest` or hyper client dependencies only if the runner tests need them in-crate.
- Create: `crates/voom-fakes/src/remote_runner.rs`, `remote_runner_test.rs`: polling runner loop and HTTP client.
- Modify: `crates/voom-fakes/src/bin/*.rs`: do not change fake provider binaries unless dispatch payload compatibility requires it.
- Create: `crates/voom-fakes/tests/remote_runner.rs`: integration tests launching API router/server and the remote runner.
- Create: `docs/superpowers/plans/2026-05-24-voom-sprint-8-closeout.md`: closeout evidence matrix.

## Task 1: Remote Persistence Schema

**Files:**
- Create: `migrations/0010_remote_execution.sql`
- Modify: `crates/voom-store/src/schema_test.rs`

- [x] **Step 1: Add failing schema tests**

Add tests to `crates/voom-store/src/schema_test.rs`:

```rust
#[tokio::test]
async fn remote_execution_schema_contains_idempotency_and_artifact_access_tables() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = crate::test_support::sqlite_url_for(tmp.path());
    crate::init(&url).await.unwrap();
    let pool = crate::connect(&url).await.unwrap();

    let idem_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'remote_idempotency_keys'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(idem_sql.contains("node_id"));
    assert!(idem_sql.contains("route_key"));
    assert!(idem_sql.contains("request_hash"));
    assert!(idem_sql.contains("response_json"));
    assert!(idem_sql.contains("worker_scope_id"));
    assert!(idem_sql.contains("UNIQUE (node_id, route_key, worker_scope_id, idempotency_key)"));

    let plan_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'artifact_access_plans'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(plan_sql.contains("lease_id"));
    assert!(plan_sql.contains("ticket_id"));
    assert!(plan_sql.contains("worker_id"));
    assert!(plan_sql.contains("node_id"));
    assert!(plan_sql.contains("selected_access_mode"));
    assert!(plan_sql.contains("CHECK (status IN ('selected','consumed','rejected','failed'))"));
    assert!(plan_sql.contains("CHECK (json_valid(input_handles))"));
    assert!(plan_sql.contains("CHECK (json_valid(output_handles))"));
    assert!(plan_sql.contains("CHECK (json_valid(evidence))"));
}
```

- [x] **Step 2: Run focused failure**

Run:

```bash
cargo test -p voom-store remote_execution_schema_contains_idempotency_and_artifact_access_tables
```

Expected: FAIL because the tables do not exist.

- [x] **Step 3: Add migration**

Create `migrations/0010_remote_execution.sql`:

```sql
-- Sprint 8 - remote execution idempotency and synthetic artifact access plans.

CREATE TABLE remote_idempotency_keys (
    id                  INTEGER PRIMARY KEY,
    node_id             INTEGER NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
    route_key           TEXT NOT NULL,
    worker_scope_id     INTEGER NOT NULL,
    worker_id           INTEGER REFERENCES workers(id) ON DELETE RESTRICT,
    idempotency_key     TEXT NOT NULL,
    request_hash        TEXT NOT NULL,
    response_json       TEXT CHECK (response_json IS NULL OR json_valid(response_json)),
    status              TEXT NOT NULL CHECK (status IN ('in_progress','completed')),
    created_at          TEXT NOT NULL,
    UNIQUE (node_id, route_key, worker_scope_id, idempotency_key),
    CHECK ((worker_scope_id = 0 AND worker_id IS NULL) OR worker_scope_id = worker_id),
    CHECK ((status = 'in_progress' AND response_json IS NULL)
        OR (status = 'completed' AND response_json IS NOT NULL))
) STRICT;

CREATE INDEX remote_idempotency_by_node_created
    ON remote_idempotency_keys (node_id, created_at, id);

CREATE TABLE artifact_access_plans (
    id                      INTEGER PRIMARY KEY,
    lease_id                INTEGER NOT NULL REFERENCES leases(id) ON DELETE RESTRICT,
    ticket_id               INTEGER NOT NULL REFERENCES tickets(id) ON DELETE RESTRICT,
    worker_id               INTEGER NOT NULL REFERENCES workers(id) ON DELETE RESTRICT,
    node_id                 INTEGER NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
    input_handles           TEXT NOT NULL CHECK (json_valid(input_handles)),
    output_handles          TEXT NOT NULL CHECK (json_valid(output_handles)),
    selected_access_mode    TEXT NOT NULL CHECK (selected_access_mode IN (
                                'shared_mount',
                                'control_plane_placeholder',
                                'staged_output_placeholder'
                            )),
    status                  TEXT NOT NULL CHECK (status IN ('selected','consumed','rejected','failed')),
    reason                  TEXT,
    evidence                TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(evidence)),
    created_at              TEXT NOT NULL,
    updated_at              TEXT NOT NULL,
    UNIQUE (lease_id)
) STRICT;

CREATE INDEX artifact_access_plans_by_ticket
    ON artifact_access_plans (ticket_id, id);

CREATE INDEX artifact_access_plans_by_worker
    ON artifact_access_plans (worker_id, id);

CREATE INDEX artifact_access_plans_by_node
    ON artifact_access_plans (node_id, id);

CREATE INDEX artifact_access_plans_by_mode_status
    ON artifact_access_plans (selected_access_mode, status, id);
```

- [x] **Step 4: Verify and commit**

Run:

```bash
cargo test -p voom-store remote_execution_schema_contains_idempotency_and_artifact_access_tables
cargo test -p voom-store migration_inventory
```

Expected: PASS after updating any migration inventory assertion to include `0010_remote_execution.sql`.

Commit:

```bash
git add migrations/0010_remote_execution.sql crates/voom-store/src/schema_test.rs
git commit -m "feat: add remote execution persistence schema"
```

## Task 2: Idempotency Repository

**Files:**
- Create: `crates/voom-store/src/repo/remote_idempotency.rs`
- Create: `crates/voom-store/src/repo/remote_idempotency_test.rs`
- Modify: `crates/voom-store/src/repo/mod.rs`

- [x] **Step 1: Add failing repository tests**

Create `crates/voom-store/src/repo/remote_idempotency_test.rs` with tests for insert, same-request replay, and conflict:

```rust
use serde_json::json;
use time::OffsetDateTime;
use voom_core::{NodeId, WorkerId};

use super::*;

#[tokio::test]
async fn same_scope_key_and_hash_replays_stored_response() {
    let fixture = fixture().await;
    let repo = &fixture.repo;
    let node_id = fixture.node_id;
    let worker_id = fixture.worker_id;
    let now = OffsetDateTime::UNIX_EPOCH;

    let mut tx = fixture.pool.begin().await.unwrap();
    let first = repo
        .reserve_or_replay_in_tx(&mut tx, RemoteIdempotencyInput {
            node_id,
            route_key: "POST /v1/execution/lease/acquire".to_owned(),
            worker_id: Some(worker_id),
            idempotency_key: "same-key".to_owned(),
            request_hash: "hash-a".to_owned(),
            created_at: now,
        })
        .await
        .unwrap();
    assert_eq!(first, IdempotencyOutcome::Reserved);
    repo.complete_in_tx(
        &mut tx,
        node_id,
        "POST /v1/execution/lease/acquire",
        Some(worker_id),
        "same-key",
        json!({"status":"ok","data":{"lease_id":1}}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = fixture.pool.begin().await.unwrap();
    let replay = repo
        .reserve_or_replay_in_tx(&mut tx, RemoteIdempotencyInput {
            node_id,
            route_key: "POST /v1/execution/lease/acquire".to_owned(),
            worker_id: Some(worker_id),
            idempotency_key: "same-key".to_owned(),
            request_hash: "hash-a".to_owned(),
            created_at: now,
        })
        .await
        .unwrap();
    assert_eq!(
        replay,
        IdempotencyOutcome::Replay(json!({"status":"ok","data":{"lease_id":1}}))
    );
}

#[tokio::test]
async fn same_scope_key_with_different_hash_is_conflict() {
    let fixture = fixture().await;
    let repo = &fixture.repo;
    let node_id = fixture.node_id;
    let worker_id = fixture.worker_id;
    let now = OffsetDateTime::UNIX_EPOCH;

    let mut tx = fixture.pool.begin().await.unwrap();
    repo.reserve_or_replay_in_tx(&mut tx, RemoteIdempotencyInput {
        node_id,
        route_key: "POST /v1/execution/lease/1/complete".to_owned(),
        worker_id: Some(worker_id),
        idempotency_key: "complete-key".to_owned(),
        request_hash: "hash-a".to_owned(),
        created_at: now,
    })
    .await
    .unwrap();
    repo.complete_in_tx(
        &mut tx,
        node_id,
        "POST /v1/execution/lease/1/complete",
        Some(worker_id),
        "complete-key",
        json!({"status":"ok"}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = fixture.pool.begin().await.unwrap();
    let err = repo
        .reserve_or_replay_in_tx(&mut tx, RemoteIdempotencyInput {
            node_id,
            route_key: "POST /v1/execution/lease/1/complete".to_owned(),
            worker_id: Some(worker_id),
            idempotency_key: "complete-key".to_owned(),
            request_hash: "hash-b".to_owned(),
            created_at: now,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "CONFLICT");
}
```

Define `fixture()` in the same test file by initializing a temp database, inserting a node and node-linked worker through raw SQL or existing test support, then returning a small fixture struct containing `pool`, `repo`, `node_id`, and `worker_id`.

- [x] **Step 2: Run focused failure**

Run:

```bash
cargo test -p voom-store remote_idempotency
```

Expected: compile failure because the repo module does not exist.

- [x] **Step 3: Implement repository**

Create `crates/voom-store/src/repo/remote_idempotency.rs`:

```rust
//! Remote execution route idempotency records.

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sqlx::SqlitePool;
use time::OffsetDateTime;
use voom_core::{NodeId, VoomError, WorkerId};

use super::Repository;
use super::common::{i64_from_u64, iso8601, serialize_json};

#[derive(Debug, Clone)]
pub struct RemoteIdempotencyInput {
    pub node_id: NodeId,
    pub route_key: String,
    pub worker_id: Option<WorkerId>,
    pub idempotency_key: String,
    pub request_hash: String,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IdempotencyOutcome {
    Reserved,
    Replay(JsonValue),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RemoteMutationReplay {
    Ok { data: JsonValue },
    Error { code: String, message: String },
}

#[async_trait]
pub trait RemoteIdempotencyRepo: Repository {
    async fn reserve_or_replay_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: RemoteIdempotencyInput,
    ) -> Result<IdempotencyOutcome, VoomError>;

    async fn complete_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        node_id: NodeId,
        route_key: &str,
        worker_id: Option<WorkerId>,
        idempotency_key: &str,
        response_json: JsonValue,
    ) -> Result<(), VoomError>;
}

#[derive(Debug, Clone)]
pub struct SqliteRemoteIdempotencyRepo {
    pool: SqlitePool,
}

impl SqliteRemoteIdempotencyRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteRemoteIdempotencyRepo {}
```

Complete `reserve_or_replay_in_tx` and `complete_in_tx` as transaction-scoped primitives. Insert `worker_scope_id = 0` when `worker_id` is `None`; otherwise set `worker_scope_id` to the worker id. This avoids SQLite's nullable-unique behavior for node-only routes. The first successful call reserves the key with `status = 'in_progress'` before any route mutation runs; the same transaction must call `complete_in_tx` with the serialized success or error response before commit.

```rust
let existing = sqlx::query(
    "SELECT request_hash, response_json, status FROM remote_idempotency_keys
     WHERE node_id = ? AND route_key = ? AND worker_scope_id = ? AND idempotency_key = ?",
)
.bind(i64_from_u64(input.node_id.0))
.bind(&input.route_key)
.bind(input.worker_id.map_or(0, |id| i64_from_u64(id.0)))
.bind(&input.idempotency_key)
.fetch_optional(&mut *conn)
.await?;
```

Use the local `common` JSON helpers and return `VoomError::Conflict("idempotency key reused with different request body".to_owned())` when the stored hash differs. If the stored hash matches and `status = 'completed'`, return `IdempotencyOutcome::Replay(response_json)`. If the stored hash matches and `status = 'in_progress'`, return `VoomError::Conflict("idempotency key is already in progress".to_owned())` without mutating route state; this only applies to truly concurrent duplicate requests because committed successful or failed outcomes must be finalized before the transaction commits. `response_json` must serialize `RemoteMutationReplay`, never the full API envelope; `voom-api` wraps replayed data/errors in its normal envelope and chooses the HTTP status from the replayed error code.

Export from `crates/voom-store/src/repo/mod.rs`:

```rust
pub mod remote_idempotency;
pub use remote_idempotency::{
    IdempotencyOutcome, RemoteIdempotencyInput, RemoteIdempotencyRepo,
    SqliteRemoteIdempotencyRepo,
};
```

- [x] **Step 4: Verify and commit**

Run:

```bash
cargo test -p voom-store remote_idempotency
```

Expected: PASS.

Commit:

```bash
git add crates/voom-store/src/repo/mod.rs crates/voom-store/src/repo/remote_idempotency.rs crates/voom-store/src/repo/remote_idempotency_test.rs
git commit -m "feat: persist remote route idempotency"
```

## Task 3: Artifact Access Plan Repository

**Files:**
- Create: `crates/voom-store/src/repo/artifact_access_plans.rs`
- Create: `crates/voom-store/src/repo/artifact_access_plans_test.rs`
- Modify: `crates/voom-store/src/repo/mod.rs`

- [x] **Step 1: Add failing repository tests**

Create tests that insert a selected plan and query it by each required dimension:

```rust
#[tokio::test]
async fn selected_plan_is_queryable_by_lease_ticket_worker_node_mode_and_status() {
    let fixture = fixture().await;
    let now = OffsetDateTime::UNIX_EPOCH;
    let plan = fixture
        .repo
        .create_selected(NewArtifactAccessPlan {
            lease_id: fixture.lease_id,
            ticket_id: fixture.ticket_id,
            worker_id: fixture.worker_id,
            node_id: fixture.node_id,
            input_handles: vec!["handle:input:1".to_owned()],
            output_handles: vec!["handle:output:1".to_owned()],
            selected_access_mode: ArtifactAccessMode::SharedMount,
            evidence: serde_json::json!({"selected_by":"remote_acquire"}),
            now,
        })
        .await
        .unwrap();

    assert_eq!(plan.status, ArtifactAccessPlanStatus::Selected);
    assert_eq!(fixture.repo.get_by_lease(fixture.lease_id).await.unwrap().unwrap().id, plan.id);
    assert_eq!(fixture.repo.list_by_ticket(fixture.ticket_id).await.unwrap().len(), 1);
    assert_eq!(fixture.repo.list_by_worker(fixture.worker_id).await.unwrap().len(), 1);
    assert_eq!(fixture.repo.list_by_node(fixture.node_id).await.unwrap().len(), 1);
    assert_eq!(
        fixture
            .repo
            .list_by_mode_and_status(ArtifactAccessMode::SharedMount, ArtifactAccessPlanStatus::Selected)
            .await
            .unwrap()
            .len(),
        1
    );
}
```

Add a second test for status transitions:

```rust
#[tokio::test]
async fn plan_status_transition_records_reason_and_evidence() {
    let fixture = fixture().await;
    let now = OffsetDateTime::UNIX_EPOCH;
    let plan = fixture.seed_selected_plan(now).await;

    let consumed = fixture
        .repo
        .mark_status(
            plan.id,
            ArtifactAccessPlanStatus::Consumed,
            Some("synthetic worker validated shared mount".to_owned()),
            serde_json::json!({"validated":true}),
            now,
        )
        .await
        .unwrap();

    assert_eq!(consumed.status, ArtifactAccessPlanStatus::Consumed);
    assert_eq!(consumed.reason.as_deref(), Some("synthetic worker validated shared mount"));
    assert_eq!(consumed.evidence["validated"], true);
}
```

- [x] **Step 2: Run focused failure**

Run:

```bash
cargo test -p voom-store artifact_access
```

Expected: compile failure because `artifact_access_plans` does not exist.

- [x] **Step 3: Implement repository**

Create typed enums and structs:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactAccessMode {
    SharedMount,
    ControlPlanePlaceholder,
    StagedOutputPlaceholder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactAccessPlanStatus {
    Selected,
    Consumed,
    Rejected,
    Failed,
}

#[derive(Debug, Clone)]
pub struct NewArtifactAccessPlan {
    pub lease_id: LeaseId,
    pub ticket_id: TicketId,
    pub worker_id: WorkerId,
    pub node_id: NodeId,
    pub input_handles: Vec<String>,
    pub output_handles: Vec<String>,
    pub selected_access_mode: ArtifactAccessMode,
    pub evidence: serde_json::Value,
    pub now: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct ArtifactAccessPlan {
    pub id: u64,
    pub lease_id: LeaseId,
    pub ticket_id: TicketId,
    pub worker_id: WorkerId,
    pub node_id: NodeId,
    pub input_handles: Vec<String>,
    pub output_handles: Vec<String>,
    pub selected_access_mode: ArtifactAccessMode,
    pub status: ArtifactAccessPlanStatus,
    pub reason: Option<String>,
    pub evidence: serde_json::Value,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}
```

Implement `ArtifactAccessPlanRepo` methods:

```rust
async fn create_selected_in_tx(
    &self,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: NewArtifactAccessPlan,
) -> Result<ArtifactAccessPlan, VoomError>;
async fn mark_status_in_tx(
    &self,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: u64,
    status: ArtifactAccessPlanStatus,
    reason: Option<String>,
    evidence: serde_json::Value,
    now: OffsetDateTime,
) -> Result<ArtifactAccessPlan, VoomError>;
async fn get_by_lease(&self, lease_id: LeaseId) -> Result<Option<ArtifactAccessPlan>, VoomError>;
async fn list_by_ticket(&self, ticket_id: TicketId) -> Result<Vec<ArtifactAccessPlan>, VoomError>;
async fn list_by_worker(&self, worker_id: WorkerId) -> Result<Vec<ArtifactAccessPlan>, VoomError>;
async fn list_by_node(&self, node_id: NodeId) -> Result<Vec<ArtifactAccessPlan>, VoomError>;
async fn list_by_mode_and_status(
    &self,
    mode: ArtifactAccessMode,
    status: ArtifactAccessPlanStatus,
) -> Result<Vec<ArtifactAccessPlan>, VoomError>;
```

`create_selected` and `mark_status` non-transactional wrappers may be added for repository tests, but remote acquire/complete/fail must call the `_in_tx` variants so artifact plan state, lease state, ticket state, events, and idempotency completion are one commit.

Export the module in `repo/mod.rs`.

- [x] **Step 4: Verify and commit**

Run:

```bash
cargo test -p voom-store artifact_access
```

Expected: PASS.

Commit:

```bash
git add crates/voom-store/src/repo/mod.rs crates/voom-store/src/repo/artifact_access_plans.rs crates/voom-store/src/repo/artifact_access_plans_test.rs
git commit -m "feat: persist artifact access plans"
```

## Task 4: Store Selection Helpers For Remote Acquire

**Files:**
- Modify: `crates/voom-store/src/repo/workers.rs`
- Modify: `crates/voom-store/src/repo/workers_test.rs`
- Modify: `crates/voom-store/src/repo/tickets.rs`
- Modify: `crates/voom-store/src/repo/tickets_test.rs`
- Modify: `crates/voom-store/src/repo/leases.rs`
- Modify: `crates/voom-store/src/repo/leases_test.rs`

- [x] **Step 1: Add failing worker eligibility tests**

Add tests asserting worker capability and grant checks:

```rust
#[tokio::test]
async fn worker_operation_eligibility_requires_capability_and_grant_without_deny() {
    let fixture = worker_fixture().await;
    fixture.insert_capability("transcode_video", &["shared_mount"]).await;
    fixture.insert_grant(&["transcode_video"], &[]).await;

    let eligible = fixture
        .repo
        .operation_eligibility(fixture.worker_id, "transcode_video")
        .await
        .unwrap();
    assert!(eligible.has_capability);
    assert!(eligible.has_grant);
    assert!(!eligible.is_denied);
    assert_eq!(eligible.artifact_access, vec!["shared_mount"]);
}

#[tokio::test]
async fn worker_operation_eligibility_surfaces_denies() {
    let fixture = worker_fixture().await;
    fixture.insert_capability("transcode_video", &["shared_mount"]).await;
    fixture.insert_grant(&["transcode_video"], &["transcode_video"]).await;

    let eligible = fixture
        .repo
        .operation_eligibility(fixture.worker_id, "transcode_video")
        .await
        .unwrap();
    assert!(eligible.is_denied);
}
```

- [x] **Step 2: Add failing ready-ticket selection tests**

Add a test that seeds three ready tickets and asserts deterministic order:

```rust
#[tokio::test]
async fn next_ready_for_operations_orders_by_priority_next_eligible_and_ticket_id() {
    let fixture = ticket_fixture().await;
    let low = fixture.ready_ticket("transcode_video", 10, 10).await;
    let high_late = fixture.ready_ticket("transcode_video", 1, 20).await;
    let high_early = fixture.ready_ticket("transcode_video", 1, 5).await;

    let selected = fixture
        .repo
        .next_ready_for_operations(&["transcode_video".to_owned()], fixture.now)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(selected.id, high_early.id);
    assert_ne!(selected.id, low.id);
    assert_ne!(selected.id, high_late.id);
}
```

- [x] **Step 3: Run focused failures**

Run:

```bash
cargo test -p voom-store operation_eligibility
cargo test -p voom-store next_ready_for_operations
```

Expected: compile failures because helper methods do not exist.

- [x] **Step 4: Implement helpers**

Add a `WorkerOperationEligibility` projection:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerOperationEligibility {
    pub has_capability: bool,
    pub has_grant: bool,
    pub is_denied: bool,
    pub artifact_access: Vec<String>,
}
```

Add trait methods:

```rust
async fn operation_eligibility_in_tx(
    &self,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    worker_id: WorkerId,
    operation: &str,
) -> Result<WorkerOperationEligibility, VoomError>;

async fn node_owned_worker_in_tx(
    &self,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    worker_id: WorkerId,
    node_id: NodeId,
) -> Result<Worker, VoomError>;
```

Implement `next_ready_for_operations_in_tx` in `TicketRepo`:

```rust
async fn next_ready_for_operations_in_tx(
    &self,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    operations: &[String],
    now: OffsetDateTime,
) -> Result<Option<Ticket>, VoomError>;
```

The SQL must use `ORDER BY priority ASC, next_eligible_at ASC, id ASC LIMIT 1` and filter `state = 'ready'`, `next_eligible_at <= now`, `attempt < max_attempts`, and `kind IN (...)`.

Add a lease ownership helper:

```rust
async fn get_held_for_worker_in_tx(
    &self,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    lease_id: LeaseId,
    worker_id: WorkerId,
) -> Result<Lease, VoomError>;
```

Return `NOT_FOUND` for missing rows and `CONFLICT` for wrong worker or non-held state. Remote execution use cases must call these `_in_tx` helpers inside the idempotency transaction; non-transactional convenience wrappers may be added for tests, but they must not be used by `remote_execution.rs`.

- [x] **Step 5: Verify and commit**

Run:

```bash
cargo test -p voom-store operation_eligibility
cargo test -p voom-store next_ready_for_operations
cargo test -p voom-store get_held_for_worker
```

Expected: PASS.

Commit:

```bash
git add crates/voom-store/src/repo/workers.rs crates/voom-store/src/repo/workers_test.rs crates/voom-store/src/repo/tickets.rs crates/voom-store/src/repo/tickets_test.rs crates/voom-store/src/repo/leases.rs crates/voom-store/src/repo/leases_test.rs
git commit -m "feat: add remote acquire selection helpers"
```

## Task 5: Control-Plane Remote Execution Use Cases

**Files:**
- Modify: `crates/voom-control-plane/src/lib.rs`
- Modify: `crates/voom-control-plane/src/cases/mod.rs`
- Modify: `crates/voom-control-plane/src/cases/nodes.rs`
- Modify: `crates/voom-control-plane/src/cases/nodes_test.rs`
- Modify: `crates/voom-control-plane/src/cases/leases.rs`
- Modify: `crates/voom-control-plane/src/cases/leases_test.rs`
- Create: `crates/voom-control-plane/src/cases/remote_execution.rs`
- Create: `crates/voom-control-plane/src/cases/remote_execution_test.rs`

- [x] **Step 1: Add failing acquire and idle tests**

Create `remote_execution_test.rs` with tests:

```rust
#[tokio::test]
async fn remote_acquire_returns_idle_when_no_ready_work() {
    let fixture = remote_fixture().await;

    let outcome = fixture
        .cp
        .remote_acquire(RemoteAcquireInput {
            node_id: fixture.node_id,
            token: fixture.token.clone(),
            worker_id: fixture.worker_id,
            idempotency_key: "idle-1".to_owned(),
            request_hash: "idle-hash".to_owned(),
            lease_ttl_seconds: 30,
        })
        .await
        .unwrap();

    assert!(matches!(outcome, RemoteAcquireOutcome::Idle { .. }));
}

#[tokio::test]
async fn remote_acquire_requires_worker_node_ownership_capability_and_grant() {
    let fixture = remote_fixture_without_grant().await;
    fixture.seed_ready_ticket("transcode_video").await;

    let err = fixture
        .cp
        .remote_acquire(fixture.acquire_input("missing-grant", "hash-a"))
        .await
        .unwrap_err();

    assert_eq!(err.code(), "CONFLICT");
}
```

- [x] **Step 2: Add failing lease transition and idempotency tests**

Add tests:

```rust
#[tokio::test]
async fn remote_complete_reuses_existing_success_path_and_replays_same_key() {
    let fixture = remote_fixture_with_ready_ticket().await;
    let acquired = fixture.acquire_once("acquire-key", "acquire-hash").await;

    let first = fixture
        .cp
        .remote_complete(RemoteCompleteInput {
            node_id: fixture.node_id,
            token: fixture.token.clone(),
            worker_id: fixture.worker_id,
            lease_id: acquired.lease_id,
            idempotency_key: "complete-key".to_owned(),
            request_hash: "complete-hash".to_owned(),
            result: serde_json::json!({"ok":true}),
        })
        .await
        .unwrap();

    let replay = fixture
        .cp
        .remote_complete(RemoteCompleteInput {
            node_id: fixture.node_id,
            token: fixture.token.clone(),
            worker_id: fixture.worker_id,
            lease_id: acquired.lease_id,
            idempotency_key: "complete-key".to_owned(),
            request_hash: "complete-hash".to_owned(),
            result: serde_json::json!({"ok":true}),
        })
        .await
        .unwrap();

    assert_eq!(first, replay);
    assert_eq!(count(&fixture.cp, voom_events::EventKind::TicketSucceeded).await, 1);
    assert_eq!(count(&fixture.cp, voom_events::EventKind::LeaseReleased).await, 1);
}

#[tokio::test]
async fn remote_same_key_different_body_rejects_without_second_mutation() {
    let fixture = remote_fixture_with_ready_ticket().await;
    let acquired = fixture.acquire_once("acquire-key", "acquire-hash").await;

    fixture.complete_once(acquired.lease_id, "complete-key", "hash-a").await;
    let err = fixture
        .cp
        .remote_complete(RemoteCompleteInput {
            node_id: fixture.node_id,
            token: fixture.token.clone(),
            worker_id: fixture.worker_id,
            lease_id: acquired.lease_id,
            idempotency_key: "complete-key".to_owned(),
            request_hash: "hash-b".to_owned(),
            result: serde_json::json!({"ok":false}),
        })
        .await
        .unwrap_err();

    assert_eq!(err.code(), "CONFLICT");
    assert_eq!(count(&fixture.cp, voom_events::EventKind::TicketSucceeded).await, 1);
}
```

- [x] **Step 3: Run focused failures**

Run:

```bash
cargo test -p voom-control-plane remote_acquire
cargo test -p voom-control-plane remote_complete
```

Expected: compile failure because remote execution types and methods do not exist.

- [x] **Step 4: Implement types and use cases**

Create request/response types:

```rust
#[derive(Debug, Clone)]
pub struct RemoteAcquireInput {
    pub node_id: NodeId,
    pub token: secrecy::SecretString,
    pub worker_id: WorkerId,
    pub idempotency_key: String,
    pub request_hash: String,
    pub lease_ttl_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RemoteAcquireOutcome {
    Idle { worker_id: WorkerId },
    Leased(RemoteLeaseDispatch),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RemoteLeaseDispatch {
    pub lease_id: LeaseId,
    pub ticket_id: TicketId,
    pub worker_id: WorkerId,
    pub operation: String,
    pub dispatch_payload: serde_json::Value,
    pub lease_ttl_seconds: i64,
    pub heartbeat_after_seconds: i64,
    pub artifact_access_plan: RemoteArtifactAccessPlan,
}
```

Implement shared authentication:

```rust
async fn authenticate_remote_node_in_tx(
    &self,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    node_id: NodeId,
    token: &secrecy::SecretString,
    require_fresh_for_acquire: bool,
) -> Result<voom_store::repo::nodes::NodeAuthRecord, VoomError>
```

Rules:

- `NOT_FOUND` for missing node.
- `CONFLICT` for token mismatch.
- `CONFLICT` for retired node.
- For acquire, `CONFLICT` for stale node or expired heartbeat.
- Stale node can heartbeat successfully through a shared `heartbeat_node_in_tx` helper. Do not call the public `heartbeat_node` method from a remote idempotent route because it opens and commits its own transaction.

Before implementing remote routes, refactor existing local lease and node use cases so their durable transition/event logic is callable inside a caller-owned transaction:

```rust
async fn heartbeat_node_in_tx(
    &self,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    node_id: NodeId,
    now: OffsetDateTime,
) -> Result<Node, VoomError>;

async fn acquire_lease_in_tx(
    &self,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: NewLease,
) -> Result<Lease, VoomError>;

async fn release_lease_in_tx(
    &self,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    lease_id: LeaseId,
    result: serde_json::Value,
    now: OffsetDateTime,
) -> Result<Lease, VoomError>;

async fn fail_lease_in_tx(
    &self,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    lease_id: LeaseId,
    reason: String,
    class: FailureClass,
    now: OffsetDateTime,
) -> Result<Lease, VoomError>;
```

The public `heartbeat_node`, `acquire_lease`, `release_lease`, and `fail_lease` methods should become thin wrappers that begin a transaction, call the helper, and commit. Remote idempotent routes must call the helpers inside the same transaction as `reserve_or_replay_in_tx` and `complete_in_tx`; otherwise duplicate-key replay and state transition events are not atomic.

Implement idempotent mutation pattern inside each remote use case:

1. Verify the bearer token matches the node hash so a caller cannot replay another node's key.
2. Call `reserve_or_replay_in_tx` before any route mutation. If it returns `Replay`, return that stored response immediately without re-running live status checks.
3. Validate live node status/freshness, worker ownership, capability/grants, and lease ownership.
4. Execute the state transition or deterministic validation failure inside the same transaction.
5. Serialize `RemoteMutationReplay::Ok` or `RemoteMutationReplay::Error` and call `complete_in_tx` for the reserved key before commit. Do not serialize an API envelope in `voom-control-plane`.
6. Commit transaction.

The idempotency reservation must be the first write in each mutating route transaction. This forces concurrent duplicate requests for the same key to serialize on the unique key before ticket, lease, node, event, or artifact-access rows can be changed twice. Retries after a lost response must check the completed record before re-running live status validation that could have changed after the original request, such as a lease already being released or a node being retired later.

Use concrete route instance keys. The key must include every path parameter that changes the mutation target, not just the route template:

```rust
const ROUTE_ACQUIRE: &str = "POST /v1/execution/lease/acquire";
fn route_node_heartbeat(node_id: NodeId) -> String {
    format!("POST /v1/execution/node/{node_id}/heartbeat")
}
fn route_lease_heartbeat(lease_id: LeaseId) -> String {
    format!("POST /v1/execution/lease/{lease_id}/heartbeat")
}
fn route_lease_complete(lease_id: LeaseId) -> String {
    format!("POST /v1/execution/lease/{lease_id}/complete")
}
fn route_lease_fail(lease_id: LeaseId) -> String {
    format!("POST /v1/execution/lease/{lease_id}/fail")
}
```

The API request hash must be computed from the HTTP method, concrete path parameters, and request body. A body-only hash is not sufficient because lease-scoped routes can have identical JSON bodies for different `lease_id` values.

For acquire, after selecting a ticket:

- Check `operation_eligibility_in_tx` inside the remote acquire transaction.
- Reject if no capability, no grant, or denied.
- Call `leases.acquire_in_tx`.
- Emit existing `lease.acquired` and `ticket.leased` events in the same transaction, matching `acquire_lease`.
- Create selected artifact access plan with handles from the ticket payload fields `artifact_access.inputs` and `artifact_access.outputs` when present, else default to `["handle:input:synthetic"]` and `["handle:output:synthetic"]`.
- Pick first compatible mode from worker capability artifact access in this order: `shared_mount`, `control_plane_placeholder`, `staged_output_placeholder`.
- If no compatible mode exists, still acquire the lease and create a selected plan using `control_plane_placeholder`; the runner will fail it visibly. This preserves Sprint 8's proof of bad-match evidence.

For terminal routes:

- `remote_complete` must read the selected artifact access plan by `lease_id`, validate the result contains `artifact_access.validated = true`, mark the plan `consumed` with the worker-provided evidence, then call `release_lease_in_tx`.
- `remote_fail` must read the selected artifact access plan by `lease_id`. Mark it `rejected` when the failure reason/class indicates incompatible selected mode, malformed policy plan, or worker-declared artifact validation failure. Mark it `failed` for worker crashes/timeouts or unrelated execution failures. Then call `fail_lease_in_tx`.
- Plan status update, lease release/fail, ticket transition, events, and idempotency completion must commit atomically. A retry of a completed terminal route must replay without changing the artifact plan a second time.

- [x] **Step 5: Verify and commit**

Run:

```bash
cargo test -p voom-control-plane remote_acquire
cargo test -p voom-control-plane remote_heartbeat
cargo test -p voom-control-plane remote_complete
cargo test -p voom-control-plane remote_fail
```

Expected: PASS.

Commit:

```bash
git add crates/voom-control-plane/src/lib.rs crates/voom-control-plane/src/cases/mod.rs crates/voom-control-plane/src/cases/remote_execution.rs crates/voom-control-plane/src/cases/remote_execution_test.rs
git commit -m "feat: add remote execution use cases"
```

## Task 6: HTTP Execution Routes

**Files:**
- Modify: `crates/voom-api/src/lib.rs`
- Create: `crates/voom-api/src/execution.rs`
- Create: `crates/voom-api/src/execution_test.rs`
- Create: `crates/voom-api/tests/remote_execution_route.rs`
- Modify: `crates/voom-api/tests/health_route.rs`

- [x] **Step 1: Add failing route tests**

Create `crates/voom-api/tests/remote_execution_route.rs`:

```rust
#[tokio::test]
async fn acquire_requires_bearer_token_and_idempotency_key() {
    let fixture = api_fixture().await;
    let res = fixture
        .app
        .clone()
        .oneshot(
            Request::post("/v1/execution/lease/acquire")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"node_id":1,"worker_id":1}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let json = response_json(res).await;
    assert_eq!(json["error"]["code"], "BAD_ARGS");
}

#[tokio::test]
async fn acquire_returns_idle_as_success() {
    let fixture = api_fixture_with_node_worker().await;
    let res = fixture
        .post_json(
            "/v1/execution/lease/acquire",
            "idle-key",
            serde_json::json!({"node_id":fixture.node_id.0,"worker_id":fixture.worker_id.0}),
        )
        .await;

    assert_eq!(res.status(), StatusCode::OK);
    let json = response_json(res).await;
    assert_eq!(json["status"], "ok");
    assert_eq!(json["data"]["outcome"], "idle");
}
```

Add tests for bad token on each route, worker/node mismatch, same-key replay, and same-key/different-body rejection.

- [x] **Step 2: Run focused failure**

Run:

```bash
cargo test -p voom-api remote_execution_route
```

Expected: compile failure because execution routes and router state are missing.

- [x] **Step 3: Add route module and state**

Update `AppState` in `lib.rs`:

```rust
#[derive(Clone, Debug)]
pub struct AppState {
    pub health_plane: HealthPlane,
    pub control_plane: Option<voom_control_plane::ControlPlane>,
    tokio_workers: usize,
}
```

Keep `router(health_plane)` for health-only tests and add:

```rust
pub fn router_with_control_plane(
    health_plane: HealthPlane,
    control_plane: voom_control_plane::ControlPlane,
) -> axum::Router {
    base_router(AppState {
        health_plane,
        control_plane: Some(control_plane),
        tokio_workers: std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
    })
}
```

In `execution.rs`, define handlers:

```rust
pub(crate) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/v1/execution/lease/acquire", post(acquire))
        .route("/v1/execution/node/{node_id}/heartbeat", post(node_heartbeat))
        .route("/v1/execution/lease/{lease_id}/heartbeat", post(lease_heartbeat))
        .route("/v1/execution/lease/{lease_id}/complete", post(complete))
        .route("/v1/execution/lease/{lease_id}/fail", post(fail))
}
```

Add extractors:

```rust
fn bearer(headers: &HeaderMap) -> Result<SecretString, ApiInputError>;
fn idempotency_key(headers: &HeaderMap) -> Result<String, ApiInputError>;
fn stable_request_hash<T: serde::Serialize>(
    method: &str,
    route_instance: &str,
    value: &T,
) -> Result<String, ApiInputError>;
```

Hash canonical serialized JSON with `blake3::hash(bytes).to_hex().to_string()` so the API does not add a second hashing crate.

- [x] **Step 4: Verify and commit**

Run:

```bash
cargo test -p voom-api remote_execution_route
cargo test -p voom-api health_route
```

Expected: PASS.

Commit:

```bash
git add crates/voom-api/src/lib.rs crates/voom-api/src/execution.rs crates/voom-api/src/execution_test.rs crates/voom-api/tests/remote_execution_route.rs crates/voom-api/tests/health_route.rs crates/voom-api/Cargo.toml Cargo.toml Cargo.lock
git commit -m "feat: expose remote execution routes"
```

## Task 7: Recovery Use Case And Tests

**Files:**
- Modify: `crates/voom-control-plane/src/cases/remote_execution.rs`
- Modify: `crates/voom-control-plane/src/cases/remote_execution_test.rs`

- [x] **Step 1: Add failing recovery tests**

Add control-plane tests:

```rust
#[tokio::test]
async fn remote_recovery_marks_stale_nodes_and_expires_due_leases() {
    let fixture = remote_fixture_with_ready_ticket().await;
    let acquired = fixture.acquire_once("acquire-key", "acquire-hash").await;
    fixture.advance_past_node_and_lease_ttl();

    let report = fixture.cp.remote_recover(fixture.cp.clock().now()).await.unwrap();

    assert_eq!(report.stale_nodes, vec![fixture.node_id]);
    assert_eq!(report.expired_leases, vec![acquired.lease_id]);
    assert_eq!(count(&fixture.cp, voom_events::EventKind::NodeMarkedStale).await, 1);
    assert_eq!(count(&fixture.cp, voom_events::EventKind::LeaseExpired).await, 1);
}
```

- [x] **Step 2: Run focused failure**

Run:

```bash
cargo test -p voom-control-plane remote_recovery
```

Expected: compile failure until the recovery use case exists.

- [x] **Step 3: Implement recovery**

Add:

```rust
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RemoteRecoveryReport {
    pub stale_nodes: Vec<NodeId>,
    pub expired_leases: Vec<LeaseId>,
    pub requeued_tickets: Vec<TicketId>,
    pub failed_tickets: Vec<TicketId>,
}
```

Implement `ControlPlane::remote_recover(now)` by calling existing `mark_stale_nodes(now)` and `expire_due(now)`, then projecting ids from their reports. This method is intentionally not idempotency-keyed because recovery is an operator/test action and the underlying operations are idempotent by due state. Do not add an HTTP recovery route in Sprint 8; the remote HTTP surface is node-authenticated worker execution, and recovery does not have a production-ready remote admin-auth model yet.

- [x] **Step 4: Verify and commit**

Run:

```bash
cargo test -p voom-control-plane remote_recovery
```

Expected: PASS.

Commit:

```bash
git add crates/voom-control-plane/src/cases/remote_execution.rs crates/voom-control-plane/src/cases/remote_execution_test.rs
git commit -m "feat: add remote recovery hook"
```

## Task 8: Synthetic Artifact Evidence In Fake Support

**Files:**
- Modify: `crates/voom-fake-support/src/lib.rs`
- Modify: `crates/voom-fake-support/src/lib_test.rs`

- [x] **Step 1: Add failing fake-support tests**

Add tests:

```rust
#[test]
fn synthetic_result_includes_validated_artifact_access_evidence() {
    let payload = serde_json::json!({
        "artifact_access_plan": {
            "input_handles": ["handle:input:1"],
            "output_handles": ["handle:output:1"],
            "selected_access_mode": "shared_mount"
        },
        "worker_artifact_access": ["shared_mount"]
    });

    let result = synthetic_artifact_access_evidence(&payload).unwrap();

    assert_eq!(result["artifact_access"]["inputs_consumed"][0], "handle:input:1");
    assert_eq!(result["artifact_access"]["outputs_declared"][0], "handle:output:1");
    assert_eq!(result["artifact_access"]["mode"], "shared_mount");
    assert_eq!(result["artifact_access"]["validated"], true);
}

#[test]
fn incompatible_artifact_access_mode_is_retriable_failure() {
    let payload = serde_json::json!({
        "artifact_access_plan": {
            "input_handles": ["handle:input:1"],
            "output_handles": ["handle:output:1"],
            "selected_access_mode": "shared_mount"
        },
        "worker_artifact_access": ["control_plane_placeholder"]
    });

    let err = synthetic_artifact_access_evidence(&payload).unwrap_err();
    assert!(err.to_string().contains("artifact access mode shared_mount is not advertised"));
}
```

- [x] **Step 2: Run focused failure**

Run:

```bash
cargo test -p voom-fake-support artifact_access
```

Expected: compile failure because helper does not exist.

- [x] **Step 3: Implement evidence helper and wire into result payload**

Add helper:

```rust
pub fn synthetic_artifact_access_evidence(
    payload: &serde_json::Value,
) -> Result<serde_json::Value, ProtocolError>
```

Rules:

- If `artifact_access_plan` is absent, return `{}` to preserve existing fake-provider tests.
- Require `selected_access_mode` to be a string.
- Require `worker_artifact_access` to contain the selected mode.
- Return:

```json
{
  "artifact_access": {
    "inputs_consumed": ["handle:input:1"],
    "outputs_declared": ["handle:output:1"],
    "mode": "shared_mount",
    "validated": true
  }
}
```

Merge this object into existing fake provider result payloads before emitting the `ProgressFrame::Result`.

- [x] **Step 4: Verify and commit**

Run:

```bash
cargo test -p voom-fake-support artifact_access
cargo test -p voom-fake-support
```

Expected: PASS.

Commit:

```bash
git add crates/voom-fake-support/src/lib.rs crates/voom-fake-support/src/lib_test.rs
git commit -m "feat: validate synthetic artifact access evidence"
```

## Task 9: Remote Synthetic Runner

**Files:**
- Modify: `crates/voom-fakes/Cargo.toml`
- Create: `crates/voom-fakes/src/remote_runner.rs`
- Create: `crates/voom-fakes/src/remote_runner_test.rs`
- Modify: `crates/voom-fakes/src/bin/remote_synthetic_runner.rs` if a binary is needed for CLI-launched manual proof.

- [x] **Step 1: Add failing runner tests**

Create tests:

```rust
#[tokio::test]
async fn runner_polls_acquires_dispatches_heartbeats_and_completes() {
    let fixture = remote_runner_fixture_with_ready_ticket().await;
    let summary = RemoteSyntheticRunner::new(RemoteRunnerConfig {
        base_url: fixture.base_url.clone(),
        node_id: fixture.node_id,
        token: fixture.token.clone(),
        worker_id: fixture.worker_id,
        max_polls: 3,
        idle_timeout: std::time::Duration::from_millis(100),
        lease_heartbeat_interval: std::time::Duration::from_millis(10),
    })
    .run_once_to_completion()
    .await
    .unwrap();

    assert_eq!(summary.completed, 1);
    assert_eq!(summary.failed, 0);
    assert_eq!(summary.idle_polls, 0);
}

#[tokio::test]
async fn runner_fails_lease_when_artifact_access_is_incompatible() {
    let fixture = remote_runner_fixture_with_incompatible_access().await;
    let summary = RemoteSyntheticRunner::new(fixture.config()).run_once_to_completion().await.unwrap();

    assert_eq!(summary.completed, 0);
    assert_eq!(summary.failed, 1);
}
```

- [x] **Step 2: Run focused failure**

Run:

```bash
cargo test -p voom-fakes remote_runner
```

Expected: compile failure because runner does not exist.

- [x] **Step 3: Implement runner**

Create:

```rust
#[derive(Debug, Clone)]
pub struct RemoteRunnerConfig {
    pub base_url: String,
    pub node_id: NodeId,
    pub token: secrecy::SecretString,
    pub worker_id: WorkerId,
    pub max_polls: u32,
    pub idle_timeout: std::time::Duration,
    pub lease_heartbeat_interval: std::time::Duration,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteRunnerSummary {
    pub acquired: u32,
    pub completed: u32,
    pub failed: u32,
    pub idle_polls: u32,
}
```

Loop:

1. POST node heartbeat with a fresh idempotency key.
2. POST acquire with a fresh idempotency key.
3. If idle, sleep/backoff until `max_polls` or idle timeout.
4. If leased, build `OperationRequest` from acquire response and dispatch through `voom_fake_support::dispatch_provider`.
5. Heartbeat lease while dispatch future is pending.
6. POST complete with result evidence or POST fail with `FailureClass::ArtifactUnavailable` for incompatible access and `FailureClass::MalformedWorkerResult` for malformed plan data.

Generate idempotency keys as deterministic strings in tests:

```rust
format!("runner-{}-{}", self.config.worker_id.0, sequence)
```

- [x] **Step 4: Verify and commit**

Run:

```bash
cargo test -p voom-fakes remote_runner
```

Expected: PASS.

Commit:

```bash
git add crates/voom-fakes/Cargo.toml crates/voom-fakes/src/remote_runner.rs crates/voom-fakes/src/remote_runner_test.rs crates/voom-fakes/src/bin/remote_synthetic_runner.rs
git commit -m "feat: add remote synthetic runner"
```

## Task 10: End-To-End Remote Execution Integration

**Files:**
- Create: `crates/voom-fakes/tests/remote_runner.rs`
- Modify: `crates/voom-control-plane/tests/durable_workflow.rs` only if existing workflow fixtures need a helper export.

- [x] **Step 1: Add failing integration tests**

Create integration tests covering acceptance:

```rust
#[tokio::test]
async fn remote_runner_executes_durable_ticket_over_http() {
    let fixture = RemoteExecutionFixture::new().await;
    fixture.seed_node_worker_capability_grant("transcode_video", &["shared_mount"]).await;
    fixture.seed_ready_ticket_with_artifact_plan("transcode_video", "shared_mount").await;

    let summary = fixture.run_remote_runner().await.unwrap();

    assert_eq!(summary.completed, 1);
    fixture.assert_ticket_succeeded().await;
    fixture.assert_events(&["lease.acquired", "ticket.leased", "lease.released", "ticket.succeeded"]).await;
    fixture.assert_artifact_plan_status("consumed").await;
}

#[tokio::test]
async fn stopped_runner_lease_expires_and_ticket_requeues() {
    let fixture = RemoteExecutionFixture::new().await;
    fixture.seed_node_worker_capability_grant("transcode_video", &["shared_mount"]).await;
    let lease = fixture.acquire_without_completing().await;
    fixture.advance_past_lease_ttl().await;

    let report = fixture.control_plane.remote_recover(fixture.now()).await.unwrap();

    assert_eq!(report.expired_leases, vec![lease.id]);
    fixture.assert_ticket_ready_again().await;
}
```

Add tests for stale node rejection and reactivation:

```rust
#[tokio::test]
async fn stale_node_cannot_acquire_until_heartbeat_reactivates_it() {
    let fixture = RemoteExecutionFixture::new().await;
    fixture.mark_node_stale().await;

    let err = fixture.acquire_over_http("stale-acquire").await.unwrap_err();
    assert_eq!(err.code, "CONFLICT");

    fixture.node_heartbeat_over_http("reactivate").await.unwrap();
    let idle = fixture.acquire_over_http("reactivated-acquire").await.unwrap();
    assert_eq!(idle.outcome, "idle");
}
```

- [x] **Step 2: Run focused failure**

Run:

```bash
cargo test -p voom-fakes remote_runner --test remote_runner
```

Expected: FAIL until runner, API route setup, and fixtures align.

- [x] **Step 3: Implement fixtures and fix integration gaps**

Implement `RemoteExecutionFixture` with:

- temp SQLite initialized through `voom_store::init`;
- `ControlPlane::open_with_pool_and_rng`;
- `HealthPlane::open`;
- `voom_api::router_with_control_plane`;
- local axum server bound to loopback;
- node registration through `register_node`;
- worker registration through `register_worker_for_node`;
- ticket creation and readiness through existing ticket control-plane/repo paths;
- event assertions through `EventRepo`.

- [x] **Step 4: Verify and commit**

Run:

```bash
cargo test -p voom-fakes remote_runner --test remote_runner
cargo test -p voom-fakes remote
```

Expected: PASS.

Commit:

```bash
git add crates/voom-fakes/tests/remote_runner.rs crates/voom-control-plane/tests/durable_workflow.rs
git commit -m "test: prove remote synthetic execution over http"
```

## Task 11: Closeout Evidence

**Files:**
- Create: `docs/superpowers/plans/2026-05-24-voom-sprint-8-closeout.md`

- [x] **Step 1: Create closeout matrix**

Create `docs/superpowers/plans/2026-05-24-voom-sprint-8-closeout.md`:

```markdown
# VOOM Sprint 8 Closeout

## Transport Boundary

- Evidence: bearer-token HTTP remote execution routes are covered by `cargo test -p voom-api remote_execution_route`.
- Boundary: Sprint 8 transport is for loopback, integration tests, or trusted isolated networks. It is not production-safe for untrusted networks because TLS, certificate management, and token rotation are out of scope.

## Acceptance Matrix

| Acceptance item | Evidence |
|---|---|
| Node-token authenticated remote execution routes | `cargo test -p voom-api remote_execution_route` |
| Worker-to-node ownership enforcement | `cargo test -p voom-control-plane remote_acquire_requires_worker_node_ownership_capability_and_grant` |
| Remote node heartbeat | `cargo test -p voom-api node_heartbeat` |
| Lease acquire, heartbeat, complete, fail | `cargo test -p voom-control-plane remote_acquire remote_heartbeat remote_complete remote_fail` |
| Idempotency replay and duplicate-key rejection | `cargo test -p voom-api idempotency` and `cargo test -p voom-store remote_idempotency` |
| Capability and grant enforcement | `cargo test -p voom-store operation_eligibility` |
| Synthetic setup with explicit grants | `cargo test -p voom-fakes remote_runner --test remote_runner` |
| Remote runner executes durable tickets | `cargo test -p voom-fakes remote_runner_executes_durable_ticket_over_http --test remote_runner` |
| Stale lease recovery | `cargo test -p voom-fakes stopped_runner_lease_expires_and_ticket_requeues --test remote_runner` |
| Stale node recovery | `cargo test -p voom-control-plane remote_recovery` |
| No audit events for individual missed heartbeats | `cargo test -p voom-control-plane remote_lease_heartbeat_emits_no_events` |
| Artifact access plan persistence | `cargo test -p voom-store artifact_access` |
| Synthetic artifact access validation | `cargo test -p voom-fake-support artifact_access` |
| Scheduler scoring and broad API hardening deferred | Sprint 8 spec §2 and this closeout transport boundary |
```

- [x] **Step 2: Verify closeout has no unchecked acceptance rows**

Run:

```bash
rg -n "Evidence:|Acceptance Matrix|not production-safe|Sprint 9|artifact access|idempotency" docs/superpowers/plans/2026-05-24-voom-sprint-8-closeout.md
```

Expected: matches each required closeout topic.

- [x] **Step 3: Commit**

```bash
git add docs/superpowers/plans/2026-05-24-voom-sprint-8-closeout.md
git commit -m "docs: add sprint 8 closeout matrix"
```

## Task 12: Final Verification And Cleanup

**Files:**
- Review all files changed by Tasks 1-11.

- [x] **Step 1: Run required targeted verification**

Run:

```bash
cargo test -p voom-api
cargo test -p voom-control-plane remote
cargo test -p voom-store artifact_access
cargo test -p voom-fakes remote
```

Expected: PASS. If a command has zero matching tests, add or rename tests so the command exercises Sprint 8 behavior before continuing.

- [x] **Step 2: Run test layout check**

Run:

```bash
just check-test-layout
```

Expected: PASS. Any new unit test file must be a sibling `*_test.rs` referenced from the source file with `#[path = "..."]`.

- [x] **Step 3: Run full CI**

Run:

```bash
just ci
```

Expected: PASS with no skipped required Sprint 8 checks.

- [x] **Step 4: Inspect final diff**

Run:

```bash
git status --short
git diff --stat HEAD
git diff --check
```

Expected: no whitespace errors; changed files match the Sprint 8 scope.

- [x] **Step 5: Commit final fixes**

If verification required fixes, replace the example file paths below with the actual files changed by that fix:

```bash
git add crates/voom-api/src/execution.rs crates/voom-control-plane/src/cases/remote_execution.rs
git commit -m "fix: complete sprint 8 verification"
```

If no fixes were needed, do not create an empty commit.

## Self-Review

- Spec coverage: The plan covers thin node-authenticated HTTP execution routes, deterministic acquire, node/worker auth, idempotency, lease heartbeat/complete/fail, recovery, remote runner, artifact access plans, fixture setup with grants, and closeout evidence.
- Scope boundary: The plan does not add broad public REST, scheduler scoring, production TLS, object storage, daemonization, web UI, or real media workers.
- Placeholder scan: The plan contains concrete file paths, type names, SQL, route names, test names, commands, and expected outcomes.
- Type consistency: `NodeId`, `WorkerId`, `TicketId`, `LeaseId`, `RemoteAcquireInput`, `RemoteAcquireOutcome`, `RemoteLeaseDispatch`, `ArtifactAccessMode`, and `ArtifactAccessPlanStatus` are introduced before later tasks use them.
