# Issue #401 Sliding File Window Design

## Goal

Replace whole-input phase barriers with a durable, bounded sliding admission
window. A file advances immediately after its preceding operation commits and
the refreshed chain-tip snapshot exists. A terminal file promotes and reclaims
safe intermediates before its slot admits another file.

This design is governed by
[ADR 0048](../../adr/0048-durable-sliding-file-window.md).

## Scope

Primary surface:

- `crates/voom-control-plane/src/workflow/coordinator/**`;
- workflow-summary/progress persistence in `voom-store`;
- migration `0028`;
- the compliance execution option and its narrow CLI plumbing;
- coordinator, repository, CLI, cancellation, resume, and integration tests;
- the execution runbook.

The worker scheduler, policy grammar, worker protocol, commit pipeline, CLI JSON
envelope, and compliance report wire types are unchanged.

## Required behavior

### Window admission

`ComplianceExecutionOptions::max_in_flight_files` and
`compliance execute --max-in-flight-files` accept a positive integer and
default to four. Zero fails before a job opens.

Job opening durably inserts one job-level window row containing the positive
configured limit and one progress row for every input. At most that durable
limit may be `active`; admission does not consult a mutable process default.
Admission is stable input-ordinal order, except resume prioritizes branches that
were active in the prior interrupted run before branches that were still
pending.

The coordinator fills empty slots while pending inputs remain. It refills one
only after the prior occupant is durably terminal.

### Per-file progression

An admitted file evaluates and executes phases in policy order. It never plans
phase `n + 1` before phase `n` has a durable file-phase row and, for a committed
operation, the new active version and its reprobe snapshot are available.

Gate semantics remain per file:

- `completed`: committed, verified, or skipped predecessor;
- `modified`: committed predecessor only.

Blocked planning ends only that file. `on_error: continue` isolates the failed
file and permits other admitted and pending files to continue. Abort strategy
stops new admission and fails the job after admitted dispatches drain.

Each file-phase executor invocation is uniquely scoped by job, input ordinal,
and phase ordinal. No invocation may claim another file's tickets.

### Terminal promotion and cleanup

After the last successful/verified phase:

- promote terminal main and sidecar locations immediately through the existing
  add-only, no-replace promotion path;
- never overwrite a destination;
- never modify or delete source media;
- delete only superseded same-lineage locations produced by this run (or its
  resumed predecessor), located under configured `.committed` working dirs;
- retain durable file versions, snapshots, tickets, commit records,
  verification evidence, file-phase summaries, and retired location rows;
- treat an already missing cleanup file as an idempotent interrupted cleanup;
- do not mark the file terminal or refill its slot until promotion and cleanup
  complete.

A promotion collision or unsafe cleanup candidate fails closed with the file
still non-terminal.

### Persistence and resume

Migration 0028 adds `workflow_file_windows` keyed by job and
`workflow_file_progress` keyed by job and branch. The former records the
positive capacity; progress records stable ordinal, admission state, next
phase, and transition times. Window/run-start/history/seed/progress creation is
atomic.

File-phase insertion and next-phase advancement are atomic and conditional on
the expected cursor. A duplicate completion returns the existing row without
advancing twice. A duplicate admission is impossible.

Resume validates progress against prior run starts and phase rows. It:

- seeds committed/verified work already durable;
- carries gate history;
- resumes at each branch's first incomplete phase;
- prioritizes previously active branches;
- terminalizes a completed-but-not-promoted branch without repeating a phase;
- carries every prior branch row and its ticket provenance into a
  terminalization replay;
- rejects phase-complete chain-tip drift and accepts incomplete drift only when
  exact prior-job committed-ticket evidence proves the new tip;
- rejects gaps, cursor disagreement, duplicate ordinals, lineage mismatch, or
  an input-set branch mismatch before opening the new job.

### Cancellation and reporting

Cancellation stops admission. Already durable per-file outcomes remain
queryable and resumable. No cancelled job is rewritten as failed.

The run keeps one job. File-phase rows persist as work completes. At drain, the
coordinator reconstructs available per-file completions from durable run
starts, history, rows, snapshots, and tickets, folds them by policy ordinal into
one phase summary/report, and merges invocation telemetry into one job summary.
Phase outcomes join entries and completion rows by exact branch id; carried
seed rows without entries cannot hide an entered branch that failed before its
completion row.
The fold must not depend on an in-memory completion log lost on process
interruption. Existing report and envelope fields keep their meanings.

## Failure modes

- No worker/capacity: admitted file waits through existing executor
  backpressure; unrelated admitted files may progress.
- Planner block: durable blocked row, no promotion, slot released.
- Continued ticket failure: failing file blocks; siblings and refill continue;
  final job error retains partial coherent summaries.
- Abort ticket failure or invariant violation: no refill, drain active work,
  preserve rows, fail the job.
- Promotion collision: fail closed; destination is untouched.
- Cleanup I/O/database interruption: leave progress non-terminal; retry/resume
  recognizes already removed bytes and completes durable retirement.
- Process interruption: prior file rows and progress determine resume priority
  and cursors; lineage guards reject a duplicate mutation if a late prior
  commit advanced the chain tip.

## Acceptance tests

1. A controlled slow phase proves file A enters phase 2 while file B remains in
   phase 1.
2. Instrumented progress proves active count never exceeds the configured
   limit and a terminal slot refills only after promotion/cleanup.
3. Separate tests cover success, planning-blocked, continue-on-error, terminal
   abort failure, and cancellation.
4. A terminal file appears in the output directory before a slow sibling
   finishes.
5. Its superseded intermediate disappears immediately, while source bytes and
   durable evidence remain.
6. Resume after partial execution does not create tickets for completed phases,
   does not admit a branch twice, and processes completed-but-unpromoted work.
7. Repository tests reject missing/zero durable limits, duplicate ordinals,
   cursor mismatch, over-window admission, and invalid state/timestamp
   combinations.
8. CLI tests prove default/explicit limit plumbing without changing the JSON
   envelope.
9. Focused tests and `just ci` pass with zero warnings.

The issue #339 real-media rerun is an operator acceptance dependency after this
change; it is not executed in this PR.
