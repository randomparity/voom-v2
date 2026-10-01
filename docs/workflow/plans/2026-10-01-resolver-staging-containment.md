# Plan — ADR 0097 containment in the commit/promotion target resolver

Goal: commit targets are contained by the staging root, promotion targets by the artifact
root's own output default, and the source-root fallback is gone (fail closed).
Architecture: `operation_source.rs` gains two entry points sharing the existing root checks;
`prepare.rs` and `promotion.rs` each call theirs; fixtures name the defaults they need.
Spec: `docs/workflow/specs/2026-10-01-resolver-staging-containment-design.md`.
Stack: Rust 2024, tokio, sqlx/SQLite; tests via `cargo test`.

Expected implementation size: 500–530 changed lines (M) — two resolver entry points (~115),
six focused tests with their root-building helpers (~300), one test-support helper (~21) and
~15 fixture call-site edits (~70). Corrected after the build: the first estimate (220–300)
predated the design review's chained-phase test and the scope audit's coordinator promotion
test, and undercounted the per-test root setup (struct literals, sibling directories).

## Global Constraints

- ADR 0050: no new control-plane filesystem capability; this edits resolution on an existing
  path only. ADR 0097 governs; no new ADR.
- Errors fail before any durable mutation; `require_contained` is unchanged.
- Out of scope: #616, #618, #623 (chaos harness files untouched; only the shared support helper
  `crates/voom-cli/tests/support/voom_cli.rs` changes), #484.
- Guardrails: `just fmt-check`, `just lint`, `just test`, `just ci` (exit 0).

## File map

| File | Change |
|---|---|
| `crates/voom-store/src/test_support.rs` | add `set_test_storage_root_self_defaults` |
| `crates/voom-control-plane/src/operation_source.rs` | replace `resolve_artifact_target`/`artifact_target_root` with `resolve_pre_promotion_target`, `resolve_output_target`, `resolve_target_in_root`, `library_root` |
| `crates/voom-control-plane/src/operation_source_test.rs` | port two tests; add five |
| `crates/voom-control-plane/src/artifact/commit/prepare.rs` | call `resolve_pre_promotion_target` |
| `crates/voom-control-plane/src/workflow/coordinator/promotion.rs` | call `resolve_output_target` |
| `crates/voom-control-plane/src/workflow/plan/envelope.rs` | actionable `destination_root` message |
| `crates/voom-control-plane/src/workflow/plan/binding.rs` | drop `expect(dead_code)` on `DestinationRole::Output` |
| `docs/adr/0097-pre-promotion-addresses-contained-by-their-own-root.md` | append `## Later decision: commit resolves the staging root` |
| fixtures (measured: 60 failing tests in 9 suites) | `voom-api/src/commit_test.rs`; `voom-control-plane/src/artifact/commit/mod_test.rs` (`fixture()`, and `set_test_default_output_root` → `set_test_default_staging_root`); `artifact/inspect_test.rs`; `workflow/coordinator/mod_test.rs`; `voom-control-plane/tests/{commit_use_lease_gate,recover_commit_gate,staged_artifact_flow}.rs`; `voom-cli/tests/{artifact_envelope,lease_commit_gate_e2e,operator_execution_e2e}.rs`; `voom-cli/tests/support/{published_grammar_execution,voom_cli}.rs` |

## Task 1 — test-root defaults (green on the old resolver)

Verification: `Mode: task-test-not-applicable` — fixture configuration only; the old resolver
already resolves a self output default to the same root, so behavior is unchanged and the
proof is Task 2's suite run.

1. Add to `crates/voom-store/src/test_support.rs` after `set_test_storage_root_path`:

```rust
/// Make the shared test root its own staging and output default — the pairing
/// ADR 0097 requires before a commit (staging) or promotion (output) resolves.
pub async fn set_test_storage_root_self_defaults(pool: &SqlitePool) -> Result<(), VoomError> {
    sqlx::query(
        "UPDATE library_roots SET default_staging_root_id = id, default_output_root_id = id \
         WHERE id = ?",
    )
    .bind(i64::try_from(TEST_STORAGE_ROOT_ID.0).map_err(|e| VoomError::Internal(e.to_string()))?)
    .execute(pool)
    .await
    .map_err(|error| VoomError::database_context("set test storage-root defaults", error))?;
    Ok(())
}
```

2. Call it after the shared root is seeded/pointed in: `voom-api/src/commit_test.rs` fixture,
   commit `mod_test.rs` `fixture()` and `inspect_test.rs` `fixture()` (via
   `cp.pool_for_test()`), `commit_use_lease_gate.rs`, `recover_commit_gate.rs`,
   `staged_artifact_flow.rs`, `lease_commit_gate_e2e.rs`. Coordinator `mod_test.rs` has no
   fixture (it uses the shared `crate::cases::cp`, which stays unchanged): call it after
   `cp().await` in each `promote_terminal_artifacts_*` test that promotes.
3. Where a fixture already sets `default_staging_root_id = id` by SQL
   (`operator_execution_e2e.rs`, `published_grammar_execution.rs`, `voom_cli.rs`
   `configure_local_root`), add `default_output_root_id = id` to that same statement.
