# Issue 49 Scheduler CLI DTO Mapping Design

## Goal

Reduce duplicated scheduler decision DTO mapping in the CLI without changing the
agent-facing JSON envelope for `voom scheduler decisions list` or
`voom scheduler decisions show`.

## Current State

`crates/voom-cli/src/commands/scheduler.rs` defines separate
`DecisionSummaryData` and `DecisionData` structs. They intentionally serialize
different shapes: list output omits `updated_at` and `explanation_json`, while
show output includes both. The two `From<SchedulerDecision>` implementations
duplicate the shared scalar field mapping.

## Design

Keep both public DTO structs so serde field order stays explicit and command
shape remains easy to inspect. Add a private `DecisionScalarData` struct that
maps the shared scalar fields from `SchedulerDecision` once. Convert
`SchedulerDecision` into `(DecisionScalarData, updated_at, explanation_json)`
before building either command DTO.

The list and show structs will continue to declare their serialized fields in
the exact current order. Their constructors will copy values from
`DecisionScalarData` instead of re-running the store-to-CLI mapping. This avoids
serde `flatten` because `show` currently places `updated_at` immediately after
`created_at`, and flattening a single common struct would move it after all
common fields.

## Constraints

- No public CLI fields are added, removed, or renamed.
- Durable store DTOs and repository code are unchanged.
- Snapshot compatibility is mandatory unless the test output proves the current
  snapshots intentionally need review.

## Verification

- `cargo test -p voom-cli --test scheduler_envelope`
- `cargo test -p voom-cli scheduler`
- `just lint`
