# Issue 161 Durable Workflow-Summary Schema + Repo Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:test-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the durable three-grain `workflow_summaries` schema (migration 0015) and `SqliteWorkflowSummaryRepo`, so the Sprint 16 coordinator (#162) has a durable surface to persist per-phase and per-`(file, phase)` workflow summaries that carry each phase's compliance report.

**Architecture:** Pinned by `docs/adr/0006-workflow-summary-schema.md`. Three tables keyed off `jobs(id)` (children do not FK the parent); scalar produced-id references are real FKs (`ON DELETE RESTRICT`), the ticket set and the `per_operation` rollup / compliance `report` ride as `json_valid` TEXT; `elapsed` is integer nanoseconds; child writes are idempotent first-write-wins (`ON CONFLICT DO NOTHING`). The repo lives in `voom-store`, which sits below `voom-control-plane`, so its input/row structs are self-contained (primitive counters + `serde_json::Value`), importing no control-plane type.

**Tech Stack:** Rust, sqlx (SQLite, `migrate` feature, no macros), `tempfile`-backed pool tests via `crate::test_support::fresh_initialized_pool_at`, `cargo test`, `just ci`.

---

## Files

- Add: `migrations/0015_workflow_summaries.sql`
- Modify: `crates/voom-store/src/migrator.rs` (register migration 15)
- Modify: `crates/voom-store/src/schema_test.rs` (bump `expected_migrations()` to 15; add a schema-shape assertion)
- Add: `crates/voom-store/src/repo/workflow_summaries.rs`
- Add: `crates/voom-store/src/repo/workflow_summaries_test.rs`
- Modify: `crates/voom-store/src/repo/mod.rs` (declare module + re-export public types)

---

## Task 1: Migration + migrator wiring

- [ ] **Step 1: Write `migrations/0015_workflow_summaries.sql`.**

Three `STRICT` tables. All timestamps are ISO-8601 TEXT (codebase convention). No leading `COMMIT;` dance is needed — this migration only *creates* tables and never rebuilds an FK'd table, so it runs inside sqlx's wrapping transaction unchanged. Do **not** append a trailing `PRAGMA foreign_key_check;`: 0012/0013 carry it only because they rebuild a populated table, and as a bare migration statement it returns rows without raising — inert here (this migration inserts no rows) and misleading. A create-only migration (cf. 0011) omits it.

