# Issue #346 implementation plan

Base: `main` at `4fc6bf74e00dba51b253d084c7cb7654cf3df47c`

## Charter

Deliver the design in
`2026-07-27-issue-346-published-phase-on-error-design.md`. The permitted
surface is phase `on_error` validation/lowering, their focused `voom-policy`
tests, and these design artifacts. Durable enum and config-reader changes are
excluded because compatibility requires them to remain intact. Runtime
execution remains owned by #335; full-corpus execution remains owned by #338.

## Step 1 — Pin source and durable compatibility behavior

Files:

- `crates/voom-policy/src/compile/validate_test.rs`
- `crates/voom-policy/src/compile/compiled_test.rs`
- `crates/voom-policy/fixtures/compiled/legacy-phase-on-error-skip-v2.json`

Add behavior tests that:

- compile both published phase values and assert exact typed strategies;
- reject `skip` in colon and colonless forms;
- reject colonless `abort` and `continue`;
- reject missing/trailing values, text before the colon, double colons,
  quoted/case variants, and non-ASCII whitespace in every position including
  after valid values immediately before newline and `}`;
- assert `invalid_on_error_value` for every rejected source;
- validate a parsed `on_error` AST against a shorter/differently aligned source
  and assert a diagnostic rather than a panic;
- deserialize an immutable complete compiled-policy fixture with a phase
  `"skip"` value and assert exact Value-level reserialization;
- assert the accepted strategies serialize exactly as `"abort"` and
  `"continue"`.

Expected red: the unpublished and colonless phase-source cases currently
compile successfully. The durable compatibility test should already pass and
serves as a non-regression pin.

Focused command:

```text
cargo test -p voom-policy
```

Commit boundary: tests and the minimal implementation in Step 2 land together
so no commit leaves repository guardrails red.

## Step 2 — Restrict validation and source lowering

Files:

- `crates/voom-policy/src/compile/validate/operations.rs`
- `crates/voom-policy/src/compile/validate.rs`
- `crates/voom-policy/src/compile/lower/phases.rs`

Replace phase validation's compatibility-oriented `setting_value` behavior
with a phase-specific complete-statement check over a bounds-checked
`Validator::source.get(statement.span().start..statement.span().end)`: exact
keyword, only ASCII whitespace before one mandatory colon, only ASCII
whitespace around the value, and exactly `abort|continue`. Extraction failure
returns `InvalidOnErrorValue` rather than panicking. Update the validator call
site so normalized statement text cannot hide trailing Unicode whitespace.
Return the same diagnostic for every rejected shape. Remove the `skip` arm from
source lowering without changing `ErrorStrategy`, serde derives, or
`CompiledConfig` decoding.

Expected green:

```text
cargo test -p voom-policy
just fmt-check
just lint
```

Re-read the diff for source/compiled boundary leakage, then commit the complete
behavior change using a Conventional Commit subject.

## Step 3 — Review and ship

Review the complete branch diff against `main` for grammar precision, hostile
input handling, and rollback compatibility. Run a security-focused parser
review and a simplification review. Resolve all material findings.

Full verification:

```text
just ci
```

Push without force, open a PR that closes #346, wait for green CI and clean
mergeability, then hand the PR to the campaign orchestrator without merging.
After the serial campaign rebases this branch onto #344, re-run the focused
wire/source matrix and `just ci` before merge.
