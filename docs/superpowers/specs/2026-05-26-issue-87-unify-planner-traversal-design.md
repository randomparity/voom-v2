# Issue 87 Planner Traversal Design

## Context

Issue #87 is still present in `crates/voom-plan/src/planner.rs`. The active
planning path already calls `snapshot_operations` before either normal operation
expansion or remux grouping. That helper evaluates nested `Conditional` and
`Rules` operations and returns a flat sequence of executable leaf operations or
leaf operations blocked by insufficient facts.

The older recursive handling still remains in `expand_operation_for_snapshot`
and `expand_rules_for_snapshot`, creating a second implementation of condition
and rule traversal that is no longer needed.

## Scope

- Keep `snapshot_operations` as the single traversal for snapshot-scoped policy
  operations.
- Remove the duplicate recursive `Conditional`/`Rules` expansion path.
- Preserve node ordering, blocked diagnostics, remux grouping behavior, and
  unsupported-operation behavior.
- Add rule-mode characterization tests so `First`, `All`, and unknown rule
  conditions are covered by the unified traversal.

## Design

`expand_operations_for_snapshot` remains the only entry point for per-snapshot
operation expansion. It receives the flattened `SnapshotOperation` values from
`snapshot_operations`, groups supported remux candidates across the flattened
sequence, and delegates each non-remux leaf to `expand_operation_for_snapshot`.

`expand_operation_for_snapshot` becomes leaf-only:

- supported leaves expand normally;
- unsupported leaves create blocked unsupported nodes;
- `Conditional` and `Rules` become unreachable through the public planning flow
  and are treated as unsupported if they ever reach the leaf function directly.

Removing `expand_rules_for_snapshot` eliminates the duplicate rule traversal.
The behavior source of truth is then `append_snapshot_operations` plus
`append_rule_operations`.

## Verification

Targeted checks:

```bash
cargo test -p voom-plan planner_test::rules
cargo test -p voom-plan planner_test::remux
cargo test -p voom-plan fixtures::tests::remux_track_selection
```

Full closeout:

```bash
just fmt-check
just ci
```