4. `artifact_envelope.rs`: after `activate_library_root`, call
   `cp.update_library_root(id, LibraryRootUpdate { default_staging_root_id: Some(Some(id)),
   default_output_root_id: Some(Some(id)), ..Default::default() })`.
5. `cargo test --workspace --all-features` → exit 0. Commit
   `test: configure staging and output defaults on commit fixtures`.

## Task 2 — resolver entry points

Verification (`Mode: focused-test`, file `operation_source_test.rs`, command
`cargo test -p voom-control-plane operation_source`):

- `pre_promotion_target_is_contained_by_the_staging_root` — source S with staging T and output
  O (sibling dirs, same library, active): target under T → `Ok((T, "x.mkv", _))`; target under
  O → `CONFIG_INVALID` "path escaped storage root T". Red before: resolves to O.
- `pre_promotion_target_without_staging_default_fails_closed` — S with only an output default →
  `CONFIG_INVALID` containing `storage root S` and `--staging-root`. Red before: succeeds.
- `output_target_without_output_default_fails_closed` — root R, no defaults, target inside R →
  `CONFIG_INVALID` containing `storage root R` and `--output-root <id>`. Red before: succeeds
  via fallback.
- `pre_promotion_target_rejects_cross_library_staging_root` — S's staging default in another
  library (SQL update bypassing config validation) → `VoomError::Database` "default staging
  root". Red before: no staging lookup.
- `chained_phase_staging_root_resolves_to_itself` — S names T as staging default, T has no
  own default: `resolve_pre_promotion_target(T, path under T)` → `Ok((T, ..))`.
- Coordinator `mod_test.rs` `promotion_resolves_the_artifact_roots_own_output_default` — the
  artifact root (9000001, no staging default) names a distinct output root O (new active root,
  same library, own dir containing the `--output-dir`); promotion lands the location on O. Red
  if `promotion.rs` called `resolve_pre_promotion_target` (no staging default) or used the
  fallback (root 9000001 recorded).
- Ported: the cross-library output test and the escape test call `resolve_output_target`; the
  escape test first sets the root's `default_output_root_id = id`.

Steps:

1. Write the five tests; run the command; expect compile failure (missing functions) — the red.
2. In `operation_source.rs` replace `resolve_artifact_target` and `artifact_target_root` with
   `resolve_pre_promotion_target(cp, label, source_storage_root_id, path)`,
   `resolve_output_target(cp, label, storage_root_id, path)` (both returning
   `Result<(StorageRootId, ProviderRelativeLocator, PathBuf), VoomError>`), private
   `resolve_target_in_root(cp, label, role, naming: &LibraryRoot, target_id, path)` (existing
   library check, `require_effective_local_root_path`, `canonical_new_leaf_no_symlink`,
   `rooted_target_address`) and `library_root(cp, id) -> Result<LibraryRoot, VoomError>`
   (`cp.libraries.get_library_root`, `NotFound("storage root <id>")`). Staging id comes from
   `crate::workflow::plan::envelope::destination_root(cp, DestinationRole::Staging, source)`;
   output id from `root.default_output_root_id` or the spec's `Config` message.
3. `prepare.rs` → `resolve_pre_promotion_target`; `promotion.rs` → `resolve_output_target`.
4. `envelope.rs` `destination_root` error → `no default {role} root configured for storage
   root {id}; configure one with `voom library root update --root-id {id} --{role}-root <id>``
   with `role = role.as_str()`. `binding.rs`: remove the `expect(dead_code)` on `Output`.
5. Commit `mod_test.rs`: rename `set_test_default_output_root` to
   `set_test_default_staging_root`, updating `default_staging_root_id`; its two callers' comment
   "default output root changed" → "default staging root changed".
6. Run the focused command → pass. Observe red per side, reverting each fault: restore
   `.unwrap_or(storage_root_id)` in `resolve_output_target` (output fail-closed test red); use
   `source_storage_root_id` as the containment root in `resolve_pre_promotion_target`
   (staging-containment and staging fail-closed tests red).
7. `just fmt-check && just lint && just test` → exit 0. Commit
   `fix(control-plane): contain commit targets in the staging root (ADR 0097)`.

## Task 3 — ADR 0097 note

Verification: `Mode: task-test-not-applicable` — append-only prose in an ADR; `just
check-adr-index` covers the record's structure. Append to ADR 0097 a `## Later decision:
commit resolves the staging root` section (issue #625): `prepare.rs` resolves its containment
root through `destination_root(Staging, source)` and records the committed location under it;
the Decision's role paragraph and "An unaddressable destination fails closed" sentences naming
"the source media root at commit" no longer hold; the output default is resolved at promotion
from the artifact's own root; a manual `voom artifact commit` lands in the staging root. Link
this spec. Commit `docs(adr): record the commit-side reading of ADR 0097`.

## Task 4 — full gate

`just ci` → exit 0 (Success 5). `just chaos-e2e-ci` when its tools are installed (it is the
only exerciser of `configure_local_root`); otherwise report it as not run. No commit unless a
fix is needed.

Rollback: revert the Task 2 commit; Task 1 is behavior-neutral on its own.
