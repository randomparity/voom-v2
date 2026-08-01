# Control-plane SQL Boundary Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development
> (recommended) or executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove every production SQLx query construction site from
`voom-control-plane`, make `voom-store` the sole owner of SQL and persisted-row
decoding, and enforce that boundary with an immediate zero-tolerance CI guard.

**Architecture:** Extend the repository that owns each table or durable vocabulary
with typed read/write operations and `_in_tx` variants where the control plane owns
the surrounding transaction. Keep use-case decisions, event ordering, retries,
cross-repository transaction sequencing, and payload interpretation in
`voom-control-plane`. Repository APIs return domain records and explicit outcomes,
never SQL rows, tuples, or unchecked persisted strings.

**Tech Stack:** Rust stable, sqlx/SQLite, tokio, serde/serde_json, time, ast-grep,
Bash, just, prek, cargo test, cargo clippy, Desloppify.

## Global Constraints

- The approved design is
  `docs/superpowers/specs/2026-08-01-control-plane-sql-boundary-design.md`.
- Existing ADRs are immutable. Before every commit, run
  `git diff --cached -- docs/adr/` and require empty output.
- Work only on `desloppify/code-health`; never implement on `main` or `master`.
- Use sibling `*_test.rs` modules for Rust unit tests. Do not add inline test modules.
- Start each behavior change with a failing test and observe the intended failure.
- Run focused tests and warnings-denied Clippy before each commit. Run `prek run`
  against the staged change; do not bypass hooks.
- Keep functions at or below 100 lines, cyclomatic complexity at or below 8, and
  lines at or below 100 characters.
- Preserve public payloads, durable tokens, schemas, event ordering, and public error
  codes unless a task below explicitly specifies a tested contract correction.
- Do not add a generic query repository, database facade, compatibility shim,
  allowlist, or checked-in baseline.
- Commit each task separately with the exact conventional commit subject listed for
  that task.
- After Tasks 3, 5, and 7, rerun the production query inventory so regressions are
  caught before the guard is introduced:

  ```bash
  rg -n 'sqlx::(query|query_as|query_scalar|raw_sql|QueryBuilder)' \
    crates/voom-control-plane/src \
    -g '!**/*_test.rs' -g '!**/tests/**'
  ```

  The starting inventory is 47 matches: 45 production calls and two `#[cfg(test)]`
  fixture helpers in `cases/mod.rs`.

---

## Task 1: Type synthesis dispatch-attempt status

**Files:**

- Modify: `crates/voom-store/src/repo/media/audio_synthesis_operations.rs`
- Modify: `crates/voom-store/src/repo/media/audio_synthesis_operations_test.rs`
- Modify: `crates/voom-control-plane/src/audio/mod.rs`
- Modify: `crates/voom-control-plane/src/audio/mod_test.rs`

- [ ] **Step 1: Add failing repository tests for the durable status vocabulary**

Add round-trip coverage for all four states and a corruption test that writes an
unknown value directly through the test pool and expects a database error containing
`audio_synthesis_dispatch_attempts.status` and the invalid value.

The four states are `active`, `terminal`, `quarantined`, and `quiesced`. Reuse the
same behavioral matrix as `AudioExtractDispatchAttemptStatus`; do not share an enum
because the extraction and synthesis lifecycle tables can evolve independently.

Run:

```bash
cargo test -p voom-store audio_synthesis_dispatch_attempt_status -- --nocapture
```

Expected: FAIL because `AudioSynthesisDispatchAttempt.status` is still `String` and
unknown values are not rejected at the repository boundary.

- [ ] **Step 2: Introduce and decode the typed status**

Add this store-owned vocabulary beside `AudioSynthesisDispatchAttempt`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSynthesisDispatchAttemptStatus {
    Active,
    Terminal,
    Quarantined,
    Quiesced,
}

impl AudioSynthesisDispatchAttemptStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Terminal => "terminal",
            Self::Quarantined => "quarantined",
            Self::Quiesced => "quiesced",
        }
    }

    fn parse(value: &str) -> Result<Self, VoomError> {
        match value {
            "active" => Ok(Self::Active),
            "terminal" => Ok(Self::Terminal),
            "quarantined" => Ok(Self::Quarantined),
            "quiesced" => Ok(Self::Quiesced),
            other => Err(VoomError::database(format!(
                "audio_synthesis_dispatch_attempts.status {other:?} not in vocab"
            ))),
        }
    }
}
```

Change `AudioSynthesisDispatchAttempt.status` to this enum and parse it in
`load_dispatch_attempt`. Update repository writes to bind `status.as_str()`.

- [ ] **Step 3: Consume the enum exhaustively in synthesis reconciliation**

Replace both string comparisons in `reconcile_synthesis_dispatch` with matches on
`AudioSynthesisDispatchAttemptStatus`. Preserve current behavior:

- `Active` follows the active reconciliation path.
- `Terminal`, `Quarantined`, and `Quiesced` follow the non-active path.

Update error formatting to use `status.as_str()`. Add a control-plane behavior test
showing a non-active attempt cannot be treated as active.

- [ ] **Step 4: Verify and commit**

Run:

```bash
cargo test -p voom-store audio_synthesis
cargo test -p voom-control-plane audio
cargo clippy -p voom-store -p voom-control-plane --all-targets --all-features -- -D warnings
git diff --check
git diff --cached -- docs/adr/
prek run
```

Stage only the four files above, then commit:

```bash
git commit -m "refactor(audio): type synthesis dispatch status"
```

---

## Task 2: Make claim-release semantics explicit

**Files:**

- Modify: `crates/voom-store/src/repo/media/audio_extract_operations.rs`
- Modify: `crates/voom-store/src/repo/media/audio_extract_operations_test.rs`
- Modify: `crates/voom-store/src/repo/media/audio_synthesis_operations.rs`
- Modify: `crates/voom-store/src/repo/media/audio_synthesis_operations_test.rs`
- Modify: `crates/voom-control-plane/src/audio/mod.rs`
- Modify: `crates/voom-control-plane/src/audio/mod_test.rs`

- [ ] **Step 1: Lock the two existing lifecycle contracts with failing tests**

For extraction, add tests proving release is idempotent when the generation/token is
stale, the claim was already released, or the operation committed. For synthesis,
add tests proving the same zero-row conditions return `VoomError::Conflict`. Include
successful release and replaced-token cases for each repository.

Run:

```bash
cargo test -p voom-store release_claim_contract -- --nocapture
```

Expected: FAIL because both methods are currently named `release_claim`, so the tests
cannot call APIs that state their different contracts.

- [ ] **Step 2: Rename the methods without unifying their semantics**

Expose these signatures:

```rust
impl SqliteAudioExtractOperationRepo {
    pub async fn release_claim_if_current(
        &self,
        claim: &NewAudioExtractClaim,
    ) -> Result<(), VoomError>;
}

impl SqliteAudioSynthesisOperationRepo {
    pub async fn release_claim_exact(
        &self,
        claim: &NewAudioSynthesisClaim,
    ) -> Result<(), VoomError>;
}
```

`release_claim_if_current` discards a zero-row result deliberately and documents why
cleanup is idempotent. `release_claim_exact` continues to require exactly one planned
row with the current generation/token and returns a contextual conflict otherwise.

- [ ] **Step 3: Update all control-plane call sites**

Use `release_claim_exact` in synthesis dispatch cleanup. Use
`release_claim_if_current` in extraction failure cleanup, resumed-attempt cleanup,
and reconciliation cleanup. Do not add a shared trait or boolean strictness flag.

- [ ] **Step 4: Verify and commit**

Run:

```bash
cargo test -p voom-store audio_extract
cargo test -p voom-store audio_synthesis
cargo test -p voom-control-plane audio
cargo clippy -p voom-store -p voom-control-plane --all-targets --all-features -- -D warnings
git diff --check
git diff --cached -- docs/adr/
prek run
```

Commit:

```bash
git commit -m "refactor(audio): clarify claim release contracts"
```

---

## Task 3: Move execution, worker, lease, and scan SQL into repositories

**Files:**

- Modify: `crates/voom-store/src/repo/execution/tickets.rs`
- Modify: `crates/voom-store/src/repo/execution/tickets_test.rs`
- Modify: `crates/voom-store/src/repo/execution/leases.rs`
- Modify: `crates/voom-store/src/repo/execution/leases_test.rs`
- Modify: `crates/voom-store/src/repo/execution/workers.rs`
- Modify: `crates/voom-store/src/repo/execution/workers_test.rs`
- Modify: `crates/voom-control-plane/src/cases/execution/tickets.rs`
- Modify: `crates/voom-control-plane/src/cases/execution/tickets_test.rs`
- Modify: `crates/voom-control-plane/src/cases/execution/remote_execution/acquire.rs`
- Modify: `crates/voom-control-plane/src/cases/execution/remote_execution/mod_test.rs`
- Modify: `crates/voom-control-plane/src/scan/persist.rs`
- Modify: `crates/voom-control-plane/src/scan/persist_test.rs`

- [ ] **Step 1: Add failing behavior tests for each repository operation**

Cover these cases before moving SQL:

- held-lease detection returns true only for a held lease on the requested ticket;
- ready-ticket pre-lease failure checks the previous attempt and atomically returns
  the transitioned typed `Ticket` for terminal and retry outcomes;
- worker candidate operations are the normalized union of capabilities and allowed
  grants, with denied or unrelated operations excluded;
- active lease count for a node counts only held leases belonging to that node;
- worker liveness accepts `Registered` and `Active`, rejects `Stale` and `Retired`,
  and reports a missing worker distinctly.

Run focused tests and observe at least one compile failure for every new API:

```bash
cargo test -p voom-store pre_lease_failure
cargo test -p voom-store candidate_operations
cargo test -p voom-store active_count_for_node
cargo test -p voom-store require_live
```

- [ ] **Step 2: Add typed execution repository APIs**

Add this transition input to `tickets.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreLeaseFailureTransition {
    Terminal,
    RetryAt(OffsetDateTime),
}
```

Add the following methods, using the caller's transaction and returning decoded
domain values:

```rust
impl SqliteTicketRepo {
    pub async fn transition_ready_before_lease_failure_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        ticket_id: TicketId,
        previous_attempt: u32,
        next_attempt: u32,
        transition: PreLeaseFailureTransition,
        now: OffsetDateTime,
    ) -> Result<Ticket, VoomError>;
}

