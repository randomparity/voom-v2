# Shared check-constraint bypass helper

## Goal

Replace all 23 open-coded `ignore_check_constraints` test sequences with one
test-support helper while preserving the existing invalid-row fixtures and
pool-affinity behavior.

## Design

`voom-store::test_support::with_check_constraints_disabled` owns acquisition,
pragma setup, the caller operation, and connection drop. Its closure receives a
mutable `SqliteConnection`, so each dependent write is visibly pinned. The
helper returns the operation result and propagates SQL errors; callers retain
their existing test assertions and may unwrap at the test boundary.

The helper is available through the existing `voom-store/test` feature. The
control-plane dev dependency already enables that feature, so its policy tests
can use the same support surface without a production dependency change.

## Regression guard

`scripts/check-check-constraint-bypass.sh` scans Rust source for the pragma and
allows it only in `crates/voom-store/src/test_support.rs`. A self-test exercises
both an allowed helper location and a rejected test location. The guard is part
of `just ci`.

## Verification

Run the affected store and control-plane tests, the source guard and its
self-test, then the repository `just ci` suite.
