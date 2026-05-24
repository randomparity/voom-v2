# Issue 52 Scheduler Decision Insert SQL Design

## Goal

Reduce duplicated scheduler decision insert/upsert setup while preserving
suppression conflict behavior and durable row shape.

## Current State

`create_in_tx` and `create_or_suppress_in_tx` both validate the decision shape,
serialize the same timestamp and explanation fields, build nearly identical
`INSERT` statements, bind the same columns, and convert the returned row.
`create_or_suppress_in_tx` also pre-reads an existing suppression-key row to
check equivalence, even though the upsert `WHERE` clause already applies the
same equivalence constraints and returns no row on an incompatible conflict.

## Design

Add a small private helper that validates a `NewSchedulerDecision`, serializes
the shared bound values, and returns them for either insert path. Keep SQL
statements explicit, but define the common insert column/value prefix once so
selected and suppressed paths cannot drift.

Remove the suppression-equivalence pre-read. Rely on the existing
`ON CONFLICT(suppression_key) ... DO UPDATE ... WHERE ... RETURNING` result:
equivalent suppression updates return the folded row, while incompatible
suppression-key reuse returns no row and maps to the existing conflict error.

## Constraints

- No migration or schema change.
- Public `SchedulerDecision` fields and durable reason/outcome strings do not
  change.
- Incompatible suppression-key reuse still returns `Conflict`.
- Selected, idle, suppressed, and rejected decision tests continue to cover the
  refactored paths.

## Verification

- `cargo test -p voom-store scheduler_decisions`
- `cargo test -p voom-control-plane remote_acquire`
- `just lint`