```sql
-- Sprint 16 -- durable three-grain workflow summaries (job / phase / file-phase).
-- Per-phase compliance reports fold into the per-phase row; there is no reports table.

CREATE TABLE workflow_summaries (
    job_id                       INTEGER PRIMARY KEY REFERENCES jobs(id) ON DELETE CASCADE,
    branch_count                 INTEGER NOT NULL CHECK (branch_count >= 0),
    ticket_count                 INTEGER NOT NULL CHECK (ticket_count >= 0),
    dispatch_count               INTEGER NOT NULL CHECK (dispatch_count >= 0),
    retry_count                  INTEGER NOT NULL CHECK (retry_count >= 0),
    failure_count                INTEGER NOT NULL CHECK (failure_count >= 0),
    peak_active_workflow_leases  INTEGER NOT NULL CHECK (peak_active_workflow_leases >= 0),
    elapsed_ns                   INTEGER NOT NULL CHECK (elapsed_ns >= 0),
    per_operation                TEXT NOT NULL CHECK (json_valid(per_operation)),
    created_at                   TEXT NOT NULL
) STRICT;

CREATE TABLE workflow_phase_summaries (
    id            INTEGER PRIMARY KEY,
    job_id        INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    phase_ordinal INTEGER NOT NULL CHECK (phase_ordinal >= 0),
    phase_name    TEXT NOT NULL,
    report_id     TEXT,
    report        TEXT CHECK (report IS NULL OR json_valid(report)),
    outcome       TEXT NOT NULL
        CHECK (outcome IN ('completed','partially-committed','skipped','blocked')),
    created_at    TEXT NOT NULL,
    -- report_id and report live or die together (skipped/blocked phases have neither).
    CHECK ((report_id IS NULL AND report IS NULL)
        OR (report_id IS NOT NULL AND report IS NOT NULL))
) STRICT;

CREATE UNIQUE INDEX workflow_phase_summaries_key
    ON workflow_phase_summaries (job_id, phase_ordinal);

CREATE TABLE workflow_file_phase_summaries (
    id                       INTEGER PRIMARY KEY,
    job_id                   INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    phase_ordinal            INTEGER NOT NULL CHECK (phase_ordinal >= 0),
    branch_id                TEXT NOT NULL,
    ticket_ids               TEXT NOT NULL CHECK (json_valid(ticket_ids)),
    produced_file_version_id   INTEGER REFERENCES file_versions(id)   ON DELETE RESTRICT,
    produced_file_location_id  INTEGER REFERENCES file_locations(id)  ON DELETE RESTRICT,
    artifact_handle_id         INTEGER REFERENCES artifact_handles(id) ON DELETE RESTRICT,
    reprobe_snapshot_id        INTEGER REFERENCES media_snapshots(id)  ON DELETE RESTRICT,
    outcome                  TEXT NOT NULL CHECK (outcome IN ('committed','skipped','blocked')),
    created_at               TEXT NOT NULL,
    -- A committed file carries its produced version, location, and re-probe snapshot
    -- (written only after commit AND re-probe; ADR-0006). Non-advancing files carry none.
    CHECK (
        (outcome = 'committed'
            AND produced_file_version_id IS NOT NULL
            AND produced_file_location_id IS NOT NULL
            AND reprobe_snapshot_id IS NOT NULL)
        OR (outcome IN ('skipped','blocked')
            AND produced_file_version_id IS NULL
            AND produced_file_location_id IS NULL
            AND artifact_handle_id IS NULL
            AND reprobe_snapshot_id IS NULL)
    )
) STRICT;

CREATE UNIQUE INDEX workflow_file_phase_summaries_key
    ON workflow_file_phase_summaries (job_id, phase_ordinal, branch_id);
```

- [ ] **Step 2: Register migration 15 in `migrator.rs`.** Add `MIGRATION_0015_SQL` `include_str!` const and a `Migration::new(15, Cow::Borrowed("workflow_summaries"), MigrationType::Simple, …, false)` entry at the end of the `migrations` vec.

- [ ] **Step 3: Bump the migration-count assertion.** In `crates/voom-store/src/schema_test.rs`, change `assert_eq!(expected_migrations(), 14)` to `15`. Run `cargo test -p voom-store schema` — expected: the count test now passes; if any seeded-upgrade test exists it must still pass (the migration is additive).

- [ ] **Step 4: Add a schema-shape test.** In `schema_test.rs`, add a test that opens a `fresh_initialized_pool_at`, asserts the three tables exist (`SELECT sql FROM sqlite_schema WHERE name = 'workflow_file_phase_summaries'` returns the expected FK columns), and asserts `pragma_foreign_key_list('workflow_file_phase_summaries')` includes exactly the four produced-id FKs plus `jobs`. Run it; confirm pass.

## Task 2: Repo types and trait (TDD)

- [ ] **Step 1: Write the failing round-trip test first.** Create `crates/voom-store/src/repo/workflow_summaries_test.rs` with a `repo()` helper mirroring `scheduler_decisions_test.rs` (`tempfile::NamedTempFile` + `fresh_initialized_pool_at`) and a `seed_refs` that inserts one `jobs` row, one `file_assets` + `file_versions` + `file_locations` + `media_snapshots` chain, one `artifact_handles`, and two `tickets`. First test: `summary_round_trips` — insert a `NewWorkflowSummary` with all seven counters, an `elapsed` of `Duration::from_nanos(1_500_000_001)`, and `per_operation = json!({"transcode_video": {"success_count": 1}})`; assert `get_summary` returns an equal `WorkflowSummary` (counters, `elapsed` exact to the nanosecond, `per_operation` Value-equal). Wire `#[cfg(test)] #[path = "workflow_summaries_test.rs"] mod tests;` at the bottom of `workflow_summaries.rs`. Run `cargo test -p voom-store workflow_summaries` — expected: FAIL to compile (types absent).

