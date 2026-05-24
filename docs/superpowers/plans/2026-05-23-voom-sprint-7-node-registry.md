# VOOM Sprint 7 Node Registry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build durable node identity, token-backed node registration, heartbeat/stale lifecycle, and node-aware worker inspection.

**Architecture:** `voom-store` owns the `nodes` table, `workers.node_id`, and repository projections. `voom-control-plane` owns token generation/hashing, token verification, event emission, heartbeat lifecycle, and node-aware worker registration transactions. `voom-cli` exposes the public single-envelope node and worker commands without ever accepting bearer tokens as plain command arguments.

**Tech Stack:** Rust 2024, sqlx SQLite migrations, tokio, serde/serde_json, rand secure token generation, SHA-256 token hashing, constant-time comparison, clap subcommands, insta JSON snapshots, sibling unit-test layout, `just ci`.

---

## Assumptions And Decisions

- Reuse the existing public `CONFLICT` error code via `VoomError::Conflict` for token mismatch and rejected node status. The spec explicitly says not to add a code unless necessary, and the codebase already has `CONFLICT`.
- Add `NodeId` to `voom-core` next to the other durable ID newtypes.
- Add workspace dependencies `base64`, `sha2`, and `hex` because Sprint 7 requires base64url tokens and SHA-256 hex hashes; keep `constant_time_eq` as the comparison primitive already present in the workspace.
- Keep legacy `ControlPlane::register_worker(NewWorker)` intact for tests and internal setup, but make new CLI worker registration call `register_worker_for_node`.
- Keep `workers.node_id` nullable and make worker projections include `node: null` for legacy rows.
- Do not expand `voom-worker-protocol`, HTTP routes, scheduler scoring, daemon heartbeat loops, or worker leasing.

## Success Criteria

- New migration creates `nodes` with JSON metadata validation, positive TTL check, status/kind checks, unique names, and a nullable `workers.node_id` foreign key.
- Existing migrated worker rows remain valid and listable with `node: null`.
- `NodeRepo` supports register, get, list, verify token hash retrieval, heartbeat, mark stale, and retire with epoch checks.
- Node register returns plaintext token once; node list/show never expose plaintext token or hash.
- Heartbeat verifies the token, updates `last_seen_at`, sets `active`, increments `epoch`, and emits `node.heartbeat_recorded`.
- Stale marking is idempotent and emits one `node.marked_stale` event per changed node.
- Retired nodes reject heartbeat and node-aware worker registration.
- Node-aware worker registration verifies token and freshness, writes worker/capability/grant rows with `workers.node_id`, and emits all required events in one transaction.
- CLI token sources are mutually exclusive: `--token-file`, `--token-env`, `--token-stdin`.
- CLI node and worker commands emit exactly one JSON envelope on stdout and reviewed insta snapshots.
- Closeout evidence documents schema, token non-disclosure, heartbeat/stale behavior, worker node context, transactional audit events, and Sprint 8 HTTP deferral.
- `just ci` passes.

## File Map

- Modify: `Cargo.toml`, `Cargo.lock`: add workspace deps `base64`, `sha2`, `hex`.
- Modify: `crates/voom-control-plane/Cargo.toml`: depend on `base64`, `constant_time_eq`, `hex`, `sha2`.
- Modify: `crates/voom-core/src/ids.rs`, `ids_test.rs`: add `NodeId`.
- Create: `migrations/0009_nodes.sql`: `nodes` table and `workers.node_id`.
- Modify: `crates/voom-store/src/schema_test.rs`: migration inventory and schema assertions.
- Create: `crates/voom-store/src/repo/nodes.rs`, `nodes_test.rs`: node repository.
- Modify: `crates/voom-store/src/repo/workers.rs`, `workers_test.rs`: nullable node id and node-context projections.
- Modify: `crates/voom-store/src/repo/mod.rs`: export `nodes`.
- Modify: `crates/voom-events/src/kind.rs`, `kind_test.rs`, `subject.rs`, `subject_test.rs`, `payload.rs`, `payload_test.rs`: node and worker-link events.
- Modify: `crates/voom-control-plane/src/lib.rs`, `lib_test.rs`: add `nodes` repo and token service.
- Create: `crates/voom-control-plane/src/node_auth.rs`, `node_auth_test.rs`: token generation/hash/verify.
- Create: `crates/voom-control-plane/src/cases/nodes.rs`, `nodes_test.rs`: node use cases.
- Modify: `crates/voom-control-plane/src/cases/workers.rs`, `workers_test.rs`: node-aware registration.
- Modify: `crates/voom-control-plane/src/cases/mod.rs`: export `nodes`.
- Modify: `crates/voom-cli/src/cli.rs`, `main.rs`, `commands/mod.rs`: add `node` and `worker` commands.
- Create: `crates/voom-cli/src/commands/token_source.rs`, `token_source_test.rs`: token-source validation and reads.
- Create: `crates/voom-cli/src/commands/node.rs`, `node_test.rs`: node envelope data and command runners.
- Create: `crates/voom-cli/src/commands/worker.rs`, `worker_test.rs`: worker envelope data and command runners.
- Create: `crates/voom-cli/tests/node_envelope.rs`, `worker_envelope.rs`: end-to-end CLI snapshots.
- Create: `crates/voom-cli/tests/snapshots/node_envelope__*.snap`, `worker_envelope__*.snap`: reviewed snapshots.
- Create: `docs/superpowers/plans/2026-05-23-voom-sprint-7-closeout.md`: closeout evidence matrix.

## Task 1: IDs And Dependencies

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `crates/voom-control-plane/Cargo.toml`
- Modify: `crates/voom-core/src/ids.rs`
- Modify: `crates/voom-core/src/ids_test.rs`

- [ ] **Step 1: Add failing `NodeId` test**

Add to `crates/voom-core/src/ids_test.rs`:

```rust
#[test]
fn node_id_display_and_json_match_public_id_contract() {
    let id = NodeId(42);
    assert_eq!(id.to_string(), "42");
    assert_eq!(serde_json::to_string(&id).unwrap(), "42");
}
```

- [ ] **Step 2: Run focused failure**

Run: `cargo test -p voom-core node_id_display_and_json_match_public_id_contract`

Expected: compile failure because `NodeId` does not exist.

