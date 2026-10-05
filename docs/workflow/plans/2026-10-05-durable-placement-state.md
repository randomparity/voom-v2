# Durable placement state for committed results (#677) — plan

Goal: every `artifact_commit_records` row carries a placement intent from prepare and a
placement state once committed. Promotion writes `placed` atomically with the address
repoint, and recovery successors inherit the intent.

Architecture: migration 0044 adds two columns with `ADD COLUMN`, backfills them, and adds
per-row triggers. `voom-store` reads and writes them, and the committing `UPDATE`s copy
the intent. The control plane takes the intent on `CommitArtifactInput`, carries it
through recovery re-prepare, and marks `placed` inside `promote_artifact`'s existing
transaction. Spec: `docs/workflow/specs/2026-10-05-durable-placement-state-design.md`.
ADR 0103.

Tech stack: Rust workspace, tokio, sqlx (SQLite), existing crates only.

Expected implementation size: 1150–1300 changed lines (L) — the file map below. About 70 SQL
lines, about 200 store lines, about 45 control-plane production lines, about 60 lines of
mechanical caller and fixture edits, and about 850 test lines. The estimate was revised after
the build. The original 650–850 under-counted the test fixtures: building a committed record
for the promotion tests takes about 180 lines (handle, staging location, verification,
pending commit, commit), and the migration, trigger, and lifecycle seeds take about 330. The
required work did not change.

## Global Constraints

- Existing dependencies and toolchain only. No version floors change. The migration
  number is exactly `0044` (physical version 7), and the ADR number is exactly `0103`.
- Preserve domain newtypes, and validate persisted values: an unknown placement string
  is `VoomError::Database`. Transactions open only through `voom_store::tx` helpers in
  production code; tests may use `pool.begin()`.
- Unit tests live in sibling `*_test.rs` files. Never pair paused tokio time with a
  real pool.
- ADR 0050 (the transitional promotion path writes only inside its existing
  transaction), ADR 0074 (`promotion_started_at` meaning unchanged), and ADR 0097 are
  preserved.
- Commits are conventional. Pre-commit hooks run via prek and are not bypassed.
  Guardrails: the focused `cargo test` commands per task, then `just ci`.

## File map

| File | Owns today | Change |
|---|---|---|
| `migrations/0044_commit_result_placement.sql` | — | new: columns, backfill, triggers, index |
| `crates/voom-store/src/migrator.rs` | embedded migration list | register version 7 |
| `crates/voom-store/src/migrator_test.rs` | 0042 guard tests | helper by version; 0044 regression |
| `crates/voom-store/src/schema_test.rs`, `init_test.rs` | migration counts | 6→7, 5→6 |
| `crates/voom-store/src/repo/media/artifacts/mod.rs` | commit record types | two enums, three fields |
| `crates/voom-store/src/repo/media/artifacts/commits.rs` | commit record SQL | bind/read/copy; two new fns |
| `crates/voom-store/src/repo/media/artifacts/tests.rs` | repo tests | fixtures + trigger/lifecycle tests |
| `crates/voom-store/src/repo/media/artifact_commit_intents_test.rs` | intent tests | raw-insert fixture column |
| `crates/voom-control-plane/src/artifact/commit/{mod,prepare,recovery}.rs` | commit driver | input field, carry-through |
| `crates/voom-control-plane/src/artifact/mod.rs` | public re-exports | re-export `CommitPlacementIntent` |
| `crates/voom-control-plane/src/artifact/commit/mod_test.rs` | commit tests | call sites + 2 tests |
| `crates/voom-control-plane/src/workflow/coordinator/promotion.rs` | transitional move | pre-check + in-tx `placed` |
| `crates/voom-control-plane/src/workflow/coordinator/promotion_test.rs` | promotion tests | 4 tests + fixture helper |
| callers / fixtures (Task 2 list) | tests, CLI | add the field / column |
| `crates/voom-cli/tests/multi_phase_flow.rs` | two-phase e2e | placement assertions |

No ownership moves. The commit record (voom-store) owns the state. Control-plane commit
and promotion are its only writers (criteria 4–6).

---

## Task 1 — Schema and store repository

Serves criteria 3, 4 (store half), 7 (record shape), and 8 (migration regression).

**Interfaces produced** (used by Tasks 2–3):