- [ ] **Step 2: Define the input/row structs and the trait** in `workflow_summaries.rs`:
  - `NewWorkflowSummary { job_id: JobId, branch_count: u32, ticket_count: u32, dispatch_count: u64, retry_count: u64, failure_count: u64, peak_active_workflow_leases: u32, elapsed: std::time::Duration, per_operation: serde_json::Value }`
  - `WorkflowSummary` = the same fields plus `created_at: OffsetDateTime`.
  - `PhaseOutcome { Completed, PartiallyCommitted, Skipped, Blocked }` with `as_str`/`parse` (kebab tokens, mirroring `JobState`).
  - `FilePhaseOutcome { Committed, Skipped, Blocked }` with `as_str`/`parse`.
  - `NewPhaseSummary { job_id, phase_ordinal: u32, phase_name: String, report: Option<PhaseReport>, outcome: PhaseOutcome }` where `PhaseReport { report_id: String, report: serde_json::Value }` keeps the both-or-neither invariant un-representable-when-violated. `PhaseSummary` adds `id: u64, created_at`.
  - `NewFilePhaseSummary { job_id, phase_ordinal: u32, branch_id: String, ticket_ids: Vec<TicketId>, produced_file_version_id: Option<FileVersionId>, produced_file_location_id: Option<FileLocationId>, artifact_handle_id: Option<ArtifactHandleId>, reprobe_snapshot_id: Option<MediaSnapshotId>, outcome: FilePhaseOutcome }`. `FilePhaseSummary` adds `id: u64, created_at`.
  - `#[async_trait] trait WorkflowSummaryRepo: Repository` with the methods from ADR-0006 (each `*_in_tx` + pool wrapper; `get_*`; `phases_for_job`; `file_phases_for_job`). Take `now: OffsetDateTime` on each writer (callers pass the clock, matching `jobs`/`scheduler_decisions`).

- [ ] **Step 3: Implement `SqliteWorkflowSummaryRepo`.** Mirror `SqliteJobRepo`: `{ pool }`, `new`, `impl Repository`. For writers, the pool wrapper does `begin` → `*_in_tx` → `commit`. Convert via `common::{i64_from_u64, u64_from_i64, u32_from_i64, iso8601, parse_iso8601, serialize_json}`. `elapsed` ↔ ns: `u64::try_from(d.as_nanos()).map_err(|e| VoomError::Database(...))` on write, `Duration::from_nanos(u64_from_i64(v))` on read. `ticket_ids` ↔ JSON array of the raw `u64`s. Child writers use `INSERT … ON CONFLICT(<key>) DO NOTHING`; if `res.rows_affected() == 0`, the row already existed — re-read it **through `&mut **tx`** by natural key and return it (do not read via `self.pool`; per `_in_tx_reread_uses_tx_handle`). On a fresh insert, build the return value from the input + `last_insert_rowid()`. `insert_summary_in_tx` is a plain insert. Run the round-trip test; confirm pass.

- [ ] **Step 4: Per-phase report-link test.** `phase_summary_links_report`: upsert a `NewPhaseSummary` with `outcome = Completed` and a `PhaseReport { report_id: "rep-abc", report: json!({"schema_version": 1}) }`; assert `get_phase_summary(job, 0)` and `phases_for_job(job)` both return it with the report_id and report intact. Add `phase_summary_skipped_has_no_report`: `outcome = Skipped`, `report: None`; assert it round-trips with `report_id`/`report` NULL. Implement until green.

