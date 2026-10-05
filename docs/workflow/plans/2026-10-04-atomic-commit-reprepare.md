# Atomic commit-recovery abort and re-prepare (#665) — plan

Goal: commit recovery's abort-and-reprepare commits the old attempt's abort only together with
the successor's prepare, so a failed re-prepare leaves the old attempt recoverable.

Architecture: `recovery::abort_and_reprepare_report` runs `prepare::prepare_commit_in_tx` on its
own `BEGIN IMMEDIATE` transaction after the abort writes and commits once. Prepare selects the
source location through a new transaction-local `operation_source::select_location_in_tx` so it
sees the abort's uncommitted staging retirement. `voom-store`'s in-tx live-location listing uses
the checked `u64_from_i64` conversion. Spec:
`docs/workflow/specs/2026-10-04-atomic-commit-reprepare-design.md`.

Tech stack: Rust workspace, tokio, sqlx (SQLite), existing crates only.

Expected implementation size: 170–230 changed lines (M) — file map below: ~25 production lines
across recovery.rs, prepare.rs, operation_source.rs, identity.rs; ~150–200 test lines.

## Global Constraints

- Existing dependencies and toolchain only; no version floors change; no migration (latest 0043).
- Preserve domain newtypes; persisted values use checked conversions (corruption is a database
  error). Transactions open only through `voom_store::tx` helpers.
- Unit tests live in sibling `*_test.rs` files; never pair paused tokio time with a real pool.
- ADR 0074 (expected facts pinned at verification; epoch fencing) and ADR 0097 (staging
  containment, fail closed without a default) are preserved.
- Guardrails: focused `cargo test`, then `just ci` (= `just source-checks` + `just
  platform-checks`). Commits are conventional; pre-commit hooks run via prek and must not be
  bypassed.

## File map

| File | Owns today | Change |
|---|---|---|
| `crates/voom-store/src/repo/media/identity.rs` | identity SQL | in-tx live-location list uses `u64_from_i64` |
| `crates/voom-store/src/repo/media/identity_test.rs` | identity tests | negative-id regression |
| `crates/voom-control-plane/src/operation_source.rs` | source selection policy | add `select_location_in_tx`; share 0/1/many rule |
| `crates/voom-control-plane/src/artifact/commit/prepare.rs` | prepare leg | `prepare_commit_in_tx` `pub(super)`; tx-local source selection |
| `crates/voom-control-plane/src/artifact/commit/recovery.rs` | recovery | abort + prepare in one transaction |
| `crates/voom-control-plane/src/artifact/commit/mod_test.rs` | commit tests | new + extended tests, three helpers |

No caller migration: `abort_and_reprepare_report` keeps its signature; `select_location` keeps
its signature and callers. No obsolete path remains — the post-abort `prepare_commit` call is
removed.

## Task 1 — checked ID conversion in the in-tx live-location list

Verification:
- Contract: a negative stored `file_locations.id` is a database error. Mode: focused-test.
  Test `list_live_file_locations_by_version_in_tx_reports_negative_id_as_database_error` in
  `identity_test.rs`; red: the assertion fails because the error is `VoomError::Internal`;
  green: `cargo test -p voom-store --lib list_live_file_locations_by_version_in_tx`.

Interfaces: consumes nothing; Task 2 relies on
`FileLocationRepo::list_live_file_locations_by_version_in_tx(&self, tx, FileVersionId) ->
Result<Vec<FileLocationId>, VoomError>` (signature unchanged).

Steps:
1. Append to `identity_test.rs` after `list_live_file_locations_by_version_in_tx_excludes_retired`:

