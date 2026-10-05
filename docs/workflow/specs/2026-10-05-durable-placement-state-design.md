# Durable placement state for committed results (#677)

Decision record: [ADR 0103](../../adr/0103-durable-placement-state-for-committed-results.md).
ADR 0097 gains a later-decision note pointing at it.

## Problem

A committed record's result location may sit under a staging root for three reasons. The
coordinator may still move it to an output root. It may have been left there on purpose
(manual commit). Or it may already have moved, in which case the location was repointed and
the record says nothing. `artifact_commit_records` (`migrations/0001_schema.sql:1021-1056`)
has no column that tells these apart. #678 (retire guard) and #679 (never-moved tip
observability) both need that fact. #622 was closed for want of it.

## Design

### Storage (migration 0044, `migrations/0044_commit_result_placement.sql`)

1. `ALTER TABLE artifact_commit_records ADD COLUMN placement_intent TEXT CHECK
   (placement_intent IS NULL OR placement_intent IN ('staged','retained'))`.
2. `ALTER TABLE ... ADD COLUMN placement_state TEXT CHECK (placement_state IS NULL OR
   placement_state IN ('staged','retained','placed'))`.
3. Backfill every row's `placement_intent`. It is `staged` when `target_path` contains a
   `/.committed/` or `\.committed\` component, else `retained`.
4. Backfill `placement_state` for `committed` rows. It is `placed` when the intent is
   `staged` and the row's `result_file_location_id` names a `file_locations` row whose
   `provider_relative_locator` has no `.committed` component, else the intent.
5. Two triggers enforce the per-row rule on insert and on update of `state`,
   `placement_intent`, or `placement_state`. Any violation aborts with
   `artifact_commit_records placement violates its per-state rule`:
   - intent non-NULL;
   - `committed` ⇔ state non-NULL;
   - state = intent, or (`placed` and intent `staged`);
   - on update only, intent unchanged and `placed` never left.
6. `CREATE INDEX artifact_commit_records_by_result_location ON artifact_commit_records
   (result_file_location_id) WHERE result_file_location_id IS NOT NULL`. Promotion and
   #678 look records up by result location.

Why ADD COLUMN plus triggers, not a rebuild or a child table: ADR 0103, Considered &
rejected. A rebuild cannot drop a parent table that has populated `RESTRICT` children
inside the migration transaction. A column CHECK is evaluated against existing rows
before the backfill.

### Store (`crates/voom-store/src/repo/media/artifacts/`)

- `mod.rs`: `pub enum CommitPlacementIntent { Staged, Retained }` and
  `pub enum CommitPlacementState { Staged, Retained, Placed }`. Each has `as_str`, plus a
  private `parse` that maps an unknown value to `VoomError::Database`, the same shape as
  `ArtifactCommitState`. `NewArtifactCommitRecord` gains
  `placement_intent: CommitPlacementIntent`. `ArtifactCommitRecord` gains
  `placement_intent` and `placement_state: Option<CommitPlacementState>`.
- `commits.rs`:
  - `create_pending_commit_in_tx` binds the intent.
  - `SELECT_ARTIFACT_COMMIT_RECORD_COLS` and `row_to_commit_record` read both columns.
  - `mark_commit_committed_in_tx` and `finalize_sidecar_commit_record_in_tx` add
    `placement_state = placement_intent` to their committing `UPDATE`.
  - New `get_commit_record_by_result_location(FileLocationId) ->
    Result<Option<ArtifactCommitRecord>>` (pool-level) restricted to `state='committed'`.
    It returns `Database` if two rows match.
  - New `mark_result_placed_in_tx(tx, ArtifactCommitRecordId) ->
    Result<ArtifactCommitRecord>`: `UPDATE ... SET placement_state='placed' WHERE id=?
    AND state='committed' AND placement_state='staged'`. When the row count is not 1, it
    returns `Conflict` naming the record.
- `evidence.rs` and `voom-artifact/commit_pipeline.rs` are unchanged. Neither reads or
  writes placement.

### Control plane

- `artifact/commit/mod.rs`: `CommitArtifactInput` gains
  `pub placement_intent: CommitPlacementIntent`, re-exported from
  `voom_control_plane::artifact`.
- `prepare.rs`: `create_prepared_record` passes `input.placement_intent` into
  `NewArtifactCommitRecord`. The value travels in `PendingIntentDraft`.
- `recovery.rs`: `abort_and_reprepare_report` builds the successor input with
  `placement_intent: record.placement_intent` beside `record.target_path`.
- `finalize.rs`: unchanged. `mark_commit_committed_in_tx` copies the intent, so node
  completion (`intent.rs` `converge_intent_in_tx`) and recovery finalization both write
  `staged` or `retained` without new plumbing.
- `workflow/coordinator/promotion.rs` `promote_artifact`:
  - Before `move_terminal_artifact`, read
    `get_commit_record_by_result_location(artifact.location_id)`. With no record, it
    continues as today. A record whose state is not `Some(Staged)` returns `Conflict`
    naming the record and its state, before any byte moves.
  - In the existing `begin_write_first` transaction, after
    `update_file_location_address_in_tx`, a record found above is marked with
    `mark_result_placed_in_tx`. Any error drops the transaction, so the address update
    rolls back with it.
- `reclaim_intermediate_location` is unchanged. A reclaimed intermediate stays `staged`
  with a retired result location (ADR 0103 decision 3).

### Callers of `CommitArtifactInput`

- `voom-cli/src/commands/media/artifact.rs` (manual commit) passes `Retained`.
- `voom-cli/tests/support/owner_node.rs` (workflow commit into `.committed/<op>`) passes
  `Staged`.
- Other test call sites pass `Retained` unless a test needs `Staged`:
  - `voom-control-plane` `artifact/commit/mod_test.rs` and `inspect_test.rs`;
  - `tests/{staged_artifact_flow,recover_commit_gate,commit_use_lease_gate}.rs`;
  - `voom-api/src/commit_test.rs`.
- Fixtures that `INSERT INTO artifact_commit_records` directly name `placement_intent`,
  and `placement_state` when committed:
  - `voom-store` `artifacts/tests.rs`;
  - `voom-cli/tests/artifact_envelope.rs`;
  - `voom-control-plane/tests/staged_artifact_flow.rs`.

### Migration bookkeeping

- `voom-store/src/migrator.rs` registers version 7, `commit_result_placement`.
- `schema_test.rs` and `init_test.rs:16` expect 7 migrations.
- `init_test.rs`'s base-schema upgrade count rises from 5 to 6.
- `migrator_test.rs` `apply_through_0041` selects `version <= 4` instead of `len - 2`, so
  its 0042 guard tests keep their meaning.

## Failure model

1. Actors and deployments
   - Local operators running `voom init` on an existing pre-release database (upgrade).
   - Operators running `voom artifact commit` / `recover-commit`.
   - The compliance coordinator promoting chain tips (transitional path, ADR 0050).
   - Storage-owner node agents completing fenced intents (`converge_intent_in_tx`).
2. Invariants and assets at stake
   - Existing committed records survive the upgrade with a placement state, and no
     preflight refuses them.
   - Every committed record has exactly one placement state, consistent with its intent.
     Triggers enforce this per row.
   - A recovery successor's intent equals its predecessor's.
   - `placed` and the location repoint commit together or not at all.
   - Bytes never move for a result whose record is not `staged`.
3. Accepted failure classes
   - The pre-transaction byte-move crash window (bytes at the destination, address not
     repointed) is existing behavior that ADR 0050 accepts until #425. It is unchanged.
   - The upgrade backfill misclassifies only when a manual commit targeted a
     `.committed` path. Such a commit becomes `staged`, which is harmless because no
     promotion scope contains it (ADR 0103 Consequences).
   - A pre-0038 legacy committed record that was already moved out of `.committed` but
     whose commit-time `target_path` lacked the component is backfilled `retained`, not
     `placed`. Pre-release databases are disposable, and #678 then over-blocks rather
     than strands.
   - `staged` also matches withdrawn intermediates. Consumers join live locations
     (ADR 0103).
4. Covered elsewhere
   - The retire guard: #678. The tip warning: #679. Node-owned placement: #425.
   - Staging-root flag and config-time pairing: #618 and #616.
   - Permanent-cause recovery release: #684.

## Validation

Focused commands: `cargo test -p voom-store`, `cargo test -p voom-control-plane`, and
`cargo test -p voom-cli --test multi_phase_flow`, then `just ci`.

- Migration (`crates/voom-store/src/migrator_test.rs`,
  `migration_0044_backfills_placement_for_populated_commit_records`). Seed a database at
  version 6 with five records:
  - pending `.committed`;
  - committed manual;
  - committed `.committed`, unmoved;
  - committed `.committed`, repointed out;
  - failed, with target `/root/.committed/audio/e.mka`.

  Run `init_on`. Assert the intents and states (`staged`/NULL, `retained`/`retained`,
  `staged`/`staged`, `staged`/`placed`, `staged`/NULL), and that the index exists.
- Triggers (`crates/voom-store/src/repo/media/artifacts/tests.rs`,
  `placement_triggers_reject_invalid_rows`). Each case fails with the trigger message:
  - insert without an intent;
  - insert pending with a state;
  - commit without a state;
  - `placed` with a `retained` intent;
  - intent change;
  - leaving `placed`.
- Repository lifecycle (`tests.rs`, `placement_lifecycle_copies_intent_and_marks_placed`).
  A pending record reads back its intent with a NULL state, and the committed record has
  state = intent. `mark_result_placed_in_tx` moves `staged` to `placed`, and a second call
  is `Conflict`. A `retained` record is `Conflict`. `get_commit_record_by_result_location`
  finds the record, and an unknown location returns `None`.
- Commit (`artifact/commit/mod_test.rs`, `commit_records_the_requested_placement`). A
  `Retained` commit finalizes `retained`, and a `Staged` commit finalizes `staged`.
- Recovery inheritance (`mod_test.rs`).
  - `recovery_successor_inherits_staged_intent` (new): a `Staged` commit authorized
    without a receipt is recovered, and the successor record's intent is `staged`.
  - The existing `recover_commit_aborts_receiptless_authorized_and_reprepares` also
    asserts `retained`. The pair bites a hard-coded value in either direction.
- Promotion (`workflow/coordinator/promotion_test.rs`):
  - `promotion_marks_a_staged_result_placed`;
  - `promotion_refuses_a_retained_result_before_moving_bytes` (the source file stays
    and the address is unchanged);
  - `promotion_of_a_result_without_a_commit_record_writes_no_placement`;
  - `placement_failure_rolls_back_the_address_repoint`. A test-installed trigger aborts
    the `placed` update, and the location keeps its old address. This proves the shared
    transaction.
- End to end (`crates/voom-cli/tests/multi_phase_flow.rs`). After the two-phase run with
  `--output-dir`, phase 1's commit record is `placed` and phase 0's is `staged` with a
  retired result location (reclaimed intermediate).
- Manual commit (`crates/voom-cli/tests/artifact_envelope.rs`,
  `artifact_full_flow_outputs_committed_envelopes`). After `voom artifact commit`, the
  record's intent and state read `retained` from the database.
- The ADR 0097 note, ADR 0103, and the index row: `task-test-not-applicable`, prose. The
  ADR index row is covered by `just check-adr-index`.