- [ ] **Step 3: Add `NodeId` and dependencies**

In `crates/voom-core/src/ids.rs`, add after `WorkerId`:

```rust
define_id!(NodeId);
```

In root `Cargo.toml` workspace dependencies, add:

```toml
base64 = "0.22.1"
hex = "0.4.3"
sha2 = "0.10.9"
```

In `crates/voom-control-plane/Cargo.toml` dependencies, add:

```toml
base64 = { workspace = true }
constant_time_eq = { workspace = true }
hex = { workspace = true }
sha2 = { workspace = true }
```

Run `cargo check -p voom-control-plane --lib` once after editing dependencies so Cargo resolves the new crates and updates `Cargo.lock`.

- [ ] **Step 4: Verify and commit**

Run:

```bash
cargo test -p voom-core node_id_display_and_json_match_public_id_contract
cargo check -p voom-control-plane --lib
```

Expected: PASS.

Commit:

```bash
git add Cargo.toml Cargo.lock crates/voom-control-plane/Cargo.toml crates/voom-core/src/ids.rs crates/voom-core/src/ids_test.rs
git commit -m "feat: add node id contract"
```

## Task 2: Node Schema Migration

**Files:**
- Create: `migrations/0009_nodes.sql`
- Modify: `crates/voom-store/src/schema_test.rs`

- [ ] **Step 1: Add failing migration tests**

Add tests in `crates/voom-store/src/schema_test.rs` that initialize a temp DB and assert:

```rust
let nodes_sql: String = sqlx::query_scalar(
    "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'nodes'",
)
.fetch_one(&pool)
.await
.unwrap();
assert!(nodes_sql.contains("CHECK (kind IN ('local','remote','synthetic'))"));
assert!(nodes_sql.contains("CHECK (status IN ('registered','active','stale','retired'))"));
assert!(nodes_sql.contains("CHECK (json_valid(metadata))"));
assert!(nodes_sql.contains("CHECK (heartbeat_ttl_seconds > 0)"));

let worker_node_col: i64 = sqlx::query_scalar(
    "SELECT COUNT(*) FROM pragma_table_info('workers') WHERE name = 'node_id'",
)
.fetch_one(&pool)
.await
.unwrap();
assert_eq!(worker_node_col, 1);

let fk_count: i64 = sqlx::query_scalar(
    "SELECT COUNT(*) FROM pragma_foreign_key_list('workers') WHERE \"table\" = 'nodes'",
)
.fetch_one(&pool)
.await
.unwrap();
assert_eq!(fk_count, 1);
```

Also add raw SQL assertions that invalid metadata, non-positive TTL, invalid kind, and invalid status are rejected.

- [ ] **Step 2: Run focused failure**

Run: `cargo test -p voom-store schema`

Expected: FAIL because migration `0009_nodes.sql` is missing.

- [ ] **Step 3: Create migration**

Create `migrations/0009_nodes.sql`:

```sql
-- Sprint 7 — durable node registry and node-aware workers.

CREATE TABLE nodes (
    id                      INTEGER PRIMARY KEY,
    name                    TEXT NOT NULL UNIQUE,
    kind                    TEXT NOT NULL CHECK (kind IN ('local','remote','synthetic')),
    status                  TEXT NOT NULL CHECK (status IN ('registered','active','stale','retired')),
    registered_at           TEXT NOT NULL,
    last_seen_at            TEXT NOT NULL,
    retired_at              TEXT,
    heartbeat_ttl_seconds   INTEGER NOT NULL CHECK (heartbeat_ttl_seconds > 0),
    auth_token_hash         TEXT NOT NULL,
    auth_token_hint         TEXT NOT NULL,
    metadata                TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata)),
    epoch                   INTEGER NOT NULL DEFAULT 0,
    CHECK ((status = 'retired' AND retired_at IS NOT NULL)
        OR (status != 'retired' AND retired_at IS NULL))
) STRICT;

CREATE INDEX nodes_by_status_seen ON nodes (status, last_seen_at, id);

ALTER TABLE workers
    ADD COLUMN node_id INTEGER REFERENCES nodes(id) ON DELETE RESTRICT;

CREATE INDEX workers_by_node ON workers (node_id) WHERE node_id IS NOT NULL;
```

- [ ] **Step 4: Verify and commit**

Run: `cargo test -p voom-store schema`

Expected: PASS.

Commit:

```bash
git add migrations/0009_nodes.sql crates/voom-store/src/schema_test.rs
git commit -m "feat: add node registry migration"
```

## Task 3: Node Repository

**Files:**
- Create: `crates/voom-store/src/repo/nodes.rs`
- Create: `crates/voom-store/src/repo/nodes_test.rs`
- Modify: `crates/voom-store/src/repo/mod.rs`

- [ ] **Step 1: Write failing repository tests**

Create `nodes_test.rs` with these tests and assertions:

```rust
#[tokio::test]
async fn register_get_and_list_round_trip_without_exposing_plaintext_token() {
    let (pool, _tmp) = fresh_pool().await;
    let repo = SqliteNodeRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let node = repo
        .register_in_tx(&mut tx, NewNode {
            name: "synthetic-a".to_owned(),
            kind: NodeKind::Synthetic,
            registered_at: T0,
            heartbeat_ttl_seconds: 60,
            auth_token_hash: "voom-node-token-sha256-v1:abc".to_owned(),
            auth_token_hint: "abc".to_owned(),
            metadata: serde_json::json!({"zone":"test"}),
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(node.status, NodeStatus::Registered);
    assert_eq!(node.last_seen_at, T0);
    assert_eq!(node.epoch, 0);
    let got = repo.get(node.id).await.unwrap().unwrap();
    assert_eq!(got.auth_token_hint, "abc");
    assert_eq!(got.metadata, serde_json::json!({"zone":"test"}));
    let listed = repo.list(Some(NodeStatus::Registered), 10).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, node.id);
}

#[tokio::test]
async fn heartbeat_moves_registered_or_stale_node_to_active_and_increments_epoch() {
    let (_tmp, pool, repo, node) = seeded_node(NodeStatus::Registered, T0).await;
    let mut tx = pool.begin().await.unwrap();
    let updated = repo
        .heartbeat_in_tx(&mut tx, node.id, T0 + Duration::seconds(10))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(updated.status, NodeStatus::Active);
    assert_eq!(updated.last_seen_at, T0 + Duration::seconds(10));
    assert_eq!(updated.epoch, node.epoch + 1);
}

#[tokio::test]
async fn mark_stale_nodes_changes_only_freshly_expired_non_retired_rows() {
    let (pool, _tmp) = fresh_pool().await;
    let repo = SqliteNodeRepo::new(pool.clone());
    let expired = seed_node(&pool, &repo, "expired", NodeStatus::Active, T0, 5).await;
    let fresh = seed_node(&pool, &repo, "fresh", NodeStatus::Active, T0 + Duration::seconds(20), 60).await;
    let stale = seed_node(&pool, &repo, "already-stale", NodeStatus::Stale, T0, 5).await;

    let mut tx = pool.begin().await.unwrap();
    let changed = repo
        .mark_stale_in_tx(&mut tx, T0 + Duration::seconds(10))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(changed.iter().map(|n| n.id).collect::<Vec<_>>(), vec![expired.id]);
    assert_eq!(repo.get(fresh.id).await.unwrap().unwrap().status, NodeStatus::Active);
    assert_eq!(repo.get(stale.id).await.unwrap().unwrap().epoch, stale.epoch);
}

#[tokio::test]
async fn retire_is_terminal_and_epoch_guarded() {
    let (_tmp, pool, repo, node) = seeded_node(NodeStatus::Active, T0).await;
    let mut tx = pool.begin().await.unwrap();
    let retired = repo
        .retire_in_tx(&mut tx, node.id, node.epoch, T0 + Duration::seconds(30))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(retired.status, NodeStatus::Retired);
    assert_eq!(retired.retired_at, Some(T0 + Duration::seconds(30)));
    let mut tx = pool.begin().await.unwrap();
    let err = repo
        .retire_in_tx(&mut tx, node.id, node.epoch, T0 + Duration::seconds(31))
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::Conflict);
}
```

Define local helpers in `nodes_test.rs`:

```rust
async fn fresh_pool() -> (sqlx::SqlitePool, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    (pool, tmp)
}

async fn seed_node(
    pool: &sqlx::SqlitePool,
    repo: &SqliteNodeRepo,
    name: &str,
    status: NodeStatus,
    last_seen_at: OffsetDateTime,
    ttl_seconds: u32,
) -> Node {
    let mut tx = pool.begin().await.unwrap();
    let mut node = repo
        .register_in_tx(&mut tx, NewNode {
            name: name.to_owned(),
            kind: NodeKind::Synthetic,
            registered_at: T0,
            heartbeat_ttl_seconds: ttl_seconds,
            auth_token_hash: format!("voom-node-token-sha256-v1:{name}"),
            auth_token_hint: name.to_owned(),
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    if status != NodeStatus::Registered || last_seen_at != T0 {
        let retired_at = (status == NodeStatus::Retired).then_some(last_seen_at);
        sqlx::query(
            "UPDATE nodes SET status = ?, last_seen_at = ?, retired_at = ?, epoch = 1 WHERE id = ?",
        )
        .bind(status.as_str())
        .bind(iso8601_for_test(last_seen_at))
        .bind(retired_at.map(iso8601_for_test))
        .bind(i64::try_from(node.id.0).unwrap())
        .execute(pool)
        .await
        .unwrap();
        node = repo.get(node.id).await.unwrap().unwrap();
    }
    node
}

async fn seeded_node(
    status: NodeStatus,
    last_seen_at: OffsetDateTime,
) -> (tempfile::NamedTempFile, sqlx::SqlitePool, SqliteNodeRepo, Node) {
    let (pool, tmp) = fresh_pool().await;
    let repo = SqliteNodeRepo::new(pool.clone());
    let node = seed_node(&pool, &repo, "seeded", status, last_seen_at, 60).await;
    (tmp, pool, repo, node)
}
```

Use a private `iso8601_for_test` helper in `nodes_test.rs` that calls `OffsetDateTime::format(&time::format_description::well_known::Iso8601::DEFAULT)`. Do not expose `repo::common::iso8601` just for tests.

- [ ] **Step 2: Run focused failure**

Run: `cargo test -p voom-store nodes_`

Expected: compile failure because `repo::nodes` does not exist.

- [ ] **Step 3: Implement repository types**

Create:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind { Local, Remote, Synthetic }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeStatus { Registered, Active, Stale, Retired }

impl NodeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Remote => "remote",
            Self::Synthetic => "synthetic",
        }
    }
}