```rust
// crates/voom-store/src/repo/media/artifacts/mod.rs
pub enum CommitPlacementIntent { Staged, Retained }      // Copy, Eq, Debug
impl CommitPlacementIntent { pub const fn as_str(self) -> &'static str; }
pub enum CommitPlacementState { Staged, Retained, Placed } // Copy, Eq, Debug
impl CommitPlacementState { pub const fn as_str(self) -> &'static str; }
// NewArtifactCommitRecord { ..., pub placement_intent: CommitPlacementIntent }
// ArtifactCommitRecord { ..., pub placement_intent: CommitPlacementIntent,
//                         pub placement_state: Option<CommitPlacementState> }
impl SqliteArtifactRepo {
    pub async fn get_commit_record_by_result_location(&self, location: FileLocationId)
        -> Result<Option<ArtifactCommitRecord>, VoomError>;
    pub async fn mark_result_placed_in_tx(&self, tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: ArtifactCommitRecordId) -> Result<ArtifactCommitRecord, VoomError>;
}
```

**Verification**

- Migration backfill (`Mode: focused-test`). Contract: existing rows of every state get
  the specified intent and state, and the index exists. Test:
  `migrator_test.rs::migration_0044_backfills_placement_for_populated_commit_records`.
  Red: the `placement_intent` column does not exist. Green:
  `cargo test -p voom-store migration_0044`.
- Per-row rule (`Mode: focused-test`). Contract: the triggers reject each invalid shape.
  Test: `tests.rs::placement_triggers_reject_invalid_rows`. Red: the inserts succeed.
  Green: `cargo test -p voom-store placement_triggers`.
- Lifecycle SQL (`Mode: focused-test`). Contract: pending reads its intent with a NULL
  state, commit copies the intent, `placed` CAS, and lookup by location. Test:
  `tests.rs::placement_lifecycle_copies_intent_and_marks_placed`. Red: the methods are
  missing. Green: `cargo test -p voom-store placement_lifecycle`.
- Migration bookkeeping (`Mode: focused-test`). Contract: 7 embedded migrations, and
  the base-schema upgrade applies 6. Tests: the existing
  `schema_test::expected_migrations_matches_embedded_count` and
  `init_test::migration_0037_preserves_every_scheduler_decision_column_index_and_sequence`.
  Red: the counts are 6 and 5. Green: `cargo test -p voom-store expected_migrations
  migration_0037`.

**Steps**

1. Write `migrations/0044_commit_result_placement.sql`:

```sql
-- Migration 0044 (physical version 7): durable placement state for committed
-- results (issue #677, ADR 0103).
--
-- `artifact_commit_records` has three incoming ON DELETE RESTRICT foreign keys,
-- so it cannot be rebuilt inside the migration transaction, and a per-state
-- column CHECK added by ALTER TABLE is tested against existing committed rows
-- before any backfill. The columns are therefore added with domain CHECKs only,
-- backfilled, and the per-row rule is enforced by BEFORE triggers.
--
-- Backfill classifies by the working-dir convention the transitional promotion
-- path matches: a commit-time target_path with a `.committed` component was a
-- workflow commit expecting a move (`staged`); anything else was retained. A
-- staged committed row whose result locator no longer has that component was
-- moved (`placed`).

ALTER TABLE artifact_commit_records ADD COLUMN placement_intent TEXT
    CHECK (placement_intent IS NULL OR placement_intent IN ('staged','retained'));
ALTER TABLE artifact_commit_records ADD COLUMN placement_state TEXT
    CHECK (placement_state IS NULL OR placement_state IN ('staged','retained','placed'));

UPDATE artifact_commit_records
SET placement_intent = CASE
    WHEN instr(target_path, '/.committed/') > 0
      OR instr(target_path, '\.committed\') > 0 THEN 'staged'
    ELSE 'retained'
END;

UPDATE artifact_commit_records
SET placement_state = CASE
    WHEN placement_intent = 'staged' AND EXISTS (
        SELECT 1 FROM file_locations fl
        WHERE fl.id = artifact_commit_records.result_file_location_id
          AND fl.provider_relative_locator IS NOT NULL
          AND instr('/' || fl.provider_relative_locator, '/.committed/') = 0
    ) THEN 'placed'
    ELSE placement_intent
END
WHERE state = 'committed';

CREATE TRIGGER artifact_commit_records_placement_insert
BEFORE INSERT ON artifact_commit_records
WHEN NOT coalesce(
    NEW.placement_intent IN ('staged','retained')
    AND ((NEW.state = 'committed'
          AND (NEW.placement_state = NEW.placement_intent
               OR (NEW.placement_state = 'placed' AND NEW.placement_intent = 'staged')))
         OR (NEW.state <> 'committed' AND NEW.placement_state IS NULL)),
    0)
BEGIN
    SELECT RAISE(ABORT, 'artifact_commit_records placement violates its per-state rule');
END;

CREATE TRIGGER artifact_commit_records_placement_update
BEFORE UPDATE OF state, placement_intent, placement_state ON artifact_commit_records
WHEN NOT coalesce(
    NEW.placement_intent IN ('staged','retained')
    AND NEW.placement_intent = OLD.placement_intent
    AND (OLD.placement_state IS NOT 'placed' OR NEW.placement_state = 'placed')
    AND ((NEW.state = 'committed'
          AND (NEW.placement_state = NEW.placement_intent
               OR (NEW.placement_state = 'placed' AND NEW.placement_intent = 'staged')))
         OR (NEW.state <> 'committed' AND NEW.placement_state IS NULL)),
    0)
BEGIN
    SELECT RAISE(ABORT, 'artifact_commit_records placement violates its per-state rule');
END;

CREATE INDEX artifact_commit_records_by_result_location
    ON artifact_commit_records (result_file_location_id)
    WHERE result_file_location_id IS NOT NULL;
```