- [ ] **Step 5: Per-`(file, phase)` link test.** `file_phase_summary_links_artifacts`: upsert a committed `NewFilePhaseSummary` citing the seeded `file_version`/`file_location`/`artifact_handle`/`media_snapshot` and `ticket_ids = vec![TicketId(1), TicketId(2)]`; assert `get_file_phase_summary` returns the exact produced ids, ticket ids (order preserved), and `Committed`. Add `committed_requires_produced_lineage`: attempt to upsert a `Committed` row with `produced_file_version_id: None` and assert the call returns `Err(VoomError::Database(_))` (the CHECK fires) — proving the invariant is enforced at the DB, not just in Rust. Add `produced_ids_must_reference_real_rows`: upsert a `Committed` row whose `produced_file_version_id` (and separately `reprobe_snapshot_id`) names an id that was never seeded (e.g. `FileVersionId(9999)`) and assert `Err(VoomError::Database(_))` from the foreign-key violation (FK enforcement is on — `pool.rs:32` sets `foreign_keys(true)`). This proves the link is FK-enforced against the right table, not merely stored — the other half of the "links the correct artifacts/snapshots" acceptance, and a guard against a column being wired to the wrong table or FK enforcement silently regressing.

- [ ] **Step 6: Half-committed-barrier test.** `half_committed_barrier_records_only_advanced_files`: for one `phase_ordinal`, upsert a `Committed` row for branch `"a"` and **no** row for branch `"b"` (the file that failed); assert `file_phases_for_job(job)` returns exactly one row, for `"a"`. This is the issue's named acceptance.

- [ ] **Step 7: Idempotency test.** `file_phase_upsert_is_first_write_wins`: upsert a `Committed` row for `(job, 0, "a")`; upsert again for the same key with a different `outcome`/produced ids; assert the **first** call's stored row is returned/retained (no error, `file_phases_for_job` still has one row, content matches the first write). Mirror with `phase_upsert_is_first_write_wins`. These lock ADR-0006's first-write-wins contract and prove resume/finalize double-writes don't throw.

## Task 3: Exports + guardrails

- [ ] **Step 1: Wire the module + re-exports** in `repo/mod.rs`: add `pub mod workflow_summaries;` and a `pub use workflow_summaries::{ … }` block exporting every public type and the `SqliteWorkflowSummaryRepo`. Keep the list alphabetized within the block to match the file's style.

- [ ] **Step 2: Run targeted tests.** `cargo test -p voom-store workflow_summaries schema` — all green.

- [ ] **Step 3: Run the full store crate + workspace doc/layout checks.** `cargo test -p voom-store`, then `just lint` and `just fmt-check`. Fix every clippy finding (use `#[expect(…, reason = "…")]` per `project_expect_over_allow`, never `#[allow]`).

- [ ] **Step 4: `just ci`.** Confirm `fmt-check`, `lint`, `check-test-layout`, `test`, `doc`, `deny`, `audit` all pass. The new repo has no new dependency, so `deny`/`audit` are unaffected.

---

## Verification

- `cargo test -p voom-store workflow_summaries` — round-trip, report link, artifact link, committed-CHECK rejection, half-barrier, first-write-wins idempotency all pass.
- `cargo test -p voom-store schema` — migration count is 15; the three tables and their FKs exist.
- `just ci` — green.
- The acceptance criteria in issue #161 map to: round-trip (Task 2 Step 1/3), per-phase report link (Step 4), per-`(file, phase)` artifact/ticket link — both stored-and-round-tripped and FK-enforced against real rows (Step 5), half-committed barrier (Step 6).

## Rollback / cleanup

- The change is additive: one new migration and one new repo module. No existing table is altered, so reverting is dropping the migration entry, the SQL file, the repo module, and the `mod.rs` exports, and restoring the `expected_migrations()` count to 14. No production data migration is implied (Sprint 16 is pre-release; no deployed DB carries migration 15 yet).
