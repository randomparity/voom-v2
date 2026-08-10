# Durable Scan Sessions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development (recommended) or
> executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for
> tracking.

**Goal:** Add durable, ordered, idempotent manual scan sessions whose successful completion is the
only operation allowed to reconcile absent rooted locations.

**Architecture:** Migration 0036 stores session, batch, and observation facts and adds
completion-only provenance pointers to roots and retained locations. `voom-store` owns checked SQL
rows, while `voom-control-plane` owns authenticated transaction ordering, replay, lifecycle events,
and atomic reconciliation; axum and the CLI expose those typed use cases without traversing bytes.

**Tech Stack:** Rust 2024, tokio, sqlx/SQLite, axum, serde/serde_json, clap, insta, `just`, and the
existing `voom-core`, `voom-events`, `voom-store`, `voom-control-plane`, `voom-api`, and `voom-cli`
crates.

## Global Constraints

- Frozen scope identity is issue #419 and token `C7BC1E00-731C-4095-99D3-97E3E96A78AE`.
- Migration number is exactly 0036 and the governing decision is ADR 0067.
- Do not add traversal, hashing, probing, filesystem access, scan tickets, or scheduler locality;
  issues #421 and #420 own those concerns.
- Keep `voom scan --root` unchanged and explicitly transitional.
- Tickets and leases remain the only work-routing mechanism.
- Use one `BEGIN IMMEDIATE` transaction for every read-then-write lifecycle operation.
- Only `succeeded` completion may retire locations or update
  `library_roots.last_scan_session_id`; no partial reconciliation is observable.
- Node mutations authenticate bearer, current incarnation, and session/root ownership before
  revealing session existence.
- Exact accepted replays are read-only, do not extend deadlines or emit events, and resolve before
  mutable root/deadline fences.
- Batch size is 1–1000 observations; API body size remains 1 MiB and request timeout remains 30s.
- Session idle timeout is 1–86400 seconds with a 300-second request default.
- Terminal reasons are 1–1024 UTF-8 bytes with no NUL; SQLite counts bytes with
  `length(CAST(terminal_reason AS BLOB))`.
- API/CLI pagination defaults to 50, caps at 100, and uses exclusive ascending ID cursors.
- Durable serde roots use annotated content structs with `#[serde(deny_unknown_fields)]` per
  ADR 0013; no inline tagged struct variants.
- Preserve domain newtypes across store, control-plane, event, API, and CLI boundaries.
- Treat every SQLite value as untrusted and classify corrupt storage as `VoomError::Database`.
- Unit tests are sibling `*_test.rs` files; DB tests use real time plus `ManualClock`, never paused
  Tokio time with `SqlitePool`.
- Keep functions at most 100 lines, cyclomatic complexity at most 8, lines at most 100 columns, and
  all lint/test output warning-free.
- Do not add dependencies.

---

## File structure

- `crates/voom-core/src/taxonomy/scan.rs`: scan status and bounded terminal-reason vocabulary.
- `crates/voom-events/src/payload/scan.rs`: strict scan-session event payload family.
- `migrations/0036_scan_sessions.sql`: normalized session/batch/observation schema and provenance
  pointers.
- `crates/voom-store/src/repo/scan/sessions.rs`: checked scan rows, transitions, candidates, and
  inspection pagination.
- `crates/voom-control-plane/src/scan/sessions.rs`: public inputs/outcomes and transaction
  orchestration.
- `crates/voom-api/src/scan.rs`: authenticated `/v1/scan` node routes.
- `crates/voom-cli/src/commands/scan_session.rs`: local operator request/inspection/cancel envelope
  commands.
- Sibling test files own unit/transaction tests; API/CLI integration tests own public contracts.

### Task 1: Define scan domain and event vocabulary

**Files:**

- Modify: `crates/voom-core/src/taxonomy/ids.rs`
- Modify: `crates/voom-core/src/taxonomy/ids_test.rs`
- Create: `crates/voom-core/src/taxonomy/scan.rs`
- Create: `crates/voom-core/src/taxonomy/scan_test.rs`
- Modify: `crates/voom-core/src/taxonomy/mod.rs`
- Modify: `crates/voom-core/src/lib.rs`
- Modify: `crates/voom-events/src/kind.rs`
- Modify: `crates/voom-events/src/kind_test.rs`
- Modify: `crates/voom-events/src/subject.rs`
- Modify: `crates/voom-events/src/subject_test.rs`
- Create: `crates/voom-events/src/payload/scan.rs`
- Create: `crates/voom-events/src/payload/scan_test.rs`
- Modify: `crates/voom-events/src/payload/mod.rs`
- Modify: `crates/voom-events/src/payload/mod_test.rs`

**Interfaces:**

- Consumes: existing `define_id!`, explicit string-vocabulary patterns, `StorageRootId`, `NodeId`,
  `NodeIncarnationId`, and strict event content structs.
- Produces: `ScanSessionId`, `ScanSessionStatus`, `ScanTerminalReason`,
  `SubjectType::ScanSession`, seven `EventKind` variants, and matching strict `Event` variants.

- [ ] **Step 1: Write failing core vocabulary tests**

Add sibling tests that require the exact lifecycle tokens and UTF-8 byte contract:

```rust
#[test]
fn scan_status_round_trips_exact_durable_tokens() {
    for (status, wire) in [
        (ScanSessionStatus::Requested, "requested"),
        (ScanSessionStatus::Running, "running"),
        (ScanSessionStatus::Succeeded, "succeeded"),
        (ScanSessionStatus::Failed, "failed"),
        (ScanSessionStatus::Cancelled, "cancelled"),
        (ScanSessionStatus::Stale, "stale"),
    ] {
        assert_eq!(status.as_str(), wire);
        assert_eq!(
            ScanSessionStatus::parse_database("scan_sessions.status", wire).unwrap(),
            status,
        );
    }
}

#[test]
fn terminal_reason_is_bounded_by_encoded_bytes() {
    assert!(ScanTerminalReason::new("é".repeat(512)).is_ok());
    assert!(ScanTerminalReason::new(format!("{}a", "é".repeat(512))).is_err());
    assert!(ScanTerminalReason::new(" ").is_err());
    assert!(ScanTerminalReason::new("bad\0reason").is_err());
}
```

- [ ] **Step 2: Run the core tests and confirm the new symbols are missing**

Run: `cargo test -p voom-core --all-features taxonomy::scan`

