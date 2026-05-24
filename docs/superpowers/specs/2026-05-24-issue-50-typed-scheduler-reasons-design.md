# Issue 50 Typed Scheduler Reasons Design

## Goal

Stop exposing known scheduler outcomes from `voom-scheduler` as raw reason
strings while preserving the durable `scheduler_decisions.reason_code` strings.

## Current State

`voom-scheduler` returns `ScoreDecision.reason_code` as `&'static str` and stores
candidate rejection reasons as string values in the explanation JSON. Both
`voom-scheduler` and `voom-control-plane` maintain local string priority tables.
`voom-store` owns `SchedulerReasonCode` for durable persistence and validates
database values against that vocabulary.

## Design

Add a scheduler-owned `ScoreReasonCode` enum in `voom-scheduler` for scorer
outcomes and hard-gate rejection reasons. `ScoreDecision.reason_code` becomes
`ScoreReasonCode`. The scorer will keep explanation JSON unchanged by serializing
reason enums through `ScoreReasonCode::as_str()`, so public decision output and
durable reason strings remain stable.

`voom-control-plane` will import `ScoreReasonCode` and replace its duplicate
string priority/parser helpers with scheduler-owned helpers. The durable
conversion to `voom_store::repo::scheduler_decisions::SchedulerReasonCode` stays
in control-plane code as an exhaustive `match`, preserving crate layering:
`voom-scheduler` does not depend on `voom-store`, and store repositories do not
learn scorer internals.

Capacity recheck decisions created directly in the control plane continue to use
store `SchedulerReasonCode` because they are persistence decisions, not scorer
decisions.

## Constraints

- Durable `reason_code` strings do not change.
- Explanation JSON reason arrays keep string values.
- Adding a new scheduler reason must require an explicit control-plane
  conversion before it can persist.
- No store schema or migration change is required.

## Verification

- `cargo test -p voom-scheduler`
- `cargo test -p voom-control-plane remote_acquire`
- `cargo test -p voom-store scheduler_decisions`
- `just lint`