2. Register it in `crates/voom-store/src/migrator.rs`. After `MIGRATION_0043_SQL`, add:

```rust
/// Migration 0044 (physical version 7): durable placement state for committed
/// results (issue #677, ADR 0103). Adds `placement_intent`/`placement_state`
/// to `artifact_commit_records`, backfills existing rows, and enforces the
/// per-row rule with triggers; see the file header.
const MIGRATION_0044_SQL: &str =
    include_str!("../../../migrations/0044_commit_result_placement.sql");
```

   Then append `Migration::new(7, Cow::Borrowed("commit_result_placement"),
   MigrationType::Simple, Cow::Borrowed(MIGRATION_0044_SQL), false)` to the vector.
3. Bookkeeping:
   - In `schema_test.rs` and `init_test.rs:16`, change
     `assert_eq!(expected_migrations(), 6)` to `7`.
   - In `init_test.rs`, change `assert_eq!(report.migrations_applied, 5)` to `6` and
     append "and 0044 (commit result placement)" to the comment above it.
   - In `migrator_test.rs`, rename `apply_through_0041` to `apply_through_version(pool,
     last: i64)`. It filters `embedded.migrations.iter().filter(|m| m.version <= last)`
     instead of slicing `len - 2`.
   - `pool_one_behind()` calls `apply_through_version(&pool, 4)`.
4. Add the types to `artifacts/mod.rs`, beside `ArtifactCommitState`:

```rust
/// What the committer expects after commit (ADR 0103): a move to an output
/// root (`Staged`) or none (`Retained`). Set at prepare; never changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitPlacementIntent {
    Staged,
    Retained,
}

impl CommitPlacementIntent {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Retained => "retained",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "staged" => Ok(Self::Staged),
            "retained" => Ok(Self::Retained),
            other => Err(VoomError::database(format!(
                "artifact_commit_records.placement_intent {other:?} not in vocab"
            ))),
        }
    }
}

/// Where a committed result is (ADR 0103). `None` until the record commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitPlacementState {
    Staged,
    Retained,
    Placed,
}

impl CommitPlacementState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Retained => "retained",
            Self::Placed => "placed",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "staged" => Ok(Self::Staged),
            "retained" => Ok(Self::Retained),
            "placed" => Ok(Self::Placed),
            other => Err(VoomError::database(format!(
                "artifact_commit_records.placement_state {other:?} not in vocab"
            ))),
        }
    }
}
```

   Add `pub placement_intent: CommitPlacementIntent` to `NewArtifactCommitRecord`. Add
   `pub placement_intent: CommitPlacementIntent` and
   `pub placement_state: Option<CommitPlacementState>` to `ArtifactCommitRecord`, after
   `state`. Import both enums into `commits.rs` through its `use super::{...}` list.