```rust
#[tokio::test]
async fn list_live_file_locations_by_version_in_tx_reports_negative_id_as_database_error() {
    // A negative id can only come from corrupt storage; it must surface as a
    // database error, not an internal invariant failure (AGENTS.md).
    let (repo, _tmp) = fresh().await;
    let asset = repo.create_file_asset(T0).await.unwrap();
    let version = repo
        .create_file_version(NewFileVersion {
            file_asset_id: asset.id,
            content_hash: "neg".to_owned(),
            size_bytes: 1,
            produced_by: ProducedBy::Ingest,
            produced_from_version_id: None,
            created_at: T0,
        })
        .await
        .unwrap();
    let mut tx = repo.pool.begin().await.unwrap();
    let location = repo
        .create_file_location_in_tx(
            &mut tx,
            NewFileLocation {
                file_version_id: version.id,
                storage_root_id: crate::test_support::TEST_STORAGE_ROOT_ID,
                provider_relative_locator: crate::test_support::test_relative_locator(
                    "/srv/media/negative.mkv",
                ),
                proof: None,
                observed_at: T0,
            },
        )
        .await
        .unwrap();
    sqlx::query("UPDATE file_locations SET id = -7 WHERE id = ?")
        .bind(i64::try_from(location.id.0).unwrap())
        .execute(&mut *tx)
        .await
        .unwrap();

    let err = repo
        .list_live_file_locations_by_version_in_tx(&mut tx, version.id)
        .await
        .unwrap_err();

    assert!(matches!(err, VoomError::Database { .. }), "got {err:?}");
}
```

2. Run the green command; expect the new test to FAIL (`got Internal(...)`).
3. In `identity.rs` `list_live_file_locations_by_version_in_tx`, replace the `rows.into_iter()
   .map(|id| u64::try_from(id)...)` block with:

```rust
        rows.into_iter()
            .map(|id| u64_from_i64(id, "file_locations.id").map(FileLocationId))
            .collect()
```

4. Re-run; expect 3 passed. Commit `fix(store): classify negative in-tx location ids as database errors`.

## Task 2 — abort and re-prepare in one transaction

Verification:
- Contract: a failed re-prepare commits nothing and a later recovery succeeds. Mode:
  focused-test. `failed_reprepare_keeps_the_old_attempt_recoverable_until_the_default_returns`
  (`mod_test.rs`); red at base: `left: "aborted" right: "authorized"`.
- Contract: prepare's source selection sees the transaction's own staging retirement. Mode:
  focused-test. Same test's retry half and
  `recover_commit_aborts_receiptless_authorized_and_reprepares`; red if Task 2 step 4 is
  skipped: `CommitFailure` naming "multiple live rooted source locations".
- Contract: stale callers of the aborted intent cannot apply or complete over the successor.
  Mode: focused-test. `recovery_fences_the_aborted_intent_after_reprepare`; this passes before
  and after the change (it pins the ADR 0074 fence across the new path; record its pre-change
  pass as characterization, not red).
- Contract: the epoch compare-and-set still fails closed with no durable change, and an
  occupied target still fails without stranding. Mode: focused-test. Extended
  `recovery_abort_fails_closed_when_a_receipt_lands_after_classification` and
  `recover_commit_requires_operator_when_target_already_exists`; the latter is red at base
  (old intent `aborted`).
- Green command for all: `cargo test -p voom-control-plane --lib artifact::commit`; then
  `cargo test -p voom-control-plane --test recover_commit_gate --test staged_artifact_flow
  --test commit_use_lease_gate`.

Interfaces: consumes Task 1's list. Defines
`pub(crate) async fn select_location_in_tx(cp: &ControlPlane, tx: &mut
sqlx::Transaction<'_, sqlx::Sqlite>, file_version_id: FileVersionId) -> Result<FileLocation,
VoomError>` and makes `prepare::prepare_commit_in_tx(cp, tx, CommitArtifactInput, OffsetDateTime)
-> Result<PreparedCommit, PrepareCommitError>` `pub(super)`.

Steps:
1. Tests in `mod_test.rs`. Insert before `recovery_abort_fails_closed_when_a_receipt_lands_after_classification`
   the failure-then-retry test:

```rust
#[tokio::test]
async fn failed_reprepare_keeps_the_old_attempt_recoverable_until_the_default_returns() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");
    let task = spawn_commit_task(&cp, staged.artifact_handle_id, &target);
    let old_intent_id = wait_pending_intent_id(&cp, staged.artifact_handle_id).await;
    node_authorize(&cp, &node, old_intent_id).await.unwrap();
    task.abort();
    let old_record_id = latest_record_id(&cp, staged.artifact_handle_id).await;
    let old_staging_location_id = intent_staging_location_id(&cp, old_intent_id).await;

    // The operator cleared the staging default after prepare: the successor
    // cannot resolve its containment root (ADR 0097 fails closed). Recovery
    // must not durably abort the old attempt it cannot replace.
    clear_test_default_staging_root(&cp).await;
    let err = cp
        .recover_commit(staged.artifact_handle_id)
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::CommitFailure);
    assert_eq!(intent_state(&cp, old_intent_id).await, "authorized");
    assert_eq!(record_state(&cp, old_record_id).await, "pending");
    assert!(!file_location_retired(&cp, old_staging_location_id).await);
    assert_eq!(
        count_commit_records(&cp, staged.artifact_handle_id).await,
        1
    );

    // Once the operator restores the default, the same recovery succeeds.
    set_test_default_staging_root(&cp, voom_store::test_support::TEST_STORAGE_ROOT_ID).await;
    let report = cp.recover_commit(staged.artifact_handle_id).await.unwrap();

    assert_eq!(report.state, ArtifactCommitState::Pending);
    assert_ne!(report.commit_record_id, old_record_id);
    assert_eq!(intent_state(&cp, old_intent_id).await, "aborted");
    assert_eq!(record_state(&cp, old_record_id).await, "failed");
    assert!(file_location_retired(&cp, old_staging_location_id).await);
    assert!(!target.exists());
}
```

   Insert these helpers immediately before `fn artifact_tempdir()`:

```rust
async fn clear_test_default_staging_root(cp: &ControlPlane) {
    sqlx::query("UPDATE library_roots SET default_staging_root_id = NULL WHERE id = 9000001")
        .execute(cp.pool_for_test())
        .await
        .unwrap();
}

async fn intent_staging_location_id(
    cp: &ControlPlane,
    intent_id: ArtifactCommitIntentId,
) -> voom_core::FileLocationId {
    let id: i64 =
        sqlx::query_scalar("SELECT staging_location_id FROM artifact_commit_intents WHERE id = ?")
            .bind(i64::try_from(intent_id.0).unwrap())
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    voom_core::FileLocationId(u64::try_from(id).unwrap())
}

async fn file_location_retired(cp: &ControlPlane, id: voom_core::FileLocationId) -> bool {
    let retired_at: Option<String> =
        sqlx::query_scalar("SELECT retired_at FROM file_locations WHERE id = ?")
            .bind(i64::try_from(id.0).unwrap())
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    retired_at.is_some()
}
```

   Add the fencing test after the failure-then-retry test:

```rust
#[tokio::test]
async fn recovery_fences_the_aborted_intent_after_reprepare() {
    let (cp, _db, dir) = fixture().await;
    let node = simulated_node(&cp).await;
    let staged = stage_and_verify_bytes(&cp, dir.path(), b"source bytes").await;
    let target = dir.path().join("target.bin");
    let task = spawn_commit_task(&cp, staged.artifact_handle_id, &target);
    let old_intent_id = wait_pending_intent_id(&cp, staged.artifact_handle_id).await;
    let authorized = node_authorize(&cp, &node, old_intent_id).await.unwrap();
    task.abort();

    let report = cp.recover_commit(staged.artifact_handle_id).await.unwrap();
    let new_intent_id: i64 =
        sqlx::query_scalar("SELECT id FROM artifact_commit_intents WHERE commit_record_id = ?")
            .bind(i64::try_from(report.commit_record_id.0).unwrap())
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    let new_intent_id = ArtifactCommitIntentId(u64::try_from(new_intent_id).unwrap());

    // A node still holding the aborted intent's fence cannot journal,
    // report, or complete over the successor generation.
    let applying = node_report_applying(&cp, &node, old_intent_id).await.unwrap_err();
    assert_eq!(applying.error_code(), ErrorCode::Conflict);
    let complete = node_complete(&cp, &node, old_intent_id, &authorized.fence_hex)
        .await
        .unwrap_err();
    assert_eq!(complete.error_code(), ErrorCode::Conflict);
    assert_eq!(intent_state(&cp, old_intent_id).await, "aborted");
    assert_eq!(intent_state(&cp, new_intent_id).await, "pending");
    assert!(!target.exists());
}
```

   If either rejection returns a code other than `Conflict`, assert the code the fence
   actually returns and note it in the commit message — the contract is rejection, not the code.

2. Extend `recovery_abort_fails_closed_when_a_receipt_lands_after_classification` after its
   message assertion:

```rust
    assert_eq!(record_state(&cp, record.id).await, "pending");
    assert_eq!(
        count_commit_records(&cp, staged.artifact_handle_id).await,
        1
    );
```

   Extend `recover_commit_requires_operator_when_target_already_exists`: capture
   `old_intent_id` (spawn/wait/authorize inline as in step 1), then after the existing asserts:

```rust
    assert_eq!(intent_state(&cp, old_intent_id).await, "authorized");
    std::fs::remove_file(&target).unwrap();
    let report = cp.recover_commit(staged.artifact_handle_id).await.unwrap();
    assert_eq!(report.state, ArtifactCommitState::Pending);
```

3. Run the green command; expect the failure-then-retry and occupied-target tests to FAIL.
4. `operation_source.rs`: replace the `match rooted_locations.as_slice()` in `select_location`
   with `single_live_rooted(file_version_id, rooted_locations)` and add:

```rust
/// [`select_location`] on the caller's transaction, without an explicit
/// location id. Commit preparation needs it: inside commit recovery the same
/// transaction has just retired the aborted attempt's staging location, and a
/// pool read cannot see that uncommitted retirement.
pub(crate) async fn select_location_in_tx(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    file_version_id: FileVersionId,
) -> Result<FileLocation, VoomError> {
    let mut rooted_locations = Vec::new();
    for id in cp
        .identity
        .list_live_file_locations_by_version_in_tx(tx, file_version_id)
        .await?
    {
        let location = cp
            .identity
            .get_file_location_in_tx(tx, id)
            .await?
            .ok_or_else(|| {
                VoomError::database(format!("live file_location {id} vanished in its transaction"))
            })?;
        if matches!(location.address, FileLocationAddress::Rooted { .. }) {
            rooted_locations.push(location);
        }
    }
    single_live_rooted(file_version_id, rooted_locations)
}

fn single_live_rooted(
    file_version_id: FileVersionId,
    rooted_locations: Vec<FileLocation>,
) -> Result<FileLocation, VoomError> {
    let mut locations = rooted_locations.into_iter();
    match (locations.next(), locations.next()) {
        (Some(location), None) => Ok(location),
        (None, _) => Err(VoomError::Config(format!(
            "file_version {file_version_id} has no live rooted source locations"
        ))),
        (Some(_), Some(_)) => Err(VoomError::Config(format!(
            "file_version {file_version_id} has multiple live rooted source locations"
        ))),
    }
}
```

5. `prepare.rs`: make `prepare_commit_in_tx` `pub(super)`; replace the
   `crate::operation_source::select_location(cp, inputs.source.source_file_version_id, None)`
   call with `crate::operation_source::select_location_in_tx(cp, tx,
   inputs.source.source_file_version_id)`.
6. `recovery.rs` `abort_and_reprepare_report`: delete the `commit_tx(tx).await?;` that follows
   the retirement and the whole `prepare_commit(...)` call, and in their place write:

```rust
    // The successor prepares on the same transaction: if it fails, dropping
    // the transaction rolls the abort back and the classified attempt stays
    // recoverable (#665).
    let prepared = prepare_commit_in_tx(
        cp,
        &mut tx,
        CommitArtifactInput {
            artifact_handle_id: record.artifact_handle_id,
            target_path: std::path::PathBuf::from(&record.target_path),
        },
        now,
    )
    .await
    .map_err(|error| match error {
        PrepareCommitError::PreMutation(report) => VoomError::CommitFailure(report.message),
        PrepareCommitError::AfterPending(error) => error,
    })?;
    commit_tx(tx).await?;
```

   Change the import to `use crate::artifact::commit::prepare::{PrepareCommitError,
   evaluate_commit_safety_gate, prepare_commit_in_tx};` and update the doc comment of
   `abort_and_reprepare_report` to say the abort and the successor's prepare commit together.
7. Run both green commands; expect all pass. Run `just lint` and `just fmt-check`; expect exit 0.
8. Commit `fix(control-plane): abort and re-prepare commit recovery atomically`.

Cleanup: tests use the existing `fixture()` temp database and directory guards; no extra state.

## Final

Run `just ci` in the foreground with a raised timeout; expect exit 0.