impl SqliteLeaseRepo {
    pub async fn has_held_for_ticket_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        ticket_id: TicketId,
    ) -> Result<bool, VoomError>;

    pub async fn active_count_for_node_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        node_id: NodeId,
    ) -> Result<u32, VoomError>;
}
```

The ticket mutation must include state and previous-attempt predicates and fail with
a contextual conflict on zero rows. Keep jitter selection and event construction in
the control plane.

- [ ] **Step 3: Add typed worker repository APIs**

Add:

```rust
impl SqliteWorkerRepo {
    pub async fn candidate_operations_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        worker_id: WorkerId,
    ) -> Result<Vec<TicketOperation>, VoomError>;

    pub async fn require_live_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        worker_id: WorkerId,
    ) -> Result<Worker, VoomError>;
}
```

Decode operation and worker-status vocabulary in the repository. Sort and deduplicate
candidate operations before returning them. `require_live_in_tx` returns `NotFound`
for a missing worker and `Conflict` with the current typed status for a non-live one.

- [ ] **Step 4: Replace the six control-plane queries**

Update `require_no_held_lease`, `transition_pre_lease_failure_ticket`,
`worker_candidate_operations_in_tx`, `active_lease_count_for_node_in_tx`, and
`ensure_worker_live_in_tx` to call the new APIs. Delete local SQLite ID conversion and
timestamp helpers that become unused. Preserve transaction and event ordering.

- [ ] **Step 5: Verify the seam and inventory reduction**

Run:

```bash
cargo test -p voom-store execution
cargo test -p voom-control-plane cases::execution
cargo test -p voom-control-plane scan
cargo clippy -p voom-store -p voom-control-plane --all-targets --all-features -- -D warnings
rg -n 'sqlx::(query|query_as|query_scalar|raw_sql|QueryBuilder)' \
  crates/voom-control-plane/src/{cases/execution,scan/persist.rs} \
  -g '!**/*_test.rs'
git diff --check
git diff --cached -- docs/adr/
prek run
```

Expected inventory output: no matches in the listed production paths.

Commit:

```bash
git commit -m "refactor(store): own execution persistence queries"
```

---

## Task 4: Move workflow execution and summary SQL into repositories

**Files:**

- Modify: `crates/voom-store/src/repo/execution/tickets.rs`
- Modify: `crates/voom-store/src/repo/execution/tickets_test.rs`
- Modify: `crates/voom-store/src/repo/execution/leases.rs`
- Modify: `crates/voom-store/src/repo/execution/leases_test.rs`
- Modify: `crates/voom-store/src/repo/audit/events.rs`
- Modify: `crates/voom-store/src/repo/audit/events_test.rs`
- Modify: `crates/voom-control-plane/src/workflow/execution/executor/errors.rs`
- Modify: `crates/voom-control-plane/src/workflow/execution/executor/expansion.rs`
- Modify: `crates/voom-control-plane/src/workflow/execution/executor/mod_test.rs`
- Modify: `crates/voom-control-plane/src/workflow/plan/expansion.rs`
- Modify: `crates/voom-control-plane/src/workflow/plan/expansion_test.rs`
- Modify: `crates/voom-control-plane/src/workflow/summary.rs`
- Modify: `crates/voom-control-plane/src/workflow/summary_test.rs`
- Modify: callers of `WorkflowRunSummary::refresh_counts` found by
  `rg -n 'refresh_counts' crates/voom-control-plane/src`

- [ ] **Step 1: Add failing ticket-projection tests**

Test the exact durable outcomes used by workflow execution:

- succeeded workflow node IDs are ordered and deduplicated;
- a node-ticket existence query respects job, workflow, branch, and node identity;
- ready workflow tickets return typed `Ticket` values in deterministic order;
- finished/idle facts distinguish pending, ready, leased, failed, and succeeded rows;
- first failed workflow ticket is deterministic;
- retry eligibility returns a typed timestamp;
- phase identity lookup and dependency existence use job/workflow/branch/node/file
  scope without exposing JSON SQL to the caller;
- job ticket listing returns all typed tickets in deterministic ID order.

Define store-owned identity input rather than accepting a control-plane payload type:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowTicketIdentity<'a> {
    pub job_id: JobId,
    pub workflow_id: &'a str,
    pub branch_id: &'a str,
    pub node_id: &'a str,
    pub source_file_version_id: Option<FileVersionId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowTicketFacts {
    pub unfinished: u32,
    pub ready: u32,
    pub leased: u32,
    pub failed: u32,
}
```