5. In `commits.rs`:
   - Append `, c.placement_intent, c.placement_state` to
     `SELECT_ARTIFACT_COMMIT_RECORD_COLS`.
   - In `create_pending_commit_in_tx`, add `placement_intent` to the column list and a
     `?` to `VALUES`. Bind `input.placement_intent.as_str()` after `target_path`. The
     returned struct sets `placement_intent: input.placement_intent,
     placement_state: None`.
   - In `row_to_commit_record`, read
     `let placement_intent: String = row.try_get("placement_intent")` and
     `let placement_state: Option<String> = row.try_get("placement_state")`, each with
     `map_row_err`. Map them with `CommitPlacementIntent::parse(&placement_intent)?` and
     `placement_state.as_deref().map(CommitPlacementState::parse).transpose()?`.
   - In `mark_commit_committed_in_tx` and `finalize_sidecar_commit_record_in_tx`, change
     `SET state = 'committed', ...` to `SET state = 'committed', placement_state =
     placement_intent, ...`.
   - In `validate_sidecar_commit_input`, the `NewArtifactCommitRecord` literal gains
     `placement_intent: pending.placement_intent`.
   - Add these to `impl SqliteArtifactRepo`:

```rust
    /// The committed record whose result is `location`, if any (ADR 0103).
    ///
    /// # Errors
    /// `Database` when more than one committed record names the location.
    pub async fn get_commit_record_by_result_location(
        &self,
        location: FileLocationId,
    ) -> Result<Option<ArtifactCommitRecord>, VoomError> {
        let sql = SELECT_ARTIFACT_COMMIT_RECORD_COLS.to_owned()
            + " FROM artifact_commit_records c \
               WHERE c.result_file_location_id = ? AND c.state = 'committed' LIMIT 2";
        let rows = sqlx::query(&sql)
            .bind(i64_from_u64(location.0, "artifact_commit_records.result_file_location_id")?)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| VoomError::database_context("artifact_commit_records by location", e))?;
        if rows.len() > 1 {
            return Err(VoomError::database(format!(
                "file_locations {location} is the result of more than one committed record"
            )));
        }
        rows.first().map(row_to_commit_record).transpose()
    }

    /// Record that a `staged` committed result reached its output root, in the
    /// caller's address-update transaction (ADR 0103 decision 4).
    ///
    /// # Errors
    /// `Conflict` when the record is not committed and `staged`.
    pub async fn mark_result_placed_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: ArtifactCommitRecordId,
    ) -> Result<ArtifactCommitRecord, VoomError> {
        let res = sqlx::query(
            "UPDATE artifact_commit_records SET placement_state = 'placed' \
             WHERE id = ? AND state = 'committed' AND placement_state = 'staged'",
        )
        .bind(i64_from_u64(id.0, "artifact_commit_records.id")?)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("artifact_commit_records place", e))?;
        if res.rows_affected() != 1 {
            return Err(VoomError::Conflict(format!(
                "artifact_commit_records place: id={id} is not a committed staged result"
            )));
        }
        get_commit_record_in_tx(tx, id).await?.ok_or_else(|| {
            VoomError::Internal(format!("artifact_commit_records post-place get vanished: {id}"))
        })
    }
```

6. Make the workspace compile against the new field:
   - In `prepare.rs` `create_prepared_record`, set `placement_intent:
     CommitPlacementIntent::Retained` in `NewArtifactCommitRecord`. Task 2 replaces this
     with the caller's value.
   - In `tests.rs` `pending_commit(...)`, `mod_test.rs` `create_pending_commit_result`,
     and `inspect_test.rs:547`, add `placement_intent: CommitPlacementIntent::Retained`.
   - In `finalize_test.rs` `create_pending_commit`, add
     `placement_intent: CommitPlacementIntent::Staged`. Its targets model workflow
     results.
   - Import `CommitPlacementIntent` from `voom_store::repo::media::artifacts` in each
     file.
7. Raw `INSERT INTO artifact_commit_records` fixtures add the `placement_intent` column
   with `'retained'`. Rows inserted with `state` `'committed'` also add `placement_state`
   `'retained'`. The sites:
   - `voom-store` `artifacts/tests.rs:341`, `:3126`, `:3131`;
   - `artifact_commit_intents_test.rs:147`;
   - `voom-control-plane` `cases/policy/safety_gate_test.rs:204`;
   - `voom-control-plane/tests/staged_artifact_flow.rs:291`;
   - `voom-cli/tests/artifact_envelope.rs:560`.
