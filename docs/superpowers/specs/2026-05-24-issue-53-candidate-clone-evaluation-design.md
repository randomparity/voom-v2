# Issue 53 Candidate Clone Evaluation Design

## Goal

Evaluate the remaining multi-operation scheduler candidate cloning and make the
selected tradeoff explicit.

## Current State

`score_remote_candidates` groups candidates by operation with
`HashMap<String, Vec<SchedulerCandidate>>`, cloning each candidate into its
operation bucket before calling `SchedulerScorer::score(&[SchedulerCandidate])`.
The clone no longer includes ticket payload data. Remote acquire candidate
breadth is intentionally controlled to ready tickets for the requesting worker,
as documented in the Sprint 9 design.

## Design

Keep the current scorer API and candidate bucket clones for now. Avoiding the
clones would require either an indexed/reference-based scoring path or changing
`voom-scheduler` to score borrowed groups. That would touch scorer API shape,
test fixtures, and selection logic for a small bounded cost in the current
remote-acquire path.

Document the tradeoff directly at the grouping site: the clone is intentional
because remote acquire candidate breadth is bounded and the scorer consumes a
simple homogeneous slice per operation. Revisit this only if candidate breadth
grows beyond the current single-worker ready-ticket snapshot.

## Constraints

- No behavior, scoring, reason aggregation, or deterministic tie-breaking
  changes.
- No scorer API change in this cycle.
- The comment must not imply clones are free; it should name the bounded-scope
  reason they are acceptable.

## Verification

- `cargo test -p voom-scheduler`
- `cargo test -p voom-control-plane remote_acquire`
- `just lint`