Run focused tests and observe failures for missing methods.

- [ ] **Step 2: Implement focused `SqliteTicketRepo` workflow methods**

Add methods named:

```rust
succeeded_workflow_node_ids(...)
workflow_ticket_exists_in_tx(...)
ready_workflow_tickets(...)
workflow_ticket_facts(...)
first_failed_workflow_ticket(...)
retry_eligible_at(...)
find_workflow_ticket_id_in_tx(...)
dependency_exists_in_tx(...)
list_for_job(...)
```

Each method returns typed IDs, `Ticket`, `WorkflowTicketFacts`, or
`Option<OffsetDateTime>`. JSON path matching remains SQL-owned inside the repository;
decoding `WorkflowTicketPayload` stays in the control plane.

- [ ] **Step 3: Move event and lease summary projections**

In `events.rs`, add a method returning the latest typed event payload for a ticket
failure, not a raw JSON string:

```rust
pub async fn latest_ticket_failure(
    &self,
    ticket_id: TicketId,
) -> Result<Option<EventEnvelope>, VoomError>;
```

In `leases.rs`, add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseInterval {
    pub worker_id: WorkerId,
    pub acquired_at: OffsetDateTime,
    pub released_at: Option<OffsetDateTime>,
}

pub async fn timeline_for_job(
    &self,
    job_id: JobId,
) -> Result<Vec<LeaseInterval>, VoomError>;
```

Test held and released intervals, worker separation, and deterministic ordering.

- [ ] **Step 4: Replace workflow execution, plan, and summary queries**

Replace all SQL in:

- `executor/errors.rs` with ticket/event repository reads;
- `executor/expansion.rs` with ticket facts and typed ticket reads;
- `workflow/plan/expansion.rs` with identity/dependency repository calls;
- `workflow/summary.rs` with `list_for_job` and `timeline_for_job`.

Change `WorkflowRunSummary::refresh_counts` to:

```rust
pub(super) async fn refresh_counts(
    &mut self,
    tickets: &SqliteTicketRepo,
    leases: &SqliteLeaseRepo,
    job_id: JobId,
    elapsed: Duration,
) -> Result<(), VoomError>
```

Remove the three `if let Ok(...)` branches that silently discarded database errors.
Propagate errors at every caller, including changing executor `refresh`,
`finish_success`, and their callers to return `Result`. Add a behavior test with a
closed/broken pool showing refresh now fails loudly. Preserve the existing handling
of non-workflow tickets while returning errors for repository read/row-decode
failures.

- [ ] **Step 5: Verify and commit**

Run:

```bash
cargo test -p voom-store tickets
cargo test -p voom-store leases
cargo test -p voom-store events
cargo test -p voom-control-plane workflow
cargo clippy -p voom-store -p voom-control-plane --all-targets --all-features -- -D warnings
rg -n 'sqlx::(query|query_as|query_scalar|raw_sql|QueryBuilder)' \
  crates/voom-control-plane/src/workflow/{execution,plan,summary.rs} \
  -g '!**/*_test.rs'
git diff --check
git diff --cached -- docs/adr/
prek run
```

Expected inventory output: no matches.

Commit:

```bash
git commit -m "refactor(store): own workflow execution queries"
```

---

## Task 5: Move compliance projections into worker and ticket repositories

**Files:**

- Modify: `crates/voom-store/src/repo/execution/workers.rs`
- Modify: `crates/voom-store/src/repo/execution/workers_test.rs`
- Modify: `crates/voom-store/src/repo/execution/tickets.rs`
- Modify: `crates/voom-store/src/repo/execution/tickets_test.rs`
- Modify: `crates/voom-control-plane/src/cases/policy/compliance.rs`
- Modify: `crates/voom-control-plane/src/cases/policy/compliance_test.rs`

- [ ] **Step 1: Add failing projection tests**

Test that runtime capability rows include only live workers, preserve worker epoch,
decode `TicketOperation`, and retain capability `extra`. Test succeeded ticket result
selection by job and operation, including malformed JSON failure, non-succeeded row
exclusion, and deterministic ticket order.

Use these store-owned DTOs:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeWorkerCapability {
    pub worker_id: WorkerId,
    pub worker_epoch: u64,
    pub operation: TicketOperation,
    pub extra: JsonValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SucceededTicketResult {
    pub ticket_id: TicketId,
    pub result: JsonValue,
}
```

- [ ] **Step 2: Implement the repository methods**

Add:

```rust
impl SqliteWorkerRepo {
    pub async fn runtime_capabilities_for_operations(
        &self,
        operations: &[TicketOperation],
    ) -> Result<Vec<RuntimeWorkerCapability>, VoomError>;
}

impl SqliteTicketRepo {
    pub async fn succeeded_results_for_job_and_operation(
        &self,
        job_id: JobId,
        operation: TicketOperation,
    ) -> Result<Vec<SucceededTicketResult>, VoomError>;
}
```

Empty operation input returns an empty vector without generating invalid SQL. Decode
all persisted vocabulary and JSON in the store with operation-specific error context.

- [ ] **Step 3: Replace all four compliance queries**

Use the worker projection in `policy_runtime_registry` and
`operations_lost_to_dead_endpoints`. Keep endpoint/secret parsing, runtime probing,
and live/dead comparison in the control plane. Use the ticket projection in
`audio_extract_outputs_for_job` and `audio_synthesis_companions_for_job`; keep
compliance result decoding in the control plane.

- [ ] **Step 4: Verify, rescan the bounded cluster, and commit**

Run:

```bash
cargo test -p voom-store workers
cargo test -p voom-store tickets
cargo test -p voom-control-plane compliance
cargo clippy -p voom-store -p voom-control-plane --all-targets --all-features -- -D warnings
rg -n 'sqlx::(query|query_as|query_scalar|raw_sql|QueryBuilder)' \
  crates/voom-control-plane/src/cases/policy/compliance.rs
desloppify scan --path .
git diff --check
git diff --cached -- docs/adr/
prek run
```

Expected inventory output: no matches.

Commit:

```bash
git commit -m "refactor(store): own compliance persistence queries"
```

---

## Task 6: Move artifact, media, bootstrap, and audio lease SQL into repositories

**Files:**

- Modify: `crates/voom-store/src/repo/media/artifacts.rs`
- Modify: `crates/voom-store/src/repo/media/artifacts_test.rs`
- Modify: `crates/voom-store/src/repo/media/identity.rs`
- Modify: `crates/voom-store/src/repo/media/identity_test.rs`
- Modify: `crates/voom-store/src/repo/execution/workers.rs`
- Modify: `crates/voom-store/src/repo/execution/workers_test.rs`
- Modify: `crates/voom-store/src/repo/execution/leases.rs`
- Modify: `crates/voom-store/src/repo/execution/leases_test.rs`
- Modify: `crates/voom-control-plane/src/artifact/bootstrap.rs`
- Modify: `crates/voom-control-plane/src/artifact/bootstrap_test.rs`
- Modify: `crates/voom-control-plane/src/artifact/inspect.rs`
- Modify: `crates/voom-control-plane/src/artifact/inspect_test.rs`
- Modify: `crates/voom-control-plane/src/artifact/verify.rs`
- Modify: `crates/voom-control-plane/src/artifact/verify_test.rs`
- Modify: `crates/voom-control-plane/src/artifact/commit/prepare.rs`
- Modify: `crates/voom-control-plane/src/artifact/commit/mod_test.rs`
- Modify: `crates/voom-control-plane/src/artifact/commit/finalize.rs`
- Modify: `crates/voom-control-plane/src/audio/mod.rs`
- Modify: `crates/voom-control-plane/src/audio/mod_test.rs`

- [ ] **Step 1: Add failing artifact repository tests**