8. Write the three focused tests and run them red, then green.
   - `migration_0044_...`:
     - `pool_one_behind`-style setup with `apply_through_version(&pool, 6)`.
     - Seed the parent rows with raw SQL, following `artifacts/tests.rs:3110-3135`
       (handles, staging locations, verifications, file versions and locations).
     - Seed five commit rows with ids 1–5:
       1. pending, `target_path` `/root/.committed/transcode/a.mkv`;
       2. committed manual, `target_path` `/root/b.mkv`, result locator `b.mkv`;
       3. committed, `/root/.committed/transcode/c.mkv`, result locator
          `.committed/transcode/c.mkv`;
       4. committed, `/root/.committed/remux/d.mkv`, result locator `out/d.mkv`;
       5. failed, `/root/.committed/audio/e.mka`.
     - Run `crate::init::init_on(&pool)`.
     - Assert `(intent, state)` per id: `(staged, NULL)`, `(retained, retained)`,
       `(staged, staged)`, `(staged, placed)`, `(staged, NULL)`.
     - Assert `artifact_commit_records_by_result_location` exists in `sqlite_schema`.
   - `placement_triggers_reject_invalid_rows`. On a fresh pool with the
     `pending_record_fixture` parents, each statement must fail with an error whose
     message contains `placement violates its per-state rule`:
     - raw insert without `placement_intent`;
     - raw pending insert with `placement_state = 'staged'`;
     - `UPDATE ... SET state='committed', result_file_version_id=?,
       result_file_location_id=?, finished_at=...` without a placement state;
     - on a committed `retained` row, `SET placement_state='placed'`;
     - `SET placement_intent='staged'` on a `retained` row;
     - on a `placed` row, `SET placement_state='staged'`.
   - `placement_lifecycle_copies_intent_and_marks_placed`. Use the existing
     `pending_record_fixture` path with a `Staged` intent, then `mark_commit_committed_in_tx`
     with the fixture's result version and location.
     - The pending record reads intent `Staged` and state `None`.
     - The committed record reads `Some(Staged)`.
     - `get_commit_record_by_result_location(location)` returns it.
     - `mark_result_placed_in_tx` returns `Some(Placed)`, and a second call errs with
       `ErrorCode::Conflict`.
     - A `Retained` commit's `mark_result_placed_in_tx` errs with `Conflict`.
     - `get_commit_record_by_result_location(FileLocationId(999_999))` is `None`.
9. Run `cargo test -p voom-store` (expect all pass) and `cargo test -p voom-control-plane
   artifact::commit` (expect all pass). Commit `feat(store): record durable placement
   state for committed results`.

---

## Task 2 — Placement intent on the commit input and through recovery

Serves criteria 4 (intent supplied at prepare), 5 (recovery inheritance), and manual vs
workflow callers.

**Interfaces consumed:** Task 1's `CommitPlacementIntent` and
`ArtifactCommitRecord::placement_intent`.

**Interfaces produced:** `CommitArtifactInput { artifact_handle_id, target_path,
pub placement_intent: CommitPlacementIntent }`, and
`voom_control_plane::artifact::CommitPlacementIntent` (re-export).

**Verification**

- Commit writes the requested state (`Mode: focused-test`). Test:
  `mod_test.rs::commit_records_the_requested_placement`. Red: a `Staged` commit
  finalizes `retained` because of the Task 1 constant. Green:
  `cargo test -p voom-control-plane commit_records_the_requested_placement`.
- Recovery inheritance (`Mode: focused-test`). Tests:
  `mod_test.rs::recovery_successor_inherits_staged_intent`, plus a new assertion in
  `recover_commit_aborts_receiptless_authorized_and_reprepares` that the successor
  intent is `Retained`. Red: the successor reads `Retained` for a `Staged` original.
  Green: `cargo test -p voom-control-plane recovery_successor_inherits
  recover_commit_aborts_receiptless`.
- Manual CLI commit is `retained` (`Mode: focused-test`). Test:
  `artifact_envelope.rs::artifact_full_flow_outputs_committed_envelopes`. It gains a
  query asserting `placement_intent = 'retained' AND placement_state = 'retained'` for
  the handle's committed record. Red: temporarily pass `Staged` in the CLI and observe
  the failure, then revert. Green:
  `cargo test -p voom-cli --test artifact_envelope artifact_full_flow`.

**Steps**

1. In `artifact/commit/mod.rs`, add the field with a doc comment:

