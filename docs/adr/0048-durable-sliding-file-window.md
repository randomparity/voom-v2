# ADR 0048: Coordinate policy execution through a durable sliding file window

Status: Accepted

Date: 2026-07-28

Issue: #401

## Context

The phase-barrier coordinator plans and executes one phase across every active
input before any file enters the next phase. That preserves per-file phase
order, but it couples unrelated files and retains every committed intermediate
until the complete input finishes. The issue #339 canary retained 26 GiB in
staging while producing 6.3 GiB of final output.

The executor already owns operation capacity through durable tickets, leases,
and worker grants. The coordinator needs a separate bound on files whose
intermediate lineage may occupy staging. That bound must survive interruption
and must not become a second worker scheduler.

ADR 0007 deliberately chose one `plan_phase` call per phase across the complete
active set. A sliding window cannot retain that choice: a fast file must plan
its next phase from newly probed facts while a slow sibling is still executing
the preceding phase.

## Decision

### One job, independent ordered file pipelines

One coordinator job continues to own the complete execution and its job,
phase, and file-phase reporting. Each admitted file runs its policy phases
sequentially:

1. evaluate the file's gate from its durable/inherited phase history;
2. plan only that file and phase from its current chain-tip snapshot;
3. dispatch the resulting invocation in the shared job;
4. persist the file-phase outcome;
5. refresh the file's version and probe snapshot before planning its next
   phase.

Different admitted files may be in different phases. Each file uses a unique,
deterministic workflow invocation identity derived from the job, input ordinal,
and phase ordinal. Concurrent executor calls therefore cannot claim or inspect
another file's tickets. Tickets and leases remain the only operation scheduler.

This supersedes ADR 0007 only where it requires one whole-input `plan_phase`
call and a barrier between phases. ADR 0007's single-job ownership, executor
reuse, chain-tip authority, and append-only failure consequences remain.

### Durable admission and cursor state

Migration 0028 adds `workflow_file_progress`, one row for every
`(job_id, branch_id)`, with:

- stable input ordinal;
- state `pending`, `active`, or `terminal`;
- next phase ordinal;
- admitted and terminal timestamps.

The row has a composite foreign key to `workflow_file_run_starts`. Job opening
inserts run starts, inherited history, reconciliation seeds, and progress rows
in one transaction. A transaction may move the next ordinal `pending` row to
`active` only when the active count is below the job's configured file-window
limit. The primary key prevents duplicate admission, and the ordinal gives
restart a deterministic refill order.

A file-phase row and the progress cursor advance in the same transaction.
First-write-wins file-phase persistence plus the expected current cursor makes
replayed completion idempotent. Resume still opens a new job per ADR 0009 and
ADR 0037, but it derives each new progress cursor from validated prior rows and
admits interrupted branches before untouched pending branches. Completed
operations are seeded or inherited, not dispatched again.

The configured `max_in_flight_files` is a positive execution option and CLI
argument. It defaults to four. It bounds admitted file pipelines, not tickets,
leases, or operations.

### Terminalization precedes refill

A successful file does not release its slot when its final phase row lands. It
first:

1. promotes every terminal main/sidecar artifact scoped to that file using the
   existing add-only promotion path;
2. identifies earlier same-lineage produced locations under coordinator-owned
   working directories;
3. removes only those retired intermediate files, then retires their durable
   locations while retaining versions, snapshots, commit records, verification
   records, tickets, and file-phase rows as evidence; and
4. marks the progress row `terminal`.

Missing cleanup files are treated as an interrupted cleanup and completed
idempotently. Source locations are never candidates because cleanup requires a
job-produced file-phase location under a configured committed working
directory, and the active chain-tip location is excluded. Promotion or cleanup
failure leaves the row active, fails the run, and prevents slot refill.

Blocked planning and continued per-file ticket failure terminalize without
promotion. An abort-strategy failure stops further admission and drains the
already admitted work. External cancellation likewise stops admission; the
job remains cancelled and its committed file-phase rows remain resumable.

### Coherent summaries without barriers

Per-file rows remain incremental and authoritative. The coordinator collects
the refreshed snapshot and gate decision for every completed file phase. When
the run drains—successfully, after cancellation, or after failure—it folds the
available completions by phase ordinal into the existing phase summaries and
reports. A phase report therefore contains every file that actually entered
that phase, regardless of completion order. Job counters continue to come from
job-scoped durable tickets and the merged invocation telemetry.

No CLI JSON envelope or compliance report type changes.

## Consequences

- A fast file may execute, verify, promote, and reclaim intermediates while a
  slow sibling remains in an earlier phase.
- Staging residency is bounded by the configured admitted-file window rather
  than total input count.
- Planning performs more small calls, one per file phase, instead of one large
  call per phase. This is the necessary cost of refreshed per-file progression.
- The progress table duplicates the next ordinal derivable from rows, but makes
  admission and cursor transitions atomic and inspectable. Resume validates
  both representations and fails closed on disagreement.
- Phase reports become drain-time folds rather than barrier-time writes.
  File-phase rows continue to expose partial progress while execution runs.

## Considered and rejected

### Fixed cohorts

Rejected because a fast file cannot release its cohort's slot until the slowest
cohort member finishes. Staging remains coupled to stragglers.

### In-memory semaphore without durable progress

Rejected because restart could neither prove which files occupied the window
nor distinguish first admission from replay.

### One job per file

Rejected because it fragments the existing job/reporting contract and makes
whole-run cancellation and inspection incoherent.

### Keep whole-input planning and overlap only dispatch

Rejected because a later phase must use each file's newly probed chain-tip
facts. Waiting to form a whole-input plan recreates the barrier.

### Delete every retired file-version location

Rejected because it could delete sources or artifacts outside this run and
would erase recovery inputs. Cleanup is limited to this run's earlier produced
locations under coordinator-owned working directories.
