# Issue 51 Remote Acquire Capacity Recheck Design

## Goal

Make selected remote-acquire capacity behavior explicit without weakening lease
creation correctness.

## Current State

Remote acquire builds scheduler candidates from worker and node capacity facts,
scores them, then re-reads the selected worker-operation and node capacity inside
the transaction immediately before creating the lease. If either selected limit
is full at that point, the code records a capacity-full no-candidate decision
instead of creating a lease.

## Design

Keep the second read. It is an intentional transaction-local recheck between
advisory scoring facts and the durable lease mutation. Scoring explains why a
candidate looked eligible when considered; the selected-path recheck protects the
actual lease write from stale capacity observations.

Document this directly at the selected-path recheck site in
`crates/voom-control-plane/src/cases/remote_execution.rs`. Do not carry the
candidate capacity facts through to lease creation, because doing so would make
lease creation depend on earlier advisory observations rather than current
transaction-local facts.

## Constraints

- Remote acquire behavior, decision IDs, and decision shapes do not change.
- Worker and node capacity-full decisions still use the rechecked active/limit
  values observed immediately before lease creation.
- No store, scheduler, or schema changes are required.

## Verification

- `cargo test -p voom-control-plane remote_acquire`
- `cargo test -p voom-control-plane node_default_limit_blocks_second_concurrent_remote_acquire`
- `just lint`