```rust
#[derive(Debug)]
pub struct CommitArtifactInput {
    pub artifact_handle_id: ArtifactHandleId,
    pub target_path: PathBuf,
    /// Whether a move to an output root follows this commit (ADR 0103):
    /// `Staged` for a workflow commit into its working dir, `Retained` for a
    /// result deliberately left at its commit address.
    pub placement_intent: CommitPlacementIntent,
}
```

   Import `CommitPlacementIntent` from `voom_store::repo::media::artifacts`. In
   `artifact/mod.rs`, add `pub use voom_store::repo::media::artifacts::CommitPlacementIntent;`.
2. In `prepare.rs`:
   - Add `placement_intent: CommitPlacementIntent` to `PendingIntentDraft`.
   - Set it from `input.placement_intent` where the draft is built in
     `prepare_commit_in_tx`.
   - In `create_prepared_record`, replace the Task 1 constant with
     `placement_intent: draft.placement_intent`.
3. In `recovery.rs` `abort_and_reprepare_report`, change the successor input to:

```rust
        CommitArtifactInput {
            artifact_handle_id: record.artifact_handle_id,
            target_path: std::path::PathBuf::from(&record.target_path),
            placement_intent: record.placement_intent,
        },
```

4. Update every other `CommitArtifactInput { ... }` literal by adding a
   `placement_intent` field:
   - `voom-cli/src/commands/media/artifact.rs:287` gets
     `placement_intent: CommitPlacementIntent::Retained`, imported from
     `voom_control_plane::artifact`;
   - `voom-cli/tests/support/owner_node.rs:591` gets `CommitPlacementIntent::Staged`;
   - `voom-control-plane/src/artifact/commit/mod_test.rs` (17 sites, including
     `spawn_commit_task`) gets `Retained`;
   - `inspect_test.rs` gets `Retained`;
   - `voom-control-plane/tests/{staged_artifact_flow,recover_commit_gate,commit_use_lease_gate}.rs`
     get `Retained`;
   - `voom-api/src/commit_test.rs` gets `Retained`.
5. In `mod_test.rs`, add `spawn_commit_task_with(cp, artifact_handle_id, target_path,
   placement_intent)`. It is `spawn_commit_task`'s body with the intent parameterized,
   and `spawn_commit_task` delegates to it with `Retained`. Then add the tests:

```rust
#[tokio::test]
async fn commit_records_the_requested_placement() {
    for intent in [CommitPlacementIntent::Retained, CommitPlacementIntent::Staged] {
        let (cp, _db, dir) = fixture().await;
        let node = simulated_node(&cp).await;
        let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
        let target = dir.path().join("target.bin");
        let report = commit_with_node(
            &cp,
            &node,
            CommitArtifactInput {
                artifact_handle_id: staged.artifact_handle_id,
                target_path: target,
                placement_intent: intent,
            },
        )
        .await
        .unwrap();
        let record = cp
            .artifacts()
            .get_commit_record(report.commit_record_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.placement_intent, intent);
        let expected = match intent {
            CommitPlacementIntent::Staged => CommitPlacementState::Staged,
            CommitPlacementIntent::Retained => CommitPlacementState::Retained,
        };
        assert_eq!(record.placement_state, Some(expected));
    }
}

#[tokio::test]
async fn recovery_successor_inherits_staged_intent() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");
    let task = spawn_commit_task_with(
        &cp,
        staged.artifact_handle_id,
        &target,
        CommitPlacementIntent::Staged,
    );
    let intent_id = wait_pending_intent_id(&cp, staged.artifact_handle_id).await;
    node_authorize(&cp, &node, intent_id).await.unwrap();
    task.abort();

    let report = cp.recover_commit(staged.artifact_handle_id).await.unwrap();

    let successor = cp
        .artifacts()
        .get_commit_record(report.commit_record_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(successor.placement_intent, CommitPlacementIntent::Staged);
    assert_eq!(successor.placement_state, None);
}
```

   `commit_with_node` (`mod_test.rs:2087`) already exists. It spawns
   `drive_pending_commit_local` beside `commit_artifact`.

   In `recover_commit_aborts_receiptless_authorized_and_reprepares`, after the existing
   asserts, read the successor with `get_commit_record(report.commit_record_id)` and
   assert `placement_intent == CommitPlacementIntent::Retained`.
