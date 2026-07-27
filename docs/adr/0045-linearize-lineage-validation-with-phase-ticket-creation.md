---
status: accepted
date: 2026-07-27
deciders: [VOOM core]
---

# 0045 — Linearize lineage validation with phase ticket creation

## Context

The phase-barrier coordinator resolves each input file's active version and
latest snapshot before opening a fresh or resumed job. It plans a phase from
those retained facts, then hands the bridged workflow to the executor. A
concurrent writer can append a newer live `file_versions` row for the same
`FileAsset` between resolution and dispatch. The executor would then create,
lease, and send tickets derived from a superseded version.

A standalone freshness query immediately before the executor call is not a
sufficient fence. Another writer can promote the lineage after that query and
before root-ticket creation. VOOM needs a durable dispatch commitment with an
unambiguous ordering relative to lineage promotion.

ADR 0001 makes tickets, rather than events, the durable work-routing authority.
The commit that creates and readies a phase invocation's root tickets is
therefore the earliest durable point at which dispatch has begun.

Design:
[`docs/superpowers/specs/2026-07-27-issue-352-superseded-dispatch-design.md`](../superpowers/specs/2026-07-27-issue-352-superseded-dispatch-design.md).

## Decision

For every phase invocation that has planned work, the executor performs these
steps in order:

1. Render all root-ticket specifications without writing durable state.
2. Open one SQLite `BEGIN IMMEDIATE` transaction.
3. Inside that transaction, compare every planned branch's expected
   `(file_asset_id, file_version_id)` with the greatest live version ID for the
   same asset.
4. If any expectation is stale or has no live version, return
   `STALE_IDENTITY_EVIDENCE` and roll back the transaction.
5. Otherwise create every root ticket, append its `ticket.created` event,
   promote it to `ready`, append its `ticket.ready` event, and commit once.

That commit is the phase's dispatch linearization point. `BEGIN IMMEDIATE`
serializes it with every lineage promotion transaction:

- a promotion committed before the guarded transaction is visible and causes
  rejection; and
- a promotion that obtains the write lock after the guarded commit occurred
  after dispatch began.

The store owns the active-version predicate. The coordinator supplies only
planned branches; blocked, skipped, compliant, and `run_if`-excluded branches
do not guard unrelated work. One stale planned branch aborts the whole phase
before any root ticket becomes durable, so no sibling can be partially sent
from that plan.

The production in-job executor entry point requires a validated, non-empty
`PlannedLineageGuard`. Its constructor rejects empty and duplicate-asset
expectations. There is no production unguarded in-job method for the
coordinator to call; unguarded executor helpers are test-only. This makes
omitting the guard a compile-time call-site failure rather than a convention.

`WorkflowRunError` records whether the guarded root batch committed. The
executor sets `dispatch_started = false` for plan validation, ticket rendering,
lineage rejection, or root-batch rollback, and `true` only after the
linearization commit. The coordinator carries a run summary into partial-phase
finalization only when this marker is true. A stale pre-dispatch failure
therefore cannot mistake the external version that caused rejection for an
artifact committed by this job.

Fresh and resumed runs use that one production entry point. Per ADR 0009,
resume opens a new job and new phase invocation. Tickets belonging to the prior
failed job are historical evidence and are never redispatched. Earlier phases'
tickets in the replacement job have distinct workflow IDs; every later phase
creates its own roots through the same guard.

## Consequences

- No phase ticket, lease, worker request, file-phase row, or phase summary can
  arise from a plan already stale at the dispatch linearization point.
- A stale run retains the normal failed job, `job.opened`/`job.failed` events,
  and file-run-start provenance. The rolled-back guarded transaction leaves no
  ticket lifecycle event. Its `dispatch_started = false` error prevents the
  partial finalizer from recording the externally promoted tip as a committed
  file-phase result.
- The error identifies the asset, expected version, observed active version
  (or absence), and tells the caller to replan or resume from current inputs.
- Root-ticket creation becomes atomic for coordinator invocations. Test-only
  unguarded executor entry points keep their existing behavior.
- The contract fences lineage version identity. It does not change the
  published DSL, compiled-policy JSON, input-set schema, policy semantics,
  worker protocol, or database schema.
- A newer version may commit after the root-ticket transaction. That is ordered
  after dispatch began; preventing such later promotion would require a
  long-lived lineage/use lease and heartbeat/recovery lifecycle outside this
  issue.
- Correlating a post-dispatch active-tip change with the exact job/ticket that
  produced it is the independent pre-existing provenance gap tracked by #378.
  This ADR does not infer new ownership: its pre-dispatch error is explicitly
  kept out of partial finalization, and post-dispatch behavior is unchanged.

## Considered and rejected alternatives

### Query immediately before calling the executor

Rejected. It leaves a time-of-check/time-of-use window before ticket creation.

### Replan automatically when a mismatch is found

Rejected. Automatic bounded replanning needs a durable retry budget and
operator-visible semantics. Failing before dispatch is deterministic and is one
of issue #352's accepted outcomes.

### Hold a blocking asset-use lease for the complete workflow

Rejected. It adds acquisition, heartbeat, expiry, crash recovery, release, and
commit-gate interactions. The ticket-commit linearization point provides the
required pre-dispatch ordering without that lifecycle.

### Validate when acquiring each worker lease

Rejected. Root tickets would already be durable, siblings could lease under
different lineage states, and the phase could partially dispatch before a
later stale branch is checked.

### Guard the input set's originally selected version

Rejected. Planning intentionally refreshes each member to its current active
chain tip. The expectation must be the exact active version whose snapshot
produced the phase plan, including tips produced by prior phases.