impl NodeStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::Active => "active",
            Self::Stale => "stale",
            Self::Retired => "retired",
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewNode {
    pub name: String,
    pub kind: NodeKind,
    pub registered_at: OffsetDateTime,
    pub heartbeat_ttl_seconds: u32,
    pub auth_token_hash: String,
    pub auth_token_hint: String,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub id: NodeId,
    pub name: String,
    pub kind: NodeKind,
    pub status: NodeStatus,
    pub registered_at: OffsetDateTime,
    pub last_seen_at: OffsetDateTime,
    pub retired_at: Option<OffsetDateTime>,
    pub heartbeat_ttl_seconds: u32,
    pub auth_token_hint: String,
    pub metadata: serde_json::Value,
    pub epoch: u64,
}
```

Implement private `NodeKind::parse(&str) -> Result<Self, VoomError>` and `NodeStatus::parse(&str) -> Result<Self, VoomError>` with the exact strings from `migrations/0009_nodes.sql`. The public `as_str` methods must return those same strings and are used by tests and event payload construction.

Keep `auth_token_hash` out of `Node`. Add `NodeAuthRecord { id, status, last_seen_at, heartbeat_ttl_seconds, auth_token_hash }` for verification-only reads.

Implement trait methods:

```rust
async fn register_in_tx(&self, tx: &mut Transaction<'_, Sqlite>, input: NewNode) -> Result<Node, VoomError>;
async fn get(&self, id: NodeId) -> Result<Option<Node>, VoomError>;
async fn list(&self, status: Option<NodeStatus>, limit: u32) -> Result<Vec<Node>, VoomError>;
async fn auth_record_in_tx(&self, tx: &mut Transaction<'_, Sqlite>, id: NodeId) -> Result<Option<NodeAuthRecord>, VoomError>;
async fn heartbeat_in_tx(&self, tx: &mut Transaction<'_, Sqlite>, id: NodeId, now: OffsetDateTime) -> Result<Node, VoomError>;
async fn mark_stale_in_tx(&self, tx: &mut Transaction<'_, Sqlite>, now: OffsetDateTime) -> Result<Vec<Node>, VoomError>;
async fn retire_in_tx(&self, tx: &mut Transaction<'_, Sqlite>, id: NodeId, expected_epoch: u64, now: OffsetDateTime) -> Result<Node, VoomError>;
```

Implement stale selection in Rust after fetching non-retired nodes:

```rust
let expires_at = node.last_seen_at + Duration::seconds(i64::from(node.heartbeat_ttl_seconds));
if node.status != NodeStatus::Stale && expires_at <= now {
    sqlx::query("UPDATE nodes SET status = 'stale', epoch = epoch + 1 WHERE id = ?")
        .bind(i64_from_u64(node.id.0))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("nodes mark stale: {e}")))?;
}
```

This avoids SQLite timestamp precision and modifier-format differences. Already stale rows must be skipped so `mark_stale_in_tx` is idempotent and emits no duplicate events.

- [ ] **Step 4: Export and verify**

In `crates/voom-store/src/repo/mod.rs`, add:

```rust
pub mod nodes;
```

Run: `cargo test -p voom-store nodes_`

Expected: PASS.

Commit:

```bash
git add crates/voom-store/src/repo/mod.rs crates/voom-store/src/repo/nodes.rs crates/voom-store/src/repo/nodes_test.rs
git commit -m "feat: add node repository"
```

## Task 4: Worker Node Context Projections

**Files:**
- Modify: `crates/voom-store/src/repo/workers.rs`
- Modify: `crates/voom-store/src/repo/workers_test.rs`

- [ ] **Step 1: Add failing worker projection tests**

Add tests with these assertions:

```rust
#[tokio::test]
async fn legacy_worker_without_node_remains_listable_with_null_node_context() {
    let (_tmp, repo) = worker_repo_with_current_schema().await;
    let worker = repo
        .register(NewWorker {
            name: "legacy".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: T0,
            node_id: None,
        })
        .await
        .unwrap();

    let inspection = repo.get_inspection(worker.id).await.unwrap().unwrap();
    assert_eq!(inspection.worker.id, worker.id);
    assert!(inspection.node.is_none());
}

#[tokio::test]
async fn worker_registered_with_node_id_projects_node_context() {
    let (_tmp, worker_repo, node) = worker_repo_with_seeded_node().await;
    let worker = worker_repo
        .register(NewWorker {
            name: "linked".to_owned(),
            kind: WorkerKind::Remote,
            registered_at: T0,
            node_id: Some(node.id),
        })
        .await
        .unwrap();

    let inspection = worker_repo.get_inspection(worker.id).await.unwrap().unwrap();
    let context = inspection.node.unwrap();
    assert_eq!(context.id, node.id);
    assert_eq!(context.name, node.name);
    assert_eq!(context.kind, node.kind);
    assert_eq!(context.status, node.status);
    assert_eq!(context.last_seen_at, node.last_seen_at);
}
```

Add local helpers to `workers_test.rs`:

```rust
async fn worker_repo_with_current_schema() -> (tempfile::NamedTempFile, SqliteWorkerRepo) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    (tmp, SqliteWorkerRepo::new(pool))
}

async fn worker_repo_with_seeded_node() -> (tempfile::NamedTempFile, SqliteWorkerRepo, Node) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    let node_repo = SqliteNodeRepo::new(pool.clone());
    let mut tx = pool.begin().await.unwrap();
    let node = node_repo
        .register_in_tx(&mut tx, NewNode {
            name: "node-a".to_owned(),
            kind: NodeKind::Remote,
            registered_at: T0,
            heartbeat_ttl_seconds: 60,
            auth_token_hash: "voom-node-token-sha256-v1:node-a".to_owned(),
            auth_token_hint: "node-a".to_owned(),
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    (tmp, SqliteWorkerRepo::new(pool), node)
}
```

- [ ] **Step 2: Run focused failure**

Run: `cargo test -p voom-store worker`

Expected: compile failure because `NewWorker.node_id` and node projection types are missing.

- [ ] **Step 3: Extend worker repo**

Add:

```rust
pub struct WorkerNodeContext {
    pub id: NodeId,
    pub name: String,
    pub kind: NodeKind,
    pub status: NodeStatus,
    pub last_seen_at: OffsetDateTime,
}

pub struct WorkerInspection {
    pub worker: Worker,
    pub node: Option<WorkerNodeContext>,
}
```

Add `pub node_id: Option<NodeId>` to `NewWorker` and `Worker`. Change insert SQL to include `node_id`; bind `NULL` when absent. Add `get_inspection(id)` and `list_inspections(status: Option<WorkerStatus>, limit)` using a left join to `nodes`.

- [ ] **Step 4: Update existing callers and verify**

Update all existing `NewWorker` constructions to pass `node_id: None`.

Run: `cargo test -p voom-store worker`

Expected: PASS.

Commit:

```bash
git add crates/voom-store/src/repo/workers.rs crates/voom-store/src/repo/workers_test.rs crates/voom-control-plane crates/voom-cli
git commit -m "feat: project worker node context"
```

## Task 5: Event Vocabulary

**Files:**
- Modify: `crates/voom-events/src/kind.rs`, `kind_test.rs`
- Modify: `crates/voom-events/src/subject.rs`, `subject_test.rs`
- Modify: `crates/voom-events/src/payload.rs`, `payload_test.rs`

- [ ] **Step 1: Add failing event tests**

Add round-trip assertions for:

```rust
EventKind::NodeRegistered => "node.registered"
EventKind::NodeHeartbeatRecorded => "node.heartbeat_recorded"
EventKind::NodeMarkedStale => "node.marked_stale"
EventKind::NodeRetired => "node.retired"
EventKind::WorkerLinkedToNode => "worker.linked_to_node"
SubjectType::Node => "node"
```

Add payload serialization tests for `NodeRegisteredPayload`, `NodeHeartbeatRecordedPayload`, `NodeMarkedStalePayload`, `NodeRetiredPayload`, and `WorkerLinkedToNodePayload`.

- [ ] **Step 2: Run focused failure**

Run: `cargo test -p voom-events node worker_linked`

Expected: compile failure for missing variants and payloads.

- [ ] **Step 3: Implement event variants**

Add event kind variants, subject variant, payload structs:

```rust
pub struct NodeRegisteredPayload { pub node_id: u64, pub name: String, pub kind: String, pub status: String, pub heartbeat_ttl_seconds: u32 }
pub struct NodeHeartbeatRecordedPayload { pub node_id: u64, pub status: String, #[serde(with = "time::serde::iso8601")] pub last_seen_at: OffsetDateTime, pub epoch: u64 }
pub struct NodeMarkedStalePayload { pub node_id: u64, #[serde(with = "time::serde::iso8601")] pub marked_stale_at: OffsetDateTime, pub epoch: u64 }
pub struct NodeRetiredPayload { pub node_id: u64, #[serde(with = "time::serde::iso8601")] pub retired_at: OffsetDateTime, pub epoch: u64 }
pub struct WorkerLinkedToNodePayload { pub worker_id: u64, pub node_id: u64 }
```

Add matching `Event` enum variants and `Event::kind()` arms.

- [ ] **Step 4: Verify and commit**

Run: `cargo test -p voom-events`

Expected: PASS.

Commit:

```bash
git add crates/voom-events/src/kind.rs crates/voom-events/src/kind_test.rs crates/voom-events/src/subject.rs crates/voom-events/src/subject_test.rs crates/voom-events/src/payload.rs crates/voom-events/src/payload_test.rs
git commit -m "feat: add node audit events"
```

## Task 6: Token Service

**Files:**
- Create: `crates/voom-control-plane/src/node_auth.rs`
- Create: `crates/voom-control-plane/src/node_auth_test.rs`
- Modify: `crates/voom-control-plane/src/lib.rs`

- [ ] **Step 1: Add failing token tests**

Create tests:

```rust
#[test]
fn generated_token_uses_v1_prefix_and_256_bits() {
    let token = generate_token_from_bytes([7_u8; 32]).unwrap();
    let exposed = token.expose_secret();
    assert!(exposed.starts_with("voom-node-v1."));
    assert_eq!(exposed.trim_start_matches("voom-node-v1.").len(), 43);
}

#[test]
fn token_hash_uses_versioned_domain_separated_sha256_hex() {
    let hash = hash_node_token("voom-node-v1.test");
    assert_eq!(
        hash,
        "voom-node-token-sha256-v1:08356516626c757dd822687cdc9f324f329761b82869f5bc5a6a297062197c4b"
    );
}

#[test]
fn verification_uses_hash_equality_without_exposing_secret() {
    let hash = hash_node_token("voom-node-v1.valid");
    assert!(verify_node_token("voom-node-v1.valid", &hash));
    assert!(!verify_node_token("voom-node-v1.invalid", &hash));
}

#[test]
fn token_hint_is_short_suffix_only() {
    let hint = token_hint("voom-node-v1.abcdefghijklmnopqrstuvwxyz0123456789");
    assert_eq!(hint, "23456789");
    assert!(!hint.starts_with("voom-node-v1."));
}
```

- [ ] **Step 2: Run focused failure**

Run: `cargo test -p voom-control-plane node_auth`

Expected: compile failure because module is missing.

- [ ] **Step 3: Implement token service**

Create `NodeTokenService` with:

```rust
pub struct GeneratedNodeToken {
    pub plaintext: secrecy::SecretString,
    pub hash: String,
    pub hint: String,
}

pub trait NodeTokenGenerator: Send + Sync {
    fn generate(&self) -> Result<secrecy::SecretString, VoomError>;
}
```

Production generator fills 32 random bytes from `ControlPlane` RNG, encodes with `base64::engine::general_purpose::URL_SAFE_NO_PAD`, and prefixes `voom-node-v1.`. Hash format:

```rust
format!(
    "voom-node-token-sha256-v1:{}",
    hex::encode(Sha256::digest(format!("voom-node-token-v1:{token}").as_bytes()))
)
```

Verify with `constant_time_eq::constant_time_eq`.

- [ ] **Step 4: Wire into `ControlPlane` and verify**

Add a private `ControlPlane::generate_node_token(&self) -> Result<GeneratedNodeToken, VoomError>` helper that locks the existing shared RNG, fills 32 bytes, releases the lock before returning, hashes the token, and computes the hint. Tests call `ControlPlane::open_with_pool_and_rng` with `FrozenRng` for deterministic token material.

Run: `cargo test -p voom-control-plane node_auth`

Expected: PASS.

Commit:

```bash
git add crates/voom-control-plane/src/lib.rs crates/voom-control-plane/src/node_auth.rs crates/voom-control-plane/src/node_auth_test.rs
git commit -m "feat: add node token service"
```

## Task 7: Node Control-Plane Use Cases

**Files:**
- Modify: `crates/voom-control-plane/src/lib.rs`
- Modify: `crates/voom-control-plane/src/cases/mod.rs`
- Create: `crates/voom-control-plane/src/cases/nodes.rs`
- Create: `crates/voom-control-plane/src/cases/nodes_test.rs`

- [ ] **Step 1: Add failing use-case tests**

Create tests covering:

```rust
register_node_returns_plaintext_token_once_and_emits_event();
heartbeat_with_valid_token_activates_node_and_emits_event();
heartbeat_with_invalid_token_returns_conflict_without_mutation();
mark_stale_nodes_is_idempotent_and_emits_once_per_changed_node();
retire_node_is_terminal_and_emits_event();
list_and_show_nodes_do_not_expose_token_hash();
```

Each test must assert durable state and audit evidence:

- `register_node_returns_plaintext_token_once_and_emits_event`: assert returned token starts with `voom-node-v1.`, `nodes.auth_token_hash` starts with `voom-node-token-sha256-v1:`, stored hash differs from plaintext, and one `node.registered` event exists for `SubjectType::Node`.
- `heartbeat_with_valid_token_activates_node_and_emits_event`: register a node, heartbeat with the returned token, then assert status `Active`, epoch `1`, `last_seen_at == cp.clock().now()`, and one `node.heartbeat_recorded` event.
- `heartbeat_with_invalid_token_returns_conflict_without_mutation`: assert `ErrorCode::Conflict`, status remains `Registered`, epoch remains `0`, and no heartbeat event exists.
- `mark_stale_nodes_is_idempotent_and_emits_once_per_changed_node`: seed two expired active nodes and one already stale node, call twice, assert first call returns two nodes, second call returns none, and exactly two `node.marked_stale` events exist.
- `retire_node_is_terminal_and_emits_event`: retire with expected epoch, assert `retired_at.is_some()`, epoch increments, a second retire returns `CONFLICT`, and only one `node.retired` event exists.
- `list_and_show_nodes_do_not_expose_token_hash`: serialize command-facing DTOs to `serde_json::Value` and assert recursive key search finds no `token`, `auth_token_hash`, or plaintext token value.

- [ ] **Step 2: Run focused failure**

Run: `cargo test -p voom-control-plane nodes_`

Expected: compile failure because node cases are missing.

- [ ] **Step 3: Implement node cases**

Add request/response structs:

```rust
pub struct RegisterNodeInput { pub name: String, pub kind: NodeKind, pub heartbeat_ttl_seconds: u32, pub metadata: serde_json::Value }
pub struct RegisteredNode { pub node: Node, pub token: secrecy::SecretString }
```

Implement:

```rust
pub async fn register_node(&self, input: RegisterNodeInput) -> Result<RegisteredNode, VoomError>;
pub async fn heartbeat_node(&self, node_id: NodeId, token: &str) -> Result<Node, VoomError>;
pub async fn mark_stale_nodes(&self, now: OffsetDateTime) -> Result<Vec<Node>, VoomError>;
pub async fn retire_node(&self, node_id: NodeId, expected_epoch: u64, now: OffsetDateTime) -> Result<Node, VoomError>;
pub async fn get_node(&self, node_id: NodeId) -> Result<Option<Node>, VoomError>;
pub async fn list_nodes(&self, status: Option<NodeStatus>, limit: u32) -> Result<Vec<Node>, VoomError>;
```

Every mutating method opens one transaction, calls repo `_in_tx`, appends the matching event, and commits.

- [ ] **Step 4: Verify and commit**

Run: `cargo test -p voom-control-plane nodes_`

Expected: PASS.

Commit:

```bash
git add crates/voom-control-plane/src/lib.rs crates/voom-control-plane/src/cases/mod.rs crates/voom-control-plane/src/cases/nodes.rs crates/voom-control-plane/src/cases/nodes_test.rs
git commit -m "feat: add node lifecycle use cases"
```

## Task 8: Node-Aware Worker Registration

**Files:**
- Modify: `crates/voom-control-plane/src/cases/workers.rs`
- Modify: `crates/voom-control-plane/src/cases/workers_test.rs`

- [ ] **Step 1: Add failing tests**

Add tests:

```rust
register_worker_for_node_links_worker_and_emits_required_event_sequence();
invalid_node_token_rejects_worker_registration_without_partial_rows();
stale_node_rejects_worker_registration_until_heartbeat();
retired_node_rejects_worker_registration();
registered_node_with_fresh_ttl_can_register_worker_without_heartbeat_mutation();
```

Required assertions:

- Successful registration asserts exactly one worker row, `workers.node_id = node.id`, all capability rows exist, and events appear in this order: `worker.registered`, `worker.linked_to_node`, one `worker.capability_recorded` per capability, then one `worker.grant_recorded` per grant.
- Invalid token asserts `ErrorCode::Conflict`, zero new workers, zero capabilities, zero grants, and zero worker events.
- Stale node asserts worker registration fails with `CONFLICT`; after `heartbeat_node` with the same token, registration succeeds.
- Retired node asserts heartbeat and worker registration both fail with `CONFLICT`.
- Fresh registered node asserts worker registration succeeds without changing the node's `last_seen_at`, status, or epoch.

- [ ] **Step 2: Run focused failure**

Run: `cargo test -p voom-control-plane register_worker_for_node`

Expected: compile failure because the use case does not exist.

- [ ] **Step 3: Implement node-aware registration**

Add:

```rust
pub struct RegisterWorkerForNodeInput {
    pub node_id: NodeId,
    pub token: String,
    pub name: String,
    pub kind: WorkerKind,
    pub capabilities: Vec<NewWorkerCapabilityDraft>,
    pub grants: Vec<NewWorkerGrantDraft>,
}

pub struct NewWorkerCapabilityDraft {
    pub operation: String,
    pub codecs: Vec<String>,
    pub hardware: Vec<String>,
    pub artifact_access: Vec<String>,
    pub extra: serde_json::Value,
}

pub struct NewWorkerGrantDraft {
    pub can_execute: Vec<String>,
    pub can_access_read: Vec<String>,
    pub can_access_write: Vec<String>,
    pub denies: Vec<String>,
    pub max_parallel: serde_json::Value,
}
```

Draft fields omit `worker_id`; implementation fills it after worker insert before calling `record_capability_in_tx` or `record_grant_in_tx`. Reject empty capabilities for CLI path at the CLI layer, but keep control-plane capable of registering grants/capabilities supplied by tests.

Inside one transaction:

1. Load `NodeAuthRecord` with `auth_record_in_tx`.
2. Return `VoomError::NotFound` if missing.
3. Verify token hash; return `VoomError::Conflict("node token verification failed".into())` on mismatch.
4. Reject `stale` and `retired`.
5. Reject `registered` or `active` if `last_seen_at + ttl <= now`.
6. Insert worker with `node_id: Some(node_id)`.
7. Emit `worker.registered`.
8. Emit `worker.linked_to_node`.
9. Insert each capability and emit `worker.capability_recorded`.
10. Insert each grant and emit `worker.grant_recorded`.
11. Commit.

- [ ] **Step 4: Verify rollback and events**

Run: `cargo test -p voom-control-plane register_worker_for_node`

Expected: PASS, including event ordering and no partial worker row on injected failure.

Commit:

```bash
git add crates/voom-control-plane/src/cases/workers.rs crates/voom-control-plane/src/cases/workers_test.rs
git commit -m "feat: register workers through node auth"
```

## Task 9: CLI Token Sources

**Files:**
- Create: `crates/voom-cli/src/commands/token_source.rs`
- Create: `crates/voom-cli/src/commands/token_source_test.rs`
- Modify: `crates/voom-cli/src/commands/mod.rs`

- [ ] **Step 1: Add failing token-source tests**

Create tests:

```rust
#[tokio::test]
async fn token_file_reads_trimmed_token() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("node.token");
    tokio::fs::write(&path, "voom-node-v1.file\n").await.unwrap();
    let token = read_token(&TokenSourceArgs {
        token_file: Some(path),
        token_env: None,
        token_stdin: false,
    })
    .await
    .unwrap();
    assert_eq!(token, "voom-node-v1.file");
}

#[test]
fn token_source_rejects_zero_or_multiple_sources_as_bad_args() {
    assert_eq!(
        validate_token_source(&TokenSourceArgs {
            token_file: None,
            token_env: None,
            token_stdin: false,
        })
        .unwrap_err()
        .code(),
        ErrorCode::BadArgs
    );
    assert_eq!(
        validate_token_source(&TokenSourceArgs {
            token_file: Some(PathBuf::from("token")),
            token_env: Some("VOOM_TOKEN".to_owned()),
            token_stdin: false,
        })
        .unwrap_err()
        .code(),
        ErrorCode::BadArgs
    );
}
```

Do not use `std::env::set_var` in unit tests; Rust 2024 treats process-wide environment mutation as unsafe and the workspace forbids unsafe code. Cover successful `--token-env` reads in the end-to-end CLI tests with `std::process::Command::env`, which is safe and already used elsewhere in `crates/voom-cli/tests/`.

- [ ] **Step 2: Run focused failure**

Run: `cargo test -p voom-cli token_source`

Expected: compile failure because helper is missing.

- [ ] **Step 3: Implement helper**

Implement:

```rust
#[derive(Debug, Clone)]
pub struct TokenSourceArgs {
    pub token_file: Option<PathBuf>,
    pub token_env: Option<String>,
    pub token_stdin: bool,
}

#[derive(Debug, Clone)]
pub enum TokenSourceError {
    BadArgs(String),
}

impl TokenSourceError {
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        ErrorCode::BadArgs
    }
}

pub async fn read_token(args: &TokenSourceArgs) -> Result<String, TokenSourceError>;
pub fn validate_token_source(args: &TokenSourceArgs) -> Result<(), TokenSourceError>;
```

Define a local `TokenSourceError` with `code() -> ErrorCode` and return `ErrorCode::BadArgs` for zero sources, multiple sources, empty token values, missing env var, unreadable token file, or stdin read failure. Command runners must catch this error and call `emit_err("node", ErrorCode::BadArgs.as_str(), err.to_string(), Some("Pass exactly one token source".to_owned()), Some(local))` or `emit_err("worker", ErrorCode::BadArgs.as_str(), err.to_string(), Some("Pass exactly one token source".to_owned()), Some(local))` themselves so these operator mistakes cannot fall through `main.rs` as `CONFIG_INVALID` or `INTERNAL`.

Trim one trailing newline or CRLF from file/env/stdin values before validating non-empty content.

- [ ] **Step 4: Verify and commit**

Run: `cargo test -p voom-cli token_source`

Expected: PASS.

Commit:

```bash
git add crates/voom-cli/src/commands/mod.rs crates/voom-cli/src/commands/token_source.rs crates/voom-cli/src/commands/token_source_test.rs
git commit -m "feat: add cli token source reader"
```

## Task 10: Node CLI Commands

**Files:**
- Modify: `crates/voom-cli/src/cli.rs`
- Modify: `crates/voom-cli/src/main.rs`
- Modify: `crates/voom-cli/src/commands/mod.rs`
- Create: `crates/voom-cli/src/commands/node.rs`
- Create: `crates/voom-cli/src/commands/node_test.rs`
- Create: `crates/voom-cli/tests/node_envelope.rs`

- [ ] **Step 1: Add failing CLI tests**

Add end-to-end tests for:

```rust
node_register_outputs_token_once();
node_show_and_list_do_not_expose_token_hash_or_plaintext();
node_heartbeat_with_env_token_activates_node();
node_heartbeat_with_bad_token_returns_conflict_envelope();
node_retire_outputs_retired_status();
node_token_sources_are_mutually_exclusive_bad_args();
```

Snapshot assertions must redact only filesystem-local fields and deterministic token material that varies with RNG. They must not remove the presence or absence of keys. Add explicit JSON assertions before snapshotting: register contains `data.token`, show/list do not contain `token` or `auth_token_hash`, invalid heartbeat returns `error.code == "CONFLICT"`, and multiple token sources return `error.code == "BAD_ARGS"` with exit code `1`.

- [ ] **Step 2: Run focused failure**

Run: `cargo test -p voom-cli node_envelope`

Expected: clap failure because `node` command is unknown.

- [ ] **Step 3: Add clap surface and command runner**

Add `Command::Node(NodeCommand)` with:

```rust
Register { name: String, kind: NodeKindArg, heartbeat_ttl_seconds: Option<u32> }
Heartbeat { node_id: u64, token_file: Option<PathBuf>, token_env: Option<String>, token_stdin: bool }
List { status: Option<NodeStatusArg> }
Show { node_id: u64 }
Retire { node_id: u64, expected_epoch: u64 }
```

Default heartbeat TTL to `60`. Emit command name `"node"`. Open `ControlPlane` through `ControlPlane::open(&cfg.database_url)`; never create DBs outside `init`.

`node register` data includes:

```json
{"node":{"id":1,"name":"local-a","kind":"local","status":"registered","heartbeat_ttl_seconds":60,"epoch":0},"token":"voom-node-v1.AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE","token_hint":"BAQEBAQE"}
```

List/show omit `token` and `auth_token_hash`.

- [ ] **Step 4: Verify snapshots and commit**

Run: `cargo test -p voom-cli node_envelope`

Review deliberate snapshots with: `cargo insta review`

Commit:

```bash
git add crates/voom-cli/src/cli.rs crates/voom-cli/src/main.rs crates/voom-cli/src/commands/mod.rs crates/voom-cli/src/commands/node.rs crates/voom-cli/src/commands/node_test.rs crates/voom-cli/tests/node_envelope.rs crates/voom-cli/tests/snapshots
git commit -m "feat: add node cli envelopes"
```

## Task 11: Worker CLI Commands

**Files:**
- Modify: `crates/voom-cli/src/cli.rs`
- Modify: `crates/voom-cli/src/main.rs`
- Modify: `crates/voom-cli/src/commands/mod.rs`
- Create: `crates/voom-cli/src/commands/worker.rs`
- Create: `crates/voom-cli/src/commands/worker_test.rs`
- Create: `crates/voom-cli/tests/worker_envelope.rs`

- [ ] **Step 1: Add failing worker CLI tests**

Add end-to-end tests:

```rust
worker_register_requires_node_token_and_capability();
worker_register_with_valid_node_token_outputs_node_context();
worker_register_bad_token_returns_conflict_and_no_worker();
worker_list_shows_linked_node_and_legacy_null_node();
worker_show_shows_linked_node_context();
```

Before snapshotting, assert worker register/show/list envelopes include `data.worker.node.id`, `name`, `kind`, `status`, and `last_seen_at` for linked workers. Seed the legacy case with `cp.register_worker(NewWorker { name: "legacy".to_owned(), kind: WorkerKind::Synthetic, registered_at: cp.clock().now(), node_id: None })` and assert it serializes `"node": null`. For bad token registration, assert exit code `2`, `error.code == "CONFLICT"`, and a direct SQL count confirms no worker row with the requested name exists.

- [ ] **Step 2: Run focused failure**

Run: `cargo test -p voom-cli worker_envelope`

Expected: clap failure because `worker` command is unknown.

- [ ] **Step 3: Add worker command runner**

Add `Command::Worker(WorkerCommand)`:

```rust
Register {
    node_id: u64,
    name: String,
    kind: WorkerKindArg,
    capability: Vec<String>,
    token_file: Option<PathBuf>,
    token_env: Option<String>,
    token_stdin: bool,
}
List { status: Option<WorkerStatusArg> }
Show { worker_id: u64 }
```

For each CLI `--capability`, create a draft with `operation`, empty `codecs`, empty `hardware`, empty `artifact_access`, and `extra: {}`. Reject zero capabilities with `BAD_ARGS`. Emit command name `"worker"`.

- [ ] **Step 4: Verify snapshots and commit**

Run: `cargo test -p voom-cli worker_envelope`

Review deliberate snapshots with: `cargo insta review`

Commit:

```bash
git add crates/voom-cli/src/cli.rs crates/voom-cli/src/main.rs crates/voom-cli/src/commands/mod.rs crates/voom-cli/src/commands/worker.rs crates/voom-cli/src/commands/worker_test.rs crates/voom-cli/tests/worker_envelope.rs crates/voom-cli/tests/snapshots
git commit -m "feat: add worker node cli envelopes"
```

## Task 12: Closeout Evidence And Full Verification

**Files:**
- Create: `docs/superpowers/plans/2026-05-23-voom-sprint-7-closeout.md`
- Modify: any files surfaced by final lint/test failures.

- [ ] **Step 1: Run targeted test set**

Run:

```bash
cargo test -p voom-core node_id
cargo test -p voom-events node worker_linked
cargo test -p voom-store nodes_ worker
cargo test -p voom-control-plane node_auth nodes_ register_worker_for_node
cargo test -p voom-cli node_envelope worker_envelope token_source
```

Expected: all PASS.

- [ ] **Step 2: Run layout and leak checks**

Run:

```bash
just check-test-layout
rg -n "T[B]D|T[O]DO|auth_token_hash|voom-node-v1" docs/superpowers/plans/2026-05-23-voom-sprint-7-closeout.md crates/voom-cli/tests/snapshots
```

Expected: layout PASS. The `rg` output must not show leaked token hashes in CLI list/show snapshots; `voom-node-v1` may appear only in node register snapshots or token-source tests.

- [ ] **Step 3: Write closeout matrix**

Create `docs/superpowers/plans/2026-05-23-voom-sprint-7-closeout.md` with sections:

```markdown
# VOOM Sprint 7 Closeout Evidence

## Schema And Migration
- Evidence: `cargo test -p voom-store schema nodes_ worker`
- Covers: `nodes`, nullable `workers.node_id`, JSON checks, foreign keys.

## Token Storage And Non-Disclosure
- Evidence: `cargo test -p voom-control-plane node_auth nodes_`; `cargo test -p voom-cli node_envelope`
- Covers: plaintext returned only by register; list/show omit token hash and plaintext.

## Heartbeat And Stale State
- Evidence: `cargo test -p voom-control-plane nodes_`
- Covers: heartbeat activation, stale idempotence, retired rejection.

## Node-Aware Worker Registration
- Evidence: `cargo test -p voom-control-plane register_worker_for_node`; `cargo test -p voom-cli worker_envelope`
- Covers: node token verification, freshness, linked worker inspection, legacy null node.

## Audit Events
- Evidence: `cargo test -p voom-events node worker_linked`; control-plane event-order tests.
- Covers: node and worker-link event payloads in same transaction as mutations.

## Explicit Deferrals
- Sprint 8: HTTP registration and heartbeat routes.
- Sprint 9: scheduler scoring, node-level policy, locality, and concurrency.
- Deferred: daemon heartbeat loops, token rotation, TLS/cert management, real media workers, web UI.
```

- [ ] **Step 4: Run full CI**

Run: `just ci`

Expected: PASS.

- [ ] **Step 5: Commit closeout and final fixes**

Commit:

```bash
git add docs/superpowers/plans/2026-05-23-voom-sprint-7-closeout.md
git commit -m "docs: record sprint 7 closeout evidence"
```

## Self-Review Notes

- Spec coverage: migration, node repo, token auth, heartbeat/stale/retire, node-aware worker registration, audit events, CLI node/worker surfaces, golden snapshots, and closeout matrix are each assigned to a task.
- Deferred scope: HTTP, remote leasing/dispatch, artifact access plans, scheduler scoring, node concurrency, TLS/certs, token rotation, real workers, daemon loops, and web UI remain out of scope.
- Type consistency: `NodeId`, `NodeKind`, `NodeStatus`, `WorkerNodeContext`, `RegisterWorkerForNodeInput`, and `TokenSourceArgs` are introduced before use by later CLI and control-plane tasks.
- Verification loop: every task has a focused failure command, a passing command, and a commit checkpoint; final task runs targeted tests, layout checks, snapshot review, and `just ci`.
