# Issue #419 scope restructuring design

## Authority

This design records the operator-approved narrowing of issue #419 after its scope audit. It
supersedes only the affected implementation scope; it does not replace issue #419's original
acceptance criteria or ADR 0067's retained durable-scan decisions.

## Outcome

Issue #419 remains the durable scan-session vertical slice. It retains the public limit of
100,000 cumulative observations per session and functional boundary coverage, while unrelated
test infrastructure and performance policy leave the branch.

## Split

### SQLite test helper prerequisite

Move the pooled-connection constraint-reset repair and its focused regression out of #419 into
one prerequisite bugfix PR. Merge that PR first, then update #419 onto the resulting `main`.
Issue #419 may use the corrected helper but must not carry the helper's implementation diff.

### Performance follow-up

Remove the dedicated GitHub Actions scale-test step, the 25-second cross-platform completion SLO,
and timing assertions or normative prose that implement that SLO. File one follow-up issue that
owns deciding and implementing a performance budget and CI gate. Do not implement that follow-up
inside this campaign.

An ignored large-ledger diagnostic may remain only when it has no wall-clock assertion and does
not require a dedicated CI invocation.

## Retained #419 behavior

- Durable sessions, ordered idempotent batches, lifecycle transitions, and completion-only
  reconciliation remain.
- The 100,000-observation session cap remains a public contract.
- Functional tests prove 100,000 observations are accepted, a genuinely new crossing batch is a
  replayable `CONFLICT`, and exact accepted replay still succeeds at the cap.
- API and CLI inspection, owner-incarnation fencing, failure safety, and reconciliation evidence
  remain within the original issue authority.

## Bounded completion

After restructuring, fix only the two open correctness findings from branch-review iteration 3:

1. Reject a trigger-valid phantom current or future batch before replay/cache success by
   validating the parent session frontier.
2. Validate persisted `retired_location_count` against the attributed retirement rows on get,
   list, API, and CLI inspection paths.

Run one focused adversarial review limited to issue #419's original acceptance criteria and these
two fixes. A further finding is fixed in #419 only when it demonstrates that an original
acceptance criterion is false. Otherwise file it as follow-up work. The branch then runs focused
tests, repository guardrails, full `just ci`, and normal PR/CI/mergeability checks.

## Success criteria

- The test-helper repair is merged independently and absent from the #419 diff.
- The performance SLO and dedicated CI gate are absent from #419 and represented by a follow-up
  issue.
- The 100,000 functional cap contract and non-timing boundary tests remain.
- Both named correctness findings are resolved and the single focused review has no finding that
  invalidates an original #419 acceptance criterion.
- The final #419 branch is green and mergeable without new scope expansion.
