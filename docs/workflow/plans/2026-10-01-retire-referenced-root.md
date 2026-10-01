# Plan: refuse retiring a referenced storage root (#626)

Goal: `retire_library_root_in_tx` refuses a root that another non-retired root names as a
default, per `docs/workflow/specs/2026-10-01-retire-referenced-root-design.md`.
Architecture: one private referential check in `voom-store`'s `library_roots.rs`, called by
retire inside its existing transaction; `transition_state` (`mark unavailable`) is untouched.
Tech stack: Rust, sqlx/SQLite, tokio tests.

Expected implementation size: 90–130 changed lines (S) — one helper (~35), one call, two
tests (~75), ADR append (~15).

## Global Constraints

- Sibling unit tests only (`library_roots_test.rs`); `just check-test-layout` enforces it.
- Production code: no `unwrap`/`expect`/`panic` (workspace lints). Persisted ids are converted
  with `u64_from_i64`; row decode errors map through `map_row_err`.
- Error variant `VoomError::Conflict` (existing code `CONFLICT`); no new error code.
- ADR 0097 is merged and append-only: add a section at the end, edit nothing above it.
- Guardrails: `just fmt-check`, `just lint`, `just test`, `just ci`.

## Task 1 — referential guard on retire

Files: modify `crates/voom-store/src/repo/library/library_roots.rs`; test
`crates/voom-store/src/repo/library/library_roots_test.rs`.
Interfaces: consumes `required_root_in_tx`, `root_i64`, `u64_from_i64`, `map_row_err`
(all existing in that module/imports). Adds private
`async fn require_no_live_default_references(tx: &mut Transaction<'_, Sqlite>, id: StorageRootId) -> Result<(), VoomError>`.

Verification: the spec's Validation entries for criteria 1–4 (two `focused-test` cases).

Steps:
1. Add to the test file a helper and the two tests:

```rust
async fn retire(
    repo: &SqliteLibraryRepo,
    id: StorageRootId,
    now: OffsetDateTime,
) -> Result<LibraryRoot, VoomError> {
    let mut tx = crate::tx::begin_read_then_write(&repo.pool, "test: retire").await.unwrap();
    let result = repo.retire_library_root_in_tx(&mut tx, id, now).await;
    match &result {
        Ok(_) => commit(tx).await.unwrap(),
        Err(_) => rollback(tx).await.unwrap(),
    }
    result
}
```

   The refusal test creates `target` (`/staging`), `staging_user` (staging default = target),
   `output_user` (output and backup defaults = target), and `retired_user` (output default =
   target, then retired via `retire`); then sets target's own staging default to itself.
   It asserts `retire(target)` is `VoomError::Conflict` whose message equals exactly the
   spec's Decision 1 text (referencers in id order, retired and self references absent), and that target's persisted state is still `Configured`. It then repoints
   `staging_user`'s staging default to itself, clears both `output_user` defaults
   (`Some(None)`), and asserts `retire(target)` returns state `Retired`.
   The policy test creates `target` and a referencer (staging = target), then in one
   `begin_read_then_write` transaction calls `activate_library_root_in_tx(target,
   "volume-identity")` and `mark_library_root_unavailable_in_tx(target)`, commits, and
   asserts state `Unavailable`.
2. Run `cargo test -p voom-store --lib retire_refuses_a_root` — expect FAIL
   (`called Result::unwrap_err() on an Ok value`).
3. Add the helper below `transition_state` and call it in `retire_library_root_in_tx`
   immediately after the already-retired check:

```rust
async fn require_no_live_default_references(
    tx: &mut Transaction<'_, Sqlite>,
    id: StorageRootId,
) -> Result<(), VoomError> {
    let rows = sqlx::query(
        "SELECT id, default_output_root_id IS ?1 AS output_ref, \
                default_staging_root_id IS ?1 AS staging_ref, \
                default_backup_root_id IS ?1 AS backup_ref \
         FROM library_roots WHERE id != ?1 AND state != 'retired' \
           AND ?1 IN (default_output_root_id, default_staging_root_id, default_backup_root_id) \
         ORDER BY id",
    )
    .bind(root_i64(id)?)
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| VoomError::database_context("library_roots default references", error))?;
    if rows.is_empty() {
        return Ok(());
    }
    let mut referencing = Vec::with_capacity(rows.len());
    for row in &rows {
        let raw: i64 = row.try_get("id").map_err(|error| map_row_err("library_roots", error))?;
        let mut columns = Vec::new();
        for (alias, column) in [
            ("output_ref", "default_output_root_id"),
            ("staging_ref", "default_staging_root_id"),
            ("backup_ref", "default_backup_root_id"),
        ] {
            let hit: bool = row.try_get(alias).map_err(|error| map_row_err("library_roots", error))?;
            if hit {
                columns.push(column);
            }
        }
        let root = StorageRootId(u64_from_i64(raw, "library_roots.id")?);
        referencing.push(format!("root {root} ({})", columns.join(", ")));
    }
    Err(VoomError::Conflict(format!(
        "storage root {id} cannot retire while other roots name it as a default: {}; \
         repoint those defaults or retire the referencing roots first",
        referencing.join(", ")
    )))
}
```

4. Run both focused commands — expect PASS. Run `cargo test -p voom-store -p
   voom-control-plane -p voom-cli`; any existing test retiring a still-referenced root is a
   behaviour change to report, not to weaken the guard for.
5. `just fmt-check && just lint`; commit `fix(store): refuse retiring a root another root names as a default`.

## Task 2 — ADR 0097 later decision

File: append to `docs/adr/0097-pre-promotion-addresses-contained-by-their-own-root.md`
a `## Later decision: retirement refuses a referenced default` section: issue #626 narrows the
Consequences residual "a staging root can be retired while it is another root's staging
default" — repointing the default and then retiring can still strand a committed-but-unpromoted
artifact, which stays unowned; retirement now refuses while any other non-retired root names the root in any
default column; `active -> unavailable` is deliberately unguarded (ADR 0055 validation-loss
fact, reversible); the rest of the record stands; link the spec.
Verification: `task-test-not-applicable` — prose; run `just check-adr-index`.
Commit `docs(adr): record that #626 narrows 0097's retirement residual`.