6. In `artifact_envelope.rs::artifact_full_flow_outputs_committed_envelopes`, after
   `commit`, add:

```rust
    let placement: (String, Option<String>) = sqlx::query_as(
        "SELECT placement_intent, placement_state FROM artifact_commit_records \
         WHERE artifact_handle_id = ? AND state = 'committed'",
    )
    .bind(i64::try_from(artifact_handle_id).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(placement, ("retained".to_owned(), Some("retained".to_owned())));
```

7. Run the three focused commands (expect pass). Then run `cargo test -p voom-control-plane`
   and `cargo test -p voom-api` (expect pass). Commit `feat(control-plane): carry
   placement intent from commit input through recovery`.

---

## Task 3 — Promotion writes `placed` with the address repoint

Serves criteria 6 and 7 (no-record outputs and reclaimed intermediates), and the
end-to-end proof.

**Interfaces consumed:** `get_commit_record_by_result_location` and
`mark_result_placed_in_tx` (Task 1). `CommitPlacementState` (Task 1).

**Verification**

- Staged becomes placed (`Mode: focused-test`). Test:
  `promotion_test.rs::promotion_marks_a_staged_result_placed`. Red: the state stays
  `staged`. Green: `cargo test -p voom-control-plane promotion_marks_a_staged`.
- Non-staged refused before the byte move (`Mode: focused-test`). Test:
  `promotion_refuses_a_retained_result_before_moving_bytes`. Red: the move happens.
  Green: `cargo test -p voom-control-plane promotion_refuses_a_retained`.
- No-record output (`Mode: focused-test`). Test:
  `promotion_of_a_result_without_a_commit_record_writes_no_placement`. It passes before
  the change, and is kept to guard the "no record means no write" branch: an
  implementation that errors on a missing record fails it. Green:
  `cargo test -p voom-control-plane promotion_of_a_result_without`.
- Same transaction (`Mode: focused-test`). Test:
  `placement_failure_rolls_back_the_address_repoint`. Red: with the `placed` write in a
  separate transaction, the address is repointed despite the failure. Green:
  `cargo test -p voom-control-plane placement_failure_rolls_back`.
- End to end (`Mode: focused-test`). Test: `multi_phase_flow.rs`, extended. Red: phase
  1's record reads `staged`. Green: `cargo test -p voom-cli --test multi_phase_flow`.

**Steps**

1. In `promotion.rs` `promote_artifact`, after computing `dest` and before
   `move_terminal_artifact`, add:

```rust
        let record = self
            .artifacts
            .get_commit_record_by_result_location(artifact.location_id)
            .await?;
        if let Some(record) = &record
            && record.placement_state != Some(CommitPlacementState::Staged)
        {
            return Err(VoomError::Conflict(format!(
                "terminal artifact location {} is the result of commit record {} whose \
                 placement is {}; only a staged result is moved to an output root (ADR 0103)",
                artifact.location_id,
                record.id,
                record.placement_state.map_or("unset", CommitPlacementState::as_str),
            )));
        }
```

   In the transaction, after `update_file_location_address_in_tx(...).await?;` and before
   `commit_tx(tx)`, add:

```rust
        if let Some(record) = record {
            self.artifacts
                .mark_result_placed_in_tx(&mut tx, record.id)
                .await?;
        }
```

   Import `voom_store::repo::media::artifacts::CommitPlacementState`. Update
   `promote_artifact`'s doc comment to: "Move a terminal artifact into `dest_dir`,
   repoint its location, and mark its commit record `placed` in the same transaction
   (ADR 0103)."
