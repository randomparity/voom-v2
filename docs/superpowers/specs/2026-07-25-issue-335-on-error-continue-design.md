# Issue 335: per-file `on_error: continue`

## Goal

Execute the published `abort|continue` error strategy honestly. `abort` remains
fail-fast. `continue` isolates terminal ticket failures to their files,
continues independent files through later phases, and returns a failed job with
a complete partial result.

## Review charter

- Base: `main`.
- Surface: workflow executor failure handling, phase-barrier coordinator
  finalization/reporting/promotion, and focused tests and corpus documentation.
- Direct dependencies: ADRs 0008, 0009, 0018, 0035, 0038; existing ticket
  states, terminal issues, file-phase rows, and job summaries.
- Exclusions: remux execution (#331, #332), other condition/default execution
  (#336-#339), metadata/tool changes, new DSL forms, and unpublished
  `on_error: skip`. Those concerns are blocking only if this design depends on
  or worsens them.
- Newly discovered independent gaps belong under #325.

## Invariants

1. Only a durably failed workflow ticket supplies a file isolation boundary.
   Database, bridge, validation, join, and inconsistent-state failures abort.
2. Continue mode dispatches every independent branch that can still run and
   returns only after active work is drained.
3. Each phase execution has a unique workflow invocation id. Ticket scheduling
   and failure detection are invocation-scoped; metrics remain job-scoped.
4. A planned file is committed, skipped, or blocked from durable terminal
   ticket state plus chain-tip advancement; it is never inferred from the
   aggregate error alone.
5. Failed and planner-blocked files do not enter later phases. Successful files
   retain their per-file run-gate history.
6. Every completed phase, including a continued partial phase, has per-file
   rows and a refreshed phase report.
7. Any continued ticket failure makes the final job failed and the public
   result an error with `partial: Some(...)`. Counts include all phases.
8. Only survivor assets are promoted. Failed files' earlier successful
   artifacts stay in working storage for diagnosis/recovery.
9. Resume excludes blocked files and never retries their terminal phase.
10. Existing compiled versions remain readable; source grammar does not change.

## Execution boundary

The executor keeps its existing abort entry point. A second caller-owned entry
point selects `ContinueIndependentBranches`. The run loop distinguishes:

- `TicketFailure`: the ticket is durably terminal failed; remember the first
  source and continue scheduling.
- `Fatal`: no trustworthy per-ticket boundary; drain active work, fail the job,
  and return immediately.

When all runnable tickets are terminal, a remembered ticket failure returns an
error and leaves the job open. Success still returns normally and leaves the
job open. A coordinator-supplied invocation id, derived from job id and phase
ordinal, scopes root tickets, ready-work, retry, completion, and first-failure
queries. Job summary refresh remains cumulative. The coordinator alone decides
the final shared-job status.

## Coordinator state

`PhaseLoop` retains the first continued error. A continued failed phase queries
each planned node's ticket states, finalizes all entering files, persists the
phase row/report, removes blocked files, and continues. An abort phase uses the
existing immediate partial-finalization path.

At loop completion, promotion is restricted to current survivor asset IDs. If
promotion succeeds and a continued error exists, the coordinator inserts the
cumulative summary, fails the job, and returns the accumulated phase/file rows
as the partial outcome. Promotion or persistence failures remain fatal and may
replace the final surfaced source because the requested terminal placement or
durable report did not complete. A later abort-strategy phase failure likewise
supersedes the retained continued error while preserving earlier partial rows.

## Failure and resume cases

- Mixed terminal failure: failed file blocked; siblings commit and continue.
- All files fail: phase persists as blocked, later phases have no participants,
  nothing is promoted, final job fails with partial rows.
- Pre-dispatch bridge/validation failure: whole job aborts; no isolation claim.
- Planner-blocked file: existing per-file block behavior remains and does not
  masquerade as a ticket failure.
- Resume from continued failure: blocked files are excluded; surviving tails
  and inherited run-gate history remain consistent.
- Repeated resume: terminal files remain absent and survivors never re-run
  recorded phases.

## Compatibility and rollout

No migration or new public syntax is required. Existing `abort`, implicit
abort, config defaults, phase overrides, and legacy compiled `skip` readability
remain. The runtime continues rejecting effective `skip`.

## Test strategy

- Executor tests prove abort still fails immediately, continuation dispatches
  undispatched siblings, leaves the job open only for ticket-isolated failure,
  fails the job for infrastructure/state errors, and does not rediscover an
  earlier phase's failed ticket in a later invocation.
- Coordinator tests prove mixed/all-fail classification, later-phase survivor
  execution, cumulative failed summary/partial result, survivor-only promotion,
  planner-block behavior, phase override precedence, and resume exclusion.
- Published grammar corpus continues compiling without new syntax.
- Focused tests and `just ci` are required before merge.

## Deferred concerns

No independent concern is deferred by this design. If review discovers one,
it must have an owning issue under #325 and a stated non-regression boundary
before approval.
