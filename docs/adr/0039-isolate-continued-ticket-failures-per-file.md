---
status: accepted
date: 2026-07-25
deciders: [VOOM core]
---

# 0039 — Isolate continued ticket failures per file

## Context

The published grammar allows effective phase strategy `abort` or `continue`.
Typed policy config already supplies phase defaults and explicit phase values
override them (ADR 0035), but the coordinator still rejects `continue`.

The workflow executor currently stops scheduling after the first terminal
ticket, drains work already in flight, and marks the caller-owned job failed.
That behavior cannot satisfy `continue`: an independent file whose root ticket
was not dispatched would be abandoned, and a failed job cannot own later phase
tickets. The coordinator must also distinguish a file that failed from a
successful no-op before it can write honest per-file rows.

## Decision

1. The executor gains a caller-owned continuation mode used only for an
   effective `on_error: continue` phase. In that mode, a terminal failure
   durably attached to a workflow ticket is isolated to its branch. The
   executor continues scheduling other ready independent branches, drains all
   active work, returns the first ticket failure with the cumulative summary,
   and leaves the job open. Existing abort mode is unchanged.

2. Each coordinator phase supplies a unique workflow invocation id derived
   from the job and phase ordinal. Ready-work, completion, retry, and failed-
   ticket queries are scoped to that invocation. Durable ticket, retry, and
   failure counts remain job-cumulative; invocation-local dispatch/success
   telemetry is accumulated by the coordinator across phases. This prevents a
   failed ticket retained from a continued phase from being rediscovered as the
   error for every later phase without losing whole-run reporting.

3. Errors that cannot be assigned to a terminal ticket remain fatal and fail
   the job immediately. This includes plan validation, bridge errors, database
   or ticket-state inspection failures, task-join failures without a durable
   ticket identity, and impossible unfinished-work states. `continue` is not a
   general error-swallowing mode.

4. After a continued phase returns a ticket failure, the coordinator inspects
   the terminal ticket states for each planned policy node:

   - any failed ticket makes that file `Blocked`;
   - no failed ticket and an advanced chain tip makes it `Committed`;
   - no failed ticket and no advanced tip makes it `Skipped`;
   - a missing or non-terminal ticket state is an internal fatal error.

   Existing planner-blocked files remain `Blocked`; planner blocking is
   already per-file and does not itself turn a successful job into a ticket
   failure. A blocked or ticket-failed file is removed from later phases.

5. A continued ticket failure is retained as the run's primary error while
   later phases execute for survivors. Completed phases still receive their
   phase row and refreshed report. At the end, only artifacts associated with
   surviving branches are eligible for promotion. Durable file-phase ticket
   IDs and produced references define that association, including sidecars
   owned by synthetic root tickets. The coordinator writes the cumulative
   workflow summary, marks the job failed, and returns `CoordinatorError` with
   the complete partial outcome. It never reports success when any continued
   ticket failed. A later abort-strategy or infrastructure failure supersedes
   the retained ticket error because the requested continuation itself could
   not complete; the partial outcome still contains every earlier durable row.

6. Resume treats a `Blocked` file-phase row as terminal exactly as ADR 0009
   already specifies. A resumed job never re-enters the failed file. The
   inherited committed/skipped history introduced by ADR 0038 remains for
   surviving files only and needs no schema change.

7. The legacy compiled enum value `skip` remains readable but rejected before
   a job opens. `skip` is not published source syntax and receives no execution
   semantics.

## Consequences

- Abort behavior, public DSL syntax, and stored compiled-policy compatibility
  remain unchanged.
- Continue phases may finish with committed, skipped, and blocked files in one
  durable phase summary, while the final job is honestly failed.
- Terminal-failure issues remain owned by the existing atomic ticket-failure
  transition (ADR 0018); the coordinator does not create a second issue.
- Promotion must follow durable surviving-branch ticket and produced-result
  associations rather than collecting every successful ticket result.

## Considered and rejected alternatives

### Run every file as a separate job

Rejected because it breaks the one coordinator job and phase-barrier summary
model, complicates reports and promotion, and is unnecessary once terminal
ticket failures can be isolated by branch.

### Let the executor fail the job and keep writing later phases

Rejected because terminal jobs cannot accept the normal lifecycle of later
phase execution and the resulting record would violate the job state machine.

### Treat every executor error as continuable

Rejected because infrastructure and durable-state errors do not identify a
safe file boundary. Continuing after them could duplicate work or conceal
corruption.

### Promote every successful intermediate artifact

Rejected because an earlier artifact from a file that later failed is not that
run's successful terminal output.
