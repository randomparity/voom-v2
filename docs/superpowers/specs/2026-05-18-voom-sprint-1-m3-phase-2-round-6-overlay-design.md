---
title: VOOM Sprint 1 M3 Phase 2 — Codex Round-6 Overlay Design
status: draft
created: 2026-05-18
parent_spec: docs/superpowers/specs/2026-05-18-voom-sprint-1-m3-phase-2-plan.md
parent_sections: §3 decision table, §4 commit 3, §9 follow-ups
arch_spec: docs/specs/voom-control-plane-design.md (Commit Safety Gate)
sprint_spec: docs/superpowers/specs/2026-05-16-voom-sprint-1-design.md
branch: feat/sprint-1-m3-phase-2
scope: one fix(store) commit overlay addressing three Codex round-6 findings on the M3 Phase 2 scaffold
---

# VOOM Sprint 1 M3 Phase 2 — Codex Round-6 Overlay Design

## 1. Purpose

Codex round-6 adversarial review of the branch through commit `edba8e4`
returned verdict `needs-attention` with two **high** and one **medium**
finding. This spec captures the design choices that fold all three
findings into a single `fix(store):` commit on top of `edba8e4`,
mirroring the round-5 precedent (`ae76b22`).

The findings, in Codex's words:

1. **[high] Replacement can move a location onto the wrong file
   version** (`crates/voom-store/src/repo/identity.rs:1370-1378`).
   `replace_file_location_in_tx` retires the old row and inserts the
   replacement under `new_location.file_version_id` without checking
   it against the retired row's current version. The "Phase C is the
   sole caller" assumption is not enforceable: the method is a public
   `IdentityRepo` trait method, so any future caller — including a
   buggy Phase C — can retire a row under version A and create its
   replacement under version B.

2. **[high] Failed insert can leave the old location retired**
   (`crates/voom-store/src/repo/identity.rs:1355-1378`). The method
   runs UPDATE retire then INSERT new in the caller's outer
   transaction. If the INSERT fails and the caller catches `Err` to
   record recovery state then commits the outer tx, data loss: old
   row retired, no replacement.

3. **[medium] Authorized commit rows can be missing their authorized
   closure** (`migrations/0005_commit_intents_persistent_permit.sql:53-58`).
   The CHECK branches for `authorized`, `completed`, and
   `recovery_required` require `target_row_epochs IS NOT NULL` but
   leave `closure_authorized` nullable. A row reaching post-Phase-B
   states can satisfy the constraint but lack the closure needed for
   crash recovery, list, or finalize inspection.

Intended outcome: the scaffold passes a re-run of
`/codex:adversarial-review --base main` cleanly before Phase C code
(commit 7) lands on top of it.

## 2. Decision summary

Captured here so the per-file changes below are unambiguous:

| Decision | Choice | Rationale |
|---|---|---|
| `replace_file_location_in_tx` cross-version guard | Enforced inside identity (defense-in-depth) | The previous design pinned a "trusts caller" stance via test `replace_file_location_trusts_caller_supplied_version_id_by_design` (`edba8e4`), with the cross-version invariant living only at the gate boundary on `FileLocationProposal`. Codex round-6 finding #1: the method is a public `IdentityRepo` trait method, so the "Phase C is the sole caller" assumption is not enforceable by the type system. Pre-fetch retired row in-tx, reject mismatch with `VoomError::Conflict` before the retire UPDATE runs. The gate-boundary type-level invariant (`FileLocationProposal` has no `file_version_id`) still holds — this is the inner ring. |
| Pre-check vs `UPDATE ... RETURNING` | Separate SELECT before SAVEPOINT | Codex offered both. Separate SELECT (`get_file_location_in_tx`) keeps the row-not-found and version-mismatch error messages distinct, at the cost of one extra round-trip. The TOCTOU window between the SELECT and the UPDATE is closed by the existing epoch guard on the UPDATE (`rows_affected != 1` → `Conflict`). |
| `replace_file_location_in_tx` retire+insert atomicity | SAVEPOINT (`tx.begin()` → nested) wraps the pair | Codex round-6 finding #2: previously the method ran UPDATE retire then INSERT new in the caller's outer tx. SAVEPOINT wraps the pair; ROLLBACK TO on any insert error restores the outer tx to pre-UPDATE state, so a caller that commits the outer tx after the inner failure sees the old row still live. |
| `commit_intents` post-Phase-B CHECK | Require `closure_authorized IS NOT NULL` for `authorized` / `completed` / `recovery_required` | Codex round-6 finding #3. Closes the schema-level gap at migration 0005 rather than relying on Phase B's UPDATE to populate both columns together. The `aborted` branch is deliberately left as-is because aborted-from-pending has neither column set; aborted-from-trip-wire has mixed shape depending on which wire fires. |
| Migration 0005 amendment | Edit in place | The migration is pre-release, on this feature branch, with no rows in any environment (the migration itself does drop-and-recreate against migration 0004's empty tables). Round-4's own commit message established the in-place amendment precedent. New 0006 migration would double the ledger for the same feature with no operational benefit. |
| Forced-insert-failure test mechanism | Temporary `BEFORE INSERT` trigger with `RAISE(ABORT)` keyed off a sentinel `value` | The schema has no UNIQUE constraints on `file_locations` and the FK on `file_version_id` is satisfied by the new round-6 #1 guard, so deterministically forcing the INSERT to fail through the existing API surface is not possible without a temporary trigger. The trigger is installed at test start and dropped at the end; uses real SQLite plumbing rather than mocking sqlx internals. |
| Commit shape | One `fix(store):` commit on top of `edba8e4` | Matches the round-2/3 (`1f6f474`), round-4 (`90fd5fa`/`ddf85ba`/`df6fcfa`), and round-5 (`ae76b22`) precedents — each adversarial round folds into a single overlay commit on the active feature branch. |

## 3. File inventory

Three tracked files; two doc updates.

| File | Change |
|---|---|
| `migrations/0005_commit_intents_persistent_permit.sql` | Tighten three CHECK branches (`authorized`, `completed`, `recovery_required`) to require `closure_authorized IS NOT NULL`. Update the header comment to reference round-6. |
| `crates/voom-store/src/repo/identity.rs` | Rewrite `replace_file_location_in_tx` impl: pre-fetch retired row, version-check, SAVEPOINT around UPDATE retire + INSERT new, RELEASE on success. Trait declaration doc-comment updated to describe the two new defense-in-depth guards. |
| `crates/voom-store/src/repo/identity_test.rs` | (a) Invert `replace_file_location_trusts_caller_supplied_version_id_by_design` → `replace_file_location_rejects_cross_version_supply`. (b) Add `replace_file_location_savepoint_rolls_back_on_insert_failure` using a temporary trigger. |
| `crates/voom-store/src/repo/schema_meta_test.rs` (or sibling, per implementation plan) | Add schema-level negative-coverage tests for the tightened CHECK. The implementation plan picks the exact file based on existing coverage. |
| `docs/superpowers/specs/2026-05-18-voom-sprint-1-m3-phase-2-plan.md` (tracked) | §3 append three round-6 decision rows. §4 commit 3 rewrite the `replace_file_location_in_tx` bullet + sibling-test bullet. §9 append round-6 follow-up. |
| `docs/superpowers/plans/2026-05-18-m3-phase-2-commit-3.md` (gitignored working plan) | Task 6 Step 4 description flipped. Task 6 test count updated 4→5. Task 8 Step 2 commit-message body updated. |

## 4. `replace_file_location_in_tx` redesign

### 4.1 New impl body sketch

```rust
async fn replace_file_location_in_tx<'tx>(
    &self,
    tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
    retired_id: FileLocationId,
    retired_expected_epoch: u64,
    new_location: NewFileLocation,
    retired_at: OffsetDateTime,
) -> Result<FileLocationId, VoomError> {
    // 1. Pre-check (read against outer tx): retired row must exist,
    //    be live, and live on the same FileVersion as new_location.
    //    Without this guard, a buggy caller could retire a row on
    //    version A and insert its replacement under version B —
    //    poisoning closure/evidence decisions for both versions
    //    (Codex round-6 finding #1).
    let retired_row = get_file_location_in_tx(tx, retired_id)
        .await?
        .ok_or_else(|| {
            VoomError::Conflict(format!("file_locations replace: id={retired_id} not found"))
        })?;
    if retired_row.retired_at.is_some() {
        return Err(VoomError::Conflict(format!(
            "file_locations replace: id={retired_id} already retired"
        )));
    }
    if retired_row.file_version_id != new_location.file_version_id {
        return Err(VoomError::Conflict(format!(
            "file_locations replace: id={retired_id} on version={} but \
             new_location targets version={}",
            retired_row.file_version_id, new_location.file_version_id
        )));
    }

    // 2. SAVEPOINT around UPDATE retire + INSERT new. ROLLBACK TO on
    //    any inner Err restores the outer tx to pre-UPDATE state, so a
    //    caller that commits the outer tx after the inner failure
    //    observes the old row still live (Codex round-6 finding #2).
    let mut sp = tx.begin().await.map_err(|e| {
        VoomError::Database(format!("file_locations replace savepoint begin: {e}"))
    })?;

    let ts = iso8601(retired_at)?;
    let res = sqlx::query(
        "UPDATE file_locations SET retired_at = ?, epoch = epoch + 1 \
         WHERE id = ? AND epoch = ? AND retired_at IS NULL",
    )
    .bind(&ts)
    .bind(i64_from_u64(retired_id.0))
    .bind(i64_from_u64(retired_expected_epoch))
    .execute(&mut *sp)
    .await
    .map_err(|e| VoomError::Database(format!("file_locations replace-retire: {e}")))?;
    if res.rows_affected() != 1 {
        // Dropping sp here ROLLBACKs TO the savepoint; outer tx restored.
        return Err(VoomError::Conflict(format!(
            "file_locations replace: id={retired_id} expected_epoch={retired_expected_epoch} \
             stale or row already retired"
        )));
    }

    let new_id = insert_file_location(
        &mut sp,
        new_location.file_version_id,
        new_location.kind,
        &new_location.value,
        new_location.proof.as_ref(),
        new_location.observed_at,
    )
    .await?; // ← if Err, sp dropped, outer tx restored, return Err.

    sp.commit().await.map_err(|e| {
        VoomError::Database(format!("file_locations replace savepoint release: {e}"))
    })?;
    Ok(new_id)
}
```

### 4.2 Trait doc-comment

Replaces the existing four-paragraph doc-comment on the trait method.
The old "method does not defensively re-check, by design" sentence is
removed; the new wording names both defense-in-depth guards:

```rust
/// Atomically retire `retired_id` under its `expected_epoch` guard
/// and insert a new `FileLocation` on the same `FileVersion`. Two
/// defense-in-depth guards layered inside this method (Codex
/// round-6):
///
/// 1. **Cross-version pre-check.** The retired row is fetched in
///    the caller's transaction and rejected with `VoomError::Conflict`
///    if `new_location.file_version_id` differs from the retired
///    row's version. The gate boundary (`FileLocationProposal` has
///    no `file_version_id`) is the outer ring; this is the inner
///    ring that catches the case where a buggy caller bypasses the
///    proposal conversion.
///
/// 2. **SAVEPOINT atomicity.** The retire UPDATE and the insert
///    INSERT run inside a SAVEPOINT. On any insert failure, the
///    savepoint rolls back; a caller that catches the `Err` and
///    commits the outer transaction observes the old row still live.
///    Without the savepoint, the caller would have to drop the outer
///    transaction to avoid data loss.
///
/// `Conflict` is returned on: row not found, row already retired,
/// stale `expected_epoch` on a live row, or cross-version mismatch.
/// In every `Conflict` case the old row stays live; in every
/// insert-failure case the savepoint guarantees the same.
```

## 5. Migration 0005 CHECK tightening

### 5.1 Diff

```diff
     CHECK (
            (state = 'pending'           AND authorized_at IS NULL     AND finalized_at IS NULL     AND aborted_at IS NULL     AND abort_reason IS NULL     AND recovery_reason IS NULL     AND target_row_epochs IS NULL)
-        OR (state = 'authorized'        AND authorized_at IS NOT NULL AND finalized_at IS NULL     AND aborted_at IS NULL     AND abort_reason IS NULL     AND recovery_reason IS NULL     AND target_row_epochs IS NOT NULL)
-        OR (state = 'completed'         AND authorized_at IS NOT NULL AND finalized_at IS NOT NULL AND aborted_at IS NULL     AND abort_reason IS NULL     AND recovery_reason IS NULL     AND target_row_epochs IS NOT NULL)
+        OR (state = 'authorized'        AND authorized_at IS NOT NULL AND finalized_at IS NULL     AND aborted_at IS NULL     AND abort_reason IS NULL     AND recovery_reason IS NULL     AND target_row_epochs IS NOT NULL AND closure_authorized IS NOT NULL)
+        OR (state = 'completed'         AND authorized_at IS NOT NULL AND finalized_at IS NOT NULL AND aborted_at IS NULL     AND abort_reason IS NULL     AND recovery_reason IS NULL     AND target_row_epochs IS NOT NULL AND closure_authorized IS NOT NULL)
         OR (state = 'aborted'           AND finalized_at IS NULL      AND aborted_at IS NOT NULL   AND abort_reason IS NOT NULL AND recovery_reason IS NULL)
-        OR (state = 'recovery_required' AND authorized_at IS NOT NULL AND finalized_at IS NULL     AND aborted_at IS NULL     AND abort_reason IS NULL     AND recovery_reason IS NOT NULL AND target_row_epochs IS NOT NULL)
+        OR (state = 'recovery_required' AND authorized_at IS NOT NULL AND finalized_at IS NULL     AND aborted_at IS NULL     AND abort_reason IS NULL     AND recovery_reason IS NOT NULL AND target_row_epochs IS NOT NULL AND closure_authorized IS NOT NULL)
     ),
```

### 5.2 `aborted` branch deliberately not tightened

- `aborted-from-pending` → `target_row_epochs IS NULL` and
  `closure_authorized IS NULL` (prepare never had a snapshot).
- `aborted-from-trip-wire` (per Phase 2 plan §4 commit 6) →
  `closure_authorized` is set; `target_row_epochs` may or may not
  be set depending on which trip-wire fired.

Requiring either column on `aborted` would reject legitimate rows.
The `abort_reason IS NOT NULL` and `recovery_reason IS NULL`
fragments already pin the meaningful invariant.

### 5.3 Header comment

Append one paragraph at the end of the existing round-4 rationale
block:

> Codex round-6 review (post commit `edba8e4`) tightened the CHECK
> further: the three post-Phase-B states (`authorized`, `completed`,
> `recovery_required`) now require `closure_authorized IS NOT NULL`.
> Before the tightening, a row could satisfy the constraint with the
> closure column unset, defeating crash recovery / list / finalize
> inspection. The `aborted` branch is deliberately untouched —
> aborted-from-pending has no closure; aborted-from-trip-wire has
> mixed shape depending on which wire fired.

## 6. Test plan

### 6.1 Invert `replace_file_location_trusts_caller_supplied_version_id_by_design`

Same slot in `identity_test.rs`. Rename → `replace_file_location_rejects_cross_version_supply`.
New body asserts:

- `replace_file_location_in_tx` called with mismatched version returns
  `VoomError::Conflict`.
- The retired row is unchanged (`retired_at.is_none()`, `epoch == 0`).
- The new row is never inserted (verified via
  `list_file_locations_by_version` before/after counts).

The header comment explains this is the inner-ring defense-in-depth
test and references both round-2 (gate-boundary type-level invariant)
and round-6 (identity-level enforcement).

### 6.2 Add `replace_file_location_savepoint_rolls_back_on_insert_failure`

New test. Uses a temporary `BEFORE INSERT` trigger to deterministically
fail the INSERT after the version-check passes:

```sql
CREATE TRIGGER force_replace_insert_failure
BEFORE INSERT ON file_locations
WHEN NEW.value = '__force_failure_marker__'
BEGIN SELECT RAISE(ABORT, 'forced for atomicity test'); END
```

The test then:

1. Calls `replace_file_location_in_tx` with `value =
   '__force_failure_marker__'`. The pre-check passes (matching
   version). The retire UPDATE runs on the savepoint. The INSERT
   trips the trigger and Errs. The savepoint rolls back.
2. **Commits the outer tx anyway** — this is the load-bearing
   assertion Codex called out.
3. In a fresh read, confirms the retired row is still live
   (`retired_at.is_none()`, `epoch == 0`).
4. Drops the trigger.

### 6.3 No change to existing `_no_insert` tests

The two `replace_file_location_*_is_conflict_and_no_insert` tests
from `edba8e4` continue to pass under the new shape:

- The already-terminal case is caught by the new pre-check (returns
  `Conflict` from `retired_row.retired_at.is_some()`), earlier than
  the UPDATE. Outcome unchanged.
- The stale-epoch case is caught by the UPDATE's `rows_affected != 1`
  branch inside the savepoint. Savepoint drops, outer state restored.
  Outcome unchanged.

If they break under `just ci`, that's a code bug to fix, not a design
change.

### 6.4 Schema-CHECK negative coverage

Add either three new tests or one parametrized test asserting that
SQLite rejects an INSERT into `commit_intents` with:

- `state = 'authorized'`, `authorized_at` set, `target_row_epochs`
  set, `closure_authorized = NULL`.
- Same for `state = 'completed'`.
- Same for `state = 'recovery_required'`.

If existing schema-level coverage for the `commit_intents` CHECK
exists in `schema_meta_test.rs` or a sibling, extend it; otherwise
add. The implementation plan decides where to land them.

### 6.5 Test count delta

- 4 existing `replace_*` tests from `edba8e4` → 3 remain unchanged +
  1 inverted = 4.
- + 1 new (`savepoint_rolls_back_on_insert_failure`) = 5.
- + N schema CHECK tests (1–3 depending on parametrization).

`voom-store --lib` total: **184 + N** (vs. `edba8e4` baseline 183).

## 7. Verification

1. **Build green.** `cargo build --workspace --all-features`.
2. **Targeted tests.** `cargo test -p voom-store --lib replace_file_location_`
   → **5 passed**.
3. **Schema tests.** `cargo test -p voom-store --lib commit_intents_check`
   (or equivalent) → all new negative-coverage tests pass.
4. **Full CI.** `just ci` → `==> All CI checks passed`.
5. **Migration applies.** `just smoke` exercises the amended 0005
   CHECK against a fresh DB.
6. **Adversarial recheck.** Re-run `/codex:adversarial-review --base main`.
   Expected verdict: `looks-good`, or remaining findings unrelated
   to round-6.

## 8. Out of scope

Deliberately not changed by this overlay:

- **Phase C dispatch wiring** (commit 7 of the M3 sequence). The
  round-6 changes harden the `replace_file_location_in_tx` primitive;
  the gate's `FileLocationProposal → NewFileLocation` conversion
  still belongs to commit 7.
- **`retire_file_location_in_tx` / `retire_file_version_in_tx`.**
  Single-UPDATE shape; no savepoint needed.
- **`closure_initial` nullability.** Round-6 finding is post-Phase-B
  only.
- **`commit_intents.target_row_epochs` nullability for `aborted`
  rows.** Round-4 design intent preserved.
- **Public API surface.** No new trait methods, no new types. The
  trait signature of `replace_file_location_in_tx` is unchanged;
  only the impl body and doc-comment change.
- **The two existing `_no_insert` sibling tests** from `edba8e4`.
  Their contracts hold under the new shape; no edits planned.

## 9. Commit message ref block

```
Refs:
- docs/superpowers/specs/2026-05-18-voom-sprint-1-m3-phase-2-round-6-overlay-design.md
  (this design doc).
- docs/superpowers/specs/2026-05-18-voom-sprint-1-m3-phase-2-plan.md
  §3 (three new round-6 decision rows), §4 commit 3 patched,
  §9 round-6 follow-up.
- edba8e4 (M3 P2 #3) — original replace_file_location_in_tx + tests
  this commit overlays.
- ae76b22 — round-5 overlay (DeleteFileVersion deferred, in-tx alias
  enumeration).
- 90fd5fa — round-4 overlay (migration 0005 introduced).
- Codex round-6 review (verdict needs-attention; 2 high + 1 medium).
```

## 10. Open follow-ups

None. All three Codex findings resolved by this overlay; no new
design tradeoffs surfaced.