2. Add a fixture helper to `promotion_test.rs`,
   `committed_result(cp, path, intent) -> WorkingDirArtifact`. It follows
   `finalize_test.rs` `create_verified_staging` and `create_sidecar_commit_result`:
   1. Write the bytes to `path` with `write`.
   2. Ingest a source file with `record_discovered_file`, as the existing cleanup tests
      do, to get `file_version_id`.
   3. Register a worker with `cp.register_worker(RegisterWorkerInput { name, kind:
      WorkerKind::Synthetic })`.
   4. Create the handle with `cp.create_artifact_handle(NewArtifactHandle {
      file_version_id: Some(source), .. })`, using the same field values as
      `create_verified_staging`.
   5. Record the staging artifact location with `cp.record_artifact_location`.
   6. Record a succeeded verification with `record_verification_in_tx` inside
      `begin_read_then_write`.
   7. Create the pending commit with `create_pending_commit_in_tx`: `target_path =
      path.display()`, `placement_intent = intent`, report `rooted_target`
      `{TEST_STORAGE_ROOT_ID, test_relative_locator(path)}`.
   8. Commit with `record_verified_sidecar_commit_rows_in_tx(NewSidecarArtifactCommit
      { commit_record_id, target_path, storage_root_id: TEST_STORAGE_ROOT_ID,
      provider_relative_locator: test_relative_locator(path), content_hash, size_bytes,
      observed_at: UNIX_EPOCH, finished_at: UNIX_EPOCH })` inside `begin_write_first`.
   9. Build `WorkingDirArtifact { location_id, asset_id: committed.file_asset_id,
      storage_root_id: TEST_STORAGE_ROOT_ID, provider_relative_locator, epoch }`, taking
      the epoch from `cp.identity().get_file_location(location_id)`.

   Every test calls
   `voom_store::test_support::set_test_storage_root_self_defaults(&cp.pool)` so
   `resolve_output_target` has an output root.
3. Write the four tests. Each uses `crate::cases::cp()`, a `tempfile::TempDir` `tmp`,
   source `tmp/working/out.mkv`, and `dest_dir = tmp/output`.
   - `promotion_marks_a_staged_result_placed`. Run `committed_result(.., Staged)`, then
     `cp.promote_artifact(&artifact, &path, &dest_dir).await.unwrap()`. Assert the record
     from `get_commit_record_by_result_location(artifact.location_id)` has
     `placement_state == Some(Placed)`, and `dest_dir/out.mkv` exists.
   - `promotion_refuses_a_retained_result_before_moving_bytes`. Run
     `committed_result(.., Retained)`. `promote_artifact` errs with
     `ErrorCode::Conflict`. `path` still exists, `dest_dir/out.mkv` does not, and the
     location's `provider_relative_locator` is unchanged.
   - `promotion_of_a_result_without_a_commit_record_writes_no_placement`. Ingest `path`
     with `record_discovered_file` only, building `WorkingDirArtifact` from the outcome.
     `promote_artifact` succeeds, and
     `get_commit_record_by_result_location(location)` is `None`.
   - `placement_failure_rolls_back_the_address_repoint`. Run
     `committed_result(.., Staged)`. Then execute on `cp.pool`:
     `CREATE TRIGGER test_refuse_placed BEFORE UPDATE OF placement_state ON
     artifact_commit_records WHEN NEW.placement_state = 'placed' BEGIN SELECT RAISE(ABORT,
     'test refuses placed'); END`. `promote_artifact` errs. The location's
     `provider_relative_locator` still equals the pre-promotion locator, and the record is
     still `Some(Staged)`. Bytes have moved, which is the accepted pre-transaction window
     (spec, Failure model 3), so the test does not assert on them.
4. In `multi_phase_flow.rs`, after `assert_execute_committed_two_phases`, read the
   produced versions from `file_phases` (as that helper does) and add:

```rust
async fn placement_of(url: &str, version: u64) -> (String, bool) {
    let pool = voom_store::connect(url).await.unwrap();
    sqlx::query_as(
        "SELECT c.placement_state, fl.retired_at IS NOT NULL \
         FROM artifact_commit_records c \
         JOIN file_locations fl ON fl.id = c.result_file_location_id \
         WHERE c.result_file_version_id = ? AND c.state = 'committed'",
    )
    .bind(i64::try_from(version).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap()
}
```

   Assert `placement_of(&url, produced_v1).await == ("staged".to_owned(), true)` (phase 0
   intermediate, reclaimed) and `placement_of(&url, produced_v2).await ==
   ("placed".to_owned(), false)` (phase 1 tip, moved to `--output-dir`). Use the
   `assert_eq!` message "ADR 0103: reclaimed intermediates stay staged; moved tips are
   placed".
5. Run the focused commands (expect pass). Then run `cargo test -p voom-control-plane
   workflow::coordinator` and `cargo test -p voom-cli --test multi_phase_flow` (expect
   pass). Commit `feat(control-plane): mark promoted results placed with the address
   repoint`.

---

## Final verification

`just ci` (= `just source-checks` + `just platform-checks`). Expect exit 0. It includes
`check-adr-index`, which needs the ADR 0103 row already added to `docs/adr/README.md`
with the design artifacts.

## Deferrals

None.
