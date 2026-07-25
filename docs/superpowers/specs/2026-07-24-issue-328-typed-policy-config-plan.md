# Issue #328 implementation plan

Date: 2026-07-24

## Verification contract

Each task starts with a failing behavior test, then the smallest implementation
that makes it pass. Focused tests run before every commit. `prek run` and
`just ci` gate shipment.

## Task 1 — Parse and validate only published config settings

Files:

- `crates/voom-policy/src/syntax/ast.rs`
- `crates/voom-policy/src/syntax/parser.rs`
- sibling parser tests
- `crates/voom-policy/src/compile/validate.rs`
- sibling validation tests
- `crates/voom-policy/src/diagnostic.rs`

Steps:

1. Add parser tests for typed `languages` and `on_error` settings.
2. Add validation failures for wrong value shapes, unquoted languages,
   unpublished error strategies, duplicate keys, and unknown keys.
3. Retype `PolicyAst.config` to `Vec<SettingAst>` and reuse the settings-block
   parser.
4. Implement typed config validation and a duplicate-setting diagnostic.
5. Run `cargo test -p voom-policy syntax::` and focused validation tests.
6. Commit the parser/validation slice.

## Task 2 — Add the typed compiled config with legacy reads

Files:

- `crates/voom-policy/src/compile/compiled.rs`
- `crates/voom-policy/src/compile/lower/phases.rs`
- `crates/voom-policy/src/compile/lower/mod.rs`
- `crates/voom-policy/src/lib.rs`
- sibling compiled/pipeline tests

Steps:

1. Add tests for canonical typed JSON, missing fields, legacy raw strings, and
   malformed legacy values.
2. Introduce `CompiledConfig` with serde-defaulted languages and `on_error`.
3. Implement the narrow legacy compatibility deserializer.
4. Lower typed AST settings directly, preserving language order.
5. Add `CompiledPolicy::apply_execution_defaults()` and tests for policy
   fallback, phase override, and idempotence.
6. Call default application after compilation.
7. Run `cargo test -p voom-policy`.
8. Commit the compiled-contract slice.

## Task 3 — Normalize stored policies before planning

Files:

- `crates/voom-control-plane/src/cases/policy/plans.rs`
- `crates/voom-control-plane/src/cases/policy/compliance.rs`
- `crates/voom-control-plane/src/workflow/coordinator/planning.rs`
- sibling tests

Steps:

1. Add a stored-legacy-policy planning test.
2. Add coordinator tests proving policy continue is rejected before job open
   and phase abort overrides it.
3. Apply execution defaults in both stored-policy loaders before planning.
4. Keep the existing fail-loud rejection until #335.
5. Run the focused control-plane library and coordinator tests with all
   features.
6. Commit the execution-default slice.

## Task 4 — Canonical fixtures and compatibility evidence

Files:

- policy source fixtures using legacy config spellings
- compiled policy fixture JSON
- published grammar corpus fixtures/tests

Steps:

1. Rewrite only config spellings to the published grammar.
2. Regenerate compiled JSON with the repository compiler.
3. Confirm explicit `where` filters are unchanged in the compiled fixtures.
4. Run policy fixtures and `published_grammar_corpus`.
5. Commit fixture updates.

## Task 5 — Review and ship

1. Run strict Clippy on touched crates.
2. Run the adversarial review loop; fix defensible findings and re-review.
3. Run `prek run`.
4. Fetch and rebase on `origin/main` if necessary.
5. Run `just ci` after the rebase/current-main check.
6. Push, open a PR closing #328, wait for all required CI, and merge serially.
7. Remove workflow labels and clean the merged branch.