Expected: compilation fails because `ScanSessionStatus` and `ScanTerminalReason` do not exist.

- [ ] **Step 3: Implement the core types and exports**

Add `define_id!(ScanSessionId)` beside storage-root IDs. In `taxonomy/scan.rs`, implement this
exact public shape and explicit parsing; `parse_database` maps invalid persisted text to
`VoomError::Database`, while `new` maps invalid operator/node input to `VoomError::Config`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanSessionStatus {
    Requested,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanTerminalReason(String);

impl ScanTerminalReason {
    pub fn new(value: impl Into<String>) -> Result<Self, VoomError>;
    pub fn parse_database(field: &str, value: String) -> Result<Self, VoomError>;
    #[must_use]
    pub fn as_str(&self) -> &str;
}
```

Validation is `!value.trim().is_empty()`, `value.len() <= 1024`, and
`!value.as_bytes().contains(&0)`. Add
`#[cfg(test)] #[path = "scan_test.rs"] mod tests;` and re-export all three public types from
`voom_core`. Implement `Serialize` as the inner string and custom `Deserialize` through `new` so
JSON cannot construct an invalid reason.

- [ ] **Step 4: Write failing event vocabulary and strict-payload tests**

Require `SubjectType::ScanSession`, all seven dotted event strings, JSON round trips, unknown-field
rejection on every concrete content struct, and typed IDs/statuses. The payload family is:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanSessionLifecyclePayload {
    pub scan_session_id: ScanSessionId,
    pub storage_root_id: StorageRootId,
    pub status: ScanSessionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanObservationBatchAcceptedPayload {
    pub scan_session_id: ScanSessionId,
    pub sequence: u64,
    pub batch_observation_count: u64,
    pub cumulative_observation_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanSessionSucceededPayload {
    pub scan_session_id: ScanSessionId,
    pub storage_root_id: StorageRootId,
    pub observation_count: u64,
    pub retired_location_count: u64,
}
```

Use `ScanSessionLifecyclePayload` for requested, started, failed, cancelled, and stale events.

- [ ] **Step 5: Run the event tests and confirm the variants are missing**

Run: `cargo test -p voom-events --all-features scan`

Expected: compilation fails on missing scan subject, kinds, payloads, and `Event` variants.

- [ ] **Step 6: Implement explicit event mappings**

Add `ScanSessionRequested`, `ScanSessionStarted`, `ScanObservationBatchAccepted`,
`ScanSessionSucceeded`, `ScanSessionFailed`, `ScanSessionCancelled`, and `ScanSessionStale` to
`EventKind`, with exact `scan_session.*` `as_str`/`from_str` arms. Add newtype `Event` variants with
matching `#[serde(rename = "...")]` attributes and `Event::kind()` arms. Do not use inline tagged
struct variants.

- [ ] **Step 7: Run focused checks and commit**

Run:

```bash
cargo test -p voom-core --all-features taxonomy::scan
cargo test -p voom-events --all-features scan
just fmt-check
```

Expected: all commands pass with zero warnings.

Commit:

```bash
git add crates/voom-core crates/voom-events
git commit -m "feat: add scan session vocabulary"
```

### Task 2: Add migration 0036 and checked store records

**Files:**

- Create: `migrations/0036_scan_sessions.sql`
- Modify: `crates/voom-store/src/migrator.rs`
- Modify: `crates/voom-store/src/init_test.rs`
- Modify: `crates/voom-store/src/schema_test.rs`
- Modify: `crates/voom-store/src/repo/mod.rs`
- Create: `crates/voom-store/src/repo/scan/mod.rs`
- Create: `crates/voom-store/src/repo/scan/sessions.rs`
- Create: `crates/voom-store/src/repo/scan/sessions_test.rs`
- Modify: `crates/voom-store/src/repo/library/library_roots.rs`
- Modify: `crates/voom-store/src/repo/library/library_roots_test.rs`
- Modify: `crates/voom-store/src/repo/media/identity.rs`
- Modify: `crates/voom-store/src/repo/media/identity_test.rs`

**Interfaces:**

- Consumes: Task 1 types, `repo::common` checked conversions, library-root rows, rooted
  `FileLocation` rows, and the embedded migrator pattern.
- Produces: migration 0036, `SqliteScanSessionRepo`, `ScanSession`, `ScanObservation`,
  `ScanBatchOutcome`, `ScanReconciliationEvidence`, and provenance-aware root/location decoding.

- [ ] **Step 1: Write failing migration and corruption tests**

Add tests that initialize through 0035, insert representative roots and locations, apply 0036,
and assert preservation plus these database rejections:

```rust
assert_eq!(expected_migrations(), 36);
assert_sql_rejected("status = 'unknown'").await;
assert_sql_rejected("terminal_reason = ''").await;
assert_sql_rejected(&format!("terminal_reason = '{}'", "é".repeat(513))).await;
assert_sql_rejected("next_sequence = -1").await;
assert_sql_rejected("live location with retired_by_scan_session_id").await;
```

Also assert the partial unique index rejects two `requested`/`running` rows for one root but allows
active sessions for different roots and historical terminal rows.

- [ ] **Step 2: Run migration tests and verify migration count is still 35**

Run:

```bash
cargo test -p voom-store --all-features --lib schema_test
cargo test -p voom-store --all-features --lib init_test
```

Expected: failures report expected migration 36 and missing scan tables/columns.

- [ ] **Step 3: Write migration 0036 and register it**

Create the three strict tables and indexes from the approved spec. The state constraint must make
terminal shape explicit, and these schema fragments are load-bearing:

```sql
CREATE UNIQUE INDEX scan_sessions_one_active_per_root
ON scan_sessions(storage_root_id)
WHERE status IN ('requested', 'running');

CREATE UNIQUE INDEX scan_observations_one_locator_per_session
ON scan_observations(scan_session_id, provider_relative_locator);

ALTER TABLE library_roots ADD COLUMN last_scan_session_id INTEGER
    REFERENCES scan_sessions(id) ON DELETE RESTRICT;

ALTER TABLE file_locations ADD COLUMN retired_by_scan_session_id INTEGER
    REFERENCES scan_sessions(id) ON DELETE RESTRICT
    CHECK (retired_by_scan_session_id IS NULL OR retired_at IS NOT NULL);
```

`terminal_reason` uses
`length(CAST(terminal_reason AS BLOB)) BETWEEN 1 AND 1024` and
`instr(terminal_reason, char(0)) = 0`. Observation object identity uses the same byte expression
with bounds 1 and 4096. Register version 36 in the explicit `MIGRATOR` vector; do not introduce a
second migration or a down migration.

- [ ] **Step 4: Define store records and checked decoders**

Implement typed records in `repo/scan/sessions.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanSession {
    pub id: ScanSessionId,
    pub storage_root_id: StorageRootId,
    pub root_epoch: u64,
    pub owner_node_id: NodeId,
    pub owner_incarnation_id: Option<NodeIncarnationId>,
    pub status: ScanSessionStatus,
    pub next_sequence: u64,
    pub batch_count: u64,
    pub observation_count: u64,
    pub idle_timeout_seconds: u32,
    pub progress_deadline_at: OffsetDateTime,
    pub location_high_watermark_id: Option<FileLocationId>,
    pub requested_at: OffsetDateTime,
    pub started_at: Option<OffsetDateTime>,
    pub terminal_at: Option<OffsetDateTime>,
    pub terminal_reason: Option<ScanTerminalReason>,
    pub retired_location_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanObservation {
    pub provider_relative_locator: ProviderRelativeLocator,
    pub provider_object_identity: String,
    pub size_bytes: u64,
    pub modified_at: OffsetDateTime,
    pub stability_started_at: OffsetDateTime,
    pub stability_confirmed_at: OffsetDateTime,
}
```

Decode every integer with checked helpers, timestamps with the existing ISO-8601 parser, status and
reason through Task 1, locators through `ProviderRelativeLocator`, and object identity through a
dedicated 1–4096-byte/no-NUL validator. Extend `LibraryRoot` with
`last_scan_session_id: Option<ScanSessionId>` and `FileLocation` with
`retired_by_scan_session_id: Option<ScanSessionId>` without flattening either ID.

- [ ] **Step 5: Test direct row corruption and pointer-preserving decoders**

Inject negative counts, invalid status/timestamps/locator/object identity, terminal-shape mismatch,
and out-of-contract persisted reasons through direct SQL. Each public read must return
`VoomError::Database`, never absence/conflict/config. Confirm old root/location fixtures decode with
null provenance and newly retired fixtures retain typed provenance.

- [ ] **Step 6: Run focused store gates and commit**

Run:

```bash
cargo test -p voom-store --all-features --lib schema_test
cargo test -p voom-store --all-features --lib scan::sessions
cargo test -p voom-store --all-features --lib library_roots
cargo test -p voom-store --all-features --lib identity
just fmt-check
```

Expected: migration count, schema constraints, preservation, and all checked decoders pass.

Commit:

```bash
git add migrations/0036_scan_sessions.sql crates/voom-store
git commit -m "feat: persist durable scan sessions"
```

### Task 3: Implement repository lifecycle, batches, and inspection

**Files:**

- Modify: `crates/voom-store/src/repo/scan/sessions.rs`
- Modify: `crates/voom-store/src/repo/scan/sessions_test.rs`

**Interfaces:**

- Consumes: Task 2 records and schema, `Transaction<'_, Sqlite>`, typed IDs, and checked cursor
  helpers.
- Produces: the complete in-transaction repository API used by control-plane orchestration:

```rust
pub async fn insert_requested_in_tx(
    &self,
    tx: &mut Transaction<'_, Sqlite>,
    input: NewScanSession,
) -> Result<ScanSession, VoomError>;
pub async fn get_in_tx(
    &self,
    tx: &mut Transaction<'_, Sqlite>,
    id: ScanSessionId,
) -> Result<Option<ScanSession>, VoomError>;
pub async fn start_in_tx(
    &self,
    tx: &mut Transaction<'_, Sqlite>,
    id: ScanSessionId,
    incarnation_id: NodeIncarnationId,
    location_high_watermark_id: Option<FileLocationId>,
    deadline: OffsetDateTime,
    now: OffsetDateTime,
) -> Result<ScanSession, VoomError>;
pub async fn accepted_batch_in_tx(
    &self,
    tx: &mut Transaction<'_, Sqlite>,
    input: NewScanObservationBatch,
) -> Result<ScanBatchOutcome, VoomError>;
pub async fn terminalize_in_tx(
    &self,
    tx: &mut Transaction<'_, Sqlite>,
    id: ScanSessionId,
    status: ScanSessionStatus,
    reason: ScanTerminalReason,
    now: OffsetDateTime,
) -> Result<ScanSession, VoomError>;
pub async fn list(&self, query: ScanSessionListQuery) -> Result<ScanSessionPage, VoomError>;
pub async fn reconciliation_page(
    &self,
    query: ScanReconciliationQuery,
) -> Result<ScanReconciliationPage, VoomError>;
```

Define every adjacent store input/output in the same module:

```rust
pub struct NewScanSession {
    pub storage_root_id: StorageRootId,
    pub root_epoch: u64,
    pub owner_node_id: NodeId,
    pub idle_timeout_seconds: u32,
    pub progress_deadline_at: OffsetDateTime,
    pub requested_at: OffsetDateTime,
}
pub struct NewScanObservationBatch {
    pub scan_session_id: ScanSessionId,
    pub sequence: u64,
    pub request_hash: String,
    pub observations: Vec<ScanObservation>,
    pub accepted_at: OffsetDateTime,
    pub next_progress_deadline_at: OffsetDateTime,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanBatchOutcome {
    pub scan_session_id: ScanSessionId,
    pub sequence: u64,
    pub accepted_observation_count: u64,
    pub cumulative_observation_count: u64,
}
pub struct ScanSessionListQuery {
    pub storage_root_id: Option<StorageRootId>,
    pub status: Option<ScanSessionStatus>,
    pub after_id: Option<ScanSessionId>,
    pub limit: u32,
}
pub struct ScanSessionPage {
    pub items: Vec<ScanSession>,
    pub next_after_id: Option<ScanSessionId>,
}
pub struct ScanReconciliationQuery {
    pub scan_session_id: ScanSessionId,
    pub after_id: Option<FileLocationId>,
    pub limit: u32,
}
pub struct ScanReconciliationEvidence {
    pub file_location_id: FileLocationId,
    pub retired_at: OffsetDateTime,
    pub prior_epoch: u64,
    pub retired_epoch: u64,
}
pub struct ScanReconciliationPage {
    pub items: Vec<ScanReconciliationEvidence>,
    pub next_after_id: Option<FileLocationId>,
}
```

- [ ] **Step 1: Write failing request/start transition tests**

Cover insert snapshots, timeout bounds, active-root uniqueness, start state guard, incarnation bind,
deadline reset, and maximum live rooted location ID. Use two real concurrent connections for the
same-root race and assert one durable winner; request different roots concurrently and assert both
win.

```rust
let (left, right) = tokio::join!(request(root_a), request(root_a));
assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
assert_eq!(repo.active_for_root(root_a).await.unwrap().unwrap().status,
           ScanSessionStatus::Requested);
```

- [ ] **Step 2: Run transition tests and verify methods are missing**

Run: `cargo test -p voom-store --all-features --lib scan::sessions::tests::request`

Expected: compilation fails on missing repository methods.

- [ ] **Step 3: Implement guarded request/start SQL**

Every method receives an existing immediate transaction; it must not open or commit its own
transaction. Use compare-and-set `WHERE status = ...` updates and verify exactly one affected row.
Map the partial-index uniqueness error to a conflict naming the active session ID by reading it in
the same transaction. Compute the start high-water mark with:

```sql
SELECT MAX(id)
FROM file_locations
WHERE storage_root_id = ?
  AND address_state = 'rooted'
  AND retired_at IS NULL
```

- [ ] **Step 4: Write failing batch/replay tests**

Test a new zero-based batch, same sequence/hash replay, conflicting same sequence, gap, regression,
duplicate locator within one body, duplicate locator across bodies, cross-session route-hash reuse,
1000 accepted observations, 1001 rejected observations, signed-size overflow, and reversed
stability timestamps. Assert observation and event-facing outcome counts remain unchanged on every
replay or rejection.

```rust
let first = repo.accepted_batch_in_tx(&mut tx, batch(0, "hash-a", observations())).await?;
let replay = repo.accepted_batch_in_tx(&mut tx, batch(0, "hash-a", observations())).await?;
assert_eq!(first, replay);
assert_eq!(count("scan_observation_batches").await, 1);
assert_eq!(count("scan_observations").await, observations().len());
```

- [ ] **Step 5: Implement atomic batch insert and replay lookup**

Validate the whole input before SQL. Resolve `(session_id, sequence)` first: matching hash returns
the checked stored outcome, different hash returns conflict, and absence requires `running` plus
`sequence == next_sequence`. Insert batch and all scalar observations, then update the session with
checked counters and the supplied new deadline. Convert SQLite unique violations into a conflict
that names the duplicate locator without logging provider object identity.

- [ ] **Step 6: Write failing inspection and semantic-corruption tests**

Require session list ordering by `id ASC`, exclusive cursor behavior, status/root filters, limit
1–100, and reconciliation order by `file_location_id ASC`. Directly create each FK-valid
corruption: wrong-root/non-success/older-success root pointers, wrong-root/non-success location
pointers,
mismatched terminal/retirement time, above-watermark pointer, observed attributed locator, and
retired-count mismatch. Following the affected pointer must return `VoomError::Database` while
historical session-by-ID reads still work.

- [ ] **Step 7: Implement checked inspection joins**

When following `last_scan_session_id`, require it to equal:

```sql
SELECT MAX(id) FROM scan_sessions
WHERE storage_root_id = ? AND status = 'succeeded'
```

Reconciliation pagination joins the session and location and validates succeeded status, same
root, non-null retirement, equal terminal/retirement timestamps, `location.id <= high_watermark`,
absence of a matching observation, epoch at least one, and total attributed count equal to the
session count. Derive `prior_epoch = retired_epoch - 1` with checked subtraction.

- [ ] **Step 8: Run store tests and commit**

Run:

```bash
cargo test -p voom-store --all-features --lib scan::sessions
just fmt-check
just lint
```

Expected: lifecycle, contention, batch/replay, pagination, and corruption tests pass warning-free.

Commit:

```bash
git add crates/voom-store/src/repo/scan
git commit -m "feat: add scan session repository"
```

### Task 4: Orchestrate request, start, batch, failure, cancellation, and staleness

**Files:**

- Create: `crates/voom-control-plane/src/scan/sessions.rs`
- Create: `crates/voom-control-plane/src/scan/sessions_test.rs`
- Modify: `crates/voom-control-plane/src/scan/mod.rs`
- Modify: `crates/voom-control-plane/src/lib.rs`
- Modify: `crates/voom-control-plane/src/lib_test.rs`
- Modify: `crates/voom-control-plane/src/cases/execution/remote_execution/mod.rs`
- Modify: `scripts/payload-contract-scope.txt`
- Modify: `docs/payload-contract-inventory.md`

**Interfaces:**

- Consumes: Tasks 1–3, `begin_immediate_tx`, `append_event`, remote bearer/incarnation helpers,
  `SqliteRemoteIdempotencyRepo`, effective root availability, and injected `Clock`.
- Produces: `ControlPlane` scan-session use cases and strict replay roots:

```rust
pub async fn request_scan_session(
    &self,
    storage_root_id: StorageRootId,
    idle_timeout_seconds: u32,
) -> Result<ScanSession, VoomError>;
pub async fn start_scan_session(
    &self,
    input: RemoteScanStartInput,
) -> Result<RemoteScanStartOutcome, VoomError>;
pub async fn accept_scan_observation_batch(
    &self,
    input: RemoteScanBatchInput,
) -> Result<RemoteScanBatchOutcome, VoomError>;
pub async fn fail_scan_session(
    &self,
    input: RemoteScanFailInput,
) -> Result<RemoteScanTerminalOutcome, VoomError>;
pub async fn cancel_scan_session(
    &self,
    id: ScanSessionId,
    reason: ScanTerminalReason,
) -> Result<ScanSession, VoomError>;
pub async fn scan_session(&self, id: ScanSessionId) -> Result<ScanSession, VoomError>;
pub async fn scan_sessions(&self, query: ScanSessionListQuery)
    -> Result<ScanSessionPage, VoomError>;
pub async fn scan_reconciliation(&self, query: ScanReconciliationQuery)
    -> Result<ScanReconciliationPage, VoomError>;
pub async fn inspect_remote_scan_session(
    &self,
    input: RemoteScanInspectInput,
) -> Result<ScanSession, VoomError>;
pub async fn inspect_remote_scan_reconciliation(
    &self,
    input: RemoteScanReconciliationInput,
) -> Result<ScanReconciliationPage, VoomError>;
```

Use exact remote input shapes; GET inspection inputs deliberately have no idempotency fields:

```rust
pub struct RemoteScanStartInput {
    pub node_id: NodeId,
    pub scan_session_id: ScanSessionId,
    pub incarnation_id: NodeIncarnationId,
    pub token: SecretString,
    pub idempotency_key: String,
    pub request_hash: String,
}
pub struct RemoteScanBatchInput {
    pub node_id: NodeId,
    pub scan_session_id: ScanSessionId,
    pub incarnation_id: NodeIncarnationId,
    pub token: SecretString,
    pub idempotency_key: String,
    pub request_hash: String,
    pub sequence: u64,
    pub observations: Vec<ScanObservation>,
}
pub struct RemoteScanFailInput {
    pub node_id: NodeId,
    pub scan_session_id: ScanSessionId,
    pub incarnation_id: NodeIncarnationId,
    pub token: SecretString,
    pub idempotency_key: String,
    pub request_hash: String,
    pub reason: ScanTerminalReason,
}
pub struct RemoteScanInspectInput {
    pub node_id: NodeId,
    pub scan_session_id: ScanSessionId,
    pub incarnation_id: NodeIncarnationId,
    pub token: SecretString,
}
pub struct RemoteScanReconciliationInput {
    pub auth: RemoteScanInspectInput,
    pub after_id: Option<FileLocationId>,
    pub limit: u32,
}
```

Define strict start and terminal outcomes beside `RemoteScanBatchOutcome`; each carries the session
ID and resulting status, and completion additionally carries observation and retirement counts.

- [ ] **Step 1: Write failing request and lifecycle transaction tests**

Use `ManualClock` with real SQLite. Cover root missing/corrupt/disabled/unavailable/unassigned/
retired/ownerless states, request deadline initialization, start authority/epoch/availability,
active-root conflicts, failure/cancellation, and event atomicity. Assert no operation creates a
ticket or lease and non-success paths never query or update locations/root scan pointer.

- [ ] **Step 2: Run the use-case tests and confirm the API is absent**

Run: `cargo test -p voom-control-plane --all-features scan::sessions`

Expected: compilation fails because scan-session use cases are not exported.

- [ ] **Step 3: Wire the repository into `ControlPlane` and implement request/start**

Add `pub(crate) scan_sessions: SqliteScanSessionRepo` to `ControlPlane` construction/debug wiring.
For request/start, acquire `BEGIN IMMEDIATE` before sampling `clock.now()`. Request
stale-transitions expired active rows before checking root availability. Start performs
authentication,
current-incarnation, replay reservation, checked session/root read, deadline/epoch/availability,
high-water capture, state update, one event, replay completion, and commit in that order.

- [ ] **Step 4: Write failing replay/deadline precedence tests**

Test same HTTP-key replay and same sequence/hash under a different key. Advance `ManualClock` to
exactly `progress_deadline_at`; accepted exact replays must return the old outcome with unchanged
deadline/status/event counts. A new batch, start, failure, or cancel at the same instant must
persist `stale` once and return conflict. The next recovery after replay must persist that same
single stale transition.

- [ ] **Step 5: Implement batch and terminal orchestration**

Validate observation count, object identity, size, locator, and timestamp chronology before writes.
After current authority and ownership, resolve completed remote replay or exact batch-ledger replay
before mutable root/deadline fencing. For genuinely new mutations, persist stale plus event before
returning the conflict when any fence changed. Failure/cancel use `ScanTerminalReason`; successful
terminal races use compare-and-set row counts so only the writer-lock winner commits.

- [ ] **Step 6: Define strict replay envelopes and inventory them**

Serialize the concrete strict outcome into the `data` field of the existing
`RemoteMutationReplay`; do not add another replay envelope:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteScanBatchOutcome {
    scan_session_id: ScanSessionId,
    sequence: u64,
    accepted_observation_count: u64,
    cumulative_observation_count: u64,
}
```

Give start, batch, completion, and failure outcomes their own strict content structs and decode
stored `data` into the route-specific type. Add the defining scan event and control-plane file
paths to `scripts/payload-contract-scope.txt`; document the event family and
`remote_idempotency_keys.response_json` closure in `docs/payload-contract-inventory.md`.

- [ ] **Step 7: Prove rollback behavior and run focused gates**

Install temporary SQLite triggers that abort event insertion, replay completion, and session
updates. Assert each logical operation leaves session, observation, replay, and event counts
unchanged. Then run:

```bash
cargo test -p voom-control-plane --all-features scan::sessions
just check-payload-deny-unknown
just check-control-plane-sql-boundary
just fmt-check
```

Expected: all pass and no scan SQL exists in `voom-control-plane`.

Commit:

```bash
git add crates/voom-control-plane/src/scan crates/voom-control-plane/src/lib.rs \
  crates/voom-control-plane/src/lib_test.rs \
  crates/voom-control-plane/src/cases/execution/remote_execution/mod.rs \
  scripts/payload-contract-scope.txt docs/payload-contract-inventory.md
git commit -m "feat: orchestrate scan session lifecycle"
```

### Task 5: Implement atomic successful reconciliation and scale proof

**Files:**

- Modify: `crates/voom-store/src/repo/scan/sessions.rs`
- Modify: `crates/voom-store/src/repo/scan/sessions_test.rs`
- Modify: `crates/voom-store/src/repo/media/commit_safety_gate.rs`
- Modify: `crates/voom-store/src/repo/media/commit_safety_gate_test.rs`
- Modify: `crates/voom-control-plane/src/scan/sessions.rs`
- Modify: `crates/voom-control-plane/src/scan/sessions_test.rs`
- Create: `crates/voom-control-plane/tests/scan_session_scale.rs`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**

- Consumes: Task 4 transaction/replay/fence ordering, session observations/high-water mark, retained
  rooted locations, and the commit-safety scope model.
- Produces: `complete_scan_session(RemoteScanCompleteInput) ->
  Result<RemoteScanCompleteOutcome, VoomError>` and a shared multi-location in-flight commit
  preflight covering `pending`, `authorized`, and `recovery_required`.

```rust
pub struct RemoteScanCompleteInput {
    pub node_id: NodeId,
    pub scan_session_id: ScanSessionId,
    pub incarnation_id: NodeIncarnationId,
    pub token: SecretString,
    pub idempotency_key: String,
    pub request_hash: String,
    pub last_sequence: Option<u64>,
    pub observation_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteScanCompleteOutcome {
    pub scan_session_id: ScanSessionId,
    pub status: ScanSessionStatus,
    pub observation_count: u64,
    pub retired_location_count: u64,
}
```

- [ ] **Step 1: Write failing completion and absence tests**

Cover empty success, observed/unobserved locations, another root, an above-high-water concurrent
location, wrong final sequence/count, failed/cancelled/stale sessions, root epoch/owner drift, and
unavailable root. The empty case must assert every pre-start live rooted location is retired with
one timestamp, epoch increment, provenance session, exact count, root pointer, one success event,
and no per-location events.

```rust
let outcome = cp.complete_scan_session(empty_completion(session)).await?;
assert_eq!(outcome.retired_location_count, pre_start_ids.len() as u64);
assert!(pre_start_ids.iter().all(|id| retired_by(*id) == Some(session)));
assert_eq!(event_count("scan_session.succeeded").await, 1);
assert_eq!(event_count("file_location.retired").await, 0);
```

- [ ] **Step 2: Run completion tests and verify reconciliation is absent**

Run: `cargo test -p voom-control-plane --all-features scan::sessions::tests::complete`

Expected: failure because `complete_scan_session` and reconciliation writes do not exist.

- [ ] **Step 3: Generalize the commit-lock helper without changing existing callers**

Expose a crate-public-in-store helper that accepts `&[FileLocationId]` and performs one set query
against all candidate locations. Its scan-specific state set is exactly
`('pending','authorized','recovery_required')`; keep the existing singular
`consult_pending_commit_lock_in_tx` behavior at `('pending','authorized')` for current callers.
Return the lowest `CommitId`/`FileLocationId` conflict deterministically. Add regressions for all
three states and for existing use-lease/identity behavior.

- [ ] **Step 4: Implement one-transaction completion**

After authority/replay/fences/watermark checks, select candidates ordered by ID and preflight the
entire set before mutation. Update only rows satisfying the same live/root/high-water/anti-join
predicate:

```sql
UPDATE file_locations
SET retired_at = ?,
    retired_by_scan_session_id = ?,
    epoch = epoch + 1
WHERE storage_root_id = ?
  AND address_state = 'rooted'
  AND retired_at IS NULL
  AND id <= ?
  AND NOT EXISTS (
      SELECT 1 FROM scan_observations o
      WHERE o.scan_session_id = ?
        AND o.provider_relative_locator = file_locations.provider_relative_locator
  )
```

Verify affected rows equal the preflight candidate count, then mark the session succeeded, set the
root pointer, append one summary event, complete replay, and commit. Any mismatch or write error is
`VoomError::Database` and rolls back everything.

- [ ] **Step 5: Force every post-preflight failure and verify full rollback**

Use scoped temporary triggers to fail location update, session update, event append, and replay
completion in turn. Assert all candidate locations remain live, the session remains running, the
root pointer remains unchanged, and no event/replay result commits. Add pending/authorized/
recovery-required lock cases that leave the session running and are retryable.

- [ ] **Step 6: Add the fixed 100,000-location release scale gate**

The ignored integration test creates a fresh temporary SQLite database per repetition, bulk-loads
one active root and 100,000 distinct live rooted locations outside the timer, starts an empty
session, and times only `complete_scan_session`. Run three repetitions; each must finish in at most
25 seconds and verify exactly 100,000 attributed retirements plus consistent root/session counts.

```rust
const LOCATION_COUNT: u64 = 100_000;
const REPETITIONS: usize = 3;
const MAX_COMPLETION: std::time::Duration = std::time::Duration::from_secs(25);
```

Add one CI matrix step after `just ci` on both existing OS runners:

```yaml
- name: Durable scan completion scale gate
  run: >-
    cargo test --release -p voom-control-plane --test scan_session_scale
    -- --ignored --exact empty_scan_reconciles_100k_within_api_budget --nocapture
```

- [ ] **Step 7: Run completion, mutation, and scale proofs**

Run:

```bash
cargo test -p voom-store --all-features commit_safety_gate
cargo test -p voom-control-plane --all-features scan::sessions
cargo test --release -p voom-control-plane --test scan_session_scale \
  -- --ignored --exact empty_scan_reconciles_100k_within_api_budget --nocapture
```

Expected: all functional tests pass; all three measured completions are at most 25 seconds. If any
repetition exceeds the bound, stop and return to ADR/spec design—do not chunk reconciliation.

Commit:

```bash
git add crates/voom-store/src/repo/scan \
  crates/voom-store/src/repo/media/commit_safety_gate.rs \
  crates/voom-store/src/repo/media/commit_safety_gate_test.rs \
  crates/voom-control-plane/src/scan crates/voom-control-plane/tests/scan_session_scale.rs \
  .github/workflows/ci.yml
git commit -m "feat: reconcile completed scan sessions"
```

### Task 6: Integrate incarnation termination and timeout recovery

**Files:**

- Modify: `crates/voom-store/src/repo/scan/sessions.rs`
- Modify: `crates/voom-store/src/repo/scan/sessions_test.rs`
- Modify: `crates/voom-control-plane/src/cases/execution/remote_execution/activation.rs`
- Modify: `crates/voom-control-plane/src/cases/execution/remote_execution/activation_test.rs`
- Modify: `crates/voom-control-plane/src/cases/execution/remote_execution/recover.rs`
- Modify: `crates/voom-control-plane/src/cases/execution/remote_execution/mod.rs`
- Modify: `crates/voom-control-plane/src/cases/execution/remote_execution/mod_test.rs`

**Interfaces:**

- Consumes: Task 4 stale transition/event helper and existing
  `ControlPlane::end_incarnation_in_tx`/`remote_recover` transaction ordering.
- Produces: incarnation-atomic stale scan transitions and
  `RemoteRecoverReport::stale_scan_sessions: Vec<ScanSessionId>`.

- [ ] **Step 1: Write failing incarnation and timeout recovery tests**

Start sessions under an active incarnation, then exercise supersession, graceful deactivation,
logical-node retirement, startup failure, restart exhaustion, and heartbeat expiry. Each running
session bound to the ended incarnation must become stale in the same transaction/event order;
requested sessions remain governed only by deadline recovery. Add a forced stale-event failure and
assert the incarnation ending and worker retirement also roll back. Simulate disconnect by
accepting a partial batch and making no terminal call; deadline recovery must stale that session
without retiring any location or updating the root pointer.

- [ ] **Step 2: Run recovery tests and confirm sessions remain active**

Run:

```bash
cargo test -p voom-control-plane --all-features remote_recover_marks_scan_sessions_stale
cargo test -p voom-control-plane --all-features ending_incarnation_stales_scan_sessions_atomically
```

Expected: assertions fail because lifecycle code does not yet consult scan sessions.

- [ ] **Step 3: Implement set-based stale repository transitions**

Add exact repository methods:

```rust
pub async fn stale_running_for_incarnation_in_tx(
    &self,
    tx: &mut Transaction<'_, Sqlite>,
    incarnation_id: NodeIncarnationId,
    reason: ScanTerminalReason,
    now: OffsetDateTime,
) -> Result<Vec<ScanSession>, VoomError>;

pub async fn stale_expired_in_tx(
    &self,
    tx: &mut Transaction<'_, Sqlite>,
    now: OffsetDateTime,
) -> Result<Vec<ScanSession>, VoomError>;
```

Select IDs ascending, decode before classification, update only active rows, and return checked
post-transition rows for event composition. Expiry predicate is
`status IN ('requested','running') AND progress_deadline_at <= now`.

- [ ] **Step 4: Integrate stale transitions without a second transaction**

Within `end_incarnation_in_tx`, stale running scan sessions before ending the incarnation and
append one `ScanSessionStale` event per returned session in ascending ID order. Keep the existing
worker return type so current callers do not need an unrelated public-shape change.

In `remote_recover(now)`, run stale-node recovery, then one immediate transaction for expired scan
sessions and their events, then existing lease expiry. Return sorted stale session IDs in the
expanded report. Re-running at the same `now` returns no session twice and emits nothing new.

- [ ] **Step 5: Run focused recovery tests and commit**

Run:

```bash
cargo test -p voom-control-plane --all-features cases::execution::remote_execution
cargo test -p voom-store --all-features --lib scan::sessions
just fmt-check
```

Expected: old remote recovery behavior remains green and scan stale transitions are atomic and
idempotent.

Commit:

```bash
git add crates/voom-store/src/repo/scan \
  crates/voom-control-plane/src/cases/execution/remote_execution
git commit -m "feat: recover stale scan sessions"
```

### Task 7: Expose authenticated scan-session API routes

**Files:**

- Create: `crates/voom-api/src/scan.rs`
- Create: `crates/voom-api/src/scan_test.rs`
- Modify: `crates/voom-api/src/lib.rs`
- Modify: `crates/voom-api/src/lib_test.rs`
- Modify: `crates/voom-api/src/server.rs`
- Modify: `crates/voom-api/src/server_test.rs`

**Interfaces:**

- Consumes: Task 4–6 exported remote inputs/outcomes and existing execution-route helpers for
  bearer parsing, idempotency keys, stable request hashes, strict body parsing, standard envelopes,
  1 MiB body limit, and 30-second request deadline.
- Produces: six `/v1/scan/node/{node_id}/session/{session_id}` routes from the approved table.

- [ ] **Step 1: Write failing route-shape and authentication-order tests**

For start, batch, complete, fail, GET session, and GET reconciliation, test malformed path/query,
missing/invalid bearer, missing mutation idempotency key, unknown JSON fields, body over 1 MiB,
non-owner node, stale incarnation, missing session, and exact status/code/envelope mapping. Use an
unknown session ID in auth failures and assert the response remains the same generic 401.

```rust
let response = request_without_bearer("/v1/scan/node/9/session/999/start").await;
assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
assert_eq!(json(response)["error"]["code"], "UNAUTHORIZED");
assert!(!body_text(response).contains("999"));
```

- [ ] **Step 2: Run API tests and verify routes return 404**

Run: `cargo test -p voom-api --all-features scan`

Expected: route tests fail because `/v1/scan` is not registered.

- [ ] **Step 3: Define strict request/query DTOs and route registration**

Use these exact bodies, all with `#[serde(deny_unknown_fields)]`:

```rust
struct StartRequest { incarnation_id: NodeIncarnationId }
struct BatchRequest {
    incarnation_id: NodeIncarnationId,
    observations: Vec<ScanObservation>,
}
struct CompleteRequest {
    incarnation_id: NodeIncarnationId,
    last_sequence: Option<u64>,
    observation_count: u64,
}
struct FailRequest {
    incarnation_id: NodeIncarnationId,
    reason: String,
}
struct InspectQuery {
    incarnation_id: NodeIncarnationId,
}
struct ReconciliationQuery {
    incarnation_id: NodeIncarnationId,
    after_id: Option<u64>,
    limit: Option<u32>,
}
```

The batch sequence comes only from the path. Mutation route-instance hashes include the concrete
node/session/sequence path and canonical request body. GET routes require bearer/current
incarnation but not `Idempotency-Key`.

- [ ] **Step 4: Implement handlers using shared boundary plumbing**

Factor only the credential/body/query helpers shared by three or more routes. Parse and bound all
input before opening the control-plane transaction, then pass typed IDs and `SecretString` without
logging them. Register `scan::routes()` in `base_router`. Reuse the existing API error classifier:
400 bad args, 401 unauthorized, 404 not found, 409 conflict, 413 oversized, existing blocked code,
and server error for database corruption.

- [ ] **Step 5: Test successful mutation/replay/inspection contracts**

Drive request locally through `ControlPlane`, then API start/batch/complete/fail. Assert exact
replay has identical response JSON and unchanged observation/event counts. Test authenticated GET
ownership and reconciliation pagination at default 50/max 100 with exclusive cursors and no
provider object identities or locators in responses.

- [ ] **Step 6: Run API/server gates and commit**

Run:

```bash
cargo test -p voom-api --all-features scan
cargo test -p voom-api --all-features server
just fmt-check
just lint
```

Expected: all route, timeout/body-limit, authentication-order, and secret-safety assertions pass.

Commit:

```bash
git add crates/voom-api
git commit -m "feat: expose scan session API"
```

### Task 8: Add local operator CLI session commands

**Files:**

- Modify: `crates/voom-cli/src/cli.rs`
- Modify: `crates/voom-cli/src/cli_test.rs`
- Create: `crates/voom-cli/src/commands/scan_session.rs`
- Create: `crates/voom-cli/src/commands/scan_session_test.rs`
- Modify: `crates/voom-cli/src/commands/mod.rs`
- Modify: `crates/voom-cli/src/main.rs`
- Modify: `crates/voom-cli/src/main_test.rs`
- Create: `crates/voom-cli/tests/scan_session_envelope.rs`
- Create: `crates/voom-cli/tests/snapshots/scan_session_envelope__request.snap`
- Create: `crates/voom-cli/tests/snapshots/scan_session_envelope__progress.snap`
- Create: `crates/voom-cli/tests/snapshots/scan_session_envelope__terminal_states.snap`
- Create: `crates/voom-cli/tests/snapshots/scan_session_envelope__reconciliation.snap`

**Interfaces:**

- Consumes: Task 4 local request/show/list/reconciliation/cancel use cases, `open_control_plane`,
  `Local`, existing envelope writers, and keyset page shapes.
- Produces: top-level `Command::ScanSession(ScanSessionCommand)` with request/show/list/
  reconciliation/cancel subcommands.

- [ ] **Step 1: Write failing clap contract tests**

Require these exact forms and bounds:

```text
voom scan-session request --root 7 --idle-timeout-seconds 300
voom scan-session show --id 9
voom scan-session list --root 7 --status running --after 4 --limit 50
voom scan-session reconciliation --id 9 --after 100 --limit 50
voom scan-session cancel --id 9 --reason "operator stopped scan"
```

Assert timeout 0/86401, limit 0/101, unknown status, empty/1025-byte/NUL reason, and missing
subcommand produce one `BAD_ARGS` envelope with exit 1. Confirm existing `voom scan --root` parsing
is unchanged.

- [ ] **Step 2: Run CLI parser tests and verify the subcommand is missing**

Run: `cargo test -p voom-cli --all-features --lib cli`

Expected: failures report unknown `scan-session`.

- [ ] **Step 3: Implement the clap enum and typed dispatch**

Define:

```rust
#[derive(Subcommand, Debug, Clone)]
pub enum ScanSessionCommand {
    Request { root: u64, idle_timeout_seconds: u32 },
    Show { id: u64 },
    List {
        root: Option<u64>,
        status: Option<ScanSessionStatusArg>,
        after: Option<u64>,
        limit: u32,
    },
    Reconciliation { id: u64, after: Option<u64>, limit: u32 },
    Cancel { id: u64, reason: String },
}
```

Attach clap range parsers, a default timeout of 300, and default limit 50. Parse status through an
exhaustive `ValueEnum`; validate cancellation reasons with `ScanTerminalReason::new` before the
control-plane call.

- [ ] **Step 4: Implement envelope DTOs and command functions**

Session DTOs include typed IDs rendered as numbers, root epoch, owner/incarnation, status, batch/
observation counters, deadline, requested/started/terminal timestamps, reason, high-water ID,
retired count, and `reconciliation_applied = status == succeeded`. Page DTOs include ordered items
and the established exclusive next cursor. Reconciliation items expose location ID, retired time,
prior epoch, and retired epoch—never provider object identity or locator.

- [ ] **Step 5: Write end-to-end envelope snapshots**

Create sessions in every status through test control-plane/API fixtures, invoke each CLI command as
a subprocess, assert exactly one JSON value on stdout and logs only on stderr, then snapshot
request, running progress, all terminal states, and paginated evidence. Add secret strings and
distinctive provider facts to fixtures and assert neither appears in output.

- [ ] **Step 6: Run CLI tests and commit**

Run:

```bash
cargo test -p voom-cli --all-features scan_session
cargo test -p voom-cli --all-features --test scan_session_envelope
cargo test -p voom-cli --all-features --test scan_envelope
just fmt-check
```

Expected: new snapshots pass, stdout is one envelope, and the legacy scan snapshots remain
unchanged.

Commit:

```bash
git add crates/voom-cli
git commit -m "feat: add scan session CLI"
```

### Task 9: Prove the charter end to end and run repository guardrails

**Files:**

- Create: `crates/voom-control-plane/tests/durable_scan_session_flow.rs`

**Interfaces:**

- Consumes: the complete durable session implementation and all seven charter criteria.
- Produces: one black-box cross-crate regression suite and a fully verified branch; no new public
  behavior beyond ADR 0067/spec.

- [ ] **Step 1: Write the cross-crate charter test before adjusting implementation**

In one real-SQLite scenario, request/start, accept two ordered batches, replay each through both
idempotency paths, reject sequence 3 before missing sequence 2 on a second session, complete the
first session, and inspect it through control-plane/API/CLI surfaces. Assert exact observation,
event, retirement, status, progress, terminal, and provenance counts. Separate cases cover empty
success and every non-success terminal path.

- [ ] **Step 2: Run the charter test and fix only observed contract gaps**

Run: `cargo test -p voom-control-plane --all-features --test durable_scan_session_flow`

Expected: all seven frozen completion criteria pass in the black-box suite. If it fails, stop this
task, return to the owning earlier task, add its focused regression first, and commit that fix at
the owning task boundary before rerunning this proof.

- [ ] **Step 3: Demonstrate three tests bite**

Perform each mutation separately, run its named test, observe failure, and restore the file before
the next mutation:

```text
1. Remove `status = 'running'` from completion candidate eligibility:
   failed_session_never_reconciles must FAIL.
2. Remove `next_sequence = next_sequence + 1` from batch acceptance:
   batch_gap_and_replay_are_deterministic must FAIL.
3. Remove `id <= location_high_watermark_id` from reconciliation:
   concurrent_location_above_watermark_remains_live must FAIL.
```

After restoration, rerun all three tests and require PASS. Do not commit any mutation.

- [ ] **Step 4: Run focused contract guardrails**

Run every command bare so its exit code is authoritative:

```bash
just check-test-layout
just check-paused-time-db
just check-control-plane-sql-boundary
just check-payload-deny-unknown
just check-adr-index
git diff --check origin/main...HEAD
```

Expected: every guard reports success with no skipped in-scope files.

- [ ] **Step 5: Run the full suite and release scale gate**

Run:

```bash
just ci
cargo test --release -p voom-control-plane --test scan_session_scale \
  -- --ignored --exact empty_scan_reconciles_100k_within_api_budget --nocapture
```

Expected: `just ci` passes warning-free; three scale repetitions each complete within 25 seconds
and retire exactly 100,000 locations. If the scale bound fails, stop and return to design.

- [ ] **Step 6: Re-read the final diff for scope and commit the proof**

Confirm the diff contains migration 0036 only, no traversal/hash/probe/scheduler behavior, no new
dependency, no second routing mechanism, unchanged legacy `voom scan`, strict durable roots, and
exactly the ADR 0067 README row already committed by the design phase.

Commit:

```bash
git add crates/voom-control-plane/tests/durable_scan_session_flow.rs
git commit -m "test: prove durable scan session contract"
```