Cover handle-ID keyset pagination, optional inspection facts, required verification
facts, missing checksum/size rejection, exact live staging-location selection,
retired/replaced location races, commit report update, and transaction rollback. Use
typed records:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactHandleFacts {
    pub handle: ArtifactHandle,
    pub size_bytes: Option<u64>,
    pub checksum: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactExpectedFacts {
    pub source_file_version_id: Option<FileVersionId>,
    pub size_bytes: u64,
    pub checksum: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveArtifactLocation {
    pub id: ArtifactLocationId,
    pub kind: String,
    pub value: String,
}
```

Add repository methods named:

```rust
list_handle_ids(after_id, limit)
handle_facts(handle_id)
require_expected_facts(handle_id)
require_expected_facts_in_tx(tx, handle_id)
live_location_of_kind_in_tx(tx, handle_id, kind)
require_live_location_in_tx(tx, handle_id, location_id, kind, value)
update_pending_commit_report_in_tx(tx, commit_id, report)
```

`handle_facts` preserves optional facts for inspection. The two
`require_expected_facts` forms return a contextual configuration error when size or
checksum is absent and reject a negative persisted size as a database error.
`live_location_of_kind_in_tx` must conflict when more than one live location exists;
`require_live_location_in_tx` must reject a retired, replaced, wrong-kind, or
wrong-value row.

- [ ] **Step 2: Add worker bootstrap and lease dispatch-context tests**

Move built-in worker insert-if-missing behavior behind:

```rust
pub async fn register_builtin_if_missing_in_tx(
    &self,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: NewWorker,
) -> Result<Worker, VoomError>;
```

The method must return the pre-existing row without mutating its identity. Keep
capability/grant composition and built-in identity validation in control-plane
bootstrap.

Add this lease projection and test held state, worker epoch, expiry, stale lease, and
missing worker behavior:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseDispatchContext {
    pub worker_id: WorkerId,
    pub worker_epoch: u64,
    pub expires_at: OffsetDateTime,
}

pub async fn dispatch_context(
    &self,
    lease_id: LeaseId,
) -> Result<Option<LeaseDispatchContext>, VoomError>;
```

- [ ] **Step 3: Add identity projections used by artifact inspection**

Move file-location and version facts into `FileLocationRepo`/`FileVersionRepo` methods
that return existing typed `FileLocation` and `FileVersion` values. Add only the
missing focused methods required by `read_handle_facts` and selected-location
revalidation; reuse existing `get`/`list` methods wherever their semantics match.

- [ ] **Step 4: Replace the eleven control-plane queries**

Update:

- bootstrap to call `register_builtin_if_missing_in_tx`;
- artifact inspection to call handle pagination and optional handle-facts methods;
- verification to call expected-facts and exact live-location methods;
- commit preparation to call transaction-aware expected-facts and staging-location
  methods;
- commit finalization to call `update_pending_commit_report_in_tx`;
- `audio_dispatch_lease` to call `dispatch_context`.

Delete `SqliteRow` decoders and integer conversion helpers made unnecessary by typed
repository results. Keep filesystem observation, verification dispatch, commit
sequencing, event emission, and report construction in the control plane.

- [ ] **Step 5: Verify and commit**

Run:

```bash
cargo test -p voom-store artifacts
cargo test -p voom-store identity
cargo test -p voom-store workers
cargo test -p voom-store leases
cargo test -p voom-control-plane artifact
cargo test -p voom-control-plane audio
cargo clippy -p voom-store -p voom-control-plane --all-targets --all-features -- -D warnings
rg -n 'sqlx::(query|query_as|query_scalar|raw_sql|QueryBuilder)' \
  crates/voom-control-plane/src/{artifact,audio/mod.rs} -g '!**/*_test.rs'
git diff --check
git diff --cached -- docs/adr/
prek run
```

Expected inventory output: no matches.

Commit:

```bash
git commit -m "refactor(store): own artifact persistence queries"
```

---

## Task 7: Move coordinator-finalization evidence SQL into repositories

**Files:**

- Modify: `crates/voom-store/src/repo/execution/tickets.rs`
- Modify: `crates/voom-store/src/repo/execution/tickets_test.rs`
- Modify: `crates/voom-store/src/repo/execution/workflow_progress.rs`
- Create: `crates/voom-store/src/repo/execution/workflow_progress_test.rs`
- Modify: `crates/voom-store/src/repo/execution/workflow_summaries.rs`
- Modify: `crates/voom-store/src/repo/execution/workflow_summaries_test.rs`
- Modify: `crates/voom-store/src/repo/media/artifacts.rs`
- Modify: `crates/voom-store/src/repo/media/artifacts_test.rs`
- Modify: `crates/voom-store/src/repo/media/identity.rs`
- Modify: `crates/voom-store/src/repo/media/identity_test.rs`
- Modify: `crates/voom-control-plane/src/workflow/coordinator/finalize.rs`
- Modify: `crates/voom-control-plane/src/workflow/coordinator/finalize_test.rs`

- [ ] **Step 1: Add failing tests for phase-scope and file-run projections**

In workflow progress/summary repositories, test:

- `(job_id, input_ordinal)` resolves the durable branch or returns `None`;
- `(job_id, branch_id)` resolves the file-run starting asset through its version;
- rollback leaves both projections unchanged.

Expose:

```rust
impl SqliteWorkflowProgressRepo {
    pub async fn branch_for_input_ordinal(
        &self,
        job_id: JobId,
        input_ordinal: u32,
    ) -> Result<Option<String>, VoomError>;
}

impl SqliteWorkflowSummaryRepo {
    pub async fn file_run_asset_id(
        &self,
        job_id: JobId,
        branch_id: &str,
    ) -> Result<Option<FileAssetId>, VoomError>;
}
```

Link the new sibling tests from the bottom of `workflow_progress.rs`:

```rust
#[cfg(test)]
#[path = "workflow_progress_test.rs"]
mod tests;
```

- [ ] **Step 2: Add failing identity and ticket scope tests**

Add an identity projection for the exact semantics of `working_dir_artifacts`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveChainTipLocation {
    pub location_id: FileLocationId,
    pub file_asset_id: FileAssetId,
    pub value: String,
    pub epoch: u64,
}
```

`FileLocationRepo::live_local_chain_tips(&[FileLocationId])` returns only live local
paths on active chain-tip versions, ordered by location ID. Test retired locations,
non-local kinds, superseded versions, empty input, and ordering.

Add ticket methods returning `Vec<TicketId>` for phase, phase/file,
phase/node/optional-file scope, and succeeded phase/file/operation scope. Accept
explicit store-owned scope parameters:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowPhaseScope<'a> {
    pub job_id: JobId,
    pub exact_workflow_id: &'a str,
    pub file_workflow_pattern: &'a str,
}
```

Test exact workflow IDs, file-workflow glob IDs, wrong jobs, wrong nodes, optional
source version, operation/state filtering, and deterministic ordering. The methods are
named `ticket_ids_for_workflow_phase`, `ticket_ids_for_workflow_phase_file`,
`ticket_ids_for_workflow_phase_scope`, and
`succeeded_ticket_ids_for_workflow_phase_file_and_operation`.

- [ ] **Step 3: Add typed verification and commit evidence projections**

Move the two multi-table evidence decoders out of `finalize.rs` into
`SqliteArtifactRepo`. Define typed store records that retain the optionality needed
for the control plane to distinguish missing evidence from mismatched evidence:

```rust
#[derive(Debug, Clone)]
pub struct CommittedTicketEvidence {
    pub ticket_id: TicketId,
    pub ticket_job_id: Option<JobId>,
    pub ticket_payload: JsonValue,
    pub result: JsonValue,
    pub commit: Option<ArtifactCommitEvidence>,
    pub verification: Option<ArtifactVerificationEvidence>,
    pub result_lease: Option<ResultLeaseEvidence>,
    pub source_file_asset_id: Option<FileAssetId>,
    pub result_file_asset_id: Option<FileAssetId>,
    pub location_file_version_id: Option<FileVersionId>,
    pub snapshot_file_version_id: Option<FileVersionId>,
}

#[derive(Debug, Clone)]
pub struct VerifiedTicketEvidence {
    pub verification: ArtifactVerification,
    pub file_version_id: Option<FileVersionId>,
    pub location_value: Option<String>,
}
```

Add:

```rust
committed_ticket_evidence(&[TicketId])
verified_ticket_evidence(ticket_id, lease_id, handle_id, location_id)
```

The store decodes IDs, JSON, timestamps, and all finite durable states. The control
plane retains lineage relevance, carried-row scope checks, selected-fact comparison,
and the decision about which commit is latest. Test malformed JSON, invalid durable
state, absent joined rows, multi-output results, sidecars, and deterministic ordering.

- [ ] **Step 4: Replace all ten finalization queries**

Update these functions to use repository methods:

- `validate_carried_ticket_scope`;
- `working_dir_artifacts`;
- `verified_refs_for_tickets`;
- `committed_evidence_for_tickets`;
- `file_run_asset_id`;
- `unfinalized_verified_refs`;
- `ticket_ids_for_phase`;
- `ticket_ids_for_phase_file`;
- `ticket_ids_for_phase_scope`.

The succeeded-verification lookup should use
`succeeded_ticket_ids_for_workflow_phase_file_and_operation` followed by the typed
artifact-evidence read. Change `CommittedResultFields::decode` to accept
`&serde_json::Value` after the store takes ownership of JSON decoding. Delete
`CommittedEvidenceRow::decode`, `VerifiedEvidenceRow`,
`WorkingDirArtifact::from_row`, `evidence_column`, and direct `SqliteRow` imports after
their typed store replacements are in place.

- [ ] **Step 5: Prove the enforceable production count is zero**

Run:

```bash
cargo test -p voom-store workflow
cargo test -p voom-store artifacts
cargo test -p voom-store identity
cargo test -p voom-control-plane workflow::coordinator
cargo clippy -p voom-store -p voom-control-plane --all-targets --all-features -- -D warnings
rg -n 'sqlx::(query|query_as|query_scalar|raw_sql|QueryBuilder)' \
  crates/voom-control-plane/src -g '!**/*_test.rs' -g '!**/tests/**'
```

Expected: exactly the two `#[cfg(test)]` fixture helpers in `cases/mod.rs`; no
production call remains.

- [ ] **Step 6: Verify and commit**

Run:

```bash
git diff --check
git diff --cached -- docs/adr/
prek run
```

Commit:

```bash
git commit -m "refactor(store): own workflow evidence queries"
```

---

## Task 8: Add the immediate zero-tolerance boundary guard

**Files:**

- Create: `crates/voom-control-plane/src/cases/mod_test.rs`
- Modify: `crates/voom-control-plane/src/cases/mod.rs`
- Create: `scripts/check-control-plane-sql-boundary.sh`
- Create: `scripts/check-control-plane-sql-boundary-selftest.sh`
- Modify: `justfile`
- Modify: `.pre-commit-config.yaml`

- [ ] **Step 1: Move the two inline test fixture queries into a sibling test file**

Add this link to `cases/mod.rs`:

```rust
#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

#[cfg(test)]
pub(crate) use tests::{
    TerminalFailureIssueRow, count, cp, issue_link_targets, terminal_failure_issues,
    transcodable_input,
};
```

Move all existing `#[cfg(test)]` helpers and their test-only imports from `cases/mod.rs`
to `cases/mod_test.rs`. Preserve their visibility so execution sibling tests compile.
This leaves every non-`*_test.rs` control-plane file structurally production-only for
the guard.

Run:

```bash
cargo test -p voom-control-plane cases::execution
```

Expected: PASS and the raw inventory command returns no matches.

- [ ] **Step 2: Write a failing guard self-test first**

Create `scripts/check-control-plane-sql-boundary-selftest.sh` with
`set -euo pipefail`. It creates a temporary fixture tree and invokes the guard with a
fixture root argument. Cover:

- `sqlx::query`, `query_as`, `query_scalar`, and one query macro;
- `sqlx::raw_sql`;
- `sqlx::QueryBuilder::new` and a typed `QueryBuilder::<Sqlite>::new`;
- multiline calls;
- aliased imports such as `use sqlx::query as db_query; db_query(...)`;
- similarly named functions outside SQLx, which pass;
- sibling `*_test.rs` files, which pass;
- a clean production tree, which passes;
- diagnostics containing file, line, forbidden API, and the repository-boundary fix.

Run:

```bash
./scripts/check-control-plane-sql-boundary-selftest.sh
```

Expected: FAIL because the guard script does not exist yet.

- [ ] **Step 3: Implement the structure-aware guard**

Create `scripts/check-control-plane-sql-boundary.sh` with `set -euo pipefail` and an
optional first argument defaulting to `crates/voom-control-plane/src`. Require
`ast-grep` to be installed and fail with an actionable setup command if missing.

Use ast-grep rules for direct qualified calls, macros, builders, and imported aliases.
Do not use a text-only grep as the enforcement mechanism. Exclude only paths ending
in `_test.rs` and `tests/`; there is no allowlist or baseline. Print all violations in
one run, exit non-zero when any exist, and print exactly this success line otherwise:

```text
control-plane SQL boundary: OK
```

Re-run the self-test until it passes.

- [ ] **Step 4: Wire the guard into local hooks and CI**

Add recipes:

```just
check-control-plane-sql-boundary:
    ./scripts/check-control-plane-sql-boundary.sh

check-control-plane-sql-boundary-selftest:
    ./scripts/check-control-plane-sql-boundary-selftest.sh
```

Add both recipes to `ci`. Add two local prek hooks with
`pass_filenames: false` and this file selector:

```yaml
files: '^(crates/voom-control-plane/src/.*\.rs|scripts/check-control-plane-sql-boundary.*\.sh|justfile)$'
```

- [ ] **Step 5: Verify the complete boundary**

Run:

```bash
shellcheck scripts/check-control-plane-sql-boundary.sh \
  scripts/check-control-plane-sql-boundary-selftest.sh
shfmt -d scripts/check-control-plane-sql-boundary.sh \
  scripts/check-control-plane-sql-boundary-selftest.sh
just check-test-layout
just check-control-plane-sql-boundary-selftest
just check-control-plane-sql-boundary
cargo test -p voom-control-plane cases::execution
cargo clippy -p voom-store -p voom-control-plane --all-targets --all-features -- -D warnings
git diff --check
git diff --cached -- docs/adr/
prek run
```

Expected: zero warnings, zero guard violations, and empty ADR diff.

- [ ] **Step 6: Commit the absolute invariant**

Commit:

```bash
git commit -m "ci: enforce control-plane SQL boundary"
```

---

## Task 9: Run full verification and close the Desloppify finding

**Files:**

- No production files expected.
- Local-only Desloppify state under `.desloppify/` may change and remains ignored.

- [ ] **Step 1: Run the full repository guardrails**

Run:

```bash
just ci
```

Expected: every command passes with zero warnings and no skipped checks.

- [ ] **Step 2: Verify architectural invariants directly**

Run:

```bash
test -z "$(rg -l 'sqlx::(query|query_as|query_scalar|raw_sql|QueryBuilder)' \
  crates/voom-control-plane/src -g '!**/*_test.rs' -g '!**/tests/**')"
git diff HEAD~8..HEAD -- docs/adr/
git status --short
```

Expected: no production query matches, empty ADR diff, and no uncommitted tracked
changes.

- [ ] **Step 3: Rescan and resolve only with evidence**

Run:

```bash
desloppify scan --path .
desloppify next
```

Use the exact resolve command printed by `desloppify next` for
`review::.::holistic::cross_module_architecture::control_plane_bypasses_store_boundary`.
Do not guess the resolution command or resolve another finding. Then run:

```bash
desloppify next
desloppify status
git status --short
```

Report the strict score before and after this cluster. If Desloppify generates a
tracked or untracked scorecard image, move it to Trash rather than committing it.

- [ ] **Step 4: Continue the living queue**

The next task is whatever `desloppify next` reports. Reorder with `desloppify plan`
only when the next finding has a demonstrated dependency on another open item. Keep
the session goal at strict score 92.
